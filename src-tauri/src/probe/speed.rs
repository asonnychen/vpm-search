//! 测速：直连优选地址并发下载固定字节数，换算成 MB/s。
//!
//! 设计要点（见 GLOSSARY 的「测速」）：
//! - 4 条并发流、合计 10MB、总时长上限 15 秒；
//! - 1MB = 1,000,000 字节，结果保留一位小数；
//! - 任一流失败或整体超时按失败处理，绝不写入错误的 0。

use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::store::Store;

use super::batch::{BatchEvent, BatchItem, BatchProgress};
use super::{Endpoint, ProbeOptions, ProbeStatus};

/// 测速端点；`bytes` 查询参数决定每条流下载多少字节。
pub const SPEED_URL: &str = "https://speed.cloudflare.com/__down";

/// 默认 4 条并发流。
pub const SPEED_STREAMS: usize = 4;
/// 默认合计 10MB（十进制，1MB = 1,000,000 字节）。
pub const SPEED_TOTAL_BYTES: u64 = 10_000_000;
/// 默认总时长上限 15 秒。
pub const SPEED_TIMEOUT_S: u64 = 15;
/// 换算用的百万字节。
pub const BYTES_PER_MB: f64 = 1_000_000.0;

/// 一次测速的参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeedOptions {
    pub streams: usize,
    pub total_bytes: u64,
    pub timeout: Duration,
}

impl Default for SpeedOptions {
    fn default() -> Self {
        Self::new(
            SPEED_STREAMS,
            SPEED_TOTAL_BYTES,
            Duration::from_secs(SPEED_TIMEOUT_S),
        )
    }
}

impl SpeedOptions {
    /// 把流数与时长夹到可用范围，避免除零与无限等待。
    pub fn new(streams: usize, total_bytes: u64, timeout: Duration) -> Self {
        Self {
            streams: streams.clamp(1, 32),
            total_bytes: total_bytes.max(1),
            timeout: timeout.max(Duration::from_secs(1)),
        }
    }

    /// 每条流应下载的字节数。
    pub fn bytes_per_stream(&self) -> u64 {
        (self.total_bytes / self.streams as u64).max(1)
    }

    /// 测速端点地址。
    pub fn url(&self) -> String {
        format!("{SPEED_URL}?bytes={}", self.bytes_per_stream())
    }
}

/// 多条流的总字节数与总耗时换算成 MB/s。
pub fn combined_mbps(bytes: &[u64], elapsed: Duration) -> f64 {
    let total: u64 = bytes.iter().sum();
    let seconds = elapsed.as_secs_f64();
    if total == 0 || seconds <= 0.0 {
        return 0.0;
    }
    total as f64 / BYTES_PER_MB / seconds
}

/// 保留一位小数。
pub fn round_mbps(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// 判定一次测速是否成立：任一流没有数据或整体超时都算失败。
pub fn evaluate(bytes: &[u64], elapsed: Duration, limit: Duration) -> Result<f64, String> {
    if elapsed > limit {
        return Err(format!("测速超过 {} 秒上限", limit.as_secs()));
    }
    let total: u64 = bytes.iter().sum();
    if total == 0 {
        return Err("测速没有收到数据".to_string());
    }
    let seconds = elapsed.as_secs_f64();
    if seconds <= 0.0 {
        return Err("测速耗时无效".to_string());
    }
    Ok(round_mbps(combined_mbps(bytes, elapsed)))
}

/// 并发下载固定字节数，返回 MB/s。
///
/// 客户端由调用方建好（已把探测主机名钉到目标地址）；4 条流共享同一个客户端。
/// 任一流失败、被停止或整体超过上限都会返回 `Err`，不会写入 0。
pub fn download_streams(
    client: &reqwest::blocking::Client,
    speed: &SpeedOptions,
    cancel: &AtomicBool,
) -> Result<f64, String> {
    let url = speed.url();
    let per_stream = speed.bytes_per_stream();
    let started = std::time::Instant::now();

    let results: Vec<Result<u64, String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..speed.streams)
            .map(|_| {
                let url = &url;
                scope.spawn(move || download_one(client, url, per_stream, cancel))
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|_| Err("测速线程异常结束".to_string()))
            })
            .collect()
    });

    let elapsed = started.elapsed();
    let mut bytes = Vec::with_capacity(results.len());
    for result in results {
        bytes.push(result?);
    }
    evaluate(&bytes, elapsed, speed.timeout)
}

/// 单条流：下载到指定字节数或流结束，期间响应停止信号。
fn download_one(
    client: &reqwest::blocking::Client,
    url: &str,
    limit: u64,
    cancel: &AtomicBool,
) -> Result<u64, String> {
    let response = client.get(url).send().map_err(|error| {
        if error.is_timeout() {
            format!("测速请求超时：{error}")
        } else {
            format!("测速连接失败：{error}")
        }
    })?;
    if !response.status().is_success() {
        return Err(format!(
            "测速端点返回 HTTP {}",
            response.status().as_u16()
        ));
    }

    let mut reader = response.take(limit);
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err("测速已停止".to_string());
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("读取测速数据失败：{error}"))?;
        if read == 0 {
            break;
        }
        total += read as u64;
    }
    if total == 0 {
        return Err("测速端点没有返回数据".to_string());
    }
    Ok(total)
}

/// 一条地址的测速结论。`Ok` 带 MB/s，失败时 `mbps` 必须是 `None`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeedOutcome {
    pub status: ProbeStatus,
    pub mbps: Option<f64>,
    pub error: Option<String>,
}

impl SpeedOutcome {
    pub fn ok(mbps: f64) -> Self {
        Self {
            status: ProbeStatus::Ok,
            mbps: Some(round_mbps(mbps)),
            error: None,
        }
    }

    pub fn fail(error: impl Into<String>) -> Self {
        Self {
            status: ProbeStatus::Fail,
            mbps: None,
            error: Some(error.into()),
        }
    }

    /// 被停止时还没开始的行，保留该行原有的速度值。
    pub fn untested() -> Self {
        Self {
            status: ProbeStatus::Untested,
            mbps: None,
            error: None,
        }
    }
}

/// 一条地址的测速结果。
#[derive(Debug, Clone, PartialEq)]
pub struct SpeedRow {
    pub endpoint: Endpoint,
    pub alias: String,
    pub outcome: SpeedOutcome,
}

/// 一轮测速的统计。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeedCounts {
    pub ok: usize,
    pub fail: usize,
    pub untested: usize,
}

/// 一轮测速的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct SpeedReport {
    pub rows: Vec<SpeedRow>,
    pub total: usize,
    pub completed: usize,
    pub stopped: bool,
}

impl SpeedReport {
    pub fn counts(&self) -> SpeedCounts {
        let mut counts = SpeedCounts::default();
        for row in &self.rows {
            match row.outcome.status {
                ProbeStatus::Ok => counts.ok += 1,
                ProbeStatus::Fail => counts.fail += 1,
                ProbeStatus::Untested => counts.untested += 1,
                ProbeStatus::Timeout => counts.fail += 1,
            }
        }
        counts
    }
}

/// 可跑测速的探测器：槽位池与假探测器都实现它。
pub trait SpeedProber: Send + Sync {
    fn speed_sample(
        &self,
        endpoint: &Endpoint,
        options: &ProbeOptions,
        speed: &SpeedOptions,
        cancel: &AtomicBool,
    ) -> Result<f64, String>;
}

/// 跑一轮测速。停止后已开始的地址保留结论，未开始的记为未测。
///
/// 每完成一条推一次逐行事件（`speedMbps` 有值、`latencyMs` 为空），再推计数进度。
pub fn run_speed_batch(
    prober: &dyn SpeedProber,
    items: &[BatchItem],
    options: &ProbeOptions,
    speed: &SpeedOptions,
    concurrency: usize,
    cancel: &AtomicBool,
    on_event: &(dyn Fn(BatchEvent) + Sync),
) -> Result<SpeedReport, String> {
    let total = items.len();
    let concurrency = concurrency.clamp(1, 32);
    let next = AtomicUsize::new(0);
    let completed = AtomicUsize::new(0);
    let results: Vec<Mutex<Option<SpeedRow>>> = (0..total).map(|_| Mutex::new(None)).collect();

    let workers = concurrency.min(total);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                let index = next.fetch_add(1, Ordering::SeqCst);
                if index >= total {
                    break;
                }

                let item = &items[index];
                let outcome = match prober.speed_sample(&item.endpoint, options, speed, cancel) {
                    Ok(mbps) => SpeedOutcome::ok(mbps),
                    Err(error) => SpeedOutcome::fail(error),
                };
                if let Ok(mut slot) = results[index].lock() {
                    *slot = Some(SpeedRow {
                        endpoint: item.endpoint.clone(),
                        alias: item.alias.clone(),
                        outcome: outcome.clone(),
                    });
                }

                on_event(BatchEvent::Row(crate::probe::RowEvent {
                    speed_mbps: outcome.mbps,
                    error: outcome.error.clone(),
                    ..crate::probe::RowEvent::new(&item.endpoint, outcome.status)
                }));
                let done = completed.fetch_add(1, Ordering::SeqCst) + 1;
                on_event(BatchEvent::Progress(BatchProgress {
                    completed: done,
                    total,
                }));
            });
        }
    });

    let rows = items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            results[index]
                .lock()
                .ok()
                .and_then(|slot| slot.clone())
                .unwrap_or_else(|| SpeedRow {
                    endpoint: item.endpoint.clone(),
                    alias: item.alias.clone(),
                    outcome: SpeedOutcome::untested(),
                })
        })
        .collect();

    Ok(SpeedReport {
        rows,
        total,
        completed: completed.load(Ordering::SeqCst),
        stopped: cancel.load(Ordering::SeqCst),
    })
}

/// 把测速结果写进该轮已有的结果行；没有行时才新建。
pub fn persist_speed(store: &Store, run_id: &str, rows: &[SpeedRow]) -> Result<(), String> {
    for row in rows {
        if row.outcome.status == ProbeStatus::Untested {
            // 停止时没轮到的行不覆盖原有速度。
            continue;
        }
        store
            .upsert_test_result_speed(run_id, &row.endpoint, &row.alias, &row.outcome)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::Attempt;

    #[test]
    fn 吞吐换算按百万字节与耗时() {
        assert_eq!(combined_mbps(&[4_000_000], Duration::from_secs(2)), 2.0);
        assert_eq!(combined_mbps(&[1_500_000], Duration::from_millis(500)), 3.0);
    }

    #[test]
    fn 速度保留一位小数() {
        assert_eq!(round_mbps(12.34), 12.3);
        assert_eq!(round_mbps(12.36), 12.4);
        assert_eq!(round_mbps(0.04), 0.0);
    }

    #[test]
    fn 四条流合并为总字节除以总耗时() {
        let bytes = [2_500_000, 2_500_000, 2_500_000, 2_500_000];
        assert_eq!(combined_mbps(&bytes, Duration::from_secs(2)), 5.0);
        assert_eq!(
            evaluate(&bytes, Duration::from_secs(2), Duration::from_secs(15)).unwrap(),
            5.0
        );
    }

    #[test]
    fn 超过总时长上限判失败() {
        let bytes = [2_500_000; 4];
        let error =
            evaluate(&bytes, Duration::from_secs(16), Duration::from_secs(15)).unwrap_err();
        assert!(error.contains("15"));
    }

    #[test]
    fn 没有收到数据判失败而不是写_0() {
        assert!(
            evaluate(&[0, 0, 0, 0], Duration::from_secs(1), Duration::from_secs(15)).is_err()
        );
        let outcome = SpeedOutcome::fail("连接被拒绝");
        assert_eq!(outcome.status, ProbeStatus::Fail);
        assert_eq!(outcome.mbps, None);
    }

    #[test]
    fn 默认参数是四流十兆十五秒() {
        let speed = SpeedOptions::default();
        assert_eq!(speed.streams, 4);
        assert_eq!(speed.total_bytes, 10_000_000);
        assert_eq!(speed.timeout, Duration::from_secs(15));
        assert_eq!(speed.bytes_per_stream(), 2_500_000);
        assert_eq!(
            speed.url(),
            "https://speed.cloudflare.com/__down?bytes=2500000"
        );
    }

    fn item(host: &str, port: u16) -> BatchItem {
        BatchItem::new(Endpoint::new(host, port), format!("{host} 别名"))
    }

    #[test]
    fn 停止后未开始的行保持未测() {
        let prober = crate::probe::fake::FakeProber::default()
            .with_speeds(vec![(("104.16.8.1", 8443), Ok(9.9))]);
        let items = vec![item("104.16.8.1", 8443), item("104.16.8.2", 8443)];
        let report = run_speed_batch(
            &prober,
            &items,
            &ProbeOptions::default(),
            &SpeedOptions::default(),
            2,
            &AtomicBool::new(true),
            &|_| {},
        )
        .unwrap();

        assert!(report.stopped);
        assert_eq!(report.completed, 0);
        assert!(report
            .rows
            .iter()
            .all(|row| row.outcome.status == ProbeStatus::Untested));
        assert!(prober.speed_calls().is_empty());
    }

    #[test]
    fn 测速批量写进同一结果行且不清空延迟() {
        let store = Store::open_in_memory().unwrap();
        let items = vec![item("104.16.9.1", 8443), item("104.16.9.2", 8443)];

        // 先跑一轮延迟批量并落库，形成两行结果。
        let latency = crate::probe::fake::FakeProber::scripted(vec![
            (("104.16.9.1", 8443), vec![Attempt::responded(80); 3]),
            (("104.16.9.2", 8443), vec![Attempt::responded(140); 3]),
        ]);
        let report = crate::probe::batch::run_batch(
            &latency,
            &items,
            &ProbeOptions::default(),
            crate::probe::batch::BatchOptions::new(3, 2),
            &AtomicBool::new(false),
            &|_| {},
        )
        .unwrap();
        crate::probe::batch::persist_batch(
            &store,
            "run-speed-1",
            "2026-11-13T00:00:00.000Z",
            "2026-11-13T00:00:05.000Z",
            &serde_json::json!({ "samples": 3 }),
            &report,
        )
        .unwrap();
        assert_eq!(store.count_test_results().unwrap(), 2);

        // 再独立跑一次测速：一条成功、一条失败。
        let prober = crate::probe::fake::FakeProber::scripted(vec![]).with_speeds(vec![
            (("104.16.9.1", 8443), Ok(12.3)),
            (("104.16.9.2", 8443), Err("连接被拒绝".to_string())),
        ]);
        let speed_report = run_speed_batch(
            &prober,
            &items,
            &ProbeOptions::default(),
            &SpeedOptions::default(),
            2,
            &AtomicBool::new(false),
            &|_| {},
        )
        .unwrap();
        persist_speed(&store, "run-speed-1", &speed_report.rows).unwrap();

        // 不新建重复行：仍是同一轮的两条记录。
        assert_eq!(store.count_test_results().unwrap(), 2);
        assert_eq!(store.count_test_runs().unwrap(), 1);

        let rows = store.list_test_results("run-speed-1").unwrap();
        assert_eq!(rows[0].latency_ms, Some(80), "延迟不能被测速清空");
        assert_eq!(rows[0].speed_mbps, Some(12.3));
        assert_eq!(rows[0].speed_status, "ok");
        assert_eq!(rows[1].latency_ms, Some(140), "失败的那条也保留延迟");
        assert_eq!(rows[1].speed_mbps, None, "失败不写入 0");
        assert_eq!(rows[1].speed_status, "fail");
        assert_eq!(rows[1].speed_error.as_deref(), Some("连接被拒绝"));
    }
}
