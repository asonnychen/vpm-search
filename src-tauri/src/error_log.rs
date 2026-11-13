//! 最小的错误落盘函数。
//!
//! 阶段 4 会把三类日志正式化，这里先保证任何读写失败都有一行可检索的 JSON 记录，
//! 界面不会因为存储异常而崩溃。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 应用日志文件名。阶段 4 沿用同一路径。
pub const APP_LOG_FILE: &str = "app.log";

/// 把 UTC 毫秒时间戳格式化成 `YYYY-MM-DDTHH:MM:SS.mmmZ`。
fn iso8601_from_millis(millis: u128) -> String {
    let total_secs = (millis / 1000) as i64;
    let ms = millis % 1000;
    let days = total_secs.div_euclid(86_400);
    let secs_of_day = total_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{ms:03}Z")
}

/// Howard Hinnant 的 civil_from_days：把 1970-01-01 起的天数还原成年月日。
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
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    iso8601_from_millis(millis)
}

/// 追加一行 JSON 错误记录到 `<log_dir>/app.log`。
///
/// 写入日志本身失败时只返回 `Err`，调用方应当忽略，避免二次崩溃。
pub fn append_error(log_dir: &Path, event: &str, message: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(log_dir)?;
    let path = log_dir.join(APP_LOG_FILE);
    let line = serde_json::json!({
        "time": now_iso8601(),
        "level": "ERROR",
        "event": event,
        "message": message,
    });
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "{line}")?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 时间戳格式正确() {
        // 2025-11-13T04:05:06.789Z
        assert_eq!(
            iso8601_from_millis(1_763_006_706_789),
            "2025-11-13T04:05:06.789Z"
        );
        assert_eq!(iso8601_from_millis(0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn 错误日志追加成_json_lines() {
        let dir = tempfile::tempdir().unwrap();
        append_error(dir.path(), "db.open", "无法打开数据库").unwrap();
        append_error(dir.path(), "settings.save", "写入失败").unwrap();

        let text = std::fs::read_to_string(dir.path().join(APP_LOG_FILE)).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in lines {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(value["level"], "ERROR");
            assert!(value["time"].as_str().unwrap().ends_with('Z'));
        }
    }
}
