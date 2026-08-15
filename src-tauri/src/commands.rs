use crate::browser;
use crate::config::{self, AppConfig};
use crate::state::SlotState;
use crate::update::{self, UpdateInfo};
use crate::AppState;
use std::collections::HashMap;
use tauri::{AppHandle, Manager};

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub version: String,
    pub port: u16,
}

#[tauri::command]
pub fn get_app_info(app: AppHandle) -> AppInfo {
    let state: tauri::State<AppState> = app.state();
    let port = *state.port.lock().unwrap();
    AppInfo {
        version: app.package_info().version.to_string(),
        port,
    }
}

#[tauri::command]
pub fn get_config(app: AppHandle) -> AppConfig {
    let state: tauri::State<AppState> = app.state();
    let cfg = state.config.lock().unwrap().clone();
    cfg
}

/// 保存配置（前端提交完整配置，保存后热更新已打开窗口的保活参数）
#[tauri::command]
pub fn save_config(app: AppHandle, cfg: AppConfig) -> Result<(), String> {
    {
        let state: tauri::State<AppState> = app.state();
        let mut cur = state.config.lock().unwrap();
        // 校验槽位
        if cfg.slots.len() != 9 {
            return Err("配置必须包含 9 个帐号槽位".into());
        }
        *cur = cfg;
        config::save(&app, &cur).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn start_slot(app: AppHandle, slot: u32) -> Result<(), String> {
    browser::start_slot(&app, slot)
}

#[tauri::command]
pub fn stop_slot(app: AppHandle, slot: u32) -> Result<(), String> {
    browser::stop_slot(&app, slot)
}

#[tauri::command]
pub fn toggle_slot(app: AppHandle, slot: u32) {
    browser::toggle_slot(&app, slot)
}

#[tauri::command]
pub fn show_slot(app: AppHandle, slot: u32, visible: bool) {
    browser::show_slot(&app, slot, visible)
}

#[tauri::command]
pub fn topmost_slot(app: AppHandle, slot: u32, top: bool) -> Result<(), String> {
    browser::set_topmost(&app, slot, top)
}

#[tauri::command]
pub fn rotate_slot(app: AppHandle, slot: u32) -> Result<serde_json::Value, String> {
    let (w, h) = browser::rotate_slot(&app, slot)?;
    Ok(serde_json::json!({ "width": w, "height": h }))
}

/// 导航到指定地址（不传 url 则跳转该槽位配置的首页）
#[tauri::command]
pub fn home_slot(app: AppHandle, slot: u32, url: Option<String>) -> Result<(), String> {
    let target = match url {
        Some(u) if !u.trim().is_empty() => u,
        _ => {
            let state: tauri::State<AppState> = app.state();
            let cfg = state.config.lock().unwrap();
            cfg.slots
                .iter()
                .find(|s| s.slot == slot)
                .map(|s| s.web_uri.clone())
                .unwrap_or_default()
        }
    };
    browser::nav_slot(&app, slot, &target)
}

#[tauri::command]
pub fn reload_slot(app: AppHandle, slot: u32) -> Result<(), String> {
    if let Some(win) = app.get_webview_window(&browser::slot_label(slot)) {
        win.eval("try{location.reload()}catch(e){}")
            .map_err(|e| e.to_string())
    } else {
        Err("窗口未启动".into())
    }
}

#[tauri::command]
pub fn get_states(app: AppHandle) -> HashMap<u32, SlotState> {
    let state: tauri::State<AppState> = app.state();
    let states = state.states.lock().unwrap().clone();
    states
}

#[tauri::command]
pub async fn check_update(app: AppHandle) -> UpdateInfo {
    let (settings, current) = {
        let state: tauri::State<AppState> = app.state();
        let cfg = state.config.lock().unwrap();
        (cfg.settings.clone(), app.package_info().version.to_string())
    };
    update::fetch_update(&settings, &current).await
}

#[tauri::command]
pub fn open_url(app: AppHandle, url: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn app_quit(app: AppHandle) {
    app.exit(0);
}

/// 打开日志文件所在目录（并定位当日日志）
#[tauri::command]
pub fn open_log_dir(app: AppHandle) -> Result<String, String> {
    use tauri_plugin_opener::OpenerExt;
    let path = crate::logger::log_file_path(&app);
    if let Some(dir) = path.parent() {
        app.opener()
            .open_path(dir.to_string_lossy().to_string(), None::<&str>)
            .map_err(|e| e.to_string())?;
    }
    Ok(path.to_string_lossy().to_string())
}

/// 对运行中的槽位触发 DOM 采样：页面脚本会把当前全部 class 清单写入日志
#[tauri::command]
pub fn probe_slot(app: AppHandle, slot: u32) -> Result<(), String> {
    if let Some(win) = app.get_webview_window(&browser::slot_label(slot)) {
        win.eval("try{window.__CPK_PROBE__?window.__CPK_PROBE__():'no-script'}catch(e){}")
            .map_err(|e| e.to_string())?;
        crate::logger::log(&app, slot, "sys", "已触发手动 DOM 采样");
        Ok(())
    } else {
        Err("窗口未启动".into())
    }
}
