//! 日志保留期：按文件修改时间删除过期文件。
//!
//! 三类日志各写各的日文件（`app-2026-11-13.log` 这种），保留期判断只看
//! 文件的修改时间，因此正在写入的当天文件永远不会被删掉。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::LogKind;

/// 一次清理的结果：删掉了多少个文件、多少行日志。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CleanupReport {
    pub files: usize,
    pub entries: usize,
}

/// 早于这个时间点的文件视为过期。
pub fn cutoff(now: SystemTime, days: u32) -> SystemTime {
    now.checked_sub(retention_duration(days))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// 文件的修改时间是否早于保留期。
pub fn is_expired(modified: SystemTime, now: SystemTime, days: u32) -> bool {
    modified < cutoff(now, days)
}

/// 文件名是否属于该类日志。
fn matches_kind(name: &str, kind: LogKind) -> bool {
    if !name.ends_with(".log") {
        return false;
    }
    // 阶段 2/3 的错误日志固定叫 app.log，继续算应用日志。
    if kind == LogKind::App && name == "app.log" {
        return true;
    }
    name.starts_with(&format!("{}-", kind.prefix()))
}

/// 列出某类日志在目录里的全部文件，按文件名升序（即按日期升序）。
pub fn log_files(dir: &Path, kind: LogKind) -> io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };

    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if matches_kind(name, kind) && entry.file_type()?.is_file() {
            files.push(entry.path());
        }
    }
    files.sort();
    Ok(files)
}

/// 删除某类日志里修改时间早于保留期的文件。
pub fn cleanup(dir: &Path, kind: LogKind, days: u32, now: SystemTime) -> io::Result<CleanupReport> {
    let mut report = CleanupReport::default();
    for path in log_files(dir, kind)? {
        let modified = fs::metadata(&path)?.modified()?;
        if !is_expired(modified, now, days) {
            continue;
        }
        report.entries += count_lines(&path).unwrap_or(0);
        fs::remove_file(&path)?;
        report.files += 1;
    }
    Ok(report)
}

/// 删除某类日志的全部文件（手动清理用），返回条数与文件数。
pub fn clear(dir: &Path, kind: LogKind) -> io::Result<CleanupReport> {
    let mut report = CleanupReport::default();
    for path in log_files(dir, kind)? {
        report.entries += count_lines(&path).unwrap_or(0);
        fs::remove_file(&path)?;
        report.files += 1;
    }
    Ok(report)
}

/// 数一个文件有多少行，用于「已清理 N 条」。
pub fn count_lines(path: &Path) -> io::Result<usize> {
    use std::io::{BufRead, BufReader};
    let file = fs::File::open(path)?;
    let mut count = 0;
    for line in BufReader::new(file).lines() {
        line?;
        count += 1;
    }
    Ok(count)
}

/// 保留期的天数换算成时长。
pub fn retention_duration(days: u32) -> Duration {
    Duration::from_secs(u64::from(days) * 86_400)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    fn write_log(dir: &Path, name: &str, lines: usize, age_days: u64) -> PathBuf {
        let path = dir.join(name);
        let mut text = String::new();
        for i in 0..lines {
            text.push_str(&format!("第 {i} 行\n"));
        }
        fs::write(&path, text).unwrap();
        let modified = SystemTime::now() - Duration::from_secs(age_days * 86_400);
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        path
    }

    #[test]
    fn 过期文件按修改时间删除未过期保留() {
        let dir = tempfile::tempdir().unwrap();
        let old = write_log(dir.path(), "app-2026-01-01.log", 2, 31);
        let fresh = write_log(dir.path(), "app-2026-11-13.log", 3, 1);

        let report = cleanup(dir.path(), LogKind::App, 30, SystemTime::now()).unwrap();

        assert!(!old.exists());
        assert!(fresh.exists());
        assert_eq!(report.files, 1);
        assert_eq!(report.entries, 2);
    }

    #[test]
    fn 只删同类日志文件() {
        let dir = tempfile::tempdir().unwrap();
        let app = write_log(dir.path(), "app-2026-01-01.log", 1, 90);
        let operation = write_log(dir.path(), "operation-2026-01-01.log", 1, 90);

        cleanup(dir.path(), LogKind::App, 30, SystemTime::now()).unwrap();

        assert!(!app.exists());
        assert!(operation.exists());
    }

    #[test]
    fn 手动清理删除全部文件并返回条数() {
        let dir = tempfile::tempdir().unwrap();
        write_log(dir.path(), "singbox-2026-11-12.log", 4, 1);
        write_log(dir.path(), "singbox-2026-11-13.log", 3, 0);

        let report = clear(dir.path(), LogKind::Singbox).unwrap();

        assert_eq!(report.files, 2);
        assert_eq!(report.entries, 7);
        assert!(log_files(dir.path(), LogKind::Singbox).unwrap().is_empty());
    }

    #[test]
    fn 目录不存在时不报错() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("没有这个目录");
        assert_eq!(
            cleanup(&missing, LogKind::Operation, 30, SystemTime::now()).unwrap(),
            CleanupReport::default()
        );
        assert_eq!(
            clear(&missing, LogKind::Operation).unwrap(),
            CleanupReport::default()
        );
    }
}
