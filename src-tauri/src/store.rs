//! 本地 SQLite 存储：建库、迁移、设置读写。
//!
//! 数据库位于 `app_data_dir()/data.db`（Windows 上是
//! `%APPDATA%\com.asonnychen.vpm-search\data.db`）。
//! 测试全部使用临时文件数据库，不触碰真实 `%APPDATA%`。

use rusqlite::Connection;
use serde::de::DeserializeOwned;
use std::path::Path;
use std::time::Duration;

use crate::settings::{Settings, SETTING_KEYS};
use crate::sources::parse::{CatalogSource, PoolCandidate};

/// 当前 schema 版本，写进 SQLite 的 `user_version`。
pub const SCHEMA_VERSION: i64 = 2;

/// 阶段 2 建好五张表；本期只有 `settings` 真正读写。
const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE IF NOT EXISTS pool_source (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    url         TEXT NOT NULL UNIQUE,
    name        TEXT NOT NULL DEFAULT '',
    entry_count INTEGER NOT NULL DEFAULT 0,
    broken      INTEGER NOT NULL DEFAULT 0,
    checked     INTEGER NOT NULL DEFAULT 1,
    fetched_at  TEXT,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE IF NOT EXISTS pool_entry (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    host       TEXT NOT NULL,
    port       INTEGER NOT NULL,
    alias      TEXT NOT NULL DEFAULT '',
    source_id  INTEGER REFERENCES pool_source(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (host, port)
);

CREATE TABLE IF NOT EXISTS test_run (
    id            TEXT PRIMARY KEY,
    started_at    TEXT NOT NULL,
    finished_at   TEXT,
    params_json   TEXT NOT NULL DEFAULT '{}',
    total         INTEGER NOT NULL DEFAULT 0,
    ok_count      INTEGER NOT NULL DEFAULT 0,
    timeout_count INTEGER NOT NULL DEFAULT 0,
    fail_count    INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS test_result (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id     TEXT NOT NULL REFERENCES test_run(id) ON DELETE CASCADE,
    host       TEXT NOT NULL,
    port       INTEGER NOT NULL,
    alias      TEXT NOT NULL DEFAULT '',
    status     TEXT NOT NULL,
    latency_ms INTEGER,
    speed_mbps REAL,
    exit_ip    TEXT,
    geo_zh     TEXT,
    geo_en     TEXT,
    colo       TEXT,
    error      TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_test_result_run ON test_result(run_id);
"#;

/// 阶段 3：源记录失败原因，池条目保存来源标识（手动来源没有 source_id）。
const SCHEMA_V2: &str = r#"
ALTER TABLE pool_source ADD COLUMN last_error TEXT;
ALTER TABLE pool_entry ADD COLUMN source_label TEXT NOT NULL DEFAULT '';
"#;

#[derive(Debug)]
#[allow(dead_code)] // 阶段 3 起会用到全部变体
pub enum StoreError {
    Sqlite(rusqlite::Error),
    Json(serde_json::Error),
    Io(std::io::Error),
    UnknownKey(String),
    InvalidSettings(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Sqlite(e) => write!(f, "SQLite 错误：{e}"),
            StoreError::Json(e) => write!(f, "JSON 错误：{e}"),
            StoreError::Io(e) => write!(f, "文件错误：{e}"),
            StoreError::UnknownKey(key) => write!(f, "未知的设置 key：{key}"),
            StoreError::InvalidSettings(message) => write!(f, "设置校验失败：{message}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(value: rusqlite::Error) -> Self {
        StoreError::Sqlite(value)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(value: serde_json::Error) -> Self {
        StoreError::Json(value)
    }
}

impl From<std::io::Error> for StoreError {
    fn from(value: std::io::Error) -> Self {
        StoreError::Io(value)
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

pub struct Store {
    conn: Connection,
}

impl Store {
    /// 打开（必要时创建）数据库并跑迁移。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        Self::configure(&conn)?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// 仅用于测试。
    #[allow(dead_code)] // 阶段 3 起使用
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::configure(&conn)?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn configure(conn: &Connection) -> Result<()> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(())
    }

    /// 可重复执行：已是最新版本时直接返回，不重建表、不丢数据。
    pub fn migrate(&self) -> Result<()> {
        let version: i64 = self.schema_version()?;
        if version >= SCHEMA_VERSION {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        if version < 1 {
            tx.execute_batch(SCHEMA_V1)?;
        }
        if version < 2 {
            tx.execute_batch(SCHEMA_V2)?;
        }
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        tx.commit()?;
        Ok(())
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }

    #[allow(dead_code)] // 阶段 3 起使用
    pub fn table_names(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut names = Vec::new();
        for name in rows {
            names.push(name?);
        }
        Ok(names)
    }

    /// 读取一个设置 key。未知 key 返回 `None`，不当作错误。
    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        if !SETTING_KEYS.contains(&key) {
            return Ok(None);
        }
        let mut stmt = self
            .conn
            .prepare_cached("SELECT value FROM settings WHERE key = ?1")?;
        let mut rows = stmt.query([key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// 写入一个设置 key。未知 key 直接拒绝，避免脏数据落库。
    #[allow(dead_code)] // 阶段 3 起使用
    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        if !SETTING_KEYS.contains(&key) {
            return Err(StoreError::UnknownKey(key.to_string()));
        }
        self.upsert(key, value)
    }

    #[allow(dead_code)] // 由 set_setting 与测试使用
    fn upsert(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value, updated_at)
             VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
             ON CONFLICT(key) DO UPDATE SET
                 value = excluded.value,
                 updated_at = excluded.updated_at",
            rusqlite::params![key, value],
        )?;
        Ok(())
    }

    /// 读整份设置：缺失或单个字段损坏时只回落该字段的默认值。
    pub fn load_settings(&self) -> Result<Settings> {
        let defaults = Settings::default();
        Ok(Settings {
            last_input: self
                .read_json("last_input")?
                .unwrap_or(defaults.last_input),
            node_templates: self
                .read_json("node_templates")?
                .unwrap_or(defaults.node_templates),
            max_latency_ms: self
                .read_json("max_latency_ms")?
                .unwrap_or(defaults.max_latency_ms),
            connect_timeout_s: self
                .read_json("connect_timeout_s")?
                .unwrap_or(defaults.connect_timeout_s),
            request_timeout_s: self
                .read_json("request_timeout_s")?
                .unwrap_or(defaults.request_timeout_s),
            concurrency: self
                .read_json("concurrency")?
                .unwrap_or(defaults.concurrency),
            latency_samples: self
                .read_json("latency_samples")?
                .unwrap_or(defaults.latency_samples),
            speed: self.read_json("speed")?.unwrap_or(defaults.speed),
            column_widths: self
                .read_json("column_widths")?
                .unwrap_or(defaults.column_widths),
            log_retention: self
                .read_json("log_retention")?
                .unwrap_or(defaults.log_retention),
            ui: self.read_json("ui")?.unwrap_or(defaults.ui),
        })
    }

    fn read_json<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let Some(raw) = self.get_setting(key)? else {
            return Ok(None);
        };
        // 单字段损坏不应让整份设置失效，回落默认值即可。
        Ok(serde_json::from_str(&raw).ok())
    }

    /// 整份写入，单事务完成，避免只写了一半。
    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        settings
            .validate()
            .map_err(|e| StoreError::InvalidSettings(e.to_string()))?;

        let pairs = settings_to_pairs(settings)?;
        let tx = self.conn.unchecked_transaction()?;
        for (key, value) in pairs {
            tx.execute(
                "INSERT INTO settings (key, value, updated_at)
                 VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
                 ON CONFLICT(key) DO UPDATE SET
                     value = excluded.value,
                     updated_at = excluded.updated_at",
                rusqlite::params![key, value],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// 把页面上的源清单写进 `pool_source`。
    ///
    /// 新源按页面状态决定是否默认勾选；已有源只更新名称与异常标记，
    /// 保留用户自己的勾选、条目数与最近抓取时间。
    pub fn upsert_catalog(&self, sources: &[CatalogSource]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for source in sources {
            tx.execute(
                "INSERT INTO pool_source (url, name, broken, checked)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(url) DO UPDATE SET
                     name = excluded.name,
                     broken = excluded.broken",
                rusqlite::params![
                    source.url,
                    source.name,
                    i64::from(source.broken),
                    i64::from(!source.broken),
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_pool_sources(&self) -> Result<Vec<PoolSourceRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT url, name, checked, broken, entry_count, fetched_at, last_error
             FROM pool_source
             ORDER BY id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(PoolSourceRow {
                url: row.get(0)?,
                name: row.get(1)?,
                checked: row.get::<_, i64>(2)? != 0,
                broken: row.get::<_, i64>(3)? != 0,
                entry_count: row.get::<_, i64>(4)?.max(0) as u32,
                fetched_at: row.get(5)?,
                last_error: row.get(6)?,
            })
        })?;
        let mut sources = Vec::new();
        for row in rows {
            sources.push(row?);
        }
        Ok(sources)
    }

    /// 用户勾选状态落库。
    pub fn set_source_checked(&self, url: &str, checked: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE pool_source SET checked = ?2 WHERE url = ?1",
            rusqlite::params![url, i64::from(checked)],
        )?;
        Ok(())
    }

    /// 用本次抓取的结果整体替换池内容。池是快照，不做增量合并。
    pub fn replace_pool_entries(&self, candidates: &[PoolCandidate], added_at: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM pool_entry", [])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO pool_entry (host, port, alias, source_id, source_label, created_at)
                 VALUES (?1, ?2, ?3,
                         (SELECT id FROM pool_source WHERE url = ?5),
                         ?4, ?6)
                 ON CONFLICT(host, port) DO UPDATE SET
                     alias = excluded.alias,
                     source_label = excluded.source_label",
            )?;
            for candidate in candidates {
                stmt.execute(rusqlite::params![
                    candidate.host,
                    candidate.port,
                    candidate.alias,
                    candidate.source,
                    candidate.source_url,
                    added_at,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_pool_entries(&self) -> Result<Vec<PoolEntryRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT host, port, alias, source_label, created_at
             FROM pool_entry
             ORDER BY id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(PoolEntryRow {
                host: row.get(0)?,
                port: row.get::<_, i64>(1)?.clamp(0, u16::MAX as i64) as u16,
                alias: row.get(2)?,
                source: row.get(3)?,
                added_at: row.get(4)?,
            })
        })?;
        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }
        Ok(entries)
    }

    /// 记录单个源本次抓取的结果：条目数、错误、时间。
    pub fn record_source_fetch(
        &self,
        url: &str,
        entry_count: u32,
        error: Option<&str>,
        fetched_at: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE pool_source
             SET entry_count = ?2, last_error = ?3, fetched_at = ?4
             WHERE url = ?1",
            rusqlite::params![url, i64::from(entry_count), error, fetched_at],
        )?;
        Ok(())
    }

    /// 池里最近一次成功抓取的时间；从未抓取过则为 `None`。
    pub fn last_fetched_at(&self) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT MAX(fetched_at) FROM pool_source", [], |row| {
                row.get(0)
            })?)
    }
}

/// `pool_source` 的一行，直接给前端用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolSourceRow {
    pub url: String,
    pub name: String,
    pub checked: bool,
    pub broken: bool,
    pub entry_count: u32,
    pub fetched_at: Option<String>,
    pub last_error: Option<String>,
}

/// `pool_entry` 的一行，`source` 是来源标识。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolEntryRow {
    pub host: String,
    pub port: u16,
    pub alias: String,
    pub source: String,
    pub added_at: String,
}

fn settings_to_pairs(settings: &Settings) -> Result<Vec<(&'static str, String)>> {
    Ok(vec![
        ("last_input", serde_json::to_string(&settings.last_input)?),
        (
            "node_templates",
            serde_json::to_string(&settings.node_templates)?,
        ),
        (
            "max_latency_ms",
            serde_json::to_string(&settings.max_latency_ms)?,
        ),
        (
            "connect_timeout_s",
            serde_json::to_string(&settings.connect_timeout_s)?,
        ),
        (
            "request_timeout_s",
            serde_json::to_string(&settings.request_timeout_s)?,
        ),
        ("concurrency", serde_json::to_string(&settings.concurrency)?),
        (
            "latency_samples",
            serde_json::to_string(&settings.latency_samples)?,
        ),
        ("speed", serde_json::to_string(&settings.speed)?),
        (
            "column_widths",
            serde_json::to_string(&settings.column_widths)?,
        ),
        (
            "log_retention",
            serde_json::to_string(&settings.log_retention)?,
        ),
        ("ui", serde_json::to_string(&settings.ui)?),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Settings, DEFAULT_COLUMN_WIDTHS};

    fn temp_db() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.db");
        (dir, path)
    }

    #[test]
    fn 首次启动建好五张表并可重复迁移() {
        let (_dir, path) = temp_db();
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);

        let mut tables = store.table_names().unwrap();
        tables.sort();
        assert_eq!(
            tables,
            vec![
                "pool_entry".to_string(),
                "pool_source".to_string(),
                "settings".to_string(),
                "test_result".to_string(),
                "test_run".to_string(),
            ]
        );

        store
            .set_setting("max_latency_ms", "321")
            .expect("写入已知 key 应成功");
        drop(store);

        // 第二次启动：迁移不报错，数据还在。
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.get_setting("max_latency_ms").unwrap().as_deref(),
            Some("321")
        );
    }

    #[test]
    fn 设置可以整份往返() {
        let store = Store::open_in_memory().unwrap();
        let mut settings = Settings::default();
        settings.last_input.source_urls =
            "https://bestcf.pages.dev/domain/all.txt".to_string();
        settings.last_input.manual_addresses = "162.159.198.1:8443#备注".to_string();
        settings.max_latency_ms = 260;
        settings.connect_timeout_s = 7;
        settings.request_timeout_s = 12;
        settings.concurrency = 16;
        settings.latency_samples = 3;
        settings.speed.streams = 6;
        settings.speed.size_mb = 20;
        settings.speed.timeout_s = 25;
        settings.column_widths = DEFAULT_COLUMN_WIDTHS.map(|w| w + 4).to_vec();
        settings.log_retention.operation_days = 7;
        settings.log_retention.app_days = 14;
        settings.log_retention.singbox_days = 60;

        store.save_settings(&settings).unwrap();
        assert_eq!(store.load_settings().unwrap(), settings);
    }

    #[test]
    fn 未知_key_不写入也不报错读取() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.get_setting("不存在的 key").unwrap(), None);
        assert!(matches!(
            store.set_setting("不存在的 key", "1"),
            Err(StoreError::UnknownKey(_))
        ));
        // 拒绝后库里不应留下痕迹。
        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM settings", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn 单个字段损坏时只回落该字段默认值() {
        let store = Store::open_in_memory().unwrap();
        store.save_settings(&Settings::default()).unwrap();
        store
            .set_setting("max_latency_ms", "\"不是数字\"")
            .unwrap();

        let loaded = store.load_settings().unwrap();
        assert_eq!(loaded.max_latency_ms, Settings::default().max_latency_ms);
        assert_eq!(loaded.concurrency, Settings::default().concurrency);
    }

    #[test]
    fn 从阶段二的旧库升级会补上新列且不丢数据() {
        let (_dir, path) = temp_db();
        {
            // 模拟阶段 2 的库：只有 V1 的表结构。
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
            conn.execute(
                "INSERT INTO pool_source (url, name) VALUES ('https://a/all.txt', '旧源')",
                [],
            )
            .unwrap();
        }

        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let sources = store.list_pool_sources().unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].name, "旧源");
        assert_eq!(sources[0].last_error, None);
        // 新列可写。
        store
            .record_source_fetch("https://a/all.txt", 2, Some("HTTP 404"), "2026-11-13T00:00:00.000Z")
            .unwrap();
    }

    fn catalog() -> Vec<CatalogSource> {
        vec![
            CatalogSource {
                url: "https://bestcf.pages.dev/domain/all.txt".to_string(),
                name: "BestCF·domain/all.txt".to_string(),
                broken: false,
            },
            CatalogSource {
                url: "https://bestcf.pages.dev/luoli/all.txt".to_string(),
                name: "BestCF·luoli/all.txt".to_string(),
                broken: true,
            },
        ]
    }

    fn candidate(host: &str, port: u16, source_url: Option<&str>) -> PoolCandidate {
        PoolCandidate {
            host: host.to_string(),
            port,
            alias: format!("{host} 的备注 | 来源"),
            source: "BestCF·domain/all.txt".to_string(),
            source_url: source_url.map(str::to_string),
        }
    }

    #[test]
    fn 源清单入库时异常源默认不勾选() {
        let store = Store::open_in_memory().unwrap();
        store.upsert_catalog(&catalog()).unwrap();

        let sources = store.list_pool_sources().unwrap();
        assert_eq!(sources.len(), 2);
        assert!(sources[0].checked);
        assert!(!sources[1].checked);
        assert!(sources[1].broken);

        // 用户手动勾上异常源后，再刷新清单不应被覆盖。
        store
            .set_source_checked("https://bestcf.pages.dev/luoli/all.txt", true)
            .unwrap();
        store.upsert_catalog(&catalog()).unwrap();
        let sources = store.list_pool_sources().unwrap();
        assert!(sources[1].checked);
    }

    #[test]
    fn 池条目按_host_port_唯一并带来源标识() {
        let store = Store::open_in_memory().unwrap();
        store.upsert_catalog(&catalog()).unwrap();
        let url = Some("https://bestcf.pages.dev/domain/all.txt");

        store
            .replace_pool_entries(
                &[
                    candidate("a.example.com", 443, url),
                    candidate("a.example.com", 8443, url),
                    candidate("a.example.com", 443, url),
                ],
                "2026-11-13T04:05:06.000Z",
            )
            .unwrap();

        let entries = store.list_pool_entries().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].port, 443);
        assert_eq!(entries[1].port, 8443);
        assert_eq!(entries[0].source, "BestCF·domain/all.txt");
        assert_eq!(entries[0].alias, "a.example.com 的备注 | 来源");
        assert_eq!(entries[0].added_at, "2026-11-13T04:05:06.000Z");
    }

    #[test]
    fn 抓取时间与错误落库且重启后仍在() {
        let (_dir, path) = temp_db();
        {
            let store = Store::open(&path).unwrap();
            store.upsert_catalog(&catalog()).unwrap();
            store
                .record_source_fetch(
                    "https://bestcf.pages.dev/domain/all.txt",
                    3,
                    None,
                    "2026-11-13T04:05:06.000Z",
                )
                .unwrap();
            store
                .record_source_fetch(
                    "https://bestcf.pages.dev/luoli/all.txt",
                    0,
                    Some("请求超时（10s）"),
                    "2026-11-13T04:05:07.000Z",
                )
                .unwrap();
            store
                .replace_pool_entries(
                    &[candidate("a.example.com", 443, None)],
                    "2026-11-13T04:05:06.000Z",
                )
                .unwrap();
        }

        let store = Store::open(&path).unwrap();
        let sources = store.list_pool_sources().unwrap();
        assert_eq!(sources[0].entry_count, 3);
        assert_eq!(sources[0].fetched_at.as_deref(), Some("2026-11-13T04:05:06.000Z"));
        assert_eq!(sources[1].last_error.as_deref(), Some("请求超时（10s）"));
        assert_eq!(
            store.last_fetched_at().unwrap().as_deref(),
            Some("2026-11-13T04:05:07.000Z")
        );
        assert_eq!(store.list_pool_entries().unwrap().len(), 1);
    }
}
