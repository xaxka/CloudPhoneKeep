#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod browser;
mod commands;
mod config;
mod keepalive;
mod logger;
mod report_server;
mod state;

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
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
    /// 托盘句柄（窗口启停后重建菜单）
    pub tray: Mutex<Option<tauri::tray::TrayIcon>>,
    /// 正在退出：置位后不再重建托盘菜单，避免退出流程被卡死
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
            tray: Mutex::new(None),
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
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
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
            commands::stop_slot,
            commands::app_quit
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

            // 托盘（注意：老板键不再在启动时全局注册，改为按窗口注册，
            // 多实例/二次启动时不会因热键冲突而崩溃）
            browser::create_tray(app.handle())?;
            logger::log(app.handle(), 0, "sys", "托盘已创建");

            // 设置窗口已创建 = WebView2 运行时可用（它本身就是一个 WebView2 窗口）
            logger::log(app.handle(), 0, "sys", "设置窗口已创建，WebView2 运行时正常");

            // 设置窗口关闭语义：
            //   有云手机窗口在运行 → 弹窗让用户选择「退出程序」或「隐藏到托盘继续保活」
            //   没有任何窗口      → 直接退出
            if let Some(login) = app.get_webview_window("login") {
                let win = login.clone();
                login.on_window_event(move |e| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = e {
                        let app = win.app_handle().clone();
                        let running: Vec<u32> = {
                            let state: tauri::State<AppState> = app.state();
                            let states = state.states.lock().unwrap();
                            let mut v: Vec<u32> = states
                                .iter()
                                .filter(|(_, s)| s.running)
                                .map(|(k, _)| *k)
                                .collect();
                            v.sort();
                            v
                        };
                        if running.is_empty() {
                            return; // 不阻止关闭 → 全部窗口关闭 → 进程退出
                        }
                        api.prevent_close();
                        use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
                        let ok_app = app.clone();
                        let ok_win = win.clone();
                        app.dialog()
                            .message(format!(
                                "还有 {} 个云手机窗口正在保活（索引：{}）。\n\n确定 = 退出程序并停止全部保活\n取消 = 隐藏到托盘继续保活",
                                running.len(),
                                running
                                    .iter()
                                    .map(|n| n.to_string())
                                    .collect::<Vec<_>>()
                                    .join("、")
                            ))
                            .title("退出 云手机保活？")
                            .buttons(MessageDialogButtons::OkCancelCustom(
                                "退出程序".into(),
                                "隐藏到托盘".into(),
                            ))
                            .show(move |ok| {
                                if ok {
                                    browser::quit_all(&ok_app);
                                } else {
                                    let _ = ok_win.hide();
                                }
                            });
                    }
                });
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
