//! 批量并发测试：整池地址按并发上限调度，每条采样多次后聚合、落库。
//!
//! 调度与聚合与具体探测器无关；直连探测器通过 [`BatchProber`] 接入，
//! 测试里换成假探测器即可跑通整轮。
//!
//! 并发模型：探测全是阻塞 I/O（DNS + TCP + TLS），所以在
//! [`std::thread::scope`] 里开 `concurrency` 个 OS 线程当并发槽位，
//! 每个槽位自己领地址、解析、请求、聚合，主线程只等整轮结束，不在循环里
//! 逐条 `await`。命令层用 `tauri::async_runtime::spawn_blocking`（该运行时
//! 默认多线程）把整轮挪出异步线程池，因此槽位可以真正同时跑在不同核心上。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use serde::Serialize;

use crate::store::Store;

use super::{Endpoint, ProbeOptions, ProbeOutcome, ProbeStatus};

/// 地址间并发默认值与允许范围。
///
/// 默认值不再是固定 10：按逻辑核数 × 2 取，并夹在 8–32 之间，
/// 让多核机器真正把并发槽位铺满；用户设置里已有的值优先。
pub const MIN_DEFAULT_CONCURRENCY: usize = 8;
pub const MIN_CONCURRENCY: usize = 1;
pub const MAX_CONCURRENCY: usize = 32;

/// 逻辑核数 × 2，夹在 8–32；读不到核数时按 1 核算（即下限 8）。
pub fn default_concurrency() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1);
    cores
        .saturating_mul(2)
        .clamp(MIN_DEFAULT_CONCURRENCY, MAX_CONCURRENCY)
}

/// 每条地址默认采样次数。
pub const DEFAULT_SAMPLES: usize = 3;

/// 一条待测地址与它的别名。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchItem {
    pub endpoint: Endpoint,
    pub alias: String,
}

impl BatchItem {
    pub fn new(endpoint: Endpoint, alias: impl Into<String>) -> Self {
        Self {
            endpoint,
            alias: alias.into(),
        }
    }
}

/// 一轮批量的调度参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchOptions {
    /// 每条地址采样几次，夹在 1–10。
    pub samples: usize,
    /// 同时测几条地址，夹在 1–32。
    pub concurrency: usize,
}

impl Default for BatchOptions {
    fn default() -> Self {
        Self::new(DEFAULT_SAMPLES, default_concurrency())
    }
}

impl BatchOptions {
    pub fn new(samples: usize, concurrency: usize) -> Self {
        Self {
            samples: samples.clamp(1, 10),
            concurrency: concurrency.clamp(MIN_CONCURRENCY, MAX_CONCURRENCY),
        }
    }
}

/// 批量探测器：`prepare` 做整轮前的准备（直连探测器什么都不做），`cleanup` 收尾。
pub trait BatchProber: Send + Sync {
    /// 整轮开始前准备资源。默认什么都不做。
    fn prepare(&self, _items: &[BatchItem], _concurrency: usize) -> Result<(), String> {
        Ok(())
    }

    /// 对一条地址采一次样。
    fn sample(&self, endpoint: &Endpoint, options: &ProbeOptions) -> ProbeOutcome;

    /// 整轮结束后回收资源。默认什么都不做。
    fn cleanup(&self) {}
}

/// 一条地址的完整结果：原始采样 + 聚合结论。
#[derive(Debug, Clone, PartialEq)]
pub struct BatchRow {
    pub endpoint: Endpoint,
    pub alias: String,
    /// 按采样顺序保存的原始结论；未测时为空。
    pub samples: Vec<ProbeOutcome>,
    /// 聚合后的结论（三次取最小、部分超时不影响、全超时记超时）。
    pub outcome: ProbeOutcome,
}

impl BatchRow {
    /// 三次采样落库用的 JSON 数组。
    pub fn samples_json(&self) -> String {
        serde_json::to_string(&self.samples).unwrap_or_else(|_| "[]".to_string())
    }
}

/// 推给界面的进度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchProgress {
    pub completed: usize,
    pub total: usize,
}

/// 批量测试推给界面的事件：计数进度 + 每完成一条的逐行结论。
#[derive(Debug, Clone, PartialEq)]
pub enum BatchEvent {
    Progress(BatchProgress),
    Row(crate::probe::RowEvent),
}

/// 一轮的统计计数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchCounts {
    pub ok: usize,
    pub timeout: usize,
    pub fail: usize,
    pub untested: usize,
}

/// 一轮批量的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct BatchReport {
    pub rows: Vec<BatchRow>,
    pub total: usize,
    pub completed: usize,
    pub stopped: bool,
}

impl BatchReport {
    pub fn counts(&self) -> BatchCounts {
        let mut counts = BatchCounts::default();
        for row in &self.rows {
            match row.outcome.status {
                ProbeStatus::Ok => counts.ok += 1,
                ProbeStatus::Timeout => counts.timeout += 1,
                ProbeStatus::Fail => counts.fail += 1,
                ProbeStatus::Untested => counts.untested += 1,
            }
        }
        counts
    }
}

/// 把一条地址的多次采样聚合成一个结论。
///
/// - 任意一次成功 → 成功，取成功样本里的最小值；
/// - 没有任何成功且全部超时 → 超时；
/// - 其余（含连接失败、失败与超时混合）→ 失败。
pub fn aggregate(samples: &[ProbeOutcome]) -> ProbeOutcome {
    if samples.is_empty() {
        return ProbeOutcome::untested();
    }
    if let Some(min) = samples
        .iter()
        .filter(|sample| sample.status == ProbeStatus::Ok)
        .filter_map(|sample| sample.latency_ms)
        .min()
    {
        return ProbeOutcome::ok(min);
    }
    if samples
        .iter()
        .all(|sample| sample.status == ProbeStatus::Timeout)
    {
        return ProbeOutcome::timeout(error_summary(samples, "三次采样均超时"));
    }
    ProbeOutcome::fail(error_summary(samples, "连接未建立"))
}

/// 把各次采样的错误去重后拼成一句摘要。
fn error_summary(samples: &[ProbeOutcome], fallback: &str) -> String {
    let mut messages: Vec<String> = Vec::new();
    for sample in samples {
        if let Some(error) = &sample.error {
            let error = error.trim();
            if !error.is_empty() && !messages.iter().any(|known| known == error) {
                messages.push(error.to_string());
            }
        }
    }
    if messages.is_empty() {
        fallback.to_string()
    } else {
        messages.join("；")
    }
}

/// 跑一轮批量测试。停止后已开始的地址保留结论，未开始的记为未测。
///
/// 每完成一条就推一次 [`BatchEvent::Row`]，再推一次计数进度；
/// 停止时没轮到的地址不推逐行事件，由界面按整轮结果补齐。
pub fn run_batch(
    prober: &dyn BatchProber,
    items: &[BatchItem],
    options: &ProbeOptions,
    batch: BatchOptions,
    cancel: &AtomicBool,
    on_event: &(dyn Fn(BatchEvent) + Sync),
) -> Result<BatchReport, String> {
    let total = items.len();
    prober.prepare(items, batch.concurrency)?;

    let next = AtomicUsize::new(0);
    let completed = AtomicUsize::new(0);
    let results: Vec<Mutex<Option<BatchRow>>> =
        (0..total).map(|_| Mutex::new(None)).collect();

    let workers = batch.concurrency.min(total);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                // 每个任务开始前检查停止；已经开始的任务会跑完并保留结论。
                if cancel.load(Ordering::SeqCst) {
                    break;
                }
                let index = next.fetch_add(1, Ordering::SeqCst);
                if index >= total {
                    break;
                }

                let item = &items[index];
                let mut samples = Vec::with_capacity(batch.samples);
                for _ in 0..batch.samples {
                    samples.push(prober.sample(&item.endpoint, options));
                }
                let outcome = aggregate(&samples);
                if let Ok(mut slot) = results[index].lock() {
                    *slot = Some(BatchRow {
                        endpoint: item.endpoint.clone(),
                        alias: item.alias.clone(),
                        samples,
                        outcome: outcome.clone(),
                    });
                }

                on_event(BatchEvent::Row(crate::probe::RowEvent::latency(
                    &item.endpoint,
                    &outcome,
                )));
                let done = completed.fetch_add(1, Ordering::SeqCst) + 1;
                on_event(BatchEvent::Progress(BatchProgress {
                    completed: done,
                    total,
                }));
            });
        }
    });

    prober.cleanup();

    let rows = items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            results[index]
                .lock()
                .ok()
                .and_then(|slot| slot.clone())
                .unwrap_or_else(|| BatchRow {
                    endpoint: item.endpoint.clone(),
                    alias: item.alias.clone(),
                    samples: Vec::new(),
                    outcome: ProbeOutcome::untested(),
                })
        })
        .collect();

    Ok(BatchReport {
        rows,
        total,
        completed: completed.load(Ordering::SeqCst),
        stopped: cancel.load(Ordering::SeqCst),
    })
}

/// 把整轮结果写进 `test_run` 与 `test_result`。
pub fn persist_batch(
    store: &Store,
    run_id: &str,
    started_at: &str,
    finished_at: &str,
    params: &serde_json::Value,
    report: &BatchReport,
) -> Result<(), String> {
    let counts = report.counts();
    store
        .insert_test_run(&crate::store::TestRunRecord {
            id: run_id,
            started_at,
            finished_at,
            params_json: &params.to_string(),
            total: report.total as u32,
            ok_count: counts.ok as u32,
            timeout_count: counts.timeout as u32,
            fail_count: counts.fail as u32,
        })
        .map_err(|e| e.to_string())?;

    for row in &report.rows {
        store
            .insert_test_result_with_samples(
                run_id,
                &row.endpoint,
                &row.alias,
                &row.outcome,
                &row.samples_json(),
            )
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::*;
    use crate::probe::{Attempt, Endpoint};

    fn item(host: &str, port: u16) -> BatchItem {
        BatchItem::new(Endpoint::new(host, port), format!("{host}:{port} 别名"))
    }

    fn ok(latency: u64) -> ProbeOutcome {
        ProbeOutcome::ok(latency)
    }

    #[test]
    fn 三次采样全成功取最小值() {
        let outcome = aggregate(&[ok(120), ok(80), ok(150)]);
        assert_eq!(outcome.status, ProbeStatus::Ok);
        assert_eq!(outcome.latency_ms, Some(80));
    }

    #[test]
    fn 部分采样超时不影响结论() {
        let outcome = aggregate(&[
            ProbeOutcome::timeout("第 1 次超时"),
            ok(90),
            ProbeOutcome::timeout("第 3 次超时"),
        ]);
        assert_eq!(outcome.status, ProbeStatus::Ok);
        assert_eq!(outcome.latency_ms, Some(90));
    }

    #[test]
    fn 三次全超时记超时() {
        let outcome = aggregate(&[
            ProbeOutcome::timeout("第 1 次超时"),
            ProbeOutcome::timeout("第 2 次超时"),
            ProbeOutcome::timeout("第 3 次超时"),
        ]);
        assert_eq!(outcome.status, ProbeStatus::Timeout);
        assert_eq!(outcome.latency_ms, None);
    }

    #[test]
    fn 连接失败记失败且超时与失败混合也算失败() {
        let all_failed = aggregate(&[
            ProbeOutcome::fail("连接被拒绝"),
            ProbeOutcome::fail("连接被拒绝"),
            ProbeOutcome::fail("连接被拒绝"),
        ]);
        assert_eq!(all_failed.status, ProbeStatus::Fail);
        // 只要没有成功且不是全超时，就不该伪装成超时。
        let mixed = aggregate(&[
            ProbeOutcome::timeout("超时"),
            ProbeOutcome::fail("连接被拒绝"),
            ProbeOutcome::timeout("超时"),
        ]);
        assert_eq!(mixed.status, ProbeStatus::Fail);
    }

    #[test]
    fn 没有采样时是未测() {
        assert_eq!(aggregate(&[]).status, ProbeStatus::Untested);
    }

    /// 记录同时在跑的采样数，用来验证并发上限。
    struct CountingProber {
        current: AtomicUsize,
        peak: AtomicUsize,
        calls: AtomicUsize,
        delay: Duration,
    }

    impl CountingProber {
        fn new(delay_ms: u64) -> Self {
            Self {
                current: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
                calls: AtomicUsize::new(0),
                delay: Duration::from_millis(delay_ms),
            }
        }
    }

    impl BatchProber for CountingProber {
        fn sample(
            &self,
            _endpoint: &Endpoint,
            _options: &ProbeOptions,
        ) -> ProbeOutcome {
            let now = self.current.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(self.delay);
            self.current.fetch_sub(1, Ordering::SeqCst);
            ProbeOutcome::ok(50)
        }
    }

    #[test]
    fn 地址间并发不超过上限() {
        let prober = CountingProber::new(20);
        let items: Vec<BatchItem> = (0..12)
            .map(|i| item(&format!("104.16.0.{i}"), 8443))
            .collect();
        let cancel = AtomicBool::new(false);
        let report = run_batch(
            &prober,
            &items,
            &ProbeOptions::default(),
            BatchOptions::new(1, 3),
            &cancel,
            &|_| {},
        )
        .unwrap();

        assert_eq!(report.rows.len(), 12);
        assert_eq!(report.counts().ok, 12);
        let peak = prober.peak.load(Ordering::SeqCst);
        assert!(peak <= 3, "并发峰值 {peak} 超过上限 3");
        assert!(peak >= 2, "并发没有真正展开，峰值 {peak}");
        assert_eq!(prober.calls.load(Ordering::SeqCst), 12);
    }

    #[test]
    fn 并发参数被夹在_1_到_32() {
        assert_eq!(BatchOptions::new(3, 0).concurrency, 1);
        assert_eq!(BatchOptions::new(3, 99).concurrency, 32);
        assert_eq!(BatchOptions::new(0, 10).samples, 1);
        assert_eq!(BatchOptions::new(99, 10).samples, 10);
    }

    #[test]
    fn 默认并发按核数放大且夹在_8_到_32() {
        let default = default_concurrency();
        assert!((8..=32).contains(&default), "默认并发 {default} 超出 8–32");
        assert_eq!(BatchOptions::default().concurrency, default);
        // 显式传入的值仍然优先，不被默认值改写。
        assert_eq!(BatchOptions::new(3, 5).concurrency, 5);
    }

    /// 用假探测器跑满并发槽位：既要真正并行（峰值 > 1），也不能越过上限。
    #[test]
    fn 假探测器下并发真正展开且不超上限() {
        let prober = crate::probe::fake::FakeProber::new(vec![Attempt::responded(80)])
            .with_delay(Duration::from_millis(25));
        let items: Vec<BatchItem> = (0..12)
            .map(|i| item(&format!("104.16.9.{i}"), 8443))
            .collect();

        let report = run_batch(
            &prober,
            &items,
            &ProbeOptions::default(),
            BatchOptions::new(1, 4),
            &AtomicBool::new(false),
            &|_| {},
        )
        .unwrap();

        assert_eq!(report.counts().ok, 12);
        let peak = prober.peak_concurrency();
        assert!(peak <= 4, "并发峰值 {peak} 超过上限 4");
        assert!(peak >= 2, "并发没有真正展开，峰值 {peak}");
        assert_eq!(prober.calls().len(), 12);
    }

    /// 第一次采样后请求停止，其余地址应保持未测。
    struct StopAfterFirst {
        cancel: Arc<AtomicBool>,
        calls: AtomicUsize,
    }

    impl BatchProber for StopAfterFirst {
        fn sample(
            &self,
            _endpoint: &Endpoint,
            _options: &ProbeOptions,
        ) -> ProbeOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.cancel.store(true, Ordering::SeqCst);
            ProbeOutcome::ok(66)
        }
    }

    #[test]
    fn 停止后已完成保留未开始标记未测() {
        let cancel = Arc::new(AtomicBool::new(false));
        let prober = StopAfterFirst {
            cancel: cancel.clone(),
            calls: AtomicUsize::new(0),
        };
        let items: Vec<BatchItem> = (0..5)
            .map(|i| item(&format!("104.16.1.{i}"), 2053))
            .collect();

        let report = run_batch(
            &prober,
            &items,
            &ProbeOptions::default(),
            BatchOptions::new(3, 1),
            &cancel,
            &|_| {},
        )
        .unwrap();

        assert!(report.stopped);
        assert_eq!(report.rows.len(), 5);
        assert_eq!(report.rows[0].outcome.status, ProbeStatus::Ok);
        assert_eq!(report.rows[0].samples.len(), 3);
        for row in &report.rows[1..] {
            assert_eq!(row.outcome.status, ProbeStatus::Untested);
            assert!(row.samples.is_empty());
        }
        let counts = report.counts();
        assert_eq!(counts.ok, 1);
        assert_eq!(counts.untested, 4);
    }

    #[test]
    fn 统计计数与进度回调正确() {
        let prober = crate::probe::fake::FakeProber::scripted(vec![
            (
                ("104.16.2.1", 8443),
                vec![Attempt::responded(90); 3],
            ),
            (
                ("104.16.2.2", 8443),
                vec![Attempt::no_response("超时"); 3],
            ),
            (
                ("104.16.2.3", 8443),
                vec![Attempt::connect_failed("拒绝"); 3],
            ),
        ]);
        let items = vec![
            item("104.16.2.1", 8443),
            item("104.16.2.2", 8443),
            item("104.16.2.3", 8443),
        ];
        let events = Mutex::new(Vec::new());
        let report = run_batch(
            &prober,
            &items,
            &ProbeOptions::default(),
            BatchOptions::new(3, 2),
            &AtomicBool::new(false),
            &|event| events.lock().unwrap().push(event),
        )
        .unwrap();

        let counts = report.counts();
        assert_eq!((counts.ok, counts.timeout, counts.fail), (1, 1, 1));
        assert_eq!(report.completed, 3);

        let events = events.lock().unwrap();
        let progress: Vec<BatchProgress> = events
            .iter()
            .filter_map(|event| match event {
                BatchEvent::Progress(progress) => Some(*progress),
                BatchEvent::Row(_) => None,
            })
            .collect();
        assert_eq!(progress.len(), 3);
        assert_eq!(progress.last().unwrap().completed, 3);
        assert_eq!(progress.last().unwrap().total, 3);

        // 每完成一条推一次逐行事件，状态与聚合结论一致。
        let rows: Vec<crate::probe::RowEvent> = events
            .iter()
            .filter_map(|event| match event {
                BatchEvent::Row(row) => Some(row.clone()),
                BatchEvent::Progress(_) => None,
            })
            .collect();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().any(|row| row.host == "104.16.2.1"
            && row.status == ProbeStatus::Ok
            && row.latency_ms == Some(90)));
        assert!(rows.iter().any(|row| row.host == "104.16.2.2"
            && row.status == ProbeStatus::Timeout));
        assert!(rows.iter().any(|row| row.host == "104.16.2.3"
            && row.status == ProbeStatus::Fail
            && row.error.as_deref() == Some("拒绝")));
    }

    /// 按地址预设每次采样的原始观测，采样用尽后重复最后一次。
    #[test]
    fn 整轮落库一条轮次且每条地址一行() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data.db")).unwrap();

        let prober = crate::probe::fake::FakeProber::scripted(vec![
            // 三次成功，取最小值 80。
            (
                ("104.16.3.1", 8443),
                vec![
                    Attempt::responded(120),
                    Attempt::responded(80),
                    Attempt::responded(150),
                ],
            ),
            // 一次超时两次成功，仍算成功，取最小值 70。
            (
                ("104.16.3.2", 8443),
                vec![
                    Attempt::no_response("超时"),
                    Attempt::responded(70),
                    Attempt::responded(110),
                ],
            ),
            // 三次全超时。
            (
                ("104.16.3.3", 8443),
                vec![Attempt::no_response("10s 内未收到响应"); 3],
            ),
            // 三次连接失败。
            (
                ("104.16.3.4", 8443),
                vec![Attempt::connect_failed("连接被拒绝"); 3],
            ),
        ]);
        let items = vec![
            item("104.16.3.1", 8443),
            item("104.16.3.2", 8443),
            item("104.16.3.3", 8443),
            item("104.16.3.4", 8443),
        ];
        let report = run_batch(
            &prober,
            &items,
            &ProbeOptions::default(),
            BatchOptions::new(3, 2),
            &AtomicBool::new(false),
            &|_| {},
        )
        .unwrap();

        let params = serde_json::json!({ "samples": 3, "concurrency": 2 });
        persist_batch(
            &store,
            "run-batch-1",
            "2026-11-13T00:00:00.000Z",
            "2026-11-13T00:00:05.000Z",
            &params,
            &report,
        )
        .unwrap();

        assert_eq!(store.count_test_runs().unwrap(), 1);
        assert_eq!(store.count_test_results().unwrap(), 4);

        let run = store.get_test_run("run-batch-1").unwrap().unwrap();
        assert_eq!(run.total, 4);
        assert_eq!((run.ok_count, run.timeout_count, run.fail_count), (2, 1, 1));

        let rows = store.list_test_results("run-batch-1").unwrap();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].status, "ok");
        assert_eq!(rows[0].latency_ms, Some(80));
        assert_eq!(rows[0].samples.len(), 3);
        assert_eq!(rows[1].status, "ok");
        assert_eq!(rows[1].latency_ms, Some(70));
        assert_eq!(rows[2].status, "timeout");
        assert_eq!(rows[3].status, "fail");
        assert_eq!(rows[3].error.as_deref(), Some("连接被拒绝"));
        // 同一轮的所有结果共享 runId。
        assert!(rows.iter().all(|row| row.run_id == "run-batch-1"));
    }
}
