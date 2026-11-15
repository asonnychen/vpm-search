mod export;
mod geo;
mod logs;
mod probe;
mod settings;
mod sources;
mod store;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use serde::Serialize;
use settings::Settings;
use sources::fetch::{self, CATALOG_URL};
use sources::parse::{parse_catalog, source_label, CatalogSource};
use store::{PoolEntryRow, PoolSourceRow, Store, TestRunSummaryRow};
use tauri::{Emitter, Manager};
use tauri_plugin_dialog::DialogExt;

use logs::{AppLogRecord, LogEntry, LogKind, LogLevel, LogWriter, LogsSnapshot};
use probe::Prober;

/// 启动时与界面初次加载读回的日志行数上限。
const LOG_READ_LIMIT: usize = 500;

/// 应用级共享状态：数据库连接、三类日志写入器与归属地查询器。
pub struct AppState {
    /// 打开失败时为 `None`，此时命令返回错误但界面照常可用。
    store: Mutex<Option<Store>>,
    logs: Arc<LogWriter>,
    /// 归属地查询器：离线库优先、在线兜底、按 IP 缓存。
    geo: Arc<geo::GeoResolver>,
    /// 正在跑的一轮批量测试；停止按钮通过它发信号。
    batch: Mutex<Option<Arc<BatchControl>>>,
}

/// 一轮批量的停止信号。
struct BatchControl {
    cancel: AtomicBool,
}

impl AppState {
    /// 当前设置里的三类日志保留期；读不到时用默认 30 天。
    fn retention(&self) -> settings::LogRetention {
        let Ok(guard) = self.store.lock() else {
            return settings::LogRetention::default();
        };
        match guard.as_ref() {
            Some(store) => store
                .load_settings()
                .map(|settings| settings.log_retention)
                .unwrap_or_default(),
            None => settings::LogRetention::default(),
        }
    }
}

/// 把一条刚写入的日志推给界面。
fn emit_log(app: &tauri::AppHandle, entry: &LogEntry) {
    let _ = app.emit("logs:appended", entry);
}

/// 写一条操作日志并推送。
fn log_operation(state: &AppState, app: &tauri::AppHandle, level: LogLevel, trace: &str, text: &str) {
    if let Ok(entry) = state.logs.write_operation(level, Some(trace), text) {
        emit_log(app, &entry);
    }
}

/// 写一条应用日志并推送。
fn log_app(state: &AppState, app: &tauri::AppHandle, record: AppLogRecord) {
    if let Ok(entry) = state.logs.write_app(&record) {
        emit_log(app, &entry);
    }
}

/// 逐行实时事件 `probe:row` 的完整负载。
///
/// `runId` 与 `kind`（`delay` / `speed` / `geo`）在这里补齐，行的字段来自探测器；
/// 事件里绝不带凭据。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct RowEventDto<'a> {
    run_id: &'a str,
    kind: &'a str,
    #[serde(flatten)]
    row: &'a probe::RowEvent,
}

/// 推一条逐行实时事件。
fn emit_row(app: &tauri::AppHandle, run_id: &str, kind: &str, row: &probe::RowEvent) {
    let _ = app.emit(
        "probe:row",
        RowEventDto {
            run_id,
            kind,
            row,
        },
    );
}

/// 读取整份设置。失败时返回错误字符串，前端保留默认值。
#[tauri::command]
fn get_settings(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<Settings, String> {
    let trace = logs::new_trace_id();
    let started = Instant::now();
    let loaded = {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        match guard.as_ref() {
            Some(store) => store.load_settings().map_err(|e| e.to_string()),
            None => Err("数据库不可用".to_string()),
        }
    };

    let duration_ms = started.elapsed().as_millis() as u64;
    match &loaded {
        Ok(_) => log_app(
            state.inner(),
            &app,
            AppLogRecord::new(LogLevel::Info, "settings", "settings.load")
                .trace(&trace)
                .duration(duration_ms),
        ),
        Err(message) => log_app(
            state.inner(),
            &app,
            AppLogRecord::new(LogLevel::Error, "settings", "settings.load")
                .trace(&trace)
                .duration(duration_ms)
                .error(message, None),
        ),
    }
    loaded
}

/// 写入整份设置，并按新的保留期立即清理一次过期日志。
#[tauri::command]
fn save_settings(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    mut settings: Settings,
) -> Result<(), String> {
    let trace = logs::new_trace_id();
    let started = Instant::now();

    // 只读参数先钳回契约值，旧客户端传来的值既不生效也不参与校验。
    settings.normalize_reserved();

    if let Err(e) = settings.validate() {
        let message = e.to_string();
        log_app(
            state.inner(),
            &app,
            AppLogRecord::new(LogLevel::Error, "settings", "settings.validate")
                .trace(&trace)
                .error(&message, None),
        );
        return Err(message);
    }

    let previous = {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        guard
            .as_ref()
            .and_then(|store| store.load_settings().ok())
    };

    let saved = {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        match guard.as_ref() {
            Some(store) => store.save_settings(&settings).map_err(|e| e.to_string()),
            None => Err("数据库不可用".to_string()),
        }
    };

    let duration_ms = started.elapsed().as_millis() as u64;
    match &saved {
        Ok(()) => {
            log_app(
                state.inner(),
                &app,
                AppLogRecord::new(LogLevel::Info, "settings", "settings.save")
                    .trace(&trace)
                    .duration(duration_ms)
                    .params(serde_json::json!({
                        "logRetention": {
                            "operationDays": settings.log_retention.operation_days,
                            "appDays": settings.log_retention.app_days,
                            "probeDays": settings.log_retention.probe_days,
                        },
                    })),
            );

            // 保留天数修改后立即生效；其余设置保存不重复触发清理。
            if previous.as_ref().map(|old| old.log_retention) != Some(settings.log_retention) {
                log_operation(
                    state.inner(),
                    &app,
                    LogLevel::Info,
                    &trace,
                    &format!(
                        "日志保留期已更新：操作 {} 天 / 应用 {} 天 / 探测 {} 天",
                        settings.log_retention.operation_days,
                        settings.log_retention.app_days,
                        settings.log_retention.probe_days
                    ),
                );
                for (_, _, entry) in state
                    .logs
                    .cleanup_expired(&settings.log_retention, SystemTime::now())
                {
                    if let Some(entry) = entry {
                        emit_log(&app, &entry);
                    }
                }
            }
        }
        Err(message) => log_app(
            state.inner(),
            &app,
            AppLogRecord::new(LogLevel::Error, "settings", "settings.save")
                .trace(&trace)
                .duration(duration_ms)
                .error(message, None),
        ),
    }
    saved
}

/// 内置离线归属地库的候选路径：打包后与主程序同目录 → 资源目录 → 开发期 resources。
///
/// 文件缺失时归属地查询自动回落在线的 `ipwho.is`，应用不会因此启动失败。
fn geo_candidates(app: &tauri::AppHandle) -> Vec<PathBuf> {
    let file = geo::offline::GEOIP_FILE;
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(geo::offline::GEOIP_DIR).join(file));
            candidates.push(
                dir.join("resources")
                    .join(geo::offline::GEOIP_DIR)
                    .join(file),
            );
        }
    }
    if let Ok(dir) = app.path().resource_dir() {
        candidates.push(dir.join(geo::offline::GEOIP_DIR).join(file));
        candidates.push(
            dir.join("resources")
                .join(geo::offline::GEOIP_DIR)
                .join(file),
        );
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join(geo::offline::GEOIP_DIR)
            .join(file),
    );
    candidates
}

/// 真正的探测在阻塞线程里跑，避免占住异步运行时。
fn probe_blocking(
    app: &tauri::AppHandle,
    endpoint: &probe::Endpoint,
    alias: &str,
    run_id: &str,
) -> Result<probe::ProbeOutcome, String> {
    let state = app.state::<AppState>();

    // 只短暂加锁读连接超时，测试期间不占着数据库。
    let connect_timeout_s = {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        let Some(store) = guard.as_ref() else {
            return Err("数据库不可用".to_string());
        };
        let settings = store.load_settings().map_err(|e| e.to_string())?;
        settings.connect_timeout_s
    };

    let options = probe::ProbeOptions::from_connect_timeout_s(connect_timeout_s);
    let prober = probe::DirectProber::new(state.logs.clone());

    let started_at = logs::now_iso8601();
    let outcome = prober.measure_latency(endpoint, &options);

    // 结果落库：一条轮次 + 一条结果。写库失败不影响已经测出的结论。
    {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        if let Some(store) = guard.as_ref() {
            probe::persist_single(
                store,
                run_id,
                &started_at,
                &probe::SingleProbe {
                    endpoint,
                    alias,
                    options,
                },
                &outcome,
            )?;
        }
    }

    Ok(outcome)
}

/// 单条直连延迟：把一个优选地址解析到 IP 后直连边缘测三次。
/// 单条直连延迟：把一个优选地址解析到 IP 后直连边缘测三次。
#[tauri::command]
async fn probe_single(
    app: tauri::AppHandle,
    host: String,
    port: u16,
    alias: Option<String>,
) -> Result<probe::ProbeOutcome, String> {
    let trace = logs::new_trace_id();
    let span = logs::new_span_id();
    let started = Instant::now();
    let endpoint = probe::Endpoint::new(host.trim().to_string(), port);
    let alias = alias.unwrap_or_default();
    // 单条重测也是一轮，逐行事件用同一个 runId。
    let run_id = format!("run-{}", logs::new_span_id());

    let result = tauri::async_runtime::spawn_blocking({
        let app = app.clone();
        let endpoint = endpoint.clone();
        let alias = alias.clone();
        let run_id = run_id.clone();
        move || probe_blocking(&app, &endpoint, &alias, &run_id)
    })
    .await
    .map_err(|e| format!("测试任务异常结束：{e}"))?;

    let duration_ms = started.elapsed().as_millis() as u64;
    let target = format!("{}:{}", endpoint.host, endpoint.port);
    let state = app.state::<AppState>();

    match &result {
        Ok(outcome) => {
            // 单条重测也推一条逐行事件，界面不用等整轮结束。
            emit_row(
                &app,
                &run_id,
                "delay",
                &probe::RowEvent::latency(&endpoint, outcome),
            );

            let mut record = logs::AppLogRecord::new(LogLevel::Info, "probe", "probe.latency")
                .trace(&trace)
                .span(&span)
                .duration(duration_ms)
                .params(serde_json::json!({
                    "host": endpoint.host,
                    "port": endpoint.port,
                    "alias": alias,
                }))
                .result(serde_json::json!({
                    "status": outcome.status.as_str(),
                    "latencyMs": outcome.latency_ms,
                }));
            if let Some(error) = &outcome.error {
                record = record.error(error, None);
            }
            log_app(state.inner(), &app, record);

            let (level, text) = match outcome.status {
                probe::ProbeStatus::Ok => (
                    LogLevel::Ok,
                    format!(
                        "测试 {target} 成功：{}ms",
                        outcome.latency_ms.unwrap_or_default()
                    ),
                ),
                probe::ProbeStatus::Timeout => (
                    LogLevel::Warn,
                    format!("测试 {target} 超时：{}", outcome.error.as_deref().unwrap_or("")),
                ),
                probe::ProbeStatus::Fail => (
                    LogLevel::Error,
                    format!("测试 {target} 失败：{}", outcome.error.as_deref().unwrap_or("")),
                ),
                probe::ProbeStatus::Untested => (
                    LogLevel::Info,
                    format!("测试 {target} 未测"),
                ),
            };
            log_operation(state.inner(), &app, level, &trace, &text);
        }
        Err(message) => {
            log_app(
                state.inner(),
                &app,
                logs::AppLogRecord::new(LogLevel::Error, "probe", "probe.latency")
                    .trace(&trace)
                    .span(&span)
                    .duration(duration_ms)
                    .params(serde_json::json!({
                        "host": endpoint.host,
                        "port": endpoint.port,
                        "alias": alias,
                    }))
                    .error(message, None),
            );
            log_operation(
                state.inner(),
                &app,
                LogLevel::Error,
                &trace,
                &format!("测试 {target} 无法开始：{message}"),
            );
        }
    }

    result
}

/// 一条批量结果的对外形状。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchRowDto {
    host: String,
    port: u16,
    alias: String,
    status: &'static str,
    latency_ms: Option<u64>,
    error: Option<String>,
    /// 三次原始采样，界面详情与历史回看用。
    samples: Vec<probe::ProbeOutcome>,
}

/// 一轮批量的完整结果。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchReportDto {
    run_id: String,
    total: usize,
    completed: usize,
    stopped: bool,
    counts: probe::batch::BatchCounts,
    rows: Vec<BatchRowDto>,
}

/// 一键直连测延迟：整池地址按设置里的并发与采样次数跑一轮。
///
/// 停止后已完成的地址保留结论，未开始的地址落库为「未测」。
#[tauri::command]
async fn probe_batch(app: tauri::AppHandle) -> Result<BatchReportDto, String> {
    let run_id = format!("run-{}", logs::new_span_id());
    let state = app.state::<AppState>();

    // 读设置与池：只短暂加锁，测试期间不占着数据库。
    let (options, batch, entries) = {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        let Some(store) = guard.as_ref() else {
            return Err("数据库不可用".to_string());
        };
        let settings = store.load_settings().map_err(|e| e.to_string())?;
        (
            probe::ProbeOptions::from_connect_timeout_s(settings.connect_timeout_s),
            probe::batch::BatchOptions::new(
                probe::batch::DEFAULT_SAMPLES,
                settings.concurrency as usize,
            ),
            store.list_pool_entries().map_err(|e| e.to_string())?,
        )
    };

    if entries.is_empty() {
        return Err("池里还没有地址，请先抓取优选源".to_string());
    }

    let items: Vec<probe::batch::BatchItem> = entries
        .into_iter()
        .map(|entry| {
            probe::batch::BatchItem::new(
                probe::Endpoint::new(entry.host, entry.port),
                entry.alias,
            )
        })
        .collect();

    // 同一时间只允许一轮；测试中重复触发直接拒绝。
    let control = Arc::new(BatchControl {
        cancel: AtomicBool::new(false),
    });
    {
        let mut guard = state
            .batch
            .lock()
            .map_err(|_| "测试状态锁已损坏".to_string())?;
        if guard.is_some() {
            return Err("测试正在进行，请先停止或等待结束".to_string());
        }
        *guard = Some(control.clone());
    }

    let started_at = logs::now_iso8601();
    let started = Instant::now();
    let params = serde_json::json!({
        "mode": "latency",
        "connectTimeoutS": options.connect_timeout.as_secs(),
        "requestTimeoutS": options.request_timeout.as_secs(),
        "samples": batch.samples,
        "concurrency": batch.concurrency,
        "total": items.len(),
    });

    log_operation(
        state.inner(),
        &app,
        LogLevel::Info,
        &run_id,
        &format!(
            "开始批量测试：{} 条，并发 {}，每条 {} 次采样",
            items.len(),
            batch.concurrency,
            batch.samples
        ),
    );

    // 直连探测与调度都在阻塞线程里跑，进度通过事件推给界面。
    let app_for_events = app.clone();
    let run_for_events = run_id.clone();
    let on_event = move |event: probe::batch::BatchEvent| match event {
        probe::batch::BatchEvent::Progress(progress) => {
            let _ = app_for_events.emit(
                "probe:progress",
                serde_json::json!({
                    "runId": run_for_events,
                    "mode": "latency",
                    "completed": progress.completed,
                    "total": progress.total,
                }),
            );
        }
        probe::batch::BatchEvent::Row(row) => {
            emit_row(&app_for_events, &run_for_events, "delay", &row);
        }
    };
    let cancel = control.clone();
    let prober = probe::DirectProber::new(state.logs.clone());
    let blocking_items = items.clone();

    let joined = tauri::async_runtime::spawn_blocking(move || {
        probe::batch::run_batch(
            &prober,
            &blocking_items,
            &options,
            batch,
            &cancel.cancel,
            &on_event,
        )
    })
    .await;

    // 无论成功、失败、停止还是任务异常，都要把这一轮从「正在跑」里摘掉。
    if let Ok(mut guard) = state.batch.lock() {
        *guard = None;
    }

    let report = joined.map_err(|e| format!("批量测试任务异常结束：{e}"))??;
    let finished_at = logs::now_iso8601();
    let duration_ms = started.elapsed().as_millis() as u64;

    {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        let Some(store) = guard.as_ref() else {
            return Err("数据库不可用".to_string());
        };
        probe::batch::persist_batch(
            store,
            &run_id,
            &started_at,
            &finished_at,
            &params,
            &report,
        )?;
    }

    // 每条地址一个 spanId，traceId 固定为 runId，同一轮可在日志里串起来。
    for row in &report.rows {
        let span = logs::new_span_id();
        let mut record = AppLogRecord::new(
            match row.outcome.status {
                probe::ProbeStatus::Ok => LogLevel::Info,
                probe::ProbeStatus::Timeout => LogLevel::Warn,
                probe::ProbeStatus::Fail => LogLevel::Error,
                probe::ProbeStatus::Untested => LogLevel::Info,
            },
            "probe",
            "probe.batch.item",
        )
        .trace(&run_id)
        .span(&span)
        .params(serde_json::json!({
            "host": row.endpoint.host,
            "port": row.endpoint.port,
            "alias": row.alias,
            "samples": row.samples.len(),
        }))
        .result(serde_json::json!({
            "status": row.outcome.status.as_str(),
            "latencyMs": row.outcome.latency_ms,
        }));
        if let Some(error) = &row.outcome.error {
            record = record.error(error, None);
        }
        log_app(state.inner(), &app, record);
    }

    let counts = report.counts();
    log_app(
        state.inner(),
        &app,
        AppLogRecord::new(LogLevel::Info, "probe", "probe.batch")
            .trace(&run_id)
            .duration(duration_ms)
            .params(params.clone())
            .result(serde_json::json!({
                "ok": counts.ok,
                "timeout": counts.timeout,
                "fail": counts.fail,
                "untested": counts.untested,
                "stopped": report.stopped,
            })),
    );
    log_operation(
        state.inner(),
        &app,
        if report.stopped {
            LogLevel::Warn
        } else {
            LogLevel::Ok
        },
        &run_id,
        &format!(
            "批量测试结束：成功 {} · 超时 {} · 失败 {} · 未测 {}（{}ms）",
            counts.ok, counts.timeout, counts.fail, counts.untested, duration_ms
        ),
    );

    Ok(BatchReportDto {
        run_id,
        total: report.total,
        completed: report.completed,
        stopped: report.stopped,
        counts,
        rows: report
            .rows
            .into_iter()
            .map(|row| BatchRowDto {
                host: row.endpoint.host,
                port: row.endpoint.port,
                alias: row.alias,
                status: row.outcome.status.as_str(),
                latency_ms: row.outcome.latency_ms,
                error: row.outcome.error,
                samples: row.samples,
            })
            .collect(),
    })
}

/// 一条测速目标，界面传选中行时使用。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpeedTarget {
    host: String,
    port: u16,
}

/// 一条测速结果的对外形状。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SpeedRowDto {
    host: String,
    port: u16,
    alias: String,
    status: &'static str,
    mbps: Option<f64>,
    error: Option<String>,
}

/// 一轮测速的完整结果。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SpeedReportDto {
    run_id: String,
    total: usize,
    completed: usize,
    stopped: bool,
    counts: probe::speed::SpeedCounts,
    rows: Vec<SpeedRowDto>,
}

/// 一键多线程测速：整池地址或指定选中行，直连下载并换算 MB/s。
///
/// 结果写进最近一轮 `test_result` 的速度字段，延迟列不受影响；
/// 没有历史轮次时才新建一轮。停止后已开始的行标失败，未开始的行保持原值。
#[tauri::command]
async fn probe_speed_batch(
    app: tauri::AppHandle,
    targets: Option<Vec<SpeedTarget>>,
) -> Result<SpeedReportDto, String> {
    let state = app.state::<AppState>();

    // 读设置与池：只短暂加锁，测速期间不占着数据库。
    let (options, concurrency, entries) = {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        let Some(store) = guard.as_ref() else {
            return Err("数据库不可用".to_string());
        };
        let settings = store.load_settings().map_err(|e| e.to_string())?;
        (
            probe::ProbeOptions::from_connect_timeout_s(settings.connect_timeout_s),
            settings.concurrency as usize,
            store.list_pool_entries().map_err(|e| e.to_string())?,
        )
    };

    let wanted: Option<std::collections::HashSet<(String, u16)>> = targets.as_ref().map(|list| {
        list.iter()
            .map(|target| (target.host.trim().to_string(), target.port))
            .collect()
    });

    let items: Vec<probe::batch::BatchItem> = entries
        .into_iter()
        .filter(|entry| {
            wanted
                .as_ref()
                .map(|set| set.contains(&(entry.host.clone(), entry.port)))
                .unwrap_or(true)
        })
        .map(|entry| {
            probe::batch::BatchItem::new(
                probe::Endpoint::new(entry.host, entry.port),
                entry.alias,
            )
        })
        .collect();

    if items.is_empty() {
        return Err("没有可测速的地址，请先抓取优选源或选中行".to_string());
    }

    let speed = probe::speed::SpeedOptions::default();

    // 与延迟测试共用同一个「正在跑」槽位：同一时间只允许一轮。
    let control = Arc::new(BatchControl {
        cancel: AtomicBool::new(false),
    });
    {
        let mut guard = state
            .batch
            .lock()
            .map_err(|_| "测试状态锁已损坏".to_string())?;
        if guard.is_some() {
            return Err("测试正在进行，请先停止或等待结束".to_string());
        }
        *guard = Some(control.clone());
    }

    let started_at = logs::now_iso8601();
    let started = Instant::now();
    let params = serde_json::json!({
        "mode": "speed",
        "streams": speed.streams,
        "sizeMb": speed.total_bytes / 1_000_000,
        "timeoutS": speed.timeout.as_secs(),
        "concurrency": concurrency,
        "total": items.len(),
    });

    // 挂到最近一轮结果上；没有历史轮次时才新建。
    let run_id = {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        let Some(store) = guard.as_ref() else {
            return Err("数据库不可用".to_string());
        };
        match store.latest_test_run_id().map_err(|e| e.to_string())? {
            Some(id) => id,
            None => {
                let id = format!("run-{}", logs::new_span_id());
                store
                    .insert_test_run(&store::TestRunRecord {
                        id: &id,
                        started_at: &started_at,
                        finished_at: &started_at,
                        params_json: &params.to_string(),
                        total: items.len() as u32,
                        ok_count: 0,
                        timeout_count: 0,
                        fail_count: 0,
                    })
                    .map_err(|e| e.to_string())?;
                id
            }
        }
    };

    log_operation(
        state.inner(),
        &app,
        LogLevel::Info,
        &run_id,
        &format!(
            "开始测速：{} 条，{} 流 × {}MB，并发 {}，上限 {}s",
            items.len(),
            speed.streams,
            speed.total_bytes / 1_000_000,
            concurrency,
            speed.timeout.as_secs()
        ),
    );

    let app_for_events = app.clone();
    let run_for_events = run_id.clone();
    let on_event = move |event: probe::batch::BatchEvent| match event {
        probe::batch::BatchEvent::Progress(progress) => {
            let _ = app_for_events.emit(
                "probe:progress",
                serde_json::json!({
                    "runId": run_for_events,
                    "mode": "speed",
                    "completed": progress.completed,
                    "total": progress.total,
                }),
            );
        }
        probe::batch::BatchEvent::Row(row) => {
            emit_row(&app_for_events, &run_for_events, "speed", &row);
        }
    };

    let cancel = control.clone();
    let prober = probe::DirectProber::new(state.logs.clone());
    let blocking_items = items.clone();

    let joined = tauri::async_runtime::spawn_blocking(move || {
        probe::speed::run_speed_batch(
            &prober,
            &blocking_items,
            &options,
            &speed,
            concurrency,
            &cancel.cancel,
            &on_event,
        )
    })
    .await;

    // 无论成功、失败、停止还是任务异常，都要把这一轮从「正在跑」里摘掉。
    if let Ok(mut guard) = state.batch.lock() {
        *guard = None;
    }

    let report = joined.map_err(|e| format!("测速任务异常结束：{e}"))??;
    let duration_ms = started.elapsed().as_millis() as u64;

    {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        let Some(store) = guard.as_ref() else {
            return Err("数据库不可用".to_string());
        };
        probe::speed::persist_speed(store, &run_id, &report.rows)?;
    }

    for row in &report.rows {
        let mut record = AppLogRecord::new(
            match row.outcome.status {
                probe::ProbeStatus::Ok => LogLevel::Info,
                probe::ProbeStatus::Fail => LogLevel::Warn,
                probe::ProbeStatus::Timeout => LogLevel::Warn,
                probe::ProbeStatus::Untested => LogLevel::Info,
            },
            "probe",
            "probe.speed.item",
        )
        .trace(&run_id)
        .span(&logs::new_span_id())
        .params(serde_json::json!({
            "host": row.endpoint.host,
            "port": row.endpoint.port,
            "alias": row.alias,
        }))
        .result(serde_json::json!({
            "status": row.outcome.status.as_str(),
            "mbps": row.outcome.mbps,
        }));
        if let Some(error) = &row.outcome.error {
            record = record.error(error, None);
        }
        log_app(state.inner(), &app, record);
    }

    let counts = report.counts();
    log_app(
        state.inner(),
        &app,
        AppLogRecord::new(LogLevel::Info, "probe", "probe.speed")
            .trace(&run_id)
            .duration(duration_ms)
            .params(params.clone())
            .result(serde_json::json!({
                "ok": counts.ok,
                "fail": counts.fail,
                "untested": counts.untested,
                "stopped": report.stopped,
            })),
    );
    log_operation(
        state.inner(),
        &app,
        if report.stopped {
            LogLevel::Warn
        } else {
            LogLevel::Ok
        },
        &run_id,
        &format!(
            "测速结束：成功 {} · 失败 {} · 未测 {}（{}ms）",
            counts.ok, counts.fail, counts.untested, duration_ms
        ),
    );

    Ok(SpeedReportDto {
        run_id,
        total: report.total,
        completed: report.completed,
        stopped: report.stopped,
        counts,
        rows: report
            .rows
            .into_iter()
            .map(|row| SpeedRowDto {
                host: row.endpoint.host,
                port: row.endpoint.port,
                alias: row.alias,
                status: row.outcome.status.as_str(),
                mbps: row.outcome.mbps,
                error: row.outcome.error,
            })
            .collect(),
    })
}

/// 一条归属地结果的对外形状。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeoRowDto {
    host: String,
    port: u16,
    alias: String,
    /// `ok` / `fail` / `untested`，与延迟状态互不影响。
    status: &'static str,
    country: Option<String>,
    zh: Option<String>,
    en: Option<String>,
    asn: Option<u32>,
    error: Option<String>,
}

/// 一轮归属地查询的完整结果。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeoReportDto {
    run_id: String,
    total: usize,
    completed: usize,
    stopped: bool,
    counts: geo::GeoCounts,
    rows: Vec<GeoRowDto>,
}

/// 一键查归属地：整池或选中行，查地址本身解析出的 IP 的归属地。
///
/// 结果写进最近一轮 `test_result` 的归属地列，延迟、速度不受影响；
/// 查询失败的行只留空归属地，不清空已有结论。
#[tauri::command]
async fn probe_geo_batch(
    app: tauri::AppHandle,
    targets: Option<Vec<SpeedTarget>>,
) -> Result<GeoReportDto, String> {
    let state = app.state::<AppState>();

    // 读设置与池：只短暂加锁，查询期间不占着数据库。
    let (concurrency, entries) = {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        let Some(store) = guard.as_ref() else {
            return Err("数据库不可用".to_string());
        };
        let settings = store.load_settings().map_err(|e| e.to_string())?;
        (
            settings.concurrency as usize,
            store.list_pool_entries().map_err(|e| e.to_string())?,
        )
    };

    let wanted: Option<std::collections::HashSet<(String, u16)>> = targets.as_ref().map(|list| {
        list.iter()
            .map(|target| (target.host.trim().to_string(), target.port))
            .collect()
    });

    let items: Vec<probe::batch::BatchItem> = entries
        .into_iter()
        .filter(|entry| {
            wanted
                .as_ref()
                .map(|set| set.contains(&(entry.host.clone(), entry.port)))
                .unwrap_or(true)
        })
        .map(|entry| {
            probe::batch::BatchItem::new(
                probe::Endpoint::new(entry.host, entry.port),
                entry.alias,
            )
        })
        .collect();

    if items.is_empty() {
        return Err("没有可查归属地的地址，请先抓取优选源或选中行".to_string());
    }

    // 与延迟、测速共用同一个「正在跑」槽位：同一时间只允许一轮。
    let control = Arc::new(BatchControl {
        cancel: AtomicBool::new(false),
    });
    {
        let mut guard = state
            .batch
            .lock()
            .map_err(|_| "测试状态锁已损坏".to_string())?;
        if guard.is_some() {
            return Err("测试正在进行，请先停止或等待结束".to_string());
        }
        *guard = Some(control.clone());
    }

    let started_at = logs::now_iso8601();
    let started = Instant::now();
    let params = serde_json::json!({
        "mode": "geo",
        "concurrency": concurrency,
        "total": items.len(),
    });

    // 挂到最近一轮结果上；没有历史轮次时才新建。
    let run_id = {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        let Some(store) = guard.as_ref() else {
            return Err("数据库不可用".to_string());
        };
        match store.latest_test_run_id().map_err(|e| e.to_string())? {
            Some(id) => id,
            None => {
                let id = format!("run-{}", logs::new_span_id());
                store
                    .insert_test_run(&store::TestRunRecord {
                        id: &id,
                        started_at: &started_at,
                        finished_at: &started_at,
                        params_json: &params.to_string(),
                        total: items.len() as u32,
                        ok_count: 0,
                        timeout_count: 0,
                        fail_count: 0,
                    })
                    .map_err(|e| e.to_string())?;
                id
            }
        }
    };

    // 落库的缓存先灌进内存，同一 IP 不重复查询。
    let cached = {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        match guard.as_ref() {
            Some(store) => store
                .list_geo_cache()
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|row| (row.ip, row.record))
                .collect::<Vec<_>>(),
            None => Vec::new(),
        }
    };
    state.geo.preload(cached);

    let offline_ready = geo_candidates(&app).iter().any(|path| path.is_file());
    log_operation(
        state.inner(),
        &app,
        LogLevel::Info,
        &run_id,
        &format!(
            "开始查询归属地：{} 条，并发 {}，离线库{}",
            items.len(),
            concurrency,
            if offline_ready {
                "已就绪"
            } else {
                "不可用（改用在线源）"
            }
        ),
    );

    let app_for_events = app.clone();
    let run_for_events = run_id.clone();
    let on_event = move |event: probe::batch::BatchEvent| match event {
        probe::batch::BatchEvent::Progress(progress) => {
            let _ = app_for_events.emit(
                "probe:progress",
                serde_json::json!({
                    "runId": run_for_events,
                    "mode": "geo",
                    "completed": progress.completed,
                    "total": progress.total,
                }),
            );
        }
        probe::batch::BatchEvent::Row(row) => {
            emit_row(&app_for_events, &run_for_events, "geo", &row);
        }
    };

    let cancel = control.clone();
    let blocking_items = items.clone();
    let resolver = state.geo.clone();
    let host_resolver = probe::SystemResolver;

    let joined = tauri::async_runtime::spawn_blocking(move || {
        geo::run_geo_batch(
            &host_resolver,
            &resolver,
            &blocking_items,
            concurrency,
            &cancel.cancel,
            &on_event,
        )
    })
    .await;

    // 无论成功、失败、停止还是任务异常，都要把这一轮从「正在跑」里摘掉。
    if let Ok(mut guard) = state.batch.lock() {
        *guard = None;
    }

    let report = joined.map_err(|e| format!("归属地查询任务异常结束：{e}"))?;
    let duration_ms = started.elapsed().as_millis() as u64;

    {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        let Some(store) = guard.as_ref() else {
            return Err("数据库不可用".to_string());
        };
        geo::persist_geo(store, &run_id, &report.rows)?;
    }

    for row in &report.rows {
        let mut record = AppLogRecord::new(
            if row.info.is_some() {
                LogLevel::Info
            } else {
                LogLevel::Warn
            },
            "geo",
            "geo.lookup",
        )
        .trace(&run_id)
        .span(&logs::new_span_id())
        .params(serde_json::json!({
            "host": row.endpoint.host,
            "port": row.endpoint.port,
            "alias": row.alias,
        }));
        match &row.info {
            Some(info) => {
                record = record.result(serde_json::json!({
                    "ip": info.ip,
                    "country": info.country,
                    "zh": info.zh,
                    "en": info.en,
                    "asn": info.asn,
                    "source": info.source,
                }));
            }
            None => {
                record = record.error(
                    row.error.as_deref().unwrap_or("归属地查询没有结论"),
                    None,
                );
            }
        }
        log_app(state.inner(), &app, record);
    }

    let counts = report.counts();
    log_app(
        state.inner(),
        &app,
        AppLogRecord::new(LogLevel::Info, "geo", "geo.batch")
            .trace(&run_id)
            .duration(duration_ms)
            .params(params.clone())
            .result(serde_json::json!({
                "ok": counts.ok,
                "fail": counts.fail,
                "untested": counts.untested,
                "stopped": report.stopped,
            })),
    );
    log_operation(
        state.inner(),
        &app,
        if counts.fail > 0 {
            LogLevel::Warn
        } else {
            LogLevel::Ok
        },
        &run_id,
        &format!(
            "归属地查询结束：成功 {} · 失败 {} · 未测 {}（{}ms）",
            counts.ok, counts.fail, counts.untested, duration_ms
        ),
    );

    Ok(GeoReportDto {
        run_id,
        total: report.total,
        completed: report.completed,
        stopped: report.stopped,
        counts,
        rows: report
            .rows
            .into_iter()
            .map(|row| {
                let status = if row.info.is_some() {
                    "ok"
                } else if row.error.is_some() {
                    "fail"
                } else {
                    "untested"
                };
                let info = row.info;
                GeoRowDto {
                    host: row.endpoint.host,
                    port: row.endpoint.port,
                    alias: row.alias,
                    status,
                    country: info.as_ref().map(|info| info.country.clone()),
                    zh: info.as_ref().map(|info| info.zh.clone()),
                    en: info.as_ref().map(|info| info.en.clone()),
                    asn: info.as_ref().and_then(|info| info.asn),
                    error: row.error,
                }
            })
            .collect(),
    })
}

/// 请求停止当前批量测试；没有在跑时返回 `false`。
#[tauri::command]
fn probe_cancel(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<bool, String> {
    let trace = logs::new_trace_id();
    let started = Instant::now();

    let result = (|| -> Result<bool, String> {
        let guard = state
            .batch
            .lock()
            .map_err(|_| "测试状态锁已损坏".to_string())?;
        match guard.as_ref() {
            Some(control) => {
                control.cancel.store(true, Ordering::SeqCst);
                Ok(true)
            }
            None => Ok(false),
        }
    })();

    let duration_ms = started.elapsed().as_millis() as u64;
    match &result {
        Ok(requested) => {
            log_app(
                state.inner(),
                &app,
                AppLogRecord::new(LogLevel::Info, "probe", "probe.cancel")
                    .trace(&trace)
                    .duration(duration_ms)
                    .result(serde_json::json!({ "requested": requested })),
            );
            log_operation(
                state.inner(),
                &app,
                LogLevel::Info,
                &trace,
                if *requested {
                    "已请求停止当前测试"
                } else {
                    "没有正在运行的测试，忽略停止请求"
                },
            );
        }
        Err(message) => {
            log_app(
                state.inner(),
                &app,
                AppLogRecord::new(LogLevel::Error, "probe", "probe.cancel")
                    .trace(&trace)
                    .duration(duration_ms)
                    .error(message, None),
            );
            log_operation(
                state.inner(),
                &app,
                LogLevel::Error,
                &trace,
                &format!("请求停止测试失败：{message}"),
            );
        }
    }
    result
}

/// 历史入口的一轮测试：时间、参数快照与统计。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunDto {
    id: String,
    started_at: String,
    finished_at: Option<String>,
    /// 参数快照，字段随测试类型变化；损坏的 JSON 归一成空对象。
    params: serde_json::Value,
    total: u32,
    ok: u32,
    timeout: u32,
    fail: u32,
    /// 本轮实际落库的地址数。
    address_count: u32,
}

impl From<TestRunSummaryRow> for TestRunDto {
    fn from(row: TestRunSummaryRow) -> Self {
        Self {
            id: row.id,
            started_at: row.started_at,
            finished_at: row.finished_at,
            params: serde_json::from_str(&row.params_json)
                .unwrap_or_else(|_| serde_json::json!({})),
            total: row.total,
            ok: row.ok_count,
            timeout: row.timeout_count,
            fail: row.fail_count,
            address_count: row.result_count,
        }
    }
}

/// 历史结果的一行，字段与主表格的测量列一致。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryResultDto {
    host: String,
    port: u16,
    alias: String,
    status: String,
    latency_ms: Option<u64>,
    error: Option<String>,
    speed_mbps: Option<f64>,
    speed_status: String,
    speed_error: Option<String>,
    country: Option<String>,
    zh: Option<String>,
    en: Option<String>,
}

impl From<store::TestResultRow> for HistoryResultDto {
    fn from(row: store::TestResultRow) -> Self {
        Self {
            host: row.host,
            port: row.port,
            alias: row.alias,
            status: row.status,
            latency_ms: row.latency_ms,
            error: row.error,
            speed_mbps: row.speed_mbps,
            speed_status: row.speed_status,
            speed_error: row.speed_error,
            country: row.geo_country,
            zh: row.geo_zh,
            en: row.geo_en,
        }
    }
}

/// 某一轮的全部结果。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TestRunDetailDto {
    run: TestRunDto,
    results: Vec<HistoryResultDto>,
}

/// 同一地址在某一轮的结果点。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AddressPointDto {
    run_id: String,
    started_at: String,
    status: String,
    latency_ms: Option<u64>,
    speed_mbps: Option<f64>,
    speed_status: String,
}

/// 同一地址的跨轮序列。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AddressHistoryDto {
    host: String,
    port: u16,
    points: Vec<AddressPointDto>,
}

/// 列出历史测试轮次：时间、参数快照、成功/超时/失败统计与地址数。
#[tauri::command]
fn list_test_runs(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<Vec<TestRunDto>, String> {
    let trace = logs::new_trace_id();
    let started = Instant::now();
    let listed = {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        let Some(store) = guard.as_ref() else {
            return Err("数据库不可用".to_string());
        };
        store.list_test_runs().map_err(|e| e.to_string())
    };

    let duration_ms = started.elapsed().as_millis() as u64;
    match &listed {
        Ok(runs) => log_app(
            state.inner(),
            &app,
            AppLogRecord::new(LogLevel::Info, "history", "history.runs")
                .trace(&trace)
                .duration(duration_ms)
                .result(serde_json::json!({ "runs": runs.len() })),
        ),
        Err(message) => log_app(
            state.inner(),
            &app,
            AppLogRecord::new(LogLevel::Error, "history", "history.runs")
                .trace(&trace)
                .duration(duration_ms)
                .error(message, None),
        ),
    }

    Ok(listed?.into_iter().map(TestRunDto::from).collect())
}

/// 读取某一轮的全部结果，字段与主表格一致。
#[tauri::command]
fn get_test_run_results(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    run_id: String,
) -> Result<TestRunDetailDto, String> {
    let trace = logs::new_trace_id();
    let started = Instant::now();
    let loaded = {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        let Some(store) = guard.as_ref() else {
            return Err("数据库不可用".to_string());
        };
        match store.get_test_run(&run_id).map_err(|e| e.to_string())? {
            Some(run) => {
                let results = store
                    .list_test_results(&run_id)
                    .map_err(|e| e.to_string())?;
                Ok((run, results))
            }
            None => Err(format!("测试轮次不存在：{run_id}")),
        }
    };

    let duration_ms = started.elapsed().as_millis() as u64;
    match &loaded {
        Ok((_, results)) => log_app(
            state.inner(),
            &app,
            AppLogRecord::new(LogLevel::Info, "history", "history.results")
                .trace(&trace)
                .duration(duration_ms)
                .params(serde_json::json!({ "runId": run_id }))
                .result(serde_json::json!({ "results": results.len() })),
        ),
        Err(message) => log_app(
            state.inner(),
            &app,
            AppLogRecord::new(LogLevel::Error, "history", "history.results")
                .trace(&trace)
                .duration(duration_ms)
                .params(serde_json::json!({ "runId": run_id }))
                .error(message, None),
        ),
    }

    let (run, results) = loaded?;
    Ok(TestRunDetailDto {
        run: TestRunDto {
            id: run.id,
            started_at: run.started_at,
            finished_at: run.finished_at,
            params: serde_json::from_str(&run.params_json)
                .unwrap_or_else(|_| serde_json::json!({})),
            total: run.total,
            ok: run.ok_count,
            timeout: run.timeout_count,
            fail: run.fail_count,
            address_count: results.len() as u32,
        },
        results: results.into_iter().map(HistoryResultDto::from).collect(),
    })
}

/// 按轮次整轮删除，删除后顺带刷新体积。
///
/// 只允许整轮删除；历史是事实记录，不提供逐条编辑。
#[tauri::command]
fn delete_test_run(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    run_id: String,
) -> Result<bool, String> {
    let trace = logs::new_trace_id();
    let started = Instant::now();
    let deleted = {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        let Some(store) = guard.as_ref() else {
            return Err("数据库不可用".to_string());
        };
        let deleted = store.delete_test_run(&run_id).map_err(|e| e.to_string())?;
        let size = store.database_size_bytes().unwrap_or(0);
        Ok::<_, String>((deleted, size))
    };

    let duration_ms = started.elapsed().as_millis() as u64;
    match &deleted {
        Ok((true, size)) => {
            log_app(
                state.inner(),
                &app,
                AppLogRecord::new(LogLevel::Info, "history", "history.delete")
                    .trace(&trace)
                    .duration(duration_ms)
                    .params(serde_json::json!({ "runId": run_id }))
                    .result(serde_json::json!({ "dbBytes": size })),
            );
            log_operation(
                state.inner(),
                &app,
                LogLevel::Warn,
                &trace,
                &format!(
                    "已删除测试轮次 {run_id}，数据库体积 {} MB",
                    (*size as f64 / 1024.0 / 1024.0 * 10.0).round() / 10.0
                ),
            );
        }
        Ok((false, _)) => {
            log_operation(
                state.inner(),
                &app,
                LogLevel::Warn,
                &trace,
                &format!("删除测试轮次失败：{run_id} 不存在"),
            );
        }
        Err(message) => log_app(
            state.inner(),
            &app,
            AppLogRecord::new(LogLevel::Error, "history", "history.delete")
                .trace(&trace)
                .duration(duration_ms)
                .params(serde_json::json!({ "runId": run_id }))
                .error(message, None),
        ),
    }

    let (deleted, _) = deleted?;
    Ok(deleted)
}

/// 数据库体积（字节）；界面据此在超过 50MB 时提示按轮次清理。
#[tauri::command]
fn get_database_size(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<u64, String> {
    let trace = logs::new_trace_id();
    let started = Instant::now();
    let size = {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        let Some(store) = guard.as_ref() else {
            return Err("数据库不可用".to_string());
        };
        store.database_size_bytes().map_err(|e| e.to_string())
    };

    let duration_ms = started.elapsed().as_millis() as u64;
    match &size {
        Ok(bytes) => log_app(
            state.inner(),
            &app,
            AppLogRecord::new(LogLevel::Info, "history", "history.db_size")
                .trace(&trace)
                .duration(duration_ms)
                .result(serde_json::json!({ "bytes": bytes })),
        ),
        Err(message) => log_app(
            state.inner(),
            &app,
            AppLogRecord::new(LogLevel::Error, "history", "history.db_size")
                .trace(&trace)
                .duration(duration_ms)
                .error(message, None),
        ),
    }
    size
}

/// 同一地址跨轮次的延迟与速度序列，按时间升序。
#[tauri::command]
fn get_address_history(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    host: String,
    port: u16,
) -> Result<AddressHistoryDto, String> {
    let trace = logs::new_trace_id();
    let started = Instant::now();
    let loaded = {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        let Some(store) = guard.as_ref() else {
            return Err("数据库不可用".to_string());
        };
        store
            .list_address_history(host.trim(), port)
            .map_err(|e| e.to_string())
    };

    let duration_ms = started.elapsed().as_millis() as u64;
    match &loaded {
        Ok(points) => log_app(
            state.inner(),
            &app,
            AppLogRecord::new(LogLevel::Info, "history", "history.address")
                .trace(&trace)
                .duration(duration_ms)
                .params(serde_json::json!({ "host": host, "port": port }))
                .result(serde_json::json!({ "points": points.len() })),
        ),
        Err(message) => log_app(
            state.inner(),
            &app,
            AppLogRecord::new(LogLevel::Error, "history", "history.address")
                .trace(&trace)
                .duration(duration_ms)
                .params(serde_json::json!({ "host": host, "port": port }))
                .error(message, None),
        ),
    }

    let points = loaded?;
    Ok(AddressHistoryDto {
        host,
        port,
        points: points
            .into_iter()
            .map(|point| AddressPointDto {
                run_id: point.run_id,
                started_at: point.started_at,
                status: point.status,
                latency_ms: point.latency_ms,
                speed_mbps: point.speed_mbps,
                speed_status: point.speed_status,
            })
            .collect(),
    })
}

/// 一条优选源，前端直接渲染。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PoolSourceDto {
    url: String,
    name: String,
    checked: bool,
    broken: bool,
    entry_count: u32,
    fetched_at: Option<String>,
    last_error: Option<String>,
}

impl From<PoolSourceRow> for PoolSourceDto {
    fn from(row: PoolSourceRow) -> Self {
        Self {
            url: row.url,
            name: row.name,
            checked: row.checked,
            broken: row.broken,
            entry_count: row.entry_count,
            fetched_at: row.fetched_at,
            last_error: row.last_error,
        }
    }
}

/// 池里的一条优选地址。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PoolEntryDto {
    host: String,
    port: u16,
    alias: String,
    source: String,
    added_at: String,
}

impl From<PoolEntryRow> for PoolEntryDto {
    fn from(row: PoolEntryRow) -> Self {
        Self {
            host: row.host,
            port: row.port,
            alias: row.alias,
            source: row.source,
            added_at: row.added_at,
        }
    }
}

/// 一次读取的完整池状态：源清单、条目与最近抓取时间。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PoolSnapshot {
    sources: Vec<PoolSourceDto>,
    entries: Vec<PoolEntryDto>,
    fetched_at: Option<String>,
    /// 本次抓取被去重掉的条数；从库里读回时为 0。
    duplicates: u32,
}

fn read_snapshot(store: &Store, duplicates: u32) -> Result<PoolSnapshot, String> {
    let sources = store
        .list_pool_sources()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(PoolSourceDto::from)
        .collect();
    let entries = store
        .list_pool_entries()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(PoolEntryDto::from)
        .collect();
    let fetched_at = store.last_fetched_at().map_err(|e| e.to_string())?;
    Ok(PoolSnapshot {
        sources,
        entries,
        fetched_at,
        duplicates,
    })
}

/// 读取已落库的池。断网重启后仍显示上次结果与时间。
#[tauri::command]
fn get_pool(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<PoolSnapshot, String> {
    let trace = logs::new_trace_id();
    let snapshot = {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        let Some(store) = guard.as_ref() else {
            return Err("数据库不可用".to_string());
        };
        read_snapshot(store, 0)
    };
    if let Err(message) = &snapshot {
        log_app(
            state.inner(),
            &app,
            AppLogRecord::new(LogLevel::Error, "pool", "pool.read")
                .trace(&trace)
                .error(message, None),
        );
    }
    snapshot
}

/// 抓取 bestcf 页面，刷新优选源清单；已有源的勾选与抓取结果保持不变。
#[tauri::command]
async fn refresh_source_catalog(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<PoolSnapshot, String> {
    let trace = logs::new_trace_id();
    let started = Instant::now();
    let client = fetch::build_client()?;
    let html = match fetch::fetch_text(&client, CATALOG_URL).await {
        Ok(html) => html,
        Err(e) => {
            let message = format!("抓取优选源清单失败：{e}");
            log_app(
                state.inner(),
                &app,
                AppLogRecord::new(LogLevel::Error, "sources", "source.catalog")
                    .trace(&trace)
                    .duration(started.elapsed().as_millis() as u64)
                    .params(serde_json::json!({ "url": CATALOG_URL }))
                    .error(&message, None),
            );
            log_operation(state.inner(), &app, LogLevel::Error, &trace, &message);
            return Err(message);
        }
    };

    let catalog = parse_catalog(&html);
    if catalog.is_empty() {
        let message = "页面里没有解析到任何优选源，可能页面结构已变化".to_string();
        log_app(
            state.inner(),
            &app,
            AppLogRecord::new(LogLevel::Error, "sources", "source.catalog.parse")
                .trace(&trace)
                .duration(started.elapsed().as_millis() as u64)
                .error(&message, None),
        );
        log_operation(state.inner(), &app, LogLevel::Error, &trace, &message);
        return Err(message);
    }

    let snapshot = {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        let Some(store) = guard.as_ref() else {
            return Err("数据库不可用".to_string());
        };
        store.upsert_catalog(&catalog).map_err(|e| e.to_string())?;
        read_snapshot(store, 0)
    };

    let duration_ms = started.elapsed().as_millis() as u64;
    log_app(
        state.inner(),
        &app,
        AppLogRecord::new(LogLevel::Info, "sources", "source.catalog")
            .trace(&trace)
            .duration(duration_ms)
            .params(serde_json::json!({ "url": CATALOG_URL }))
            .result(serde_json::json!({ "sources": catalog.len() })),
    );
    log_operation(
        state.inner(),
        &app,
        LogLevel::Info,
        &trace,
        &format!("优选源清单已刷新：{} 个源", catalog.len()),
    );
    snapshot
}

/// 抓取选中的优选源并把手动地址一起并入池。
///
/// 单源失败只把错误记到该源上，其余源照常解析；全部失败也返回快照，
/// 让界面能逐源标红，而不是整块报错。每个源一个 spanId，耗时与结果写进应用日志。
#[tauri::command]
async fn fetch_pool(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    source_urls: Vec<String>,
    extra_source_urls: Vec<String>,
    manual_addresses: String,
) -> Result<PoolSnapshot, String> {
    let trace = logs::new_trace_id();
    let started = Instant::now();

    // 粘贴进来的源地址也进源清单，抓取结果与异常才能逐源显示。
    let mut requested: Vec<String> = source_urls;
    for url in extra_source_urls {
        let url = url.trim().to_string();
        if url.is_empty() || requested.contains(&url) {
            continue;
        }
        requested.push(url);
    }

    log_operation(
        state.inner(),
        &app,
        LogLevel::Info,
        &trace,
        &format!("开始抓取 {} 个优选源", requested.len()),
    );

    // 先取源名称，再放掉锁去抓网络。
    let known: Vec<(String, String)> = {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        let Some(store) = guard.as_ref() else {
            return Err("数据库不可用".to_string());
        };
        let existing = store
            .list_pool_sources()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|s| (s.url, s.name))
            .collect::<Vec<_>>();
        let missing: Vec<CatalogSource> = requested
            .iter()
            .filter(|url| !existing.iter().any(|(known, _)| known == *url))
            .map(|url| CatalogSource {
                url: url.clone(),
                name: source_label(url),
                broken: false,
            })
            .collect();
        if !missing.is_empty() {
            store
                .upsert_catalog(&missing)
                .map_err(|e| e.to_string())?;
        }
        store
            .list_pool_sources()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|s| (s.url, s.name))
            .collect()
    };

    let catalog: Vec<CatalogSource> = requested
        .iter()
        .map(|url| CatalogSource {
            url: url.clone(),
            name: known
                .iter()
                .find(|(known_url, _)| known_url == url)
                .map(|(_, name)| name.clone())
                .unwrap_or_else(|| source_label(url)),
            broken: false,
        })
        .collect();

    let client = fetch::build_client()?;
    let fetched = fetch::fetch_all(&client, &catalog).await;
    let build = fetch::assemble(&fetched, &manual_addresses);

    // 每个源一个 spanId：记录耗时与结果，失败的带上错误。
    for outcome in &build.outcomes {
        let span = logs::new_span_id();
        let mut record = AppLogRecord::new(
            if outcome.error.is_some() {
                LogLevel::Error
            } else {
                LogLevel::Info
            },
            "sources",
            "source.fetch",
        )
        .trace(&trace)
        .span(&span)
        .duration(outcome.duration_ms)
        .params(serde_json::json!({
            "url": outcome.url,
            "name": outcome.name,
        }))
        .result(serde_json::json!({ "entries": outcome.entry_count }));
        if let Some(error) = &outcome.error {
            record = record.error(error, None);
        }
        log_app(state.inner(), &app, record);

        match &outcome.error {
            Some(error) => log_operation(
                state.inner(),
                &app,
                LogLevel::Error,
                &trace,
                &format!("源 {} 抓取失败：{error}", outcome.name),
            ),
            None => log_operation(
                state.inner(),
                &app,
                LogLevel::Info,
                &trace,
                &format!(
                    "源 {} 解析 {} 条（{}ms）",
                    outcome.name, outcome.entry_count, outcome.duration_ms
                ),
            ),
        }
    }

    let fetched_at = logs::now_iso8601();
    let snapshot = {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        let Some(store) = guard.as_ref() else {
            return Err("数据库不可用".to_string());
        };

        store
            .replace_pool_entries(&build.candidates, &fetched_at)
            .map_err(|e| e.to_string())?;
        for outcome in &build.outcomes {
            store
                .record_source_fetch(
                    &outcome.url,
                    outcome.entry_count,
                    outcome.error.as_deref(),
                    &fetched_at,
                )
                .map_err(|e| e.to_string())?;
        }
        read_snapshot(store, build.duplicates as u32)
    };

    let duration_ms = started.elapsed().as_millis() as u64;
    let failed = build
        .outcomes
        .iter()
        .filter(|outcome| outcome.error.is_some())
        .count();
    log_app(
        state.inner(),
        &app,
        AppLogRecord::new(LogLevel::Info, "sources", "pool.fetch")
            .trace(&trace)
            .duration(duration_ms)
            .params(serde_json::json!({
                "sources": build.outcomes.len(),
                "manual": manual_addresses.lines().count(),
            }))
            .result(serde_json::json!({
                "entries": build.candidates.len(),
                "duplicates": build.duplicates,
                "skipped": build.skipped,
                "failedSources": failed,
            })),
    );
    log_operation(
        state.inner(),
        &app,
        if failed > 0 {
            LogLevel::Warn
        } else {
            LogLevel::Ok
        },
        &trace,
        &format!(
            "抓取完成：解析 {} 条，去重后 {} 条，跳过 {} 行，失败源 {} 个（{}ms）",
            build.candidates.len() + build.duplicates,
            build.candidates.len(),
            build.skipped,
            failed,
            duration_ms
        ),
    );

    snapshot
}

/// 勾选状态落库，下次启动沿用。
#[tauri::command]
fn set_source_checked(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    url: String,
    checked: bool,
) -> Result<(), String> {
    let trace = logs::new_trace_id();
    let started = Instant::now();

    let result = (|| -> Result<(), String> {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        let Some(store) = guard.as_ref() else {
            return Err("数据库不可用".to_string());
        };
        store
            .set_source_checked(&url, checked)
            .map_err(|e| format!("保存优选源勾选状态失败：{e}"))
    })();

    let duration_ms = started.elapsed().as_millis() as u64;
    match &result {
        Ok(()) => {
            log_app(
                state.inner(),
                &app,
                AppLogRecord::new(LogLevel::Info, "sources", "source.checked")
                    .trace(&trace)
                    .duration(duration_ms)
                    .params(serde_json::json!({ "url": url, "checked": checked })),
            );
            log_operation(
                state.inner(),
                &app,
                LogLevel::Info,
                &trace,
                &format!(
                    "优选源已{}：{url}",
                    if checked { "勾选" } else { "取消勾选" }
                ),
            );
        }
        Err(message) => {
            log_app(
                state.inner(),
                &app,
                AppLogRecord::new(LogLevel::Error, "sources", "source.checked")
                    .trace(&trace)
                    .duration(duration_ms)
                    .params(serde_json::json!({ "url": url, "checked": checked }))
                    .error(message, None),
            );
            log_operation(
                state.inner(),
                &app,
                LogLevel::Error,
                &trace,
                &format!("保存优选源勾选状态失败：{message}"),
            );
        }
    }
    result
}

/// 批量勾选/取消勾选（全选、反选），一次事务落库。
#[derive(Debug, serde::Deserialize)]
struct SourceCheck {
    url: String,
    checked: bool,
}

#[tauri::command]
fn set_source_checks(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    checks: Vec<SourceCheck>,
) -> Result<(), String> {
    let trace = logs::new_trace_id();
    let started = Instant::now();
    let total = checks.len();
    let selected = checks.iter().filter(|check| check.checked).count();
    let pairs: Vec<(String, bool)> = checks
        .into_iter()
        .map(|check| (check.url, check.checked))
        .collect();

    let result = (|| -> Result<(), String> {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        let Some(store) = guard.as_ref() else {
            return Err("数据库不可用".to_string());
        };
        store
            .set_source_checks(&pairs)
            .map_err(|e| format!("批量保存优选源勾选状态失败：{e}"))
    })();

    let duration_ms = started.elapsed().as_millis() as u64;
    match &result {
        Ok(()) => {
            log_app(
                state.inner(),
                &app,
                AppLogRecord::new(LogLevel::Info, "sources", "source.checks")
                    .trace(&trace)
                    .duration(duration_ms)
                    .params(serde_json::json!({ "total": total, "selected": selected })),
            );
            log_operation(
                state.inner(),
                &app,
                LogLevel::Info,
                &trace,
                &format!("批量更新优选源勾选：{selected} / {total} 个勾选"),
            );
        }
        Err(message) => {
            log_app(
                state.inner(),
                &app,
                AppLogRecord::new(LogLevel::Error, "sources", "source.checks")
                    .trace(&trace)
                    .duration(duration_ms)
                    .params(serde_json::json!({ "total": total, "selected": selected }))
                    .error(message, None),
            );
            log_operation(
                state.inner(),
                &app,
                LogLevel::Error,
                &trace,
                &format!("批量更新优选源勾选失败：{message}"),
            );
        }
    }
    result
}

/// 导出结果：取消时 `cancelled` 为真，不写文件也不算失败。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportSummary {
    cancelled: bool,
    path: Option<String>,
    rows: usize,
    missing_geo: usize,
}

/// 把当前范围导出成客户端可直接使用的 txt。
///
/// 范围由前端给：有勾选行时传勾选行，否则传当前筛选后的可见行；
/// 行顺序就是表格当前排序。文件通过系统保存对话框选位置，
/// UTF-8 无 BOM、LF 换行，导出动作写操作日志与应用日志。
#[tauri::command]
async fn export_txt(
    app: tauri::AppHandle,
    rows: Vec<export::ExportRow>,
    selected_ids: Vec<String>,
    visible_ids: Vec<String>,
) -> Result<ExportSummary, String> {
    let trace = logs::new_trace_id();
    let started = Instant::now();
    let scope = if selected_ids.is_empty() {
        "visible"
    } else {
        "selected"
    };

    let chosen = export::select_rows(&rows, &selected_ids, &visible_ids);
    if chosen.is_empty() {
        let message = "没有可导出的地址".to_string();
        log_operation(
            app.state::<AppState>().inner(),
            &app,
            LogLevel::Warn,
            &trace,
            &message,
        );
        log_app(
            app.state::<AppState>().inner(),
            &app,
            AppLogRecord::new(LogLevel::Warn, "export", "export.write")
                .trace(&trace)
                .params(serde_json::json!({
                    "scope": scope,
                    "rows": 0,
                    "selected": selected_ids.len(),
                    "visible": visible_ids.len(),
                }))
                .error(&message, None),
        );
        return Err(message);
    }

    let content = export::render(&chosen);
    let missing_geo = export::missing_geo(&chosen);
    let default_name = export::file_name(chrono::Local::now());

    // 保存对话框要阻塞等待用户选择，必须离开主线程，否则事件循环被卡住。
    let dialog_app = app.clone();
    let dialog_name = default_name.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        dialog_app
            .dialog()
            .file()
            .set_file_name(&dialog_name)
            .add_filter("文本文件", &["txt"])
            .blocking_save_file()
    })
    .await
    .map_err(|e| format!("打开保存对话框失败：{e}"))?;

    let Some(file_path) = picked else {
        // 用户取消：不写文件，也不记成失败。
        log_app(
            app.state::<AppState>().inner(),
            &app,
            AppLogRecord::new(LogLevel::Info, "export", "export.write")
                .trace(&trace)
                .duration(started.elapsed().as_millis() as u64)
                .params(serde_json::json!({
                    "scope": scope,
                    "rows": chosen.len(),
                    "defaultName": default_name,
                }))
                .result(serde_json::json!({ "cancelled": true })),
        );
        return Ok(ExportSummary {
            cancelled: true,
            path: None,
            rows: 0,
            missing_geo: 0,
        });
    };

    let path = file_path
        .into_path()
        .map_err(|e| format!("无法解析保存路径：{e}"))?;
    if let Err(error) = export::write_file(&path, &content) {
        let message = format!("写入 {} 失败：{error}", path.display());
        log_app(
            app.state::<AppState>().inner(),
            &app,
            AppLogRecord::new(LogLevel::Error, "export", "export.write")
                .trace(&trace)
                .duration(started.elapsed().as_millis() as u64)
                .params(serde_json::json!({
                    "scope": scope,
                    "rows": chosen.len(),
                    "file": path.display().to_string(),
                }))
                .error(&message, None),
        );
        log_operation(
            app.state::<AppState>().inner(),
            &app,
            LogLevel::Error,
            &trace,
            &message,
        );
        return Err(message);
    }

    let duration_ms = started.elapsed().as_millis() as u64;
    let path_text = path.display().to_string();
    log_app(
        app.state::<AppState>().inner(),
        &app,
        AppLogRecord::new(LogLevel::Info, "export", "export.write")
            .trace(&trace)
            .span(&logs::new_span_id())
            .duration(duration_ms)
            .params(serde_json::json!({
                "scope": scope,
                "rows": chosen.len(),
                "missingGeo": missing_geo,
                "file": path_text,
            }))
            .result(serde_json::json!({
                "bytes": content.len(),
                "cancelled": false,
            })),
    );
    log_operation(
        app.state::<AppState>().inner(),
        &app,
        LogLevel::Ok,
        &trace,
        &format!(
            "已导出 {} 条到 {}（缺少归属地 {} 条，{}ms）",
            chosen.len(),
            path_text,
            missing_geo,
            duration_ms
        ),
    );

    Ok(ExportSummary {
        cancelled: false,
        path: Some(path_text),
        rows: chosen.len(),
        missing_geo,
    })
}

/// 启动时读回三类日志各最近若干行。
#[tauri::command]
fn get_logs(state: tauri::State<'_, AppState>, limit: Option<usize>) -> LogsSnapshot {
    let limit = limit.unwrap_or(LOG_READ_LIMIT).clamp(1, 2000);
    state.logs.read_all(limit)
}

/// 清理某类日志：删除磁盘文件，并写一条「已清理 N 条 / M 个文件」到应用日志。
#[tauri::command]
fn clear_logs(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    kind: String,
) -> Result<serde_json::Value, String> {
    let kind = LogKind::parse(&kind).ok_or_else(|| format!("未知的日志类型：{kind}"))?;
    let report = state.logs.clear(kind).map_err(|e| e.to_string())?;

    // 把刚写入的「已清理」记录推给界面，应用日志表立刻能看到。
    if let Some(entry) = state.logs.read_tail(LogKind::App, 1).last() {
        emit_log(&app, entry);
    }

    Ok(serde_json::json!({
        "entries": report.entries,
        "files": report.files,
    }))
}

/// 每日本地零点跑一次过期清理；保留天数取当前设置。
fn spawn_daily_log_cleanup(app: tauri::AppHandle) {
    std::thread::spawn(move || loop {
        let delay = logs::duration_until_next_local_midnight(chrono::Local::now());
        std::thread::sleep(delay);
        let state = app.state::<AppState>();
        let retention = state.retention();
        for (_, _, entry) in state.logs.cleanup_expired(&retention, SystemTime::now()) {
            if let Some(entry) = entry {
                emit_log(&app, &entry);
            }
        }
    });
}

/// 数据目录名：Windows 上落在 `%APPDATA%\vpm-search`。
const DATA_DIR_NAME: &str = "vpm-search";

/// 解析数据目录：Windows 用 `%APPDATA%\vpm-search`（spec 要求，与 identifier 无关），
/// 其他平台回落到 Tauri 的 `app_data_dir()`。
fn data_dir_for(appdata: Option<&Path>, app_data_dir: Option<&Path>) -> PathBuf {
    match appdata {
        Some(base) => base.join(DATA_DIR_NAME),
        None => app_data_dir
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(".")),
    }
}

/// 旧版数据库（`%APPDATA%\com.asonnychen.vpm-search\data.db`）只迁移一次。
///
/// 新库已存在时不动它；旧库不存在时什么也不做。复制而不是改名，
/// 迁移失败或回滚旧版本时旧数据仍然可用。
fn migrate_legacy_db(new_db: &Path, legacy_db: &Path) -> Result<bool, String> {
    if new_db.exists() || !legacy_db.is_file() {
        return Ok(false);
    }
    if let Some(parent) = new_db.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建数据目录 {} 失败：{e}", parent.display()))?;
    }
    std::fs::copy(legacy_db, new_db).map_err(|e| {
        format!(
            "迁移旧数据库 {} → {} 失败：{e}",
            legacy_db.display(),
            new_db.display()
        )
    })?;
    Ok(true)
}

/// 单实例插件必须是第一个注册的插件，否则第二个实例可能先创建窗口。
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // 第二次启动只聚焦已有窗口，不再新建窗口。
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // 旧路径（identifier 目录）只用于一次性迁移旧数据库。
            let legacy_dir = app.path().app_data_dir().ok();
            // Windows 上是 %APPDATA%\vpm-search，与 spec 的数据库路径一致。
            let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
            let app_dir = data_dir_for(appdata.as_deref(), legacy_dir.as_deref());
            let log_dir = app_dir.join("logs");
            let db_path = app_dir.join("data.db");

            let log_writer = Arc::new(LogWriter::new(log_dir));

            // 旧数据库只在目标不存在时复制一次；失败只记日志，不阻塞启动。
            if let Some(legacy_dir) = legacy_dir.as_deref() {
                let legacy_db = legacy_dir.join("data.db");
                match migrate_legacy_db(&db_path, &legacy_db) {
                    Ok(true) => {
                        let _ = log_writer.write_app(
                            &AppLogRecord::new(LogLevel::Info, "app", "app.db.migrate")
                                .result(serde_json::json!({
                                    "from": legacy_db.display().to_string(),
                                    "to": db_path.display().to_string(),
                                })),
                        );
                    }
                    Ok(false) => {}
                    Err(message) => {
                        let _ = log_writer.write_app(
                            &AppLogRecord::new(LogLevel::Warn, "app", "app.db.migrate")
                                .error(&message, None),
                        );
                        eprintln!("{message}");
                    }
                }
            }

            let store = match Store::open(&db_path) {
                Ok(store) => Some(store),
                Err(e) => {
                    let message = format!("打开 {} 失败：{e}", db_path.display());
                    let _ = log_writer.write_app(
                        &AppLogRecord::new(LogLevel::Error, "app", "db.open")
                            .error(&message, None),
                    );
                    eprintln!("{message}");
                    None
                }
            };

            // 启动时按当前设置清理一次过期日志。
            let retention = store
                .as_ref()
                .and_then(|store| store.load_settings().ok())
                .map(|settings| settings.log_retention)
                .unwrap_or_default();
            let _ = log_writer.cleanup_expired(&retention, SystemTime::now());

            let _ = log_writer.write_operation(
                LogLevel::Info,
                None,
                &format!("应用启动，日志目录 {}", log_writer.dir().display()),
            );
            let _ = log_writer.write_app(
                &AppLogRecord::new(LogLevel::Info, "app", "app.start")
                    .result(serde_json::json!({ "logDir": log_writer.dir().display().to_string() })),
            );

            app.manage(AppState {
                store: Mutex::new(store),
                logs: log_writer,
                geo: Arc::new(geo::GeoResolver::new(geo_candidates(app.handle()))),
                batch: Mutex::new(None),
            });

            spawn_daily_log_cleanup(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            get_pool,
            refresh_source_catalog,
            fetch_pool,
            set_source_checked,
            set_source_checks,
            get_logs,
            clear_logs,
            probe_single,
            probe_batch,
            probe_speed_batch,
            probe_geo_batch,
            probe_cancel,
            export_txt,
            list_test_runs,
            get_test_run_results,
            delete_test_run,
            get_database_size,
            get_address_history
        ])
        .on_window_event(|window, event| {
            // 关闭窗口即结束进程，避免残留后台进程；先收掉日志写入线程。
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                window.app_handle().exit(0);
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod packaging_tests {
    use super::*;

    fn 临时目录(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vpm-search-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    #[test]
    fn 逐行事件负载字段齐全且不带凭据() {
        let row = probe::RowEvent {
            host: "104.16.1.1".to_string(),
            port: 8443,
            status: probe::ProbeStatus::Ok,
            latency_ms: Some(88),
            speed_mbps: Some(12.3),
            country_zh: Some("加拿大".to_string()),
            country_en: Some("Canada".to_string()),
            colo: None,
            error: None,
        };

        let value = serde_json::to_value(RowEventDto {
            run_id: "run-1",
            kind: "delay",
            row: &row,
        })
        .unwrap();

        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "colo",
                "countryEn",
                "countryZh",
                "error",
                "host",
                "kind",
                "latencyMs",
                "port",
                "runId",
                "speedMbps",
                "status",
            ]
        );
        assert_eq!(value["runId"], "run-1");
        assert_eq!(value["kind"], "delay");
        assert_eq!(value["status"], "ok");
        assert_eq!(value["latencyMs"], 88);
        assert!(value["error"].is_null());
        // 事件里只有地址与测量值，没有任何凭据字段。
        let text = value.to_string();
        assert!(!text.contains("uuid"));
        assert!(!text.contains("password"));
    }

    #[test]
    fn windows_数据目录落在_appdata_下的_vpm_search() {
        let base = PathBuf::from(r"C:\Users\tester\AppData\Roaming");
        assert_eq!(
            data_dir_for(Some(&base), Some(Path::new(r"C:\ignored"))),
            base.join("vpm-search")
        );
        // 没有 APPDATA 时回落到 Tauri 给的目录。
        let fallback = PathBuf::from(r"C:\Users\tester\AppData\Roaming\com.asonnychen.vpm-search");
        assert_eq!(
            data_dir_for(None, Some(&fallback)),
            fallback,
            "非 Windows 或缺少 APPDATA 时保留 Tauri 默认目录"
        );
    }

    #[test]
    fn 旧数据库只迁移一次且不覆盖新库() {
        let dir = 临时目录("db-migrate");
        let legacy_dir = dir.join("legacy");
        let new_dir = dir.join("vpm-search");
        std::fs::create_dir_all(&legacy_dir).expect("建旧目录");
        std::fs::create_dir_all(&new_dir).expect("建新目录");
        let legacy_db = legacy_dir.join("data.db");
        let new_db = new_dir.join("data.db");
        std::fs::write(&legacy_db, b"old").expect("写旧库");

        assert!(migrate_legacy_db(&new_db, &legacy_db).expect("首次迁移"));
        assert_eq!(std::fs::read(&new_db).expect("读新库"), b"old");

        // 新库已存在：不再复制，也不覆盖已有数据。
        std::fs::write(&new_db, b"new").expect("写新库");
        assert!(!migrate_legacy_db(&new_db, &legacy_db).expect("再次迁移"));
        assert_eq!(std::fs::read(&new_db).expect("读新库"), b"new");

        // 旧库不存在：什么也不做。
        let missing = legacy_dir.join("missing.db");
        let target = dir.join("other").join("data.db");
        assert!(!migrate_legacy_db(&target, &missing).expect("无旧库"));
        assert!(!target.exists());
        std::fs::remove_dir_all(&dir).expect("清临时目录");
    }
}
