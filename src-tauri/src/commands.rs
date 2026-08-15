use crate::browser;
use crate::config::SlotConfig;
use crate::AppState;
use tauri::{AppHandle, Manager};

/// 读取某个槽位配置（设置窗口预填）
#[tauri::command]
pub fn get_slot(app: AppHandle, slot: u32) -> Result<SlotConfig, String> {
    let state: tauri::State<AppState> = app.state();
    let cfg = state.config.lock().unwrap();
    let s = cfg
        .slots
        .iter()
        .find(|s| s.slot == slot)
        .cloned()
        .unwrap_or_else(|| {
            let mut d = SlotConfig::default();
            d.slot = slot;
            d
        });
    Ok(s.normalized())
}

/// 设置窗口「进入」：保存配置并启动窗口（还原原版流程）
#[tauri::command]
pub fn launch_slot(app: AppHandle, cfg: SlotConfig) -> Result<(), String> {
    {
        let state: tauri::State<AppState> = app.state();
        let mut app_cfg = state.config.lock().unwrap();
        crate::config::upsert_slot(&mut app_cfg, cfg.clone());
        crate::config::save(&app_cfg)?;
    }
    browser::start_slot(&app, cfg.slot)?;

    // 启动成功后隐藏设置窗口（与原版一致：loginForm.show(false)）
    if let Some(win) = app.get_webview_window("login") {
        let _ = win.hide();
    }
    Ok(())
}

/// 正在运行的槽位列表（设置窗口提示用）
#[tauri::command]
pub fn get_running(app: AppHandle) -> Vec<u32> {
    let state: tauri::State<AppState> = app.state();
    let states = state.states.lock().unwrap();
    let mut v: Vec<u32> = states
        .iter()
        .filter(|(_, s)| s.running)
        .map(|(k, _)| *k)
        .collect();
    v.sort();
    v
}

/// 关闭某个帐号窗口（停止保活）
#[tauri::command]
pub fn stop_slot(app: AppHandle, slot: u32) -> Result<(), String> {
    browser::stop_slot(&app, slot)
}

#[tauri::command]
pub fn app_quit(app: AppHandle) {
    app.exit(0);
}
