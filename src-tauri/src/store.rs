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

/// 当前 schema 版本，写进 SQLite 的 `user_version`。
pub const SCHEMA_VERSION: i64 = 1;

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
}
