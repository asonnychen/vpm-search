//! 主进程侧的探测监督器：拉起子进程、逐行消费、按 30 秒增量保存。
//!
//! 这里不发起任何网络请求。停止 = 直接 `Child::kill`，随后把还没回报的目标
//! 标记为 `untested` 交给命令层落库；子进程崩溃、超时同理。
//!
//! [`WorkerHandle`] 把「真子进程」与「假子进程」抽象成同一个接口，
//! 单测可以用 [`FakeWorker`] 直接喂 JSON 行，不需要真的启动进程。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::logs::LogLevel;

use super::worker::{WorkerLine, WorkerTask, WORKER_ARG};
use super::{ProbeStatus, RowEvent};

/// 增量保存间隔。
pub const FLUSH_INTERVAL: Duration = Duration::from_secs(30);

/// 轮询子进程与停止信号的间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// 子进程接口：真进程与测试假进程都实现它。
pub trait WorkerHandle: Send + Sync {
    fn pid(&self) -> u32;
    /// 读一行 stdout；超时返回 `Timeout`，通道关闭返回 `Disconnected`。
    fn recv_timeout(&self, timeout: Duration) -> Result<String, RecvTimeoutError>;
    /// 非阻塞取一行；没有更多行时返回 `None`。
    fn try_recv(&self) -> Option<String>;
    fn kill(&self) -> Result<(), String>;
    /// 子进程退出码；还没退出时返回 `None`。
    fn wait(&self) -> Option<i32>;
}

/// 真子进程：`current_exe --probe-worker`。
pub struct ProcessWorker {
    child: Mutex<Child>,
    lines: Mutex<Receiver<String>>,
    pid: u32,
}

impl ProcessWorker {
    /// 拉起子进程并把任务写进 stdin（随后关闭 stdin）。
    ///
    /// `on_stderr` 在独立线程里逐行收到子进程的 stderr，由命令层写进应用日志。
    pub fn spawn(
        task: &WorkerTask,
        on_stderr: std::sync::Arc<dyn Fn(String) + Send + Sync>,
    ) -> Result<Self, String> {
        let exe = std::env::current_exe().map_err(|e| format!("取当前程序路径失败：{e}"))?;
        let mut child = Command::new(exe)
            .arg(WORKER_ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("启动探测子进程失败：{e}"))?;
        let pid = child.id();

        {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| "探测子进程没有 stdin".to_string())?;
            let json = serde_json::to_string(task)
                .map_err(|e| format!("序列化探测任务失败：{e}"))?;
            stdin
                .write_all(json.as_bytes())
                .map_err(|e| format!("写入探测任务失败：{e}"))?;
            // 关闭 stdin：worker 读完任务即开始，读到 EOF 视为结束。
        }

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "探测子进程没有 stdout".to_string())?;
        let (tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                    // 非 UTF-8 行直接跳过，主进程只消费合法 JSON 行。
                    Err(_) => continue,
                }
            }
        });

        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    on_stderr(line);
                }
            });
        }

        Ok(Self {
            child: Mutex::new(child),
            lines: Mutex::new(lines),
            pid,
        })
    }
}

impl WorkerHandle for ProcessWorker {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn recv_timeout(&self, timeout: Duration) -> Result<String, RecvTimeoutError> {
        match self.lines.lock() {
            Ok(receiver) => receiver.recv_timeout(timeout),
            Err(_) => Err(RecvTimeoutError::Disconnected),
        }
    }

    fn try_recv(&self) -> Option<String> {
        self.lines.lock().ok().and_then(|receiver| receiver.try_recv().ok())
    }

    fn kill(&self) -> Result<(), String> {
        let mut child = self
            .child
            .lock()
            .map_err(|_| "子进程锁已损坏".to_string())?;
        // 已经退出的进程直接视作停止成功。
        if matches!(child.try_wait(), Ok(Some(_))) {
            return Ok(());
        }
        match child.kill() {
            Ok(()) => Ok(()),
            // 竞态下进程刚好退出，也视作已完成。
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => Ok(()),
            Err(error) => Err(format!("终止探测子进程失败：{error}")),
        }
    }

    fn wait(&self) -> Option<i32> {
        // 不用阻塞 wait：停止线程可能同时持有锁，轮询更安全。
        for _ in 0..200 {
            let status = {
                let Ok(mut child) = self.child.lock() else {
                    return None;
                };
                child.try_wait()
            };
            match status {
                Ok(Some(status)) => return status.code(),
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(_) => return None,
            }
        }
        None
    }
}

/// 测试用假子进程：行由测试通过 [`FakeWorker::new`] 直接喂进来。
#[cfg_attr(not(test), allow(dead_code))]
pub struct FakeWorker {
    pid: u32,
    lines: Mutex<Receiver<String>>,
    killed: AtomicBool,
    exit_code: Option<i32>,
}

impl FakeWorker {
    pub fn new(lines: Receiver<String>, exit_code: Option<i32>) -> Self {
        Self {
            pid: 4242,
            lines: Mutex::new(lines),
            killed: AtomicBool::new(false),
            exit_code,
        }
    }

    pub fn killed(&self) -> bool {
        self.killed.load(Ordering::SeqCst)
    }
}

impl WorkerHandle for FakeWorker {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn recv_timeout(&self, timeout: Duration) -> Result<String, RecvTimeoutError> {
        match self.lines.lock() {
            Ok(receiver) => receiver.recv_timeout(timeout),
            Err(_) => Err(RecvTimeoutError::Disconnected),
        }
    }

    fn try_recv(&self) -> Option<String> {
        self.lines.lock().ok().and_then(|receiver| receiver.try_recv().ok())
    }

    fn kill(&self) -> Result<(), String> {
        self.killed.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn wait(&self) -> Option<i32> {
        self.exit_code
    }
}

/// 可注入的时钟：测试里用可控时钟验证 30 秒增量保存。
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

/// 生产用真实时钟。
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// 监督器把结果交给命令层的回调。
pub trait SuperviseSink: Send + Sync {
    /// 收到一条有效结果（已解析）。
    fn row(&self, row: &RowEvent);
    /// 增量保存：把当前已完成的行 upsert 进库。
    fn flush(&self, rows: &[RowEvent]);
    /// 非 JSON 行；调用方记日志后跳过。
    fn bad_line(&self, line: &str);
    /// 操作日志。
    fn log(&self, level: LogLevel, text: &str);
}

/// 一轮监督的参数。
pub struct SuperviseOptions<'a> {
    /// 全部目标，顺序与最终报告一致。
    pub targets: &'a [(String, u16)],
    pub cancel: &'a AtomicBool,
    pub clock: &'a dyn Clock,
    pub flush_interval: Duration,
}

/// 一轮监督的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct SupervisedRun {
    /// 全部目标一行一条，未回报的为 `untested`。
    pub rows: Vec<RowEvent>,
    pub completed: usize,
    pub stopped: bool,
    pub crashed: bool,
    pub exit_code: Option<i32>,
    /// 定时增量保存触发次数。
    pub flush_count: usize,
    pub bad_lines: usize,
}

/// 消费子进程输出直到结束、停止或崩溃。
pub fn supervise(
    handle: &dyn WorkerHandle,
    options: &SuperviseOptions<'_>,
    sink: &dyn SuperviseSink,
) -> SupervisedRun {
    let mut reported: Vec<RowEvent> = Vec::new();
    let mut slots: HashMap<(String, u16), usize> = HashMap::new();
    let mut last_flush = options.clock.now();
    let mut flush_count = 0usize;
    let mut bad_lines = 0usize;
    let mut stopped = false;
    let poll = POLL_INTERVAL.min(options.flush_interval);

    let mut accept = |line: &str,
                      reported: &mut Vec<RowEvent>,
                      slots: &mut HashMap<(String, u16), usize>| {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }
        match WorkerLine::parse(trimmed) {
            Ok(parsed) => {
                let row = parsed.to_row();
                let key = (row.host.clone(), row.port);
                match slots.get(&key) {
                    Some(&index) => reported[index] = row.clone(),
                    None => {
                        slots.insert(key, reported.len());
                        reported.push(row.clone());
                    }
                }
                sink.row(&row);
            }
            Err(error) => {
                bad_lines += 1;
                sink.bad_line(trimmed);
                sink.log(LogLevel::Warn, &format!("跳过无法解析的探测行：{error}"));
            }
        }
    };

    loop {
        if options.cancel.load(Ordering::SeqCst) {
            stopped = true;
            break;
        }
        if options.clock.now().duration_since(last_flush) >= options.flush_interval {
            sink.flush(&reported);
            flush_count += 1;
            last_flush = options.clock.now();
        }

        match handle.recv_timeout(poll) {
            Ok(line) => {
                accept(&line, &mut reported, &mut slots);
                if options.clock.now().duration_since(last_flush) >= options.flush_interval {
                    sink.flush(&reported);
                    flush_count += 1;
                    last_flush = options.clock.now();
                }
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                // 子进程被杀时 stdout 会先关闭：此时仍以取消标志为准。
                if options.cancel.load(Ordering::SeqCst) {
                    stopped = true;
                }
                break;
            }
        }
    }

    if stopped {
        if let Err(error) = handle.kill() {
            sink.log(LogLevel::Warn, &error);
        }
        sink.log(
            LogLevel::Info,
            &format!("已停止，终止探测进程 pid={}", handle.pid()),
        );
        // 停止前已经写进管道的行仍然有效，先收干净再收尾。
        while let Some(line) = handle.try_recv() {
            accept(&line, &mut reported, &mut slots);
        }
    }

    let exit_code = if stopped { None } else { handle.wait() };
    let crashed = !stopped && exit_code != Some(0);
    let stopped = stopped || options.cancel.load(Ordering::SeqCst);
    if crashed {
        sink.log(
            LogLevel::Error,
            &format!(
                "探测子进程异常退出：pid={} 退出码={}",
                handle.pid(),
                exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "未知".to_string())
            ),
        );
    }

    // 停止或崩溃时先把已完成的行保存一次，再交给命令层收尾。
    if (stopped || crashed) && !reported.is_empty() {
        sink.flush(&reported);
    }

    let rows: Vec<RowEvent> = options
        .targets
        .iter()
        .map(|(host, port)| match slots.get(&(host.clone(), *port)) {
            Some(&index) => reported[index].clone(),
            None => RowEvent::new(
                &super::Endpoint::new(host.clone(), *port),
                ProbeStatus::Untested,
            ),
        })
        .collect();

    SupervisedRun {
        rows,
        completed: reported.len(),
        stopped,
        crashed,
        exit_code,
        flush_count,
        bad_lines,
    }
}


#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::probe::{Endpoint, ProbeOutcome};

    /// 可控时钟：手动推进毫秒数。
    struct ManualClock {
        base: Instant,
        offset_ms: AtomicU64,
    }

    impl ManualClock {
        fn new() -> Self {
            Self {
                base: Instant::now(),
                offset_ms: AtomicU64::new(0),
            }
        }

        fn advance(&self, duration: Duration) {
            self.offset_ms
                .fetch_add(duration.as_millis() as u64, Ordering::SeqCst);
        }
    }

    impl Clock for ManualClock {
        fn now(&self) -> Instant {
            self.base + Duration::from_millis(self.offset_ms.load(Ordering::SeqCst))
        }
    }

    /// 记录回调的 sink。
    #[derive(Default)]
    struct RecordingSink {
        rows: Mutex<Vec<RowEvent>>,
        flushes: Mutex<Vec<usize>>,
        bad: Mutex<Vec<String>>,
        logs: Mutex<Vec<(LogLevel, String)>>,
        on_row: Mutex<Option<Arc<dyn Fn(&RowEvent) + Send + Sync>>>,
    }

    impl SuperviseSink for RecordingSink {
        fn row(&self, row: &RowEvent) {
            self.rows.lock().unwrap().push(row.clone());
            if let Some(hook) = self.on_row.lock().unwrap().as_ref() {
                hook(row);
            }
        }

        fn flush(&self, rows: &[RowEvent]) {
            self.flushes.lock().unwrap().push(rows.len());
        }

        fn bad_line(&self, line: &str) {
            self.bad.lock().unwrap().push(line.to_string());
        }

        fn log(&self, level: LogLevel, text: &str) {
            self.logs.lock().unwrap().push((level, text.to_string()));
        }
    }

    fn line(host: &str, port: u16, latency: u64) -> String {
        serde_json::to_string(&WorkerLine::from_row(
            "delay",
            &RowEvent::latency(&Endpoint::new(host, port), &ProbeOutcome::ok(latency)),
        ))
        .unwrap()
    }

    fn targets(list: &[(&str, u16)]) -> Vec<(String, u16)> {
        list.iter()
            .map(|(host, port)| (host.to_string(), *port))
            .collect()
    }

    fn run(
        handle: &dyn WorkerHandle,
        targets: &[(String, u16)],
        clock: &dyn Clock,
        cancel: &AtomicBool,
        sink: &dyn SuperviseSink,
    ) -> SupervisedRun {
        supervise(
            handle,
            &SuperviseOptions {
                targets,
                cancel,
                clock,
                flush_interval: Duration::from_secs(30),
            },
            sink,
        )
    }

    #[test]
    fn 正常结束逐行收齐且不崩溃() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(line("104.16.1.1", 8443, 88)).unwrap();
        tx.send(line("104.16.1.2", 8443, 120)).unwrap();
        drop(tx);
        let handle = FakeWorker::new(rx, Some(0));
        let clock = ManualClock::new();
        let sink = RecordingSink::default();
        let result = run(
            &handle,
            &targets(&[("104.16.1.1", 8443), ("104.16.1.2", 8443)]),
            &clock,
            &AtomicBool::new(false),
            &sink,
        );

        assert_eq!(result.completed, 2);
        assert!(!result.stopped && !result.crashed);
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.rows[0].latency_ms, Some(88));
        assert_eq!(sink.rows.lock().unwrap().len(), 2);
        // 未到 30 秒不触发增量保存。
        assert!(sink.flushes.lock().unwrap().is_empty());
    }

    #[test]
    fn 中途停止会杀死子进程并把未完成标记未测() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(line("104.16.1.1", 8443, 70)).unwrap();
        let handle = FakeWorker::new(rx, Some(0));
        let clock = ManualClock::new();
        let sink = RecordingSink::default();
        let cancel = AtomicBool::new(false);

        // 收到第一条就置位取消标志，下一次循环必然走 kill 分支。
        struct HookSink<'a>(&'a RecordingSink, &'a AtomicBool);
        impl SuperviseSink for HookSink<'_> {
            fn row(&self, row: &RowEvent) {
                self.0.row(row);
                self.1.store(true, Ordering::SeqCst);
            }
            fn flush(&self, rows: &[RowEvent]) {
                self.0.flush(rows)
            }
            fn bad_line(&self, line: &str) {
                self.0.bad_line(line)
            }
            fn log(&self, level: LogLevel, text: &str) {
                self.0.log(level, text)
            }
        }

        let result = run(
            &handle,
            &targets(&[("104.16.1.1", 8443), ("104.16.1.2", 8443)]),
            &clock,
            &cancel,
            &HookSink(&sink, &cancel),
        );

        assert!(result.stopped);
        assert!(handle.killed());
        assert_eq!(result.completed, 1);
        assert_eq!(result.rows[0].status, ProbeStatus::Ok);
        assert_eq!(result.rows[1].status, ProbeStatus::Untested);
        let logs = sink.logs.lock().unwrap();
        assert!(logs
            .iter()
            .any(|(_, text)| text.contains("已停止") && text.contains("pid=4242")));
    }

    #[test]
    fn 坏行记日志并跳过() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send("这不是 JSON".to_string()).unwrap();
        tx.send(line("104.16.1.1", 8443, 66)).unwrap();
        drop(tx);
        let handle = FakeWorker::new(rx, Some(0));
        let clock = ManualClock::new();
        let sink = RecordingSink::default();
        let result = run(
            &handle,
            &targets(&[("104.16.1.1", 8443)]),
            &clock,
            &AtomicBool::new(false),
            &sink,
        );

        assert_eq!(result.bad_lines, 1);
        assert_eq!(result.completed, 1);
        assert_eq!(sink.bad.lock().unwrap().len(), 1);
        assert!(sink
            .logs
            .lock()
            .unwrap()
            .iter()
            .any(|(level, _)| *level == LogLevel::Warn));
    }

    #[test]
    fn 崩溃退出码非零会标记未完成并记错误日志() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(line("104.16.1.1", 8443, 60)).unwrap();
        drop(tx);
        let handle = FakeWorker::new(rx, Some(3));
        let clock = ManualClock::new();
        let sink = RecordingSink::default();
        let result = run(
            &handle,
            &targets(&[("104.16.1.1", 8443), ("104.16.1.2", 8443)]),
            &clock,
            &AtomicBool::new(false),
            &sink,
        );

        assert!(result.crashed);
        assert_eq!(result.exit_code, Some(3));
        assert_eq!(result.rows[1].status, ProbeStatus::Untested);
        assert!(sink
            .logs
            .lock()
            .unwrap()
            .iter()
            .any(|(level, text)| *level == LogLevel::Error && text.contains("退出码=3")));
    }

    #[test]
    fn 每三十秒触发一次增量保存且结束后不再触发() {
        let (tx, rx) = std::sync::mpsc::channel();
        let clock = Arc::new(ManualClock::new());
        let sink = Arc::new(RecordingSink::default());
        // 每收到一行就推进 30 秒，下一行之后必然触发一次 flush。
        let clock_hook = clock.clone();
        *sink.on_row.lock().unwrap() = Some(Arc::new(move |_| {
            clock_hook.advance(Duration::from_secs(30))
        }));

        let handle = FakeWorker::new(rx, Some(0));
        for (index, host) in ["104.16.1.1", "104.16.1.2", "104.16.1.3"].iter().enumerate() {
            tx.send(line(host, 8443, 50 + index as u64)).unwrap();
        }
        drop(tx);

        let result = run(
            &handle,
            &targets(&[
                ("104.16.1.1", 8443),
                ("104.16.1.2", 8443),
                ("104.16.1.3", 8443),
            ]),
            clock.as_ref(),
            &AtomicBool::new(false),
            sink.as_ref(),
        );

        assert_eq!(result.completed, 3);
        assert_eq!(result.flush_count, 3);
        assert_eq!(*sink.flushes.lock().unwrap(), vec![1, 2, 3]);
        // 结束后时钟再推进也不会再触发。
        clock.advance(Duration::from_secs(90));
        assert_eq!(sink.flushes.lock().unwrap().len(), 3);
    }

    #[test]
    fn 重复增量保存不产生重复行() {
        use crate::store::{Store, TestRunRecord};

        struct StoreSink {
            store: Mutex<Store>,
            run_id: String,
            flushes: Mutex<usize>,
        }

        impl SuperviseSink for StoreSink {
            fn row(&self, _row: &RowEvent) {}
            fn flush(&self, rows: &[RowEvent]) {
                let store = self.store.lock().unwrap();
                for row in rows {
                    let endpoint = Endpoint::new(row.host.clone(), row.port);
                    let outcome = ProbeOutcome {
                        status: row.status,
                        latency_ms: row.latency_ms,
                        error: row.error.clone(),
                    };
                    store
                        .upsert_test_result_latency(&self.run_id, &endpoint, "别名", &outcome, "[]")
                        .unwrap();
                }
                *self.flushes.lock().unwrap() += 1;
            }
            fn bad_line(&self, _line: &str) {}
            fn log(&self, _level: LogLevel, _text: &str) {}
        }

        let store = Store::open_in_memory().unwrap();
        store
            .upsert_test_run(&TestRunRecord {
                id: "run-flush",
                started_at: "2026-11-13T00:00:00.000Z",
                finished_at: "2026-11-13T00:00:00.000Z",
                params_json: "{}",
                total: 2,
                ok_count: 0,
                timeout_count: 0,
                fail_count: 0,
            })
            .unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        let clock = Arc::new(ManualClock::new());
        let sink = Arc::new(StoreSink {
            store: Mutex::new(store),
            run_id: "run-flush".to_string(),
            flushes: Mutex::new(0),
        });
        // 每收到一行推进 30 秒，用包装 sink 在行回调里推进可控时钟。
        struct AdvancingSink {
            inner: Arc<StoreSink>,
            clock: Arc<ManualClock>,
        }
        impl SuperviseSink for AdvancingSink {
            fn row(&self, row: &RowEvent) {
                self.inner.row(row);
                self.clock.advance(Duration::from_secs(30));
            }
            fn flush(&self, rows: &[RowEvent]) {
                self.inner.flush(rows)
            }
            fn bad_line(&self, line: &str) {
                self.inner.bad_line(line)
            }
            fn log(&self, level: LogLevel, text: &str) {
                self.inner.log(level, text)
            }
        }
        let advancer = AdvancingSink {
            inner: sink.clone(),
            clock: clock.clone(),
        };

        let handle = FakeWorker::new(rx, Some(0));
        tx.send(line("104.16.1.1", 8443, 88)).unwrap();
        tx.send(line("104.16.1.2", 8443, 99)).unwrap();
        drop(tx);

        let result = run(
            &handle,
            &targets(&[("104.16.1.1", 8443), ("104.16.1.2", 8443)]),
            clock.as_ref(),
            &AtomicBool::new(false),
            &advancer,
        );

        assert_eq!(result.flush_count, 2);
        assert_eq!(*sink.flushes.lock().unwrap(), 2);
        // 两次 flush 覆盖同一批地址，库里仍是两行。
        assert_eq!(sink.store.lock().unwrap().count_test_results().unwrap(), 2);
    }
}
