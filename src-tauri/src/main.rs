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

/// 清扫残留的 WebView2 僵尸进程：只终止「命令行里带本程序 data 目录路径」的
/// msedgewebview2.exe（上次程序异常退出留下的残留进程会锁住用户数据目录，
/// 导致新窗口创建失败报 0x800700AA / 0x8007139F）。按命令行精确匹配，
/// 绝不误杀其他应用（微信/IDE 等）的 WebView2 进程。
#[cfg(windows)]
pub fn kill_zombie_webview2(app: &tauri::AppHandle) {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let marker = config::base_dir().join("data").to_string_lossy().to_string();
    // PowerShell 单引号字符串内的单引号需翻倍转义（路径含引号时仍安全）
    let ps_marker = marker.replace('\'', "''");
    let script = format!(
        "$m='{ps_marker}'.ToLower(); Get-CimInstance Win32_Process -Filter \"Name='msedgewebview2.exe'\" | \
         Where-Object {{ $_.CommandLine -and $_.CommandLine.ToLower().Contains($m) }} | \
         ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue; Write-Output $_.ProcessId }}"
    );

    match Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    {
        Ok(o) => {
            let pids: Vec<String> = String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .map(|s| s.to_string())
                .collect();
            if pids.is_empty() {
                logger::log(app, 0, "debug", "残留进程清扫：未发现占用本程序数据目录的 WebView2 进程");
            } else {
                logger::log(
                    app,
                    0,
                    "debug",
                    &format!("残留进程清扫：已强制结束 {} 个 WebView2 进程（PID: {}）", pids.len(), pids.join(",")),
                );
            }
        }
        Err(e) => {
            logger::log(app, 0, "error", &format!("残留进程清扫失败（PowerShell 调用出错，将继续尝试创建窗口）：{e}"));
        }
    }
}

#[cfg(not(windows))]
pub fn kill_zombie_webview2(_app: &tauri::AppHandle) {}

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

/// 把日志同时输出到启动本程序的终端：加 `--console` 参数或设 CPK_CONSOLE=1 启动。
/// GUI 程序默认不挂控制台；AttachConsole 挂到父终端后，本程序日志（[debug] 等全部级别）
/// 与 WebView2/Chromium 内部日志都会直接打在终端里，实时排查用
#[cfg(windows)]
fn attach_parent_console() {
    let want = std::env::args().any(|a| a.eq_ignore_ascii_case("--console"))
        || std::env::var_os("CPK_CONSOLE").is_some();
    if !want {
        return;
    }
    extern "system" {
        fn AttachConsole(dw_process_id: u32) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    let ok = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } != 0;
    logger::set_console_mirror(true);
    if !ok {
        eprintln!("[cpk] --console：未能附加到终端（双击启动没有父终端），日志仍写 logs/ 目录");
    }
}

#[cfg(not(windows))]
fn attach_parent_console() {}

fn main() {
    install_panic_hook();
    attach_parent_console();

    // WebView2/Chromium 级调试日志：让 msedgewebview2 把内部错误写进各数据目录，
    // 用于定位「窗口开了但页面空白/加载失败」这类 Rust 层完全看不到的问题。
    // 再设环境变量 CPK_NETLOG=1 可额外开启网络事件全量记录（netlog.json，体积大，按需用）
    #[cfg(windows)]
    {
        if std::env::var_os("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS").is_none() {
            let mut args = "--enable-logging --v=1".to_string();
            if std::env::var_os("CPK_NETLOG").is_some() {
                args.push_str(" --log-net-log");
            }
            std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", &args);
        }
    }

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
            commands::debug_log,
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
            #[cfg(windows)]
            logger::log(
                app.handle(),
                0,
                "debug",
                "WebView2 调试日志已开启（--enable-logging --v=1）：Chromium 级错误日志写在各 data/ 目录内；CPK_NETLOG=1 可再记录网络事件 netlog.json；加 --console 启动可把日志实时打到终端",
            );

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
