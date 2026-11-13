mod geo;
mod logs;
mod probe;
mod settings;
mod sources;
mod store;
mod template;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use serde::Serialize;
use settings::Settings;
use sources::fetch::{self, CATALOG_URL};
use sources::parse::{parse_catalog, source_label, CatalogSource};
use store::{PoolEntryRow, PoolSourceRow, Store, TestRunSummaryRow};
use tauri::{Emitter, Manager};

use logs::{AppLogRecord, LogEntry, LogKind, LogLevel, LogWriter, LogsSnapshot, SingboxLog};
use probe::Prober;

/// 启动时与界面初次加载读回的日志行数上限。
const LOG_READ_LIMIT: usize = 500;

/// 随包内置的 sing-box 可执行文件名（Tauri externalBin 按目标三元组命名）。
const SINGBOX_BINARY: &str = "sing-box-x86_64-pc-windows-msvc.exe";

/// 应用级共享状态：数据库连接、三类日志写入器与 sing-box 日志通道。
pub struct AppState {
    /// 打开失败时为 `None`，此时命令返回错误但界面照常可用。
    store: Mutex<Option<Store>>,
    logs: Arc<LogWriter>,
    /// sing-box 子进程输出统一从这里落盘。
    singbox: Arc<SingboxLog>,
    /// 临时配置目录，测完即删。
    temp_dir: PathBuf,
    /// 归属地查询器：离线库优先、在线兜底、按出口 IP 缓存。
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
    settings: Settings,
) -> Result<(), String> {
    let trace = logs::new_trace_id();
    let started = Instant::now();

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
                            "singboxDays": settings.log_retention.singbox_days,
                        },
                        "templates": settings.node_templates.len(),
                        "activeTemplate": settings
                            .node_templates
                            .iter()
                            .find(|template| template.id == settings.active_template_id)
                            .map(|template| template.name.clone()),
                    })),
            );

            // 模板的新增 / 重命名 / 删除 / 切换各留一条操作日志，只记名称与数量。
            let templates_changed = previous
                .as_ref()
                .is_none_or(|old| old.node_templates != settings.node_templates);
            if templates_changed {
                log_operation(
                    state.inner(),
                    &app,
                    LogLevel::Info,
                    &trace,
                    &format!("节点模板已更新：共 {} 条", settings.node_templates.len()),
                );
            }
            let active_changed = previous
                .as_ref()
                .is_none_or(|old| old.active_template_id != settings.active_template_id);
            if active_changed {
                let name = settings
                    .node_templates
                    .iter()
                    .find(|template| template.id == settings.active_template_id)
                    .map(|template| template.name.as_str())
                    .unwrap_or("无");
                log_operation(
                    state.inner(),
                    &app,
                    LogLevel::Info,
                    &trace,
                    &format!("已切换节点模板：{name}"),
                );
            }

            // 保留天数修改后立即生效；其余设置保存不重复触发清理。
            if previous.as_ref().map(|old| old.log_retention) != Some(settings.log_retention) {
                log_operation(
                    state.inner(),
                    &app,
                    LogLevel::Info,
                    &trace,
                    &format!(
                        "日志保留期已更新：操作 {} 天 / 应用 {} 天 / sing-box {} 天",
                        settings.log_retention.operation_days,
                        settings.log_retention.app_days,
                        settings.log_retention.singbox_days
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

/// 解析一条分享链接成节点模板。失败时返回可读错误，不写入任何东西。
///
/// 链接本身含凭据，绝不写进日志；日志只记协议、名称与脱敏后的摘要。
#[tauri::command]
fn parse_node_link(
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
    link: String,
) -> Result<template::NodeTemplate, String> {
    let trace = logs::new_trace_id();
    let started = Instant::now();
    match template::parse_link(&link) {
        Ok(mut parsed) => {
            parsed.id = format!("tpl-{}", logs::new_span_id());
            let summary = parsed.summary();
            log_app(
                state.inner(),
                &app,
                AppLogRecord::new(LogLevel::Info, "template", "template.parse")
                    .trace(&trace)
                    .duration(started.elapsed().as_millis() as u64)
                    .params(serde_json::json!({
                        "kind": parsed.kind,
                        "transport": parsed.transport,
                        "tls": parsed.tls,
                    }))
                    .result(serde_json::json!({ "template": summary })),
            );
            Ok(parsed)
        }
        Err(error) => {
            let message = error.to_string();
            log_app(
                state.inner(),
                &app,
                AppLogRecord::new(LogLevel::Error, "template", "template.parse")
                    .trace(&trace)
                    .duration(started.elapsed().as_millis() as u64)
                    .error(&message, None),
            );
            log_operation(
                state.inner(),
                &app,
                LogLevel::Error,
                &trace,
                &format!("节点模板解析失败：{message}"),
            );
            Err(message)
        }
    }
}

/// sing-box 可执行文件的候选路径：打包后与主程序同目录 → 资源目录 → 开发期 binaries。
fn singbox_candidates(app: &tauri::AppHandle) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(SINGBOX_BINARY));
        }
    }
    if let Ok(dir) = app.path().resource_dir() {
        candidates.push(dir.join(SINGBOX_BINARY));
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(SINGBOX_BINARY),
    );
    candidates
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

fn resolve_singbox(app: &tauri::AppHandle) -> Result<PathBuf, String> {    probe::singbox::pick_existing(&singbox_candidates(app)).ok_or_else(|| {
        format!(
            "未找到 sing-box（{SINGBOX_BINARY}），请先运行 scripts/fetch-singbox.ps1"
        )
    })
}

/// 真正的探测在阻塞线程里跑，避免占住异步运行时。
fn probe_blocking(
    app: &tauri::AppHandle,
    endpoint: &probe::Endpoint,
    alias: &str,
) -> Result<probe::ProbeOutcome, String> {
    let state = app.state::<AppState>();

    // 只短暂加锁读模板与超时，测试期间不占着数据库。
    let (template, connect_timeout_s) = {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        let Some(store) = guard.as_ref() else {
            return Err("数据库不可用".to_string());
        };
        let settings = store.load_settings().map_err(|e| e.to_string())?;
        let template = settings
            .node_templates
            .iter()
            .find(|template| template.id == settings.active_template_id)
            .or_else(|| settings.node_templates.first())
            .cloned()
            .ok_or_else(|| "还没有节点模板，请先新建一条模板".to_string())?;
        (template, settings.connect_timeout_s)
    };

    let binary = resolve_singbox(app)?;
    let options = probe::ProbeOptions::from_connect_timeout_s(connect_timeout_s);
    let prober = probe::SingBoxProber::new(
        binary,
        state.singbox.clone(),
        state.logs.today_path(LogKind::Singbox),
        state.temp_dir.clone(),
    );

    let started_at = logs::now_iso8601();
    let outcome = prober.measure_latency(endpoint, &template, &options);

    // 结果落库：一条轮次 + 一条结果。写库失败不影响已经测出的结论。
    let run_id = format!("run-{}", logs::new_span_id());
    {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        if let Some(store) = guard.as_ref() {
            probe::persist_single(
                store,
                &run_id,
                &started_at,
                &probe::SingleProbe {
                    endpoint,
                    template: &template,
                    alias,
                    options,
                },
                &outcome,
            )?;
        }
    }

    Ok(outcome)
}

/// 单条真连接延迟：为一个优选地址生成临时 sing-box 配置并测一次。
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

    let result = tauri::async_runtime::spawn_blocking({
        let app = app.clone();
        let endpoint = endpoint.clone();
        let alias = alias.clone();
        move || probe_blocking(&app, &endpoint, &alias)
    })
    .await
    .map_err(|e| format!("测试任务异常结束：{e}"))?;

    let duration_ms = started.elapsed().as_millis() as u64;
    let target = format!("{}:{}", endpoint.host, endpoint.port);
    let state = app.state::<AppState>();

    match &result {
        Ok(outcome) => {
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

/// 一键测试真连接延迟：整池地址按设置里的并发与采样次数跑一轮。
///
/// 停止后已完成的地址保留结论，未开始的地址落库为「未测」。
#[tauri::command]
async fn probe_batch(app: tauri::AppHandle) -> Result<BatchReportDto, String> {
    let run_id = format!("run-{}", logs::new_span_id());
    let state = app.state::<AppState>();

    // 读设置与池：只短暂加锁，测试期间不占着数据库。
    let (template, options, batch, entries) = {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        let Some(store) = guard.as_ref() else {
            return Err("数据库不可用".to_string());
        };
        let settings = store.load_settings().map_err(|e| e.to_string())?;
        let template = settings
            .node_templates
            .iter()
            .find(|template| template.id == settings.active_template_id)
            .or_else(|| settings.node_templates.first())
            .cloned()
            .ok_or_else(|| "还没有节点模板，请先新建一条模板".to_string())?;
        (
            template,
            probe::ProbeOptions::from_connect_timeout_s(settings.connect_timeout_s),
            probe::batch::BatchOptions::new(
                settings.latency_samples as usize,
                settings.concurrency as usize,
            ),
            store.list_pool_entries().map_err(|e| e.to_string())?,
        )
    };

    if entries.is_empty() {
        return Err("池里还没有地址，请先抓取优选源".to_string());
    }

    let binary = resolve_singbox(&app)?;
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
        "kind": template.kind,
        "transport": template.transport,
        "tls": template.tls,
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

    // 槽位池与调度都在阻塞线程里跑，进度通过事件推给界面。
    let app_for_progress = app.clone();
    let run_for_progress = run_id.clone();
    let on_progress = move |progress: probe::batch::BatchProgress| {
        let _ = app_for_progress.emit(
            "probe:progress",
            serde_json::json!({
                "runId": run_for_progress,
                "mode": "latency",
                "completed": progress.completed,
                "total": progress.total,
            }),
        );
    };
    let cancel = control.clone();
    let singbox = state.singbox.clone();
    let log_path = state.logs.today_path(LogKind::Singbox);
    let temp_dir = state.temp_dir.clone();
    let blocking_items = items.clone();
    let blocking_template = template.clone();

    let joined = tauri::async_runtime::spawn_blocking(move || {
        let pool = probe::slot::SlotPool::start(
            binary,
            singbox,
            log_path,
            temp_dir,
            &blocking_items,
            &blocking_template,
            batch.concurrency,
        )?;
        let report = probe::batch::run_batch(
            &pool,
            &blocking_items,
            &blocking_template,
            &options,
            batch,
            &cancel.cancel,
            &on_progress,
        )?;
        // 整轮结束（含停止）后立刻回收全部槽位进程与临时配置。
        drop(pool);
        Ok::<_, String>(report)
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

/// 一键多线程测速：整池地址或指定选中行，经同一代理链路下载并换算 MB/s。
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
    let (template, options, concurrency, entries) = {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        let Some(store) = guard.as_ref() else {
            return Err("数据库不可用".to_string());
        };
        let settings = store.load_settings().map_err(|e| e.to_string())?;
        let template = settings
            .node_templates
            .iter()
            .find(|template| template.id == settings.active_template_id)
            .or_else(|| settings.node_templates.first())
            .cloned()
            .ok_or_else(|| "还没有节点模板，请先新建一条模板".to_string())?;
        (
            template,
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

    let binary = resolve_singbox(&app)?;
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
        "kind": template.kind,
        "transport": template.transport,
        "tls": template.tls,
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

    let app_for_progress = app.clone();
    let run_for_progress = run_id.clone();
    let on_progress = move |progress: probe::batch::BatchProgress| {
        let _ = app_for_progress.emit(
            "probe:progress",
            serde_json::json!({
                "runId": run_for_progress,
                "mode": "speed",
                "completed": progress.completed,
                "total": progress.total,
            }),
        );
    };

    let cancel = control.clone();
    let singbox = state.singbox.clone();
    let log_path = state.logs.today_path(LogKind::Singbox);
    let temp_dir = state.temp_dir.clone();
    let blocking_items = items.clone();
    let blocking_template = template.clone();

    let joined = tauri::async_runtime::spawn_blocking(move || {
        let pool = probe::slot::SlotPool::start(
            binary,
            singbox,
            log_path,
            temp_dir,
            &blocking_items,
            &blocking_template,
            concurrency,
        )?;
        let report = probe::speed::run_speed_batch(
            &pool,
            &blocking_items,
            &options,
            &speed,
            concurrency,
            &cancel.cancel,
            &on_progress,
        )?;
        drop(pool);
        Ok::<_, String>(report)
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
    exit_ip: Option<String>,
    country: Option<String>,
    zh: Option<String>,
    en: Option<String>,
    colo: Option<String>,
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

/// 一键查归属地：整池或选中行，经各自代理链路取出口 IP 后查归属地。
///
/// 结果写进最近一轮 `test_result` 的出口 IP 与归属地列，延迟、速度不受影响；
/// 查询失败的行只留空归属地，不清空已有结论。
#[tauri::command]
async fn probe_geo_batch(
    app: tauri::AppHandle,
    targets: Option<Vec<SpeedTarget>>,
) -> Result<GeoReportDto, String> {
    let state = app.state::<AppState>();

    // 读设置与池：只短暂加锁，查询期间不占着数据库。
    let (template, options, concurrency, entries) = {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        let Some(store) = guard.as_ref() else {
            return Err("数据库不可用".to_string());
        };
        let settings = store.load_settings().map_err(|e| e.to_string())?;
        let template = settings
            .node_templates
            .iter()
            .find(|template| template.id == settings.active_template_id)
            .or_else(|| settings.node_templates.first())
            .cloned()
            .ok_or_else(|| "还没有节点模板，请先新建一条模板".to_string())?;
        (
            template,
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
        return Err("没有可查归属地的地址，请先抓取优选源或选中行".to_string());
    }

    let binary = resolve_singbox(&app)?;

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
        "kind": template.kind,
        "transport": template.transport,
        "tls": template.tls,
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

    // 落库的缓存先灌进内存，同一出口 IP 不重复查询。
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

    let app_for_progress = app.clone();
    let run_for_progress = run_id.clone();
    let on_progress = move |progress: probe::batch::BatchProgress| {
        let _ = app_for_progress.emit(
            "probe:progress",
            serde_json::json!({
                "runId": run_for_progress,
                "mode": "geo",
                "completed": progress.completed,
                "total": progress.total,
            }),
        );
    };

    let cancel = control.clone();
    let singbox = state.singbox.clone();
    let log_path = state.logs.today_path(LogKind::Singbox);
    let temp_dir = state.temp_dir.clone();
    let blocking_items = items.clone();
    let blocking_template = template.clone();
    let resolver = state.geo.clone();

    let joined = tauri::async_runtime::spawn_blocking(move || {
        let pool = probe::slot::SlotPool::start(
            binary,
            singbox,
            log_path,
            temp_dir,
            &blocking_items,
            &blocking_template,
            concurrency,
        )?;
        let report = geo::run_geo_batch(
            &pool,
            &resolver,
            &blocking_items,
            &options,
            concurrency,
            &cancel.cancel,
            &on_progress,
        );
        drop(pool);
        Ok::<_, String>(report)
    })
    .await;

    // 无论成功、失败、停止还是任务异常，都要把这一轮从「正在跑」里摘掉。
    if let Ok(mut guard) = state.batch.lock() {
        *guard = None;
    }

    let report = joined.map_err(|e| format!("归属地查询任务异常结束：{e}"))??;
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
                    "exitIp": info.exit_ip,
                    "country": info.country,
                    "zh": info.zh,
                    "en": info.en,
                    "colo": info.colo,
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
                    exit_ip: info.as_ref().map(|info| info.exit_ip.clone()),
                    country: info.as_ref().map(|info| info.country.clone()),
                    zh: info.as_ref().map(|info| info.zh.clone()),
                    en: info.as_ref().map(|info| info.en.clone()),
                    colo: info.as_ref().and_then(|info| info.colo.clone()),
                    asn: info.as_ref().and_then(|info| info.asn),
                    error: row.error,
                }
            })
            .collect(),
    })
}

/// 请求停止当前批量测试；没有在跑时返回 `false`。
#[tauri::command]
fn probe_cancel(state: tauri::State<'_, AppState>) -> Result<bool, String> {    let guard = state
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
    exit_ip: Option<String>,
    country: Option<String>,
    zh: Option<String>,
    en: Option<String>,
    colo: Option<String>,
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
            exit_ip: row.exit_ip,
            country: row.geo_country,
            zh: row.geo_zh,
            en: row.geo_en,
            colo: row.colo,
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
    url: String,
    checked: bool,
) -> Result<(), String> {
    let guard = state
        .store
        .lock()
        .map_err(|_| "存储锁已损坏".to_string())?;
    let Some(store) = guard.as_ref() else {
        return Err("数据库不可用".to_string());
    };
    store
        .set_source_checked(&url, checked)
        .map_err(|e| e.to_string())
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
        .setup(|app| {
            // Windows 上解析为 %APPDATA%\com.asonnychen.vpm-search。
            let app_dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| PathBuf::from("."));
            let log_dir = app_dir.join("logs");
            let db_path = app_dir.join("data.db");

            let log_writer = Arc::new(LogWriter::new(log_dir));
            let singbox = Arc::new(SingboxLog::spawn(log_writer.clone()));
            let temp_dir = app_dir.join("tmp");

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
                singbox,
                temp_dir,
                geo: Arc::new(geo::GeoResolver::new(geo_candidates(app.handle()))),
                batch: Mutex::new(None),
            });

            spawn_daily_log_cleanup(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            parse_node_link,
            get_pool,
            refresh_source_catalog,
            fetch_pool,
            set_source_checked,
            get_logs,
            clear_logs,
            probe_single,
            probe_batch,
            probe_speed_batch,
            probe_geo_batch,
            probe_cancel,
            list_test_runs,
            get_test_run_results,
            delete_test_run,
            get_database_size,
            get_address_history
        ])
        .on_window_event(|window, event| {
            // 关闭窗口即结束进程，避免残留后台进程；先收掉日志写入线程。
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                if let Some(state) = window.try_state::<AppState>() {
                    state.singbox.shutdown();
                }
                window.app_handle().exit(0);
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
