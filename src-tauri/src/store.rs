//! 本地 SQLite 存储：建库、迁移、设置读写。
//!
//! 数据库位于 `%APPDATA%\vpm-search\data.db`（非 Windows 平台回落到
//! `app_data_dir()/data.db`）。旧路径 `%APPDATA%\com.asonnychen.vpm-search\data.db`
//! 在启动时由 `lib.rs` 一次性复制迁移。
//! 测试全部使用临时文件数据库，不触碰真实 `%APPDATA%`。

use rusqlite::{Connection, OptionalExtension};
use serde::de::DeserializeOwned;
use std::path::Path;
use std::time::Duration;

use crate::settings::{Settings, SETTING_KEYS};
use crate::sources::parse::{CatalogSource, PoolCandidate};

/// 当前 schema 版本，写进 SQLite 的 `user_version`。
pub const SCHEMA_VERSION: i64 = 6;

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
    geo_zh     TEXT,
    geo_en     TEXT,
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

/// 阶段 7：批量测试每条地址保存三次原始采样。
const SCHEMA_V4: &str = r#"
ALTER TABLE test_result ADD COLUMN samples_json TEXT NOT NULL DEFAULT '[]';
"#;

/// 阶段 8：测速结果与延迟同属一条记录，用独立状态区分「没测」与「测速失败」。
const SCHEMA_V5: &str = r#"
ALTER TABLE test_result ADD COLUMN speed_status TEXT NOT NULL DEFAULT 'untested';
ALTER TABLE test_result ADD COLUMN speed_error TEXT;
"#;

/// 阶段 9：归属地补上国家码，并新增按 IP 的查询缓存。
///
/// `test_result` 的 `geo_zh` / `geo_en` 在阶段 2 就留好了，这里只补国家码；
/// 缓存表让同一 IP 只查一次。旧库里的 `exit_ip` / `colo` 列不再读写，也不删除。
const SCHEMA_V6: &str = r#"
ALTER TABLE test_result ADD COLUMN geo_country TEXT NOT NULL DEFAULT '';
CREATE TABLE IF NOT EXISTS geo_cache (
    ip         TEXT PRIMARY KEY,
    country    TEXT NOT NULL DEFAULT '',
    zh         TEXT NOT NULL DEFAULT '',
    en         TEXT NOT NULL DEFAULT '',
    asn        INTEGER,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
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
    /// 数据库文件路径；内存库为 `None`，体积按页数统计。
    path: Option<std::path::PathBuf>,
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
        let store = Self {
            conn,
            path: Some(path.to_path_buf()),
        };
        store.migrate()?;
        Ok(store)
    }

    /// 仅用于测试。
    #[allow(dead_code)] // 阶段 3 起使用
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::configure(&conn)?;
        let store = Self { conn, path: None };
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
        if version < 4 {
            tx.execute_batch(SCHEMA_V4)?;
        }
        if version < 5 {
            tx.execute_batch(SCHEMA_V5)?;
        }
        if version < 6 {
            tx.execute_batch(SCHEMA_V6)?;
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
    ///
    /// 旧版本写下的 `node_templates` / `active_template_id` 已不在 [`SETTING_KEYS`]
    /// 里，读取时自然被忽略；列宽数组长度不符（旧版 14 列）时整体回落默认值。
    pub fn load_settings(&self) -> Result<Settings> {
        let defaults = Settings::default();

        Ok({
            let mut settings = Settings {
                last_input: self
                    .read_json("last_input")?
                    .unwrap_or(defaults.last_input),
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
            };
            // 旧库里存过的只读参数在这里钳回契约值，界面与日志不会出现矛盾值。
            settings.normalize_reserved();
            settings.normalize_column_widths();
            settings
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
        // 只读参数即使被旧客户端传进来，也只按契约值落库。
        let mut settings = settings.clone();
        settings.normalize_reserved();
        settings
            .validate()
            .map_err(|e| StoreError::InvalidSettings(e.to_string()))?;

        let pairs = settings_to_pairs(&settings)?;
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

    /// 写入一条测试轮次。阶段 6 的单条测试也各占一轮，阶段 7 的整轮复用此表。
    pub fn insert_test_run(&self, run: &TestRunRecord<'_>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO test_run
                 (id, started_at, finished_at, params_json, total, ok_count, timeout_count, fail_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                run.id,
                run.started_at,
                run.finished_at,
                run.params_json,
                i64::from(run.total),
                i64::from(run.ok_count),
                i64::from(run.timeout_count),
                i64::from(run.fail_count),
            ],
        )?;
        Ok(())
    }

    /// 写入一条测试结果。
    pub fn insert_test_result(
        &self,
        run_id: &str,
        endpoint: &crate::probe::Endpoint,
        alias: &str,
        outcome: &crate::probe::ProbeOutcome,
    ) -> Result<()> {
        self.insert_test_result_with_samples(run_id, endpoint, alias, outcome, "[]")
    }

    /// 写入一条测试结果，并保存原始采样（批量测试用）。
    pub fn insert_test_result_with_samples(
        &self,
        run_id: &str,
        endpoint: &crate::probe::Endpoint,
        alias: &str,
        outcome: &crate::probe::ProbeOutcome,
        samples_json: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO test_result
                 (run_id, host, port, alias, status, latency_ms, error, samples_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                run_id,
                endpoint.host,
                i64::from(endpoint.port),
                alias,
                outcome.status.as_str(),
                outcome.latency_ms.map(|ms| ms as i64),
                outcome.error,
                samples_json,
            ],
        )?;
        Ok(())
    }

    /// 最近一轮测试的 runId；测速独立触发时挂到这一轮上。
    pub fn latest_test_run_id(&self) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM test_run ORDER BY rowid DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// 把测速结果写进该轮已有的结果行；没有行时才新建。
    ///
    /// 只更新 `speed_*` 三列，绝不触碰延迟、状态与采样，
    /// 因此独立测速不会覆盖已有的延迟结果。
    pub fn upsert_test_result_speed(
        &self,
        run_id: &str,
        endpoint: &crate::probe::Endpoint,
        alias: &str,
        outcome: &crate::probe::speed::SpeedOutcome,
    ) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE test_result
             SET speed_mbps = ?4, speed_status = ?5, speed_error = ?6
             WHERE run_id = ?1 AND host = ?2 AND port = ?3",
            rusqlite::params![
                run_id,
                endpoint.host,
                i64::from(endpoint.port),
                outcome.mbps,
                outcome.status.as_str(),
                outcome.error,
            ],
        )?;
        if changed > 0 {
            return Ok(());
        }

        self.conn.execute(
            "INSERT INTO test_result
                 (run_id, host, port, alias, status, latency_ms,
                  speed_mbps, speed_status, speed_error)
             VALUES (?1, ?2, ?3, ?4, 'untested', NULL, ?5, ?6, ?7)",
            rusqlite::params![
                run_id,
                endpoint.host,
                i64::from(endpoint.port),
                alias,
                outcome.mbps,
                outcome.status.as_str(),
                outcome.error,
            ],
        )?;
        Ok(())
    }

    /// 阶段 6 只有单条测试，阶段 7 与 11 会用到这两个计数。
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn count_test_runs(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM test_run", [], |row| row.get(0))?)
    }

    /// 把归属地写进该轮已有的结果行；没有行时才新建（状态记未测）。
    ///
    /// 只更新 `geo_*` 三列，绝不触碰延迟、速度与采样，
    /// 因此归属地查询失败或重查都不会覆盖已有测试结论。
    pub fn upsert_test_result_geo(
        &self,
        run_id: &str,
        endpoint: &crate::probe::Endpoint,
        alias: &str,
        info: &crate::geo::GeoInfo,
    ) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE test_result
             SET geo_country = ?4, geo_zh = ?5, geo_en = ?6
             WHERE run_id = ?1 AND host = ?2 AND port = ?3",
            rusqlite::params![
                run_id,
                endpoint.host,
                i64::from(endpoint.port),
                info.country,
                info.zh,
                info.en,
            ],
        )?;
        if changed > 0 {
            return Ok(());
        }

        self.conn.execute(
            "INSERT INTO test_result
                 (run_id, host, port, alias, status, latency_ms,
                  geo_country, geo_zh, geo_en)
             VALUES (?1, ?2, ?3, ?4, 'untested', NULL, ?5, ?6, ?7)",
            rusqlite::params![
                run_id,
                endpoint.host,
                i64::from(endpoint.port),
                alias,
                info.country,
                info.zh,
                info.en,
            ],
        )?;
        Ok(())
    }

    /// 写一个 IP 的归属地缓存；同一 IP 覆盖为最新结论。
    pub fn upsert_geo_cache(&self, ip: &str, record: &crate::geo::GeoRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO geo_cache (ip, country, zh, en, asn, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
             ON CONFLICT(ip) DO UPDATE SET
                 country = excluded.country,
                 zh = excluded.zh,
                 en = excluded.en,
                 asn = excluded.asn,
                 updated_at = excluded.updated_at",
            rusqlite::params![
                ip,
                record.country,
                record.zh,
                record.en,
                record.asn.map(i64::from),
            ],
        )?;
        Ok(())
    }

    /// 读全部归属地缓存，用于启动一轮查询前预灌内存缓存。
    pub fn list_geo_cache(&self) -> Result<Vec<GeoCacheRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT ip, country, zh, en, asn
             FROM geo_cache
             ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(GeoCacheRow {
                ip: row.get(0)?,
                record: crate::geo::GeoRecord {
                    country: row.get(1)?,
                    zh: row.get(2)?,
                    en: row.get(3)?,
                    asn: row
                        .get::<_, Option<i64>>(4)?
                        .map(|asn| asn.clamp(0, u32::MAX as i64) as u32),
                },
            })
        })?;
        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }
        Ok(entries)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn count_test_results(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM test_result", [], |row| row.get(0))?)
    }

    /// 读某一轮的全部结果，按写入顺序。阶段 7 与 11 的界面复用。
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn list_test_results(&self, run_id: &str) -> Result<Vec<TestResultRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, host, port, alias, status, latency_ms, error, samples_json,
                    speed_mbps, speed_status, speed_error,
                    geo_country, geo_zh, geo_en
             FROM test_result
             WHERE run_id = ?1
             ORDER BY id",
        )?;
        let rows = stmt.query_map([run_id], |row| {
            let samples_json: String = row.get(7)?;
            Ok(TestResultRow {
                run_id: row.get(0)?,
                host: row.get(1)?,
                port: row.get::<_, i64>(2)?.clamp(0, u16::MAX as i64) as u16,
                alias: row.get(3)?,
                status: row.get(4)?,
                latency_ms: row.get::<_, Option<i64>>(5)?.map(|ms| ms.max(0) as u64),
                error: row.get(6)?,
                samples: serde_json::from_str(&samples_json).unwrap_or_default(),
                speed_mbps: row.get(8)?,
                speed_status: row.get(9)?,
                speed_error: row.get(10)?,
                geo_country: row.get(11)?,
                geo_zh: row.get(12)?,
                geo_en: row.get(13)?,
            })
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// 读一轮的统计，用于历史入口与测试断言。
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn get_test_run(&self, run_id: &str) -> Result<Option<TestRunRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, started_at, finished_at, params_json, total,
                        ok_count, timeout_count, fail_count
                 FROM test_run WHERE id = ?1",
                [run_id],
                |row| {
                    Ok(TestRunRow {
                        id: row.get(0)?,
                        started_at: row.get(1)?,
                        finished_at: row.get(2)?,
                        params_json: row.get(3)?,
                        total: row.get::<_, i64>(4)?.max(0) as u32,
                        ok_count: row.get::<_, i64>(5)?.max(0) as u32,
                        timeout_count: row.get::<_, i64>(6)?.max(0) as u32,
                        fail_count: row.get::<_, i64>(7)?.max(0) as u32,
                    })
                },
            )
            .optional()?)
    }

    /// 历史入口的轮次列表：按开始时间倒序，附本轮实际落库的地址数。
    ///
    /// `result_count` 与 `total` 分开：停止的轮次、独立测速 / 归属地查询
    /// 都可能让两者不一致，历史里应展示真实存在的行数。
    pub fn list_test_runs(&self) -> Result<Vec<TestRunSummaryRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT r.id, r.started_at, r.finished_at, r.params_json, r.total,
                    r.ok_count, r.timeout_count, r.fail_count,
                    (SELECT COUNT(*) FROM test_result tr WHERE tr.run_id = r.id)
             FROM test_run r
             ORDER BY r.started_at DESC, r.rowid DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(TestRunSummaryRow {
                id: row.get(0)?,
                started_at: row.get(1)?,
                finished_at: row.get(2)?,
                params_json: row.get(3)?,
                total: row.get::<_, i64>(4)?.max(0) as u32,
                ok_count: row.get::<_, i64>(5)?.max(0) as u32,
                timeout_count: row.get::<_, i64>(6)?.max(0) as u32,
                fail_count: row.get::<_, i64>(7)?.max(0) as u32,
                result_count: row.get::<_, i64>(8)?.max(0) as u32,
            })
        })?;
        let mut runs = Vec::new();
        for row in rows {
            runs.push(row?);
        }
        Ok(runs)
    }

    /// 按轮次整轮删除（结果行随外键级联删除），随后 `VACUUM` 让文件真正收缩。
    ///
    /// 返回是否删到了该轮；不存在的 runId 返回 `false`，不当作错误。
    pub fn delete_test_run(&self, run_id: &str) -> Result<bool> {
        let changed = self
            .conn
            .execute("DELETE FROM test_run WHERE id = ?1", [run_id])?;
        if changed == 0 {
            return Ok(false);
        }
        // 语句缓存会占住数据库，VACUUM 前先释放。
        self.conn.flush_prepared_statement_cache();
        self.conn.execute_batch("VACUUM")?;
        Ok(true)
    }

    /// 数据库体积（字节）。文件库取文件大小，内存库按页数换算。
    pub fn database_size_bytes(&self) -> Result<u64> {
        if let Some(path) = &self.path {
            if let Ok(metadata) = std::fs::metadata(path) {
                if metadata.len() > 0 {
                    return Ok(metadata.len());
                }
            }
        }
        let page_count: i64 = self
            .conn
            .query_row("PRAGMA page_count", [], |row| row.get(0))?;
        let page_size: i64 = self
            .conn
            .query_row("PRAGMA page_size", [], |row| row.get(0))?;
        Ok(page_count.max(0) as u64 * page_size.max(0) as u64)
    }

    /// 同一地址跨轮次的延迟与速度序列，按开始时间升序。
    ///
    /// 只读事实记录：每一轮一行，不合并、不覆盖。
    pub fn list_address_history(&self, host: &str, port: u16) -> Result<Vec<AddressRunPoint>> {
        let mut stmt = self.conn.prepare(
            "SELECT tr.run_id, r.started_at, tr.latency_ms, tr.status,
                    tr.speed_mbps, tr.speed_status
             FROM test_result tr
             JOIN test_run r ON r.id = tr.run_id
             WHERE tr.host = ?1 AND tr.port = ?2
             ORDER BY r.started_at ASC, r.rowid ASC, tr.id ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![host, i64::from(port)], |row| {
            Ok(AddressRunPoint {
                run_id: row.get(0)?,
                started_at: row.get(1)?,
                latency_ms: row.get::<_, Option<i64>>(2)?.map(|ms| ms.max(0) as u64),
                status: row.get(3)?,
                speed_mbps: row.get(4)?,
                speed_status: row.get(5)?,
            })
        })?;
        let mut points = Vec::new();
        for row in rows {
            points.push(row?);
        }
        Ok(points)
    }
}

/// 一条测试轮次的写入参数。
pub struct TestRunRecord<'a> {
    pub id: &'a str,
    pub started_at: &'a str,
    pub finished_at: &'a str,
    pub params_json: &'a str,
    pub total: u32,
    pub ok_count: u32,
    pub timeout_count: u32,
    pub fail_count: u32,
}

/// `test_result` 的一行。
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq)]
pub struct TestResultRow {
    pub run_id: String,
    pub host: String,
    pub port: u16,
    pub alias: String,
    pub status: String,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
    /// 批量测试的三次原始采样；单条测试为空数组。
    pub samples: Vec<crate::probe::ProbeOutcome>,
    /// 测速结果，MB/s；未测或失败时为 `None`。
    pub speed_mbps: Option<f64>,
    /// 测速状态：`ok` / `fail` / `untested`，与延迟状态互不影响。
    pub speed_status: String,
    /// 测速失败原因，成功时为 `None`。
    pub speed_error: Option<String>,
    /// 地址本身的 ISO 国家码；未查询时为空。
    pub geo_country: Option<String>,
    /// 归属地中文名。
    pub geo_zh: Option<String>,
    /// 归属地英文名。
    pub geo_en: Option<String>,
}

/// `geo_cache` 的一行：IP 到归属地的缓存。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeoCacheRow {
    pub ip: String,
    pub record: crate::geo::GeoRecord,
}

/// `test_run` 的一行。
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestRunRow {
    pub id: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub params_json: String,
    pub total: u32,
    pub ok_count: u32,
    pub timeout_count: u32,
    pub fail_count: u32,
}

/// 历史入口的一行：轮次统计 + 实际结果行数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestRunSummaryRow {
    pub id: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub params_json: String,
    pub total: u32,
    pub ok_count: u32,
    pub timeout_count: u32,
    pub fail_count: u32,
    pub result_count: u32,
}

/// 同一地址在某一轮的结果，跨轮对比用。
#[derive(Debug, Clone, PartialEq)]
pub struct AddressRunPoint {
    pub run_id: String,
    pub started_at: String,
    pub latency_ms: Option<u64>,
    pub status: String,
    pub speed_mbps: Option<f64>,
    pub speed_status: String,
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
                "geo_cache".to_string(),
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
        settings.log_retention.probe_days = 60;

        store.save_settings(&settings).unwrap();
        let loaded = store.load_settings().unwrap();
        // 旧客户端传来的只读参数按契约值落库与读回。
        assert_eq!(loaded.request_timeout_s, 10);
        assert_eq!(loaded.latency_samples, 3);
        assert_eq!(loaded.speed.streams, 4);
        assert_eq!(loaded.speed.size_mb, 10);
        assert_eq!(loaded.speed.timeout_s, 15);
        settings.normalize_reserved();
        assert_eq!(loaded, settings);
    }

    #[test]
    fn 旧库里的模板字段被忽略且不影响读取() {
        let store = Store::open_in_memory().unwrap();
        store.save_settings(&Settings::default()).unwrap();
        // 旧版本留下的 key 不在白名单里，读写都被忽略。
        store
            .conn
            .execute(
                "INSERT INTO settings (key, value) VALUES ('node_templates', ?1)",
                [r#"[{"id":"old","name":"旧模板"}]"#],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO settings (key, value) VALUES ('active_template_id', '\"old\"')",
                [],
            )
            .unwrap();

        let loaded = store.load_settings().unwrap();
        assert_eq!(loaded, Settings::default());
        // 保存时也不会把它们写回去。
        store.save_settings(&loaded).unwrap();
        assert_eq!(store.get_setting("node_templates").unwrap(), None);
        assert_eq!(store.get_setting("active_template_id").unwrap(), None);
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

    #[test]
    fn 从阶段八的旧库升级补上国家码与缓存表() {
        let (_dir, path) = temp_db();
        {
            // 模拟阶段 8 的库：schema 到 V5，结果行已有延迟但没有国家码。
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA_V1).unwrap();
            conn.execute_batch(SCHEMA_V2).unwrap();
            conn.execute_batch(SCHEMA_V4).unwrap();
            conn.execute_batch(SCHEMA_V5).unwrap();
            conn.pragma_update(None, "user_version", 5).unwrap();
            conn.execute(
                "INSERT INTO test_run (id, started_at, total) VALUES ('run-old', '2026-11-13T00:00:00.000Z', 1)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO test_result (run_id, host, port, status, latency_ms)
                 VALUES ('run-old', '104.16.1.1', 8443, 'ok', 88)",
                [],
            )
            .unwrap();
        }

        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        let rows = store.list_test_results("run-old").unwrap();
        assert_eq!(rows[0].latency_ms, Some(88));
        assert_eq!(rows[0].geo_country.as_deref(), Some(""));
        assert_eq!(rows[0].geo_zh, None);
        // 新表可写。
        store
            .upsert_geo_cache(
                "203.0.113.9",
                &crate::geo::GeoRecord::from_country("CA", Some(15169)),
            )
            .unwrap();
        assert_eq!(store.list_geo_cache().unwrap().len(), 1);
    }

    fn seed_run(
        store: &Store,
        id: &str,
        started_at: &str,
        params_json: &str,
        rows: &[(&str, u16, crate::probe::ProbeOutcome)],
    ) {
        let mut ok = 0;
        let mut timeout = 0;
        let mut fail = 0;
        for (_, _, outcome) in rows {
            match outcome.status {
                crate::probe::ProbeStatus::Ok => ok += 1,
                crate::probe::ProbeStatus::Timeout => timeout += 1,
                crate::probe::ProbeStatus::Fail => fail += 1,
                crate::probe::ProbeStatus::Untested => {}
            }
        }
        store
            .insert_test_run(&TestRunRecord {
                id,
                started_at,
                finished_at: started_at,
                params_json,
                total: rows.len() as u32,
                ok_count: ok,
                timeout_count: timeout,
                fail_count: fail,
            })
            .unwrap();
        for (host, port, outcome) in rows {
            store
                .insert_test_result(
                    id,
                    &crate::probe::Endpoint::new((*host).to_string(), *port),
                    "别名",
                    outcome,
                )
                .unwrap();
        }
    }

    #[test]
    fn 历史轮次列表带统计参数与地址数() {
        let store = Store::open_in_memory().unwrap();
        seed_run(
            &store,
            "run-old",
            "2026-11-12T08:00:00.000Z",
            r#"{"samples":3,"concurrency":10}"#,
            &[
                ("104.16.1.1", 8443, crate::probe::ProbeOutcome::ok(88)),
                (
                    "104.16.1.2",
                    8443,
                    crate::probe::ProbeOutcome::timeout("10s 内未收到响应"),
                ),
            ],
        );
        seed_run(
            &store,
            "run-new",
            "2026-11-13T08:00:00.000Z",
            r#"{"mode":"speed"}"#,
            &[(
                "104.16.1.1",
                8443,
                crate::probe::ProbeOutcome::fail("连接被拒绝"),
            )],
        );

        let runs = store.list_test_runs().unwrap();
        assert_eq!(runs.len(), 2);
        // 最近的轮次排在最前。
        assert_eq!(runs[0].id, "run-new");
        assert_eq!(runs[1].id, "run-old");
        assert_eq!(runs[1].total, 2);
        assert_eq!(
            (runs[1].ok_count, runs[1].timeout_count, runs[1].fail_count),
            (1, 1, 0)
        );
        // 地址数按实际落库的结果行统计，与 total 分开。
        assert_eq!(runs[1].result_count, 2);
        assert_eq!(runs[0].result_count, 1);
        assert_eq!(runs[0].params_json, r#"{"mode":"speed"}"#);
    }

    #[test]
    fn 同一地址跨轮序列按时间升序并带速度() {
        let store = Store::open_in_memory().unwrap();
        // 故意先插较晚的一轮，验证排序靠开始时间而不是写入顺序。
        seed_run(
            &store,
            "run-late",
            "2026-11-13T08:00:00.000Z",
            "{}",
            &[("104.16.1.1", 8443, crate::probe::ProbeOutcome::ok(80))],
        );
        store
            .upsert_test_result_speed(
                "run-late",
                &crate::probe::Endpoint::new("104.16.1.1".to_string(), 8443),
                "别名",
                &crate::probe::speed::SpeedOutcome::ok(5.5),
            )
            .unwrap();
        seed_run(
            &store,
            "run-early",
            "2026-11-12T08:00:00.000Z",
            "{}",
            &[("104.16.1.1", 8443, crate::probe::ProbeOutcome::ok(100))],
        );
        // 另一个地址不进入序列。
        seed_run(
            &store,
            "run-other",
            "2026-11-14T08:00:00.000Z",
            "{}",
            &[("104.16.1.2", 8443, crate::probe::ProbeOutcome::ok(20))],
        );

        let series = store.list_address_history("104.16.1.1", 8443).unwrap();
        assert_eq!(series.len(), 2);
        assert_eq!(series[0].run_id, "run-early");
        assert_eq!(series[0].started_at, "2026-11-12T08:00:00.000Z");
        assert_eq!(series[0].latency_ms, Some(100));
        assert_eq!(series[0].speed_mbps, None);
        assert_eq!(series[1].run_id, "run-late");
        assert_eq!(series[1].latency_ms, Some(80));
        assert_eq!(series[1].speed_mbps, Some(5.5));
        assert_eq!(series[1].speed_status, "ok");
    }

    #[test]
    fn 数据库体积统计随写入增长() {
        let store = Store::open_in_memory().unwrap();
        let empty = store.database_size_bytes().unwrap();
        assert!(empty > 0, "空库也应有体积");

        let long_alias = "很长的别名".repeat(80);
        let hosts: Vec<String> = (0..300).map(|i| format!("104.16.9.{i}")).collect();
        let rows: Vec<(&str, u16, crate::probe::ProbeOutcome)> = hosts
            .iter()
            .map(|host| (host.as_str(), 8443, crate::probe::ProbeOutcome::ok(50)))
            .collect();
        seed_run(
            &store,
            "run-big",
            "2026-11-13T08:00:00.000Z",
            "{}",
            &rows,
        );
        // 别名一并写大，确保页数明显变化。
        store
            .conn
            .execute(
                "UPDATE test_result SET alias = ?1 WHERE run_id = 'run-big'",
                [long_alias],
            )
            .unwrap();

        let grown = store.database_size_bytes().unwrap();
        assert!(grown > empty, "写入后体积应增长：{empty} -> {grown}");
    }

    #[test]
    fn 按轮删除只删该轮并收缩体积() {
        let store = Store::open_in_memory().unwrap();
        let alias = "别名".repeat(200);
        let hosts: Vec<String> = (0..200).map(|i| format!("104.16.8.{i}")).collect();
        for run_id in ["run-a", "run-b"] {
            let rows: Vec<(&str, u16, crate::probe::ProbeOutcome)> = hosts
                .iter()
                .map(|host| (host.as_str(), 8443, crate::probe::ProbeOutcome::ok(50)))
                .collect();
            seed_run(
                &store,
                run_id,
                "2026-11-13T08:00:00.000Z",
                "{}",
                &rows,
            );
        }
        store
            .conn
            .execute("UPDATE test_result SET alias = ?1", [alias])
            .unwrap();
        let before = store.database_size_bytes().unwrap();

        assert!(store.delete_test_run("run-a").unwrap());
        assert_eq!(store.count_test_runs().unwrap(), 1);
        assert_eq!(store.count_test_results().unwrap(), 200);
        assert!(store.list_test_results("run-a").unwrap().is_empty());
        assert_eq!(store.list_test_results("run-b").unwrap().len(), 200);

        let after = store.database_size_bytes().unwrap();
        assert!(after < before, "删除后体积应下降：{before} -> {after}");
        // 已删除的轮次再删一次返回 false，不报错。
        assert!(!store.delete_test_run("run-a").unwrap());
    }

    #[test]
    fn 出口_ip_缓存同一_ip_覆盖不重复() {
        let store = Store::open_in_memory().unwrap();
        store
            .upsert_geo_cache(
                "203.0.113.9",
                &crate::geo::GeoRecord::from_country("CA", Some(15169)),
            )
            .unwrap();
        store
            .upsert_geo_cache(
                "203.0.113.9",
                &crate::geo::GeoRecord::from_country("JP", Some(2497)),
            )
            .unwrap();

        let cache = store.list_geo_cache().unwrap();
        assert_eq!(cache.len(), 1);
        assert_eq!(cache[0].ip, "203.0.113.9");
        assert_eq!(cache[0].record.zh, "日本");
        assert_eq!(cache[0].record.asn, Some(2497));
    }
}
