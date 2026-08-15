#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod browser;
mod commands;
mod config;
mod keepalive;
mod logger;
mod report_server;
mod selftest;
mod state;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI32};
use std::sync::Mutex;

use tauri::Manager;

pub struct AppState {
    /// 本地回环上报服务端口（页面内保活脚本通过它回报状态）
    pub port: Mutex<u16>,
    /// 持久化配置
    pub config: Mutex<config::AppConfig>,
    /// 每个帐号槽位的运行状态
    pub states: Mutex<HashMap<u32, state::SlotState>>,
    /// 每个槽位的原生看门狗任务（窗口隐藏时由 Rust 侧驱动页面 tick）
    pub watchdogs: Mutex<HashMap<u32, tauri::async_runtime::JoinHandle<()>>>,
    /// 全局热键 id -> 槽位（0 = Ctrl+U 地址栏）
    pub shortcut_ids: Mutex<HashMap<u32, u32>>,
    /// 当前聚焦的浏览器窗口槽位（Ctrl+U 地址栏作用目标）
    pub focused: Mutex<u32>,
    /// 每个云手机窗口独立的托盘图标（还原原版：每个 webForm 各建一个 win.util.tray）
    pub trays: Mutex<HashMap<u32, tauri::tray::TrayIcon>>,
    /// 硬杀兜底的退出码（默认 0；selftest 置 98 以区分「靠兜底才退掉」）
    pub force_exit_code: AtomicI32,
    /// 正在退出：置位后跳过收尾动作，避免退出流程被卡死
    pub quitting: AtomicBool,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            port: Mutex::new(0),
            config: Mutex::new(config::AppConfig::default()),
            states: Mutex::new(HashMap::new()),
            watchdogs: Mutex::new(HashMap::new()),
            shortcut_ids: Mutex::new(HashMap::new()),
            focused: Mutex::new(0),
            trays: Mutex::new(HashMap::new()),
            force_exit_code: AtomicI32::new(0),
            quitting: AtomicBool::new(false),
        }
    }
}

/// panic 记录到 exe 目录 logs/panic.log，避免静默闪退无法排查
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = format!(
            "[{}] PANIC: {info}\n",
            chrono_like_now()
        );
        let dir = config::base_dir().join("logs");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("panic.log"), &msg);
        default_hook(info);
    }));
}

fn chrono_like_now() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
        + 8 * 3600 * 1000;
    let rem = ms.rem_euclid(86_400_000);
    let (h, m, s) = (rem / 3_600_000, rem % 3_600_000 / 60_000, rem % 60_000 / 1000);
    let days = ms.div_euclid(86_400_000);
    format!("day{days} {h:02}:{m:02}:{s:02}")
}

fn main() {
    install_panic_hook();

    tauri::Builder::default()
        .manage(AppState::new())
        // 单实例必须最先注册：二次启动直接聚焦已有实例的设置窗口并退出，
        // 防止双实例争抢同一 data 目录（WebView2 用户数据目录锁死 → 窗口创建失败）
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("login") {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(browser::shortcut_handler)
                .build(),
        )
        .on_menu_event(|app, event| browser::handle_menu_event(app, event.id().as_ref()))
        .invoke_handler(tauri::generate_handler![
            commands::get_slot,
            commands::launch_slot,
            commands::get_running,
            commands::get_slot_size,
            commands::set_slot_size
        ])
        .setup(|app| {
            // 启动即写日志：版本 / exe 目录 / 门户，保证 logs/ 目录与文件一定生成（可诊断性）
            logger::log(
                app.handle(),
                0,
                "sys",
                &format!(
                    "程序启动 v{} exe目录={}",
                    app.package_info().version,
                    config::base_dir().display()
                ),
            );

            // 载入配置（exe 目录下的 config.json，便携化）
            let cfg = config::load();
            {
                let state: tauri::State<AppState> = app.state();
                *state.config.lock().unwrap() = cfg;
            }
            logger::log(app.handle(), 0, "sys", "配置已载入（config.json）");

            // 启动本地回环上报服务（页面内脚本回传保活状态）
            report_server::spawn(app.handle().clone());

            // 托盘不再全局创建：还原原版语义——每个云手机窗口启动时各建自己的托盘
            // （见 browser.rs create_slot_tray）

            // --selftest：CI 真实 Windows 环境自动化验证（窗口创建/页面加载/退出）
            if selftest::enabled() {
                selftest::spawn(app.handle().clone());
            }

            // 设置窗口已创建 = WebView2 运行时可用（它本身就是一个 WebView2 窗口）
            logger::log(app.handle(), 0, "sys", "设置窗口已创建，WebView2 运行时正常");

            // 设置窗口关闭语义（还原原版 login.aardio onClose → win.quitMessage）：
            // 点 X 即退出整个程序
            if let Some(login) = app.get_webview_window("login") {
                let win = login.clone();
                login.on_window_event(move |e| {
                    if let tauri::WindowEvent::CloseRequested { .. } = e {
                        let app = win.app_handle().clone();
                        browser::quit_all(&app);
                    }
                });
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
