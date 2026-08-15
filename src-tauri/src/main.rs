#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod browser;
mod commands;
mod config;
mod keepalive;
mod report_server;
mod state;
mod update;

use std::collections::HashMap;
use std::sync::Mutex;

use tauri::{Manager, WindowEvent};

pub struct AppState {
    /// 本地回环上报服务端口（页面内保活脚本通过它回报状态）
    pub port: Mutex<u16>,
    /// 持久化配置
    pub config: Mutex<config::AppConfig>,
    /// 每个帐号槽位的运行状态
    pub states: Mutex<HashMap<u32, state::SlotState>>,
    /// 每个槽位的原生看门狗任务（窗口隐藏时由 Rust 侧驱动页面 tick）
    pub watchdogs: Mutex<HashMap<u32, tauri::async_runtime::JoinHandle<()>>>,
    /// 全局热键 id -> 槽位
    pub shortcut_ids: Mutex<HashMap<u32, u32>>,
    /// 主面板关闭提示只弹一次
    pub main_close_notified: Mutex<bool>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            port: Mutex::new(0),
            config: Mutex::new(config::AppConfig::default()),
            states: Mutex::new(HashMap::new()),
            watchdogs: Mutex::new(HashMap::new()),
            shortcut_ids: Mutex::new(HashMap::new()),
            main_close_notified: Mutex::new(false),
        }
    }
}

fn main() {
    tauri::Builder::default()
        .manage(AppState::new())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(browser::shortcut_handler)
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            commands::get_app_info,
            commands::get_config,
            commands::save_config,
            commands::start_slot,
            commands::stop_slot,
            commands::toggle_slot,
            commands::show_slot,
            commands::topmost_slot,
            commands::rotate_slot,
            commands::home_slot,
            commands::reload_slot,
            commands::get_states,
            commands::check_update,
            commands::open_url,
            commands::app_quit
        ])
        .setup(|app| {
            // 载入配置
            let cfg = config::load(app.handle());
            let auto_start = cfg.settings.auto_start;
            {
                let state: tauri::State<AppState> = app.state();
                *state.config.lock().unwrap() = cfg;
            }

            // 启动本地回环上报服务（页面内脚本回传保活状态）
            report_server::spawn(app.handle().clone());

            // 托盘 + 全局老板键 Ctrl+1..9
            browser::create_tray(app.handle())?;
            browser::register_shortcuts(app.handle())?;

            // 主面板关闭时隐藏到托盘而不是退出
            if let Some(main) = app.get_webview_window("main") {
                let win = main.clone();
                main.on_window_event(move |e| {
                    if let WindowEvent::CloseRequested { api, .. } = e {
                        api.prevent_close();
                        let _ = win.hide();
                    }
                });
            }

            // 可选：启动时自动开启已启用的帐号
            if auto_start {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(1200));
                    let slots: Vec<u32> = {
                        let state: tauri::State<AppState> = handle.state();
                        let cfg = state.config.lock().unwrap();
                        cfg.slots
                            .iter()
                            .filter(|s| s.enabled)
                            .map(|s| s.slot)
                            .collect()
                    };
                    for slot in slots {
                        let _ = browser::start_slot(&handle, slot);
                    }
                });
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
