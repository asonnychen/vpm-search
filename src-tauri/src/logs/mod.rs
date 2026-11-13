//! 三类日志：操作日志、应用日志、sing-box 日志。
//!
//! 落盘位置为 `<app_data_dir>/logs/`，每类日志按天一个文件：
//! `operation-YYYY-MM-DD.log`、`app-YYYY-MM-DD.log`、`singbox-YYYY-MM-DD.log`。
//!
//! - 操作日志：人读的中文单行，`<ts> <LEVEL> <traceId> <文本>`；
//! - 应用日志：JSON Lines，字段 `ts / level / traceId / spanId / module /
//!   event / durationMs / params / result / error.stack`；
//! - sing-box 日志：子进程 stdout/stderr 原样落盘（阶段 6 接入进程）。

pub mod redact;
pub mod retention;

use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Datelike, Local, TimeZone, Timelike};
use serde::Serialize;
use serde_json::Value;

use crate::settings::LogRetention;
use redact::redact;
use retention::CleanupReport;

/// 日志类型。三类各自独立保留期，互不影响。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LogKind {
    Operation,
    App,
    Singbox,
}

impl LogKind {
    pub const ALL: [LogKind; 3] = [LogKind::Operation, LogKind::App, LogKind::Singbox];

    /// 文件名前缀。
    pub fn prefix(self) -> &'static str {
        match self {
            LogKind::Operation => "operation",
            LogKind::App => "app",
            LogKind::Singbox => "singbox",
        }
    }

    /// 界面上的中文名。
    pub fn label(self) -> &'static str {
        match self {
            LogKind::Operation => "操作日志",
            LogKind::App => "应用日志",
            LogKind::Singbox => "sing-box 日志",
        }
    }

    /// 前端传来的类型标识。
    pub fn as_str(self) -> &'static str {
        match self {
            LogKind::Operation => "operation",
            LogKind::App => "app",
            LogKind::Singbox => "singbox",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "operation" => Some(LogKind::Operation),
            "app" => Some(LogKind::App),
            "singbox" => Some(LogKind::Singbox),
            _ => None,
        }
    }

    /// 该类日志的保留天数。
    pub fn retention_days(self, retention: &LogRetention) -> u32 {
        match self {
            LogKind::Operation => retention.operation_days,
            LogKind::App => retention.app_days,
            LogKind::Singbox => retention.singbox_days,
        }
    }
}

/// 日志级别，与界面表格的着色一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Ok,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Info => "INFO",
            LogLevel::Ok => "OK",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "INFO" | "DEBUG" => Some(LogLevel::Info),
            "OK" => Some(LogLevel::Ok),
            "WARN" | "WARNING" => Some(LogLevel::Warn),
            "ERROR" | "FATAL" | "PANIC" => Some(LogLevel::Error),
            _ => None,
        }
    }

    /// sing-box 输出没有统一格式，只能按关键字猜一个级别。
    pub fn infer(text: &str) -> Self {
        let lower = text.to_ascii_lowercase();
        if lower.contains("error") || lower.contains("fatal") || lower.contains("panic") {
            LogLevel::Error
        } else if lower.contains("warn") {
            LogLevel::Warn
        } else {
            LogLevel::Info
        }
    }
}

/// 一条日志，前端表格直接渲染。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    /// `operation` / `app` / `singbox`。
    pub kind: String,
    pub ts: String,
    pub level: String,
    /// 操作日志是中文单行；应用日志是原始 JSON 行；sing-box 是原始输出行。
    pub text: String,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub event: Option<String>,
    pub duration_ms: Option<u64>,
}

/// 应用日志里的错误：`error.message` 与 `error.stack`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppLogError {
    pub message: String,
    pub stack: Option<String>,
}

/// 一条应用日志记录。字段与落盘 JSON 一一对应。
#[derive(Debug, Clone)]
pub struct AppLogRecord {
    pub level: LogLevel,
    pub module: String,
    pub event: String,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub duration_ms: Option<u64>,
    /// 入参摘要。
    pub params: Option<Value>,
    /// 出参摘要。
    pub result: Option<Value>,
    pub error: Option<AppLogError>,
}

impl AppLogRecord {
    pub fn new(level: LogLevel, module: &str, event: &str) -> Self {
        Self {
            level,
            module: module.to_string(),
            event: event.to_string(),
            trace_id: None,
            span_id: None,
            duration_ms: None,
            params: None,
            result: None,
            error: None,
        }
    }

    pub fn trace(mut self, trace_id: &str) -> Self {
        self.trace_id = Some(trace_id.to_string());
        self
    }

    pub fn span(mut self, span_id: &str) -> Self {
        self.span_id = Some(span_id.to_string());
        self
    }

    pub fn duration(mut self, duration_ms: u64) -> Self {
        self.duration_ms = Some(duration_ms);
        self
    }

    pub fn params(mut self, params: Value) -> Self {
        self.params = Some(params);
        self
    }

    pub fn result(mut self, result: Value) -> Self {
        self.result = Some(result);
        self
    }

    pub fn error(mut self, message: &str, stack: Option<&str>) -> Self {
        self.error = Some(AppLogError {
            message: message.to_string(),
            stack: stack.map(str::to_string),
        });
        self
    }
}

/// 一次读取的三类日志。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogsSnapshot {
    pub operation: Vec<LogEntry>,
    pub app: Vec<LogEntry>,
    pub singbox: Vec<LogEntry>,
}

/// 三类日志的落盘与读取。
#[derive(Debug, Clone)]
pub struct LogWriter {
    dir: PathBuf,
}

impl LogWriter {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 某类日志当天的文件路径。
    pub fn path_for(&self, kind: LogKind, date: &str) -> PathBuf {
        self.dir.join(format!("{}-{date}.log", kind.prefix()))
    }

    /// 某类日志今天的文件路径；错误提示里直接给用户看。
    pub fn today_path(&self, kind: LogKind) -> PathBuf {
        self.path_for(kind, &local_date(Local::now()))
    }

    /// 写一条操作日志（人读的中文单行）。
    pub fn write_operation(
        &self,
        level: LogLevel,
        trace_id: Option<&str>,
        text: &str,
    ) -> io::Result<LogEntry> {
        let ts = now_iso8601();
        let text = redact(text).replace(['\r', '\n'], " ");
        let trace = trace_id.unwrap_or("-");
        let line = format!("{ts} {} {trace} {text}", level.as_str());
        self.append(LogKind::Operation, &line)?;
        Ok(LogEntry {
            kind: LogKind::Operation.as_str().to_string(),
            ts,
            level: level.as_str().to_string(),
            text,
            trace_id: trace_id.map(str::to_string),
            span_id: None,
            event: None,
            duration_ms: None,
        })
    }

    /// 写一条应用日志（JSON Lines）。
    pub fn write_app(&self, record: &AppLogRecord) -> io::Result<LogEntry> {
        let ts = now_iso8601();
        let mut object = serde_json::Map::new();
        object.insert("ts".to_string(), Value::String(ts.clone()));
        object.insert("level".to_string(), Value::String(record.level.as_str().into()));
        if let Some(trace_id) = &record.trace_id {
            object.insert("traceId".to_string(), Value::String(trace_id.clone()));
        }
        if let Some(span_id) = &record.span_id {
            object.insert("spanId".to_string(), Value::String(span_id.clone()));
        }
        object.insert("module".to_string(), Value::String(record.module.clone()));
        object.insert("event".to_string(), Value::String(record.event.clone()));
        if let Some(duration_ms) = record.duration_ms {
            object.insert("durationMs".to_string(), Value::from(duration_ms));
        }
        if let Some(params) = &record.params {
            object.insert("params".to_string(), params.clone());
        }
        if let Some(result) = &record.result {
            object.insert("result".to_string(), result.clone());
        }
        if let Some(error) = &record.error {
            let mut detail = serde_json::Map::new();
            detail.insert("message".to_string(), Value::String(error.message.clone()));
            if let Some(stack) = &error.stack {
                detail.insert("stack".to_string(), Value::String(stack.clone()));
            }
            object.insert("error".to_string(), Value::Object(detail));
        }

        let line = redact(&Value::Object(object).to_string());
        self.append(LogKind::App, &line)?;
        Ok(LogEntry {
            kind: LogKind::App.as_str().to_string(),
            ts,
            level: record.level.as_str().to_string(),
            text: line,
            trace_id: record.trace_id.clone(),
            span_id: record.span_id.clone(),
            event: Some(record.event.clone()),
            duration_ms: record.duration_ms,
        })
    }

    /// 把 sing-box 子进程的一行输出原样落盘（凭据同样脱敏）。
    pub fn append_singbox_line(&self, _stream: &str, line: &str) -> io::Result<LogEntry> {
        let line = redact(line);
        self.append(LogKind::Singbox, &line)?;
        Ok(LogEntry {
            kind: LogKind::Singbox.as_str().to_string(),
            ts: now_iso8601(),
            level: LogLevel::infer(&line).as_str().to_string(),
            text: line,
            trace_id: None,
            span_id: None,
            event: None,
            duration_ms: None,
        })
    }

    fn append(&self, kind: LogKind, line: &str) -> io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.path_for(kind, &local_date(Local::now()));
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "{line}")
    }

    /// 读某类日志最近 `max` 行，按时间从旧到新。
    pub fn read_tail(&self, kind: LogKind, max: usize) -> Vec<LogEntry> {
        if max == 0 {
            return Vec::new();
        }
        let mut tail: VecDeque<LogEntry> = VecDeque::with_capacity(max);
        let files = match retention::log_files(&self.dir, kind) {
            Ok(files) => files,
            Err(_) => return Vec::new(),
        };

        for path in files {
            let fallback_ts = std::fs::metadata(&path)
                .and_then(|meta| meta.modified())
                .map(iso8601_from_system_time)
                .unwrap_or_else(|_| now_iso8601());
            let Ok(file) = std::fs::File::open(&path) else {
                continue;
            };
            for line in BufReader::new(file).lines() {
                let Ok(line) = line else { continue };
                if line.trim().is_empty() {
                    continue;
                }
                let Some(entry) = parse_line(kind, &line, &fallback_ts) else {
                    continue;
                };
                if tail.len() == max {
                    tail.pop_front();
                }
                tail.push_back(entry);
            }
        }
        tail.into_iter().collect()
    }

    /// 启动时读三类日志各最近 `max` 行。
    pub fn read_all(&self, max: usize) -> LogsSnapshot {
        LogsSnapshot {
            operation: self.read_tail(LogKind::Operation, max),
            app: self.read_tail(LogKind::App, max),
            singbox: self.read_tail(LogKind::Singbox, max),
        }
    }

    /// 手动清理某类日志：删除文件，并往应用日志写一条「已清理」记录。
    pub fn clear(&self, kind: LogKind) -> io::Result<CleanupReport> {
        let report = retention::clear(&self.dir, kind)?;
        // 清理动作本身必须留痕，否则「日志突然没了」无从排查。
        let record = AppLogRecord::new(LogLevel::Warn, "logs", "log.clear")
            .params(serde_json::json!({
                "kind": kind.as_str(),
                "label": kind.label(),
                "entries": report.entries,
                "files": report.files,
            }))
            .result(Value::String(format!(
                "已清理 {} 条 / {} 个文件",
                report.entries, report.files
            )));
        let _ = self.write_app(&record);
        Ok(report)
    }

    /// 按保留期清理三类日志，每次清理动作都记入应用日志。
    ///
    /// 第三个元素是刚写入应用日志的那条记录，调用方可以直接推给界面。
    pub fn cleanup_expired(
        &self,
        retention: &LogRetention,
        now: SystemTime,
    ) -> Vec<(LogKind, CleanupReport, Option<LogEntry>)> {
        let mut reports = Vec::with_capacity(LogKind::ALL.len());
        for kind in LogKind::ALL {
            let days = kind.retention_days(retention);
            let report = retention::cleanup(&self.dir, kind, days, now).unwrap_or_default();
            let record = AppLogRecord::new(LogLevel::Info, "logs", "log.cleanup")
                .params(serde_json::json!({
                    "kind": kind.as_str(),
                    "days": days,
                    "files": report.files,
                    "entries": report.entries,
                }))
                .result(Value::String(format!(
                    "已清理 {} 条 / {} 个文件",
                    report.entries, report.files
                )));
            let entry = self.write_app(&record).ok();
            reports.push((kind, report, entry));
        }
        reports
    }
}

/// 把一行原始日志解析成界面用的条目。
fn parse_line(kind: LogKind, line: &str, fallback_ts: &str) -> Option<LogEntry> {
    match kind {
        LogKind::Operation => {
            let mut parts = line.splitn(4, ' ');
            let ts = parts.next()?;
            let level = LogLevel::parse(parts.next()?)?;
            let trace = parts.next().unwrap_or("-");
            let text = parts.next().unwrap_or("");
            Some(LogEntry {
                kind: kind.as_str().to_string(),
                ts: ts.to_string(),
                level: level.as_str().to_string(),
                text: text.to_string(),
                trace_id: (trace != "-").then(|| trace.to_string()),
                span_id: None,
                event: None,
                duration_ms: None,
            })
        }
        LogKind::App => {
            let value: Value = serde_json::from_str(line).ok()?;
            let ts = value
                .get("ts")
                .or_else(|| value.get("time"))
                .and_then(Value::as_str)
                .unwrap_or(fallback_ts)
                .to_string();
            let level = value
                .get("level")
                .and_then(Value::as_str)
                .and_then(LogLevel::parse)
                .unwrap_or(LogLevel::Info);
            Some(LogEntry {
                kind: kind.as_str().to_string(),
                ts,
                level: level.as_str().to_string(),
                text: line.to_string(),
                trace_id: value
                    .get("traceId")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                span_id: value
                    .get("spanId")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                event: value
                    .get("event")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                duration_ms: value.get("durationMs").and_then(Value::as_u64),
            })
        }
        LogKind::Singbox => Some(LogEntry {
            kind: kind.as_str().to_string(),
            ts: fallback_ts.to_string(),
            level: LogLevel::infer(line).as_str().to_string(),
            text: line.to_string(),
            trace_id: None,
            span_id: None,
            event: None,
            duration_ms: None,
        }),
    }
}

/// sing-box 日志的写入通道：阶段 6 把子进程的 stdout/stderr 推进来即可。
#[allow(dead_code)] // 阶段 6 接入 sing-box 进程后使用
pub struct SingboxLog {
    sender: Mutex<Option<mpsc::Sender<(String, String)>>>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

#[allow(dead_code)] // 阶段 6 接入 sing-box 进程后使用
impl SingboxLog {
    /// 起一个后台线程，把通道里的行顺序写进文件。
    pub fn spawn(writer: Arc<LogWriter>) -> Self {
        let (sender, receiver) = mpsc::channel::<(String, String)>();
        let handle = std::thread::spawn(move || {
            for (stream, line) in receiver {
                let _ = writer.append_singbox_line(&stream, &line);
            }
        });
        Self {
            sender: Mutex::new(Some(sender)),
            handle: Mutex::new(Some(handle)),
        }
    }

    /// 推一行子进程输出。
    pub fn push(&self, stream: &str, line: impl Into<String>) {
        if let Ok(slot) = self.sender.lock() {
            if let Some(sender) = slot.as_ref() {
                let _ = sender.send((stream.to_string(), line.into()));
            }
        }
    }

    /// 关闭通道并等待写入线程结束。
    pub fn shutdown(&self) {
        if let Ok(mut slot) = self.sender.lock() {
            slot.take();
        }
        if let Ok(mut slot) = self.handle.lock() {
            if let Some(handle) = slot.take() {
                let _ = handle.join();
            }
        }
    }
}

/// UTC 毫秒时间戳格式化成 `YYYY-MM-DDTHH:MM:SS.mmmZ`。
pub fn iso8601_from_millis(millis: i64) -> String {
    let total_secs = millis.div_euclid(1000);
    let ms = millis.rem_euclid(1000);
    let days = total_secs.div_euclid(86_400);
    let secs_of_day = total_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{ms:03}Z")
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { y + 1 } else { y }, month, day)
}

pub fn now_iso8601() -> String {
    iso8601_from_system_time(SystemTime::now())
}

pub fn iso8601_from_system_time(time: SystemTime) -> String {
    let millis = time
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    iso8601_from_millis(millis)
}

/// 当天的本地日期，用于日志文件名。
fn local_date(now: DateTime<Local>) -> String {
    format!("{:04}-{:02}-{:02}", now.year(), now.month(), now.day())
}

/// 距离下一个本地零点还有多久。
pub fn duration_until_next_local_midnight(now: DateTime<Local>) -> Duration {
    let tomorrow = now.date_naive().succ_opt().unwrap_or(now.date_naive());
    let next = tomorrow
        .and_hms_opt(0, 0, 0)
        .and_then(|naive| Local.from_local_datetime(&naive).earliest())
        .unwrap_or(now);
    next.signed_duration_since(now)
        .to_std()
        .unwrap_or_else(|_| Duration::from_secs(86_400))
}

static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_hex(width: usize) -> String {
    let count = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let value = count
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(nanos);
    let masked = value & ((1u64 << (width * 4)) - 1);
    format!("{:0width$x}", masked, width = width)
}

/// 一次操作一个 traceId，同一操作下的所有日志共用。
pub fn new_trace_id() -> String {
    let now = Local::now();
    format!(
        "tr-{:04}{:02}{:02}-{:02}{:02}{:02}-{}",
        now.year(),
        now.month(),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        next_hex(6)
    )
}

/// 一个子步骤一个 spanId。
pub fn new_span_id() -> String {
    next_hex(8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::LogRetention;
    use serde_json::json;
    use std::fs::File;

    fn writer() -> (tempfile::TempDir, Arc<LogWriter>) {
        let dir = tempfile::tempdir().unwrap();
        let writer = Arc::new(LogWriter::new(dir.path()));
        (dir, writer)
    }

    fn raw_lines(dir: &Path, kind: LogKind) -> Vec<String> {
        let files = retention::log_files(dir, kind).unwrap();
        let mut lines = Vec::new();
        for file in files {
            let text = std::fs::read_to_string(file).unwrap();
            lines.extend(text.lines().map(str::to_string));
        }
        lines
    }

    #[test]
    fn 三类日志各自取自己的保留天数() {
        let retention = LogRetention {
            operation_days: 7,
            app_days: 14,
            singbox_days: 60,
        };
        assert_eq!(LogKind::Operation.retention_days(&retention), 7);
        assert_eq!(LogKind::App.retention_days(&retention), 14);
        assert_eq!(LogKind::Singbox.retention_days(&retention), 60);
        assert_eq!(LogKind::Operation.as_str(), "operation");
        assert_eq!(LogKind::parse("singbox"), Some(LogKind::Singbox));
        assert_eq!(LogKind::parse("乱写"), None);
    }

    #[test]
    fn 应用日志是_json_lines_且字段齐全() {
        let (dir, writer) = writer();
        let record = AppLogRecord::new(LogLevel::Info, "sources", "source.fetch")
            .trace("tr-1")
            .span("sp-1")
            .duration(284)
            .params(json!({
                "url": "https://bestcf.pages.dev/domain/all.txt",
                "uuid": "550e8400-e29b-41d4-a716-446655440000",
            }))
            .result(json!({ "entries": 12 }));
        writer.write_app(&record).unwrap();

        let lines = raw_lines(dir.path(), LogKind::App);
        assert_eq!(lines.len(), 1);
        let value: Value = serde_json::from_str(&lines[0]).unwrap();
        assert!(value["ts"].as_str().unwrap().ends_with('Z'));
        assert_eq!(value["level"], "INFO");
        assert_eq!(value["traceId"], "tr-1");
        assert_eq!(value["spanId"], "sp-1");
        assert_eq!(value["module"], "sources");
        assert_eq!(value["event"], "source.fetch");
        assert_eq!(value["durationMs"], 284);
        assert_eq!(value["params"]["entries"], Value::Null);
        assert_eq!(value["params"]["uuid"], "****");
        assert_eq!(value["result"]["entries"], 12);
    }

    #[test]
    fn 应用日志记录错误堆栈() {
        let (dir, writer) = writer();
        let record = AppLogRecord::new(LogLevel::Error, "sources", "source.fetch")
            .error("请求超时（10s）", Some("at fetch (fetch.rs:12)"));
        writer.write_app(&record).unwrap();

        let lines = raw_lines(dir.path(), LogKind::App);
        let value: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(value["level"], "ERROR");
        assert_eq!(value["error"]["message"], "请求超时（10s）");
        assert_eq!(value["error"]["stack"], "at fetch (fetch.rs:12)");
    }

    #[test]
    fn 操作日志是中文单行并可读回() {
        let (dir, writer) = writer();
        writer
            .write_operation(LogLevel::Warn, Some("tr-9"), "源 a 抓取失败：HTTP 404")
            .unwrap();

        let lines = raw_lines(dir.path(), LogKind::Operation);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("WARN"));
        assert!(lines[0].contains("源 a 抓取失败：HTTP 404"));

        let entries = writer.read_tail(LogKind::Operation, 10);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].level, "WARN");
        assert_eq!(entries[0].text, "源 a 抓取失败：HTTP 404");
        assert_eq!(entries[0].trace_id.as_deref(), Some("tr-9"));
    }

    #[test]
    fn 凭据在落盘前被脱敏() {
        let (dir, writer) = writer();
        writer
            .write_operation(
                LogLevel::Info,
                None,
                "模板校验 uuid=550e8400-e29b-41d4-a716-446655440000 password=hunter2",
            )
            .unwrap();

        let lines = raw_lines(dir.path(), LogKind::Operation);
        assert!(!lines[0].contains("550e8400"));
        assert!(!lines[0].contains("hunter2"));
        assert!(lines[0].contains("****"));
    }

    #[test]
    fn 手动清理写入已清理记录到应用日志() {
        let (_dir, writer) = writer();
        writer
            .write_operation(LogLevel::Info, None, "抓取完成")
            .unwrap();
        writer
            .write_operation(LogLevel::Info, None, "另一条")
            .unwrap();

        let report = writer.clear(LogKind::Operation).unwrap();
        assert_eq!(report.files, 1);
        assert_eq!(report.entries, 2);
        assert!(writer.read_tail(LogKind::Operation, 10).is_empty());

        let app = writer.read_tail(LogKind::App, 10);
        let last = app.last().unwrap();
        assert_eq!(last.event.as_deref(), Some("log.clear"));
        assert!(last.text.contains("已清理 2 条 / 1 个文件"));
    }

    #[test]
    fn 过期清理把动作记入应用日志() {
        let (dir, writer) = writer();
        // 造一个 40 天前的操作日志文件。
        let old = dir.path().join("operation-2026-01-01.log");
        std::fs::write(&old, "旧日志\n").unwrap();
        let modified = SystemTime::now() - Duration::from_secs(40 * 86_400);
        File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(modified)
            .unwrap();

        let retention = LogRetention {
            operation_days: 30,
            app_days: 30,
            singbox_days: 30,
        };
        let reports = writer.cleanup_expired(&retention, SystemTime::now());

        assert!(!old.exists());
        let operation = reports
            .iter()
            .find(|(kind, _, _)| *kind == LogKind::Operation)
            .map(|(_, report, _)| *report)
            .unwrap();
        assert_eq!(operation.files, 1);
        assert_eq!(operation.entries, 1);

        let app = writer.read_tail(LogKind::App, 10);
        let operation_record = app
            .iter()
            .find(|entry| entry.text.contains("\"kind\":\"operation\""))
            .expect("操作日志的清理记录应写入应用日志");
        assert_eq!(operation_record.event.as_deref(), Some("log.cleanup"));
        assert!(operation_record.text.contains("\"files\":1"));
        assert!(operation_record.text.contains("已清理 1 条 / 1 个文件"));
    }

    #[test]
    fn singbox_输出原样落盘并推断级别() {
        let (dir, writer) = writer();
        let sink = SingboxLog::spawn(writer.clone());
        sink.push("stdout", "sing-box 1.12.4 (go1.24) windows/amd64");
        sink.push("stderr", "outbound/vless[out-5]: WARN handshake timeout");
        sink.shutdown();

        let lines = raw_lines(dir.path(), LogKind::Singbox);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "sing-box 1.12.4 (go1.24) windows/amd64");

        let entries = writer.read_tail(LogKind::Singbox, 10);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].level, "WARN");
        assert_eq!(
            entries[1].text,
            "outbound/vless[out-5]: WARN handshake timeout"
        );
    }

    #[test]
    fn 读回跨文件按时间从旧到新() {
        let (dir, writer) = writer();
        std::fs::write(dir.path().join("operation-2026-11-12.log"), "旧一\n旧二\n").unwrap();
        std::fs::write(dir.path().join("operation-2026-11-13.log"), "新一\n").unwrap();

        // 手工文件没有合法前缀，补上时间与级别。
        std::fs::write(
            dir.path().join("operation-2026-11-12.log"),
            "2026-11-12T01:00:00.000Z INFO - 旧一\n2026-11-12T01:00:01.000Z INFO - 旧二\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("operation-2026-11-13.log"),
            "2026-11-13T01:00:00.000Z INFO - 新一\n",
        )
        .unwrap();

        let entries = writer.read_tail(LogKind::Operation, 2);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].text, "旧二");
        assert_eq!(entries[1].text, "新一");
    }

    #[test]
    fn trace_id_与_span_id_每次不同() {
        let a = new_trace_id();
        let b = new_trace_id();
        assert_ne!(a, b);
        assert!(a.starts_with("tr-"));
        assert_eq!(new_span_id().len(), 8);
        assert_ne!(new_span_id(), new_span_id());
    }

    #[test]
    fn 零点调度间隔正确() {
        let now = Local.with_ymd_and_hms(2026, 11, 13, 23, 59, 0).unwrap();
        assert_eq!(
            duration_until_next_local_midnight(now),
            Duration::from_secs(60)
        );
        let midnight = Local.with_ymd_and_hms(2026, 11, 13, 0, 0, 0).unwrap();
        assert_eq!(
            duration_until_next_local_midnight(midnight),
            Duration::from_secs(86_400)
        );
    }

    #[test]
    fn 时间戳格式正确() {
        assert_eq!(
            iso8601_from_millis(1_763_006_706_789),
            "2025-11-13T04:05:06.789Z"
        );
        assert_eq!(iso8601_from_millis(0), "1970-01-01T00:00:00.000Z");
    }
}
