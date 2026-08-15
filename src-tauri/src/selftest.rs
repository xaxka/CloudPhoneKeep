//! `--selftest`：在真实 Windows + WebView2 环境里自动验证三件事：
//!   1. 云手机窗口能创建（WebView2 环境/数据目录链路）
//!   2. 外部网页能加载（注入脚本存活并通过 127.0.0.1 回环上报 nav 记录）
//!   3. 退出能退干净（quit_all 后 7 秒内进程必须消失，1.5 秒硬杀兜底触发则以 98 区分）
//!
//! 由 CI（windows-latest，自带 WebView2 Evergreen 运行时）运行，结果写入
//! exe目录/logs/selftest.log，退出码：
//!   0=全部通过；20=对照窗口创建失败；21=门户窗口创建失败；
//!   22=启动成功后设置窗口未被隐藏（「点击进入设置不消失」回归）
//!   30=对照页面无加载证据；31=门户页面无加载证据（对照通过说明是站点可达性问题）
//!   97=quit_all 被阻塞；98=退出靠硬杀兜底才完成（仍算退出失败，需修）
use crate::browser;
use crate::config::{self, SlotConfig};
use crate::logger;
use crate::AppState;
use std::io::Write;
use tauri::Manager;

pub const FLAG: &str = "--selftest";

pub fn enabled() -> bool {
    std::env::args().any(|a| a == FLAG)
}

fn out(app: &tauri::AppHandle, line: &str) {
    eprintln!("[selftest] {line}");
    let dir = config::base_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("selftest.log"))
    {
        let _ = writeln!(f, "{line}");
    }
    logger::log(app, 0, "sys", &format!("[selftest] {line}"));
}

/// 读取运行日志（cpk-*.log）全文
fn read_run_log() -> String {
    let dir = config::base_dir().join("logs");
    let mut s = String::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with("cpk-") && name.ends_with(".log") {
                if let Ok(c) = std::fs::read_to_string(e.path()) {
                    s.push_str(&c);
                }
            }
        }
    }
    s
}

/// 页面加载证据：注入脚本上报过 [nav]（任意 URL 变化）或「页面加载正常」
fn page_evidence(slot: u32) -> bool {
    let log = read_run_log();
    log.contains(&format!("[slot={slot}] [nav]"))
        || log.contains(&format!("[slot={slot}] [sys] 页面加载正常"))
        || log.contains(&format!("[slot={slot}] [sys] 保活脚本已注入"))
}

/// 单阶段：创建窗口 → 等待 → 检查日志证据。
/// 返回 Some(错误码) 表示该阶段失败（窗口创建失败会直接退出进程）。
fn phase(
    app: &tauri::AppHandle,
    slot: u32,
    url: &str,
    name: &str,
    wait_secs: u64,
    code_win: i32,
    code_nav: i32,
) -> Option<i32> {
    out(app, &format!("PHASE {name}: 创建窗口 slot={slot} url={url}"));
    {
        let state: tauri::State<AppState> = app.state();
        let mut cfg = state.config.lock().unwrap();
        let mut s = SlotConfig::default();
        s.slot = slot;
        s.name = format!("selftest-{name}");
        s.platform = "mobile".into();
        s.web_uri = url.to_string();
        s.width = 420.0;
        s.height = 700.0;
        s.interval_ms = 2000; // 加快心跳，尽快产生证据
        config::upsert_slot(&mut cfg, s);
    }
    match browser::start_slot_ex(app, slot) {
        Ok(warnings) => {
            out(app, &format!("PHASE {name}: 窗口已创建 warnings={warnings:?}"));
        }
        Err(e) => {
            out(app, &format!("PHASE {name}: 窗口创建失败: {e}"));
            dump_tail(app);
            out(app, &format!("VERDICT: FAIL（{name} 窗口创建失败）"));
            std::process::exit(code_win);
        }
    }
    std::thread::sleep(std::time::Duration::from_secs(wait_secs));
    if page_evidence(slot) {
        out(app, &format!("PHASE {name}: 页面加载证据 OK"));
        None
    } else {
        out(app, &format!("PHASE {name}: 未发现页面加载证据（无 nav/注入记录）"));
        Some(code_nav)
    }
}

fn dump_tail(app: &tauri::AppHandle) {
    out(app, "----- 运行日志尾部 60 行 -----");
    let content = read_run_log();
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(60);
    for l in &lines[start..] {
        out(app, l);
    }
}

pub fn spawn(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        // 等待 setup 完成、回环服务就绪
        std::thread::sleep(std::time::Duration::from_secs(3));
        out(&app, "START：验证窗口创建 / 外部页面加载 / 退出，三段");

        // 阶段1：对照页（example.com，排除门户站点本身的可达性因素）
        let r1 = phase(&app, 9, "https://example.com/", "control", 15, 20, 30);

        // 进入成功后设置窗口必须被隐藏（回归验证：用户反馈「点击进入设置窗口不消失」）
        let login_visible = app
            .get_webview_window("login")
            .map(|w| w.is_visible().unwrap_or(true))
            .unwrap_or(false);
        if login_visible {
            out(&app, "设置窗口未被隐藏（进入成功后仍可见）");
            dump_tail(&app);
            out(&app, "VERDICT: FAIL（设置窗口未隐藏，码 22）");
            std::process::exit(22);
        }
        out(&app, "设置窗口已正确隐藏");

        // 阶段2：真实门户（移动云手机 H5）
        let r2 = phase(&app, 1, config::MOBILE_WEB_URI, "portal", 20, 21, 31);

        dump_tail(&app);

        // 阶段3：退出验证
        if let Some(code) = r1.or(r2) {
            out(&app, &format!("VERDICT: FAIL（页面加载未通过，码 {code}）"));
            std::process::exit(code);
        }
        out(&app, "QUIT TEST: 调用 quit_all；7 秒未死=97；靠硬杀兜底=98；正常退出=0");
        app.state::<AppState>()
            .force_exit_code
            .store(98, std::sync::atomic::Ordering::SeqCst);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(7));
            std::process::exit(97); // quit_all 后 7 秒进程仍在 = 退出链路被阻塞
        });
        browser::quit_all(&app);
        std::thread::sleep(std::time::Duration::from_secs(12));
        std::process::exit(96); // 理论不可达
    });
}
