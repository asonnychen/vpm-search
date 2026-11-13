mod error_log;
mod settings;
mod sources;
mod store;

use std::path::PathBuf;
use std::sync::Mutex;

use serde::Serialize;
use settings::Settings;
use sources::fetch::{self, CATALOG_URL};
use sources::parse::{parse_catalog, source_label, CatalogSource};
use store::{PoolEntryRow, PoolSourceRow, Store};
use tauri::Manager;

/// 应用级共享状态：数据库连接与日志目录。
pub struct AppState {
    /// 打开失败时为 `None`，此时命令返回错误但界面照常可用。
    store: Mutex<Option<Store>>,
    log_dir: PathBuf,
}

fn log_failure(state: &AppState, event: &str, message: &str) {
    // 日志本身失败不影响主流程。
    let _ = error_log::append_error(&state.log_dir, event, message);
}

/// 读取整份设置。失败时返回错误字符串，前端保留默认值。
#[tauri::command]
fn get_settings(state: tauri::State<'_, AppState>) -> Result<Settings, String> {
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

    if let Err(message) = &loaded {
        log_failure(state.inner(), "settings.load", message);
    }
    loaded
}

/// 写入整份设置。只做参数校验与转发。
#[tauri::command]
fn save_settings(state: tauri::State<'_, AppState>, settings: Settings) -> Result<(), String> {
    if let Err(e) = settings.validate() {
        let message = e.to_string();
        log_failure(state.inner(), "settings.validate", &message);
        return Err(message);
    }

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

    if let Err(message) = &saved {
        log_failure(state.inner(), "settings.save", message);
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
fn get_pool(state: tauri::State<'_, AppState>) -> Result<PoolSnapshot, String> {
    let guard = state
        .store
        .lock()
        .map_err(|_| "存储锁已损坏".to_string())?;
    let Some(store) = guard.as_ref() else {
        return Err("数据库不可用".to_string());
    };
    let snapshot = read_snapshot(store, 0);
    if let Err(message) = &snapshot {
        log_failure(state.inner(), "pool.read", message);
    }
    snapshot
}

/// 抓取 bestcf 页面，刷新优选源清单；已有源的勾选与抓取结果保持不变。
#[tauri::command]
async fn refresh_source_catalog(state: tauri::State<'_, AppState>) -> Result<PoolSnapshot, String> {
    let client = fetch::build_client()?;
    let html = fetch::fetch_text(&client, CATALOG_URL)
        .await
        .map_err(|e| {
            let message = format!("抓取优选源清单失败：{e}");
            log_failure(state.inner(), "source.catalog", &message);
            message
        })?;

    let catalog = parse_catalog(&html);
    if catalog.is_empty() {
        let message = "页面里没有解析到任何优选源，可能页面结构已变化".to_string();
        log_failure(state.inner(), "source.catalog.parse", &message);
        return Err(message);
    }

    let guard = state
        .store
        .lock()
        .map_err(|_| "存储锁已损坏".to_string())?;
    let Some(store) = guard.as_ref() else {
        return Err("数据库不可用".to_string());
    };
    store.upsert_catalog(&catalog).map_err(|e| e.to_string())?;
    read_snapshot(store, 0)
}

/// 抓取选中的优选源并把手动地址一起并入池。
///
/// 单源失败只把错误记到该源上，其余源照常解析；全部失败也返回快照，
/// 让界面能逐源标红，而不是整块报错。
#[tauri::command]
async fn fetch_pool(
    state: tauri::State<'_, AppState>,
    source_urls: Vec<String>,
    extra_source_urls: Vec<String>,
    manual_addresses: String,
) -> Result<PoolSnapshot, String> {
    // 粘贴进来的源地址也进源清单，抓取结果与异常才能逐源显示。
    let mut requested: Vec<String> = source_urls;
    for url in extra_source_urls {
        let url = url.trim().to_string();
        if url.is_empty() || requested.contains(&url) {
            continue;
        }
        requested.push(url);
    }

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

    for outcome in &build.outcomes {
        if let Some(error) = &outcome.error {
            log_failure(
                state.inner(),
                "source.fetch",
                &format!("{} 抓取失败：{error}", outcome.url),
            );
        }
    }

    let fetched_at = error_log::now_iso8601();
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

/// 单实例插件必须是第一个注册的插件，否则第二个实例可能先创建窗口。
pub fn run() {    tauri::Builder::default()
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

            let store = match Store::open(&db_path) {
                Ok(store) => Some(store),
                Err(e) => {
                    let message = format!("打开 {} 失败：{e}", db_path.display());
                    let _ = error_log::append_error(&log_dir, "db.open", &message);
                    eprintln!("{message}");
                    None
                }
            };

            app.manage(AppState {
                store: Mutex::new(store),
                log_dir,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            get_pool,
            refresh_source_catalog,
            fetch_pool,
            set_source_checked
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
