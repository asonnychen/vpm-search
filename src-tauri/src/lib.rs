mod error_log;
mod settings;
mod store;

use std::path::PathBuf;
use std::sync::Mutex;

use settings::Settings;
use store::Store;
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
        .invoke_handler(tauri::generate_handler![get_settings, save_settings])
        .on_window_event(|window, event| {
            // 关闭窗口即结束进程，避免残留后台进程。
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                window.app_handle().exit(0);
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
