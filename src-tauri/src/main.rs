// 无条件 Windows GUI 子系统：debug 构建也不弹控制台（日志始终写 logs/ 目录；
// 需要终端实时看日志时加 --console 或设 CPK_CONSOLE=1 显式附加）
#![cfg_attr(windows, windows_subsystem = "windows")]

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

/// panic 记录到数据根目录 panic-p{pid}.log（带进程号，多开互不覆盖），
/// 避免静默闪退无法排查
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = format!(
            "[pid={}] [{}] PANIC: {info}\n",
            std::process::id(),
            chrono_like_now()
        );
        let dir = config::base_dir();
        let _ = std::fs::write(dir.join(format!("panic-p{}.log", std::process::id())), &msg);
        default_hook(info);
    }));
}

/// 清扫残留的 WebView2 僵尸进程：只终止「命令行里带本程序 data 目录路径」的
/// msedgewebview2.exe（上次程序异常退出留下的残留进程会锁住用户数据目录，
/// 导致新窗口创建失败报 0x800700AA / 0x8007139F）。按命令行精确匹配，
/// 绝不误杀其他应用（微信/IDE 等）的 WebView2 进程。
///
/// 【多开保护】若检测到本程序还有其它实例在运行（同名 exe 进程数 > 1），
/// 锁定的数据目录很可能属于那个活实例——此时【跳过清扫】返回 false，
/// 调用方直接换新数据目录兜底，绝不把另一个实例的窗口杀掉。
/// 返回 true = 已执行清扫（可以原地重试）。
#[cfg(windows)]
pub fn kill_zombie_webview2(app: &tauri::AppHandle, dir: &std::path::Path) -> bool {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    if other_instances_alive() {
        logger::log(
            app,
            0,
            "sys",
            "检测到本程序还有其它实例在运行：数据目录被占用时不清扫进程（避免误杀另一实例的窗口），将改用新数据目录",
        );
        return false;
    }

    // 标记 = 被锁的数据目录本身（按帐号目录精确匹配，同实例其它帐号的活窗口不受影响）
    // + 旧版 exe/data 目录（覆盖从旧版升级后残留进程仍锁着旧目录的场景）
    let marker_new = dir.to_string_lossy().to_string();
    let marker_legacy = config::exe_dir().join("data").to_string_lossy().to_string();
    // PowerShell 单引号字符串内的单引号需翻倍转义（路径含引号时仍安全）
    let ps_new = marker_new.replace('\'', "''");
    let ps_legacy = marker_legacy.replace('\'', "''");
    let script = format!(
        "$m1='{ps_new}'.ToLower(); $m2='{ps_legacy}'.ToLower(); Get-CimInstance Win32_Process -Filter \"Name='msedgewebview2.exe'\" | \
         Where-Object {{ $_.CommandLine -and ($_.CommandLine.ToLower().Contains($m1) -or $_.CommandLine.ToLower().Contains($m2)) }} | \
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
            true
        }
        Err(e) => {
            logger::log(app, 0, "error", &format!("残留进程清扫失败（PowerShell 调用出错，将继续尝试创建窗口）：{e}"));
            true // 清扫本身失败不是多实例占用，仍允许原地重试一次
        }
    }
}

#[cfg(not(windows))]
pub fn kill_zombie_webview2(_app: &tauri::AppHandle, _dir: &std::path::Path) -> bool {
    true
}

/// 是否还有其它同名进程实例在运行（多开检测，用于清扫保护）。
/// 非平台失败时按「无其它实例」处理，不影响主流程。
#[cfg(windows)]
fn other_instances_alive() -> bool {
    let name = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_string()))
        .unwrap_or_default();
    if name.is_empty() {
        return false;
    }
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = format!(
        "@(Get-Process -Name '{name}' -ErrorAction SilentlyContinue).Count"
    );
    Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<u32>()
                .map(|n| n > 1)
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn other_instances_alive() -> bool {
    false
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

/// WebView2 特性开关调优清单（--disable-features= 后面的部分）：
/// 官方文档：普通开关重复出现只认最后一个，但 --enable-features/--disable-features
/// 例外——多处出现按【并集】合并（此清单与 wry 默认传的 disable-features 自动合并，
/// 互不覆盖）。清单里仍重复带上 wry 默认的三项，纯防御：个别运行时版本若退化成
/// last-wins 也不丢 wry 原有关闭项：
/// - msWebOOUI,msPdfOOUI,msSmartScreenProtection：wry 默认就传的三项
/// - CalculateNativeWinOcclusion：关闭 Chromium 原生窗口遮挡检测。默认开启时窗口被
///   其他窗口盖住会被当作后台而限流渲染器（有 Tauri 应用实测被误判后近乎停转）；
///   本程序云手机画面每秒都在传输，遮挡限流直接伤保活，关闭后盖住照常跑
/// - Translate / AutofillServerCommunication / OptimizationHints /
///   InterestFeedContentSuggestions：Edge 的翻译提示、表单自动填充云端上报、
///   优化指南预取、资讯流推荐——页面脚本与视频流不经过这些服务，纯省待命开销
/// - HardwareMediaKeyHandling / MediaSessionService：系统媒体键钩子与「正在播放」
///   系统集成。视频流走 WebRTC/WebSocket 传输，与此无关
/// - msEdgeBackgroundProcessing：Edge 后台维护任务
#[cfg(windows)]
const WEBVIEW2_TUNED_FEATURES: &str = "msWebOOUI,msPdfOOUI,msSmartScreenProtection,CalculateNativeWinOcclusion,Translate,AutofillServerCommunication,OptimizationHints,InterestFeedContentSuggestions,HardwareMediaKeyHandling,MediaSessionService,msEdgeBackgroundProcessing";

/// 启动早期（tauri::Builder 构造前）设置 WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS。
/// WebView2 加载器把它与内置参数合并（不整体替换），本程序全部窗口生效。
///
/// 用户/外部工具已设置该变量时【原样尊重不覆盖】——高级用户可能有自己的调优。
/// 诊断日志开关（CPK_WEBLOG / CPK_NETLOG）也在此一并拼入，语义与旧版一致：
/// WebView2/Chromium 内部日志默认不开——--enable-logging 有官方缺陷
/// （WebView2Feedback#2195）会在 Windows 弹控制台黑窗。
/// 返回 Some(实际写入的参数) 供启动日志留痕；None = 检测到外部已设置，未动。
#[cfg(windows)]
fn apply_webview2_tuning() -> Option<String> {
    const ENV: &str = "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS";
    if std::env::var_os(ENV).is_some() {
        return None;
    }
    let mut args = format!("--disable-features={WEBVIEW2_TUNED_FEATURES}");
    if std::env::var_os("CPK_WEBLOG").is_some() {
        args.push_str(" --enable-logging --v=1");
        if std::env::var_os("CPK_NETLOG").is_some() {
            args.push_str(" --log-net-log");
        }
    }
    std::env::set_var(ENV, &args);
    Some(args)
}

#[cfg(not(windows))]
fn apply_webview2_tuning() -> Option<String> {
    None
}

fn main() {
    install_panic_hook();
    attach_parent_console();
    // 必须在 tauri::Builder（首个 WebView2 环境创建）之前生效
    let webview2_args = apply_webview2_tuning();

    tauri::Builder::default()
        .manage(AppState::new())
        // 【多开支持】不再注册单实例插件：用户会同时启动多个程序（不同目录/不同帐号），
        // 单实例锁会把第二次启动直接聚焦旧实例并退出，多开根本起不来。
        // 数据目录冲突改由两道兜底保护：其它实例存活时跳过进程清扫（main.rs
        // other_instances_alive），目录被锁时自动换 -r2 新目录（browser.rs）
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
        // setup 闭包要求 'static：move 捕获 webview2_args（启动日志留痕用）
        .setup(move |app| {
            // 启动即写日志：版本 / pid / 数据根目录与 exe 目录（保证数据目录与日志文件一定生成）
            logger::log(
                app.handle(),
                0,
                "sys",
                &format!(
                    "程序启动 v{} pid={} 数据根目录={} exe目录={}",
                    app.package_info().version,
                    std::process::id(),
                    config::base_dir().display(),
                    config::exe_dir().display()
                ),
            );

            // 载入配置（AppData\LocalLow\CloudPhoneKeep\config.json；旧版 exe 目录配置自动迁移）
            let cfg = config::load();
            // 登记各帐号日志目录（= 数据目录），此后帐号级日志各自落盘
            for s in cfg.slots.iter() {
                logger::register_slot_dir(s.slot, config::profile_dir(s.slot, &s.name));
            }
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
            // WebView2 启动参数留痕（apply_webview2_tuning 在 main 早期已生效；仅 Windows）
            #[cfg(windows)]
            match webview2_args.as_deref() {
                Some(a) => logger::log(
                    app.handle(),
                    0,
                    "sys",
                    &format!(
                        "WebView2 调优参数已应用（关闭遮挡限流 CalculateNativeWinOcclusion + Edge 后台服务）：{a}"
                    ),
                ),
                None => logger::log(
                    app.handle(),
                    0,
                    "sys",
                    "检测到外部已设置 WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS，程序内置调优参数未应用（如需启用请清除该环境变量后重启）",
                ),
            }
            #[cfg(not(windows))]
            let _ = webview2_args;

            // 诊断开关提示（默认全关，绝不弹控制台；详见 apply_webview2_tuning 注释）
            logger::log(
                app.handle(),
                0,
                "debug",
                "诊断开关：--console/CPK_CONSOLE=1 终端镜像日志；CPK_WEBLOG=1 WebView2 内部日志；CPK_NETLOG=1 网络事件全量",
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
