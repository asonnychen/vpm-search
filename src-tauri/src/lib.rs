mod logs;
mod settings;
mod sources;
mod store;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use serde::Serialize;
use settings::Settings;
use sources::fetch::{self, CATALOG_URL};
use sources::parse::{parse_catalog, source_label, CatalogSource};
use store::{PoolEntryRow, PoolSourceRow, Store};
use tauri::{Emitter, Manager};

use logs::{AppLogRecord, LogEntry, LogKind, LogLevel, LogWriter, LogsSnapshot, SingboxLog};

/// 启动时与界面初次加载读回的日志行数上限。
const LOG_READ_LIMIT: usize = 500;

/// 应用级共享状态：数据库连接、三类日志写入器与 sing-box 日志通道。
pub struct AppState {
    /// 打开失败时为 `None`，此时命令返回错误但界面照常可用。
    store: Mutex<Option<Store>>,
    logs: Arc<LogWriter>,
    /// 阶段 6 接入 sing-box 进程后把输出推进来。
    #[allow(dead_code)]
    singbox: SingboxLog,
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

    let previous_retention = {
        let guard = state
            .store
            .lock()
            .map_err(|_| "存储锁已损坏".to_string())?;
        guard
            .as_ref()
            .and_then(|store| store.load_settings().ok())
            .map(|settings| settings.log_retention)
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
                        }
                    })),
            );
            // 保留天数修改后立即生效；其余设置保存不重复触发清理。
            if previous_retention != Some(settings.log_retention) {
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
            let singbox = SingboxLog::spawn(log_writer.clone());

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
            get_logs,
            clear_logs
        ])
        .on_window_event(|window, event| {
            // 关闭窗口即结束进程，避免残留后台进程。
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                window.app_handle().exit(0);
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
