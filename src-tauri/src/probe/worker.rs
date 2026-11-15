//! 探测子进程（`--probe-worker`）：所有网络探测都在这里跑。
//!
//! 主进程只负责拉起子进程、逐行读 stdout、写库与推事件；停止时直接
//! `Child::kill`，不依赖任何协作式取消。子进程从 stdin 读一份 JSON 任务，
//! 每完成一条目标就往 stdout 写一行 JSON，全部结束后以 0 退出。
//!
//! 自派生：`main` 在进入 Tauri 之前看到 `--probe-worker` 就调用 [`run`]，
//! 不初始化窗口，因此打包产物里不需要第二个二进制。

use std::io::{Read, Write};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::speed::{SpeedOptions, SpeedProber};
use super::{
    DirectProber, Endpoint, ProbeOptions, ProbeStatus, RowEvent,
};

/// 命令行参数：出现即表示以探测子进程身份运行。
pub const WORKER_ARG: &str = "--probe-worker";

/// 子进程默认的连接超时（秒）。
fn default_connect_timeout_s() -> u32 {
    5
}

/// 子进程默认的采样次数。
fn default_sample_count() -> usize {
    super::LATENCY_SAMPLES
}

/// 一条待测目标。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerTarget {
    pub host: String,
    pub port: u16,
}

/// 测速参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerSpeed {
    pub streams: usize,
    /// 合计下载字节数。
    pub bytes: u64,
    pub timeout_s: u64,
}

impl Default for WorkerSpeed {
    fn default() -> Self {
        let speed = SpeedOptions::default();
        Self {
            streams: speed.streams,
            bytes: speed.total_bytes,
            timeout_s: speed.timeout.as_secs(),
        }
    }
}

impl WorkerSpeed {
    pub fn to_options(&self) -> SpeedOptions {
        SpeedOptions::new(
            self.streams,
            self.bytes,
            Duration::from_secs(self.timeout_s.max(1)),
        )
    }
}

/// stdin 里的整份任务。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerTask {
    /// `delay` / `speed` / `geo`。
    pub mode: String,
    pub targets: Vec<WorkerTarget>,
    /// 同时测几条目标，夹在 1–32。
    pub concurrency: usize,
    #[serde(default = "default_connect_timeout_s")]
    pub connect_timeout_s: u32,
    #[serde(default = "default_sample_count")]
    pub sample_count: usize,
    #[serde(default)]
    pub speed: WorkerSpeed,
}

impl WorkerTask {
    pub fn probe_options(&self) -> ProbeOptions {
        ProbeOptions::from_connect_timeout_s(self.connect_timeout_s)
    }
}

/// stdout 里的一行结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerLine {
    pub kind: String,
    pub host: String,
    pub port: u16,
    pub status: ProbeStatus,
    #[serde(default)]
    pub latency_ms: Option<u64>,
    #[serde(default)]
    pub speed_mbps: Option<f64>,
    #[serde(default)]
    pub country_zh: Option<String>,
    #[serde(default)]
    pub country_en: Option<String>,
    /// 国家码；协议里是附加字段，主进程落库时用得上。
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

impl WorkerLine {
    /// 把内部行事件转成协议行。
    pub fn from_row(kind: &str, row: &RowEvent) -> Self {
        Self {
            kind: kind.to_string(),
            host: row.host.clone(),
            port: row.port,
            status: row.status,
            latency_ms: row.latency_ms,
            speed_mbps: row.speed_mbps,
            country_zh: row.country_zh.clone(),
            country_en: row.country_en.clone(),
            country: row.country.clone(),
            error: row.error.clone(),
        }
    }

    /// 主进程把它还原成逐行事件（`colo` 恒为 `None`）。
    pub fn to_row(&self) -> RowEvent {
        RowEvent {
            host: self.host.clone(),
            port: self.port,
            status: self.status,
            latency_ms: self.latency_ms,
            speed_mbps: self.speed_mbps,
            country_zh: self.country_zh.clone(),
            country_en: self.country_en.clone(),
            country: self.country.clone(),
            colo: None,
            error: self.error.clone(),
        }
    }

    /// 解析一行 stdout；坏行返回错误，调用方记日志后跳过。
    pub fn parse(line: &str) -> Result<Self, String> {
        serde_json::from_str(line).map_err(|e| format!("探测行不是合法 JSON：{e}"))
    }
}

/// 子进程入口：读 stdin 的任务，把结果写到 stdout，返回退出码。
pub fn run() -> i32 {
    let mut input = String::new();
    if let Err(error) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("读取探测任务失败：{error}");
        return 2;
    }
    let task: WorkerTask = match serde_json::from_str(&input) {
        Ok(task) => task,
        Err(error) => {
            eprintln!("解析探测任务失败：{error}");
            return 2;
        }
    };

    let stdout = Mutex::new(std::io::BufWriter::new(std::io::stdout()));
    run_task(&task, &|line| {
        if let Ok(mut out) = stdout.lock() {
            if let Ok(json) = serde_json::to_string(line) {
                let _ = writeln!(out, "{json}");
                let _ = out.flush();
            }
        }
    });
    0
}

/// 跑一份任务，每条完成就回调一次。测试与 `run` 共用这段调度。
pub fn run_task(task: &WorkerTask, emit: &(dyn Fn(&WorkerLine) + Sync)) {
    let options = task.probe_options();
    let concurrency = task.concurrency.clamp(1, 32).min(task.targets.len().max(1));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let cancel = AtomicBool::new(false);
    let prober = DirectProber::new_silent();
    let geo = match task.mode.as_str() {
        "geo" => Some(Arc::new(crate::geo::GeoResolver::new(
            crate::geo::offline_candidates(),
        ))),
        _ => None,
    };
    let speed = task.speed.to_options();
    let sample_count = task.sample_count.clamp(1, 10);

    std::thread::scope(|scope| {
        for _ in 0..concurrency {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let Some(target) = task.targets.get(index) else {
                    break;
                };
                let endpoint = Endpoint::new(target.host.clone(), target.port);
                let line = match task.mode.as_str() {
                    "speed" => speed_line(&prober, &endpoint, &options, &speed, &cancel),
                    "geo" => geo_line(geo.as_ref().expect("geo 任务已建好查询器"), &endpoint),
                    _ => delay_line(&prober, &endpoint, &options, sample_count),
                };
                emit(&line);
            });
        }
    });
}

fn delay_line(
    prober: &DirectProber,
    endpoint: &Endpoint,
    options: &ProbeOptions,
    samples: usize,
) -> WorkerLine {
    let outcome = prober.measure_latency_samples(endpoint, options, samples);
    WorkerLine::from_row("delay", &RowEvent::latency(endpoint, &outcome))
}

fn speed_line(
    prober: &DirectProber,
    endpoint: &Endpoint,
    options: &ProbeOptions,
    speed: &SpeedOptions,
    cancel: &AtomicBool,
) -> WorkerLine {
    let (status, mbps, error) = match prober.speed_sample(endpoint, options, speed, cancel) {
        Ok(mbps) => (ProbeStatus::Ok, Some(mbps), None),
        Err(error) => (ProbeStatus::Fail, None, Some(error)),
    };
    let row = RowEvent {
        speed_mbps: mbps,
        error,
        ..RowEvent::new(endpoint, status)
    };
    WorkerLine::from_row("speed", &row)
}

fn geo_line(geo: &crate::geo::GeoResolver, endpoint: &Endpoint) -> WorkerLine {
    let mut row = RowEvent::new(endpoint, ProbeStatus::Untested);
    let mut country = None;
    match super::resolve_host(&endpoint.host) {
        Ok(ip) => match geo.locate(&ip.to_string()) {
            Ok(info) => {
                row.status = ProbeStatus::Ok;
                country = Some(info.country.clone());
                row.country_zh = Some(info.zh);
                row.country_en = Some(info.en);
            }
            Err(error) => {
                row.status = ProbeStatus::Fail;
                row.error = Some(error);
            }
        },
        Err(error) => {
            row.status = ProbeStatus::Fail;
            row.error = Some(error);
        }
    }
    let mut line = WorkerLine::from_row("geo", &row);
    line.country = country;
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 任务默认值可省略采样与测速字段() {
        let task: WorkerTask = serde_json::from_str(
            r#"{"mode":"delay","targets":[{"host":"1.1.1.1","port":443}],"concurrency":4}"#,
        )
        .unwrap();
        assert_eq!(task.connect_timeout_s, 5);
        assert_eq!(task.sample_count, super::super::LATENCY_SAMPLES);
        assert_eq!(task.speed, WorkerSpeed::default());
    }

    #[test]
    fn 结果行可以往返() {
        let row = RowEvent {
            host: "104.16.1.1".into(),
            port: 8443,
            status: ProbeStatus::Ok,
            latency_ms: Some(88),
            speed_mbps: None,
            country_zh: Some("加拿大".into()),
            country_en: Some("Canada".into()),
            country: Some("CA".into()),
            colo: None,
            error: None,
        };
        let line = WorkerLine::from_row("delay", &row);
        let json = serde_json::to_string(&line).unwrap();
        assert!(json.contains("\"latencyMs\":88"));
        assert!(json.contains("\"kind\":\"delay\""));
        let parsed = WorkerLine::parse(&json).unwrap();
        assert_eq!(parsed.to_row(), row);
    }

    #[test]
    fn 坏行解析返回错误() {
        assert!(WorkerLine::parse("not json").is_err());
    }

    #[test]
    fn 测速参数按协议换算() {
        let speed = WorkerSpeed {
            streams: 4,
            bytes: 8_000_000,
            timeout_s: 12,
        };
        let options = speed.to_options();
        assert_eq!(options.streams, 4);
        assert_eq!(options.total_bytes, 8_000_000);
        assert_eq!(options.timeout, Duration::from_secs(12));
    }
}
