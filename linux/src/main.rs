//! CloudPhoneKeep Linux/Docker 版入口（一个容器 = 一个账号）
//! ---------------------------------------------------------------------------
//! 对应 Windows 版 main.rs + browser.rs + report_server.rs 组合：
//!   1. 解析环境变量配置（config.rs）
//!   2. 初始化按天滚动日志（logger.rs → {data}/logs/ + stdout 镜像）
//!   3. 先绑定回环上报服务（端口要写进注入脚本，必须先绑定）
//!   4. 引擎线程启动 Chromium Headless Shell，经 CDP 注入与 Windows 版同源
//!      的保活脚本；看门狗每 1 秒驱动 __CPK_TICK__（stopCheck 1s / actionTick 5s）
//!   5. 分级自动恢复：页面重载 → CDP 重连 → Chromium 重启（指数退避）
//!
//! 自检模式 CPK_SELFTEST=1：不启动 Chromium，验证配置/脚本生成/回环服务；
//! 冒烟模式 CPK_SMOKE=1：完整跑 CPK_SMOKE_SECONDS 秒后按指标退出（CI 用）。

mod cdp;
mod config;
mod engine;
mod keepalive;
mod logger;
mod report_server;
mod util;
mod ws;

use engine::SharedState;
use logger::Logger;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

pub const VERSION: &str = "1.0.0-linux";
pub const WIN_VERSION: &str = "1.11.0"; // 移植时对齐的 Windows 版本

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_sig: i32) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

// 信号处理走 glibc signal()：docker stop 的 SIGTERM → 优雅停 Chromium
extern "C" {
    fn signal(signum: i32, handler: extern "C" fn(i32)) -> usize;
}

fn install_signal_handlers() {
    unsafe {
        signal(2, on_signal); // SIGINT
        signal(15, on_signal); // SIGTERM
    }
}

fn on_off(b: bool) -> &'static str {
    if b { "开" } else { "关" }
}

fn main() {
    let cfg = config::Config::from_env();
    let logger = Arc::new(Logger::new(cfg.log_dir.clone()));
    logger.log(
        0,
        "sys",
        &format!(
            "CloudPhoneKeep Linux(Rust) {VERSION} 启动（对齐 Windows v{WIN_VERSION}）pid={}",
            std::process::id()
        ),
    );
    logger.log(
        0,
        "sys",
        &format!(
            "账号={} 平台={}({}) 分辨率={}x{} 动作周期={}ms 保活={} 模拟空闲活动={} 驱动=宿主CDP看门狗",
            cfg.account,
            cfg.platform,
            cfg.platform_label,
            cfg.width,
            cfg.height,
            cfg.interval_ms,
            on_off(cfg.keep_alive),
            on_off(cfg.simulate_activity)
        ),
    );
    logger.log(
        0,
        "sys",
        &format!(
            "数据目录={} Profile={} 绑定={}:{}",
            cfg.data_dir.display(),
            cfg.profile_dir.display(),
            cfg.bind,
            cfg.report_port
        ),
    );

    if cfg.selftest {
        std::process::exit(selftest(&cfg, &logger));
    }

    install_signal_handlers();

    let shared = SharedState::new(&cfg);
    let (ctrl_tx, ctrl_rx) = mpsc::channel();
    let rcfg = report_server::ReportCfg::from(&cfg);
    let port = match report_server::start(rcfg, logger.clone(), shared.clone(), ctrl_tx.clone()) {
        Ok(p) => p,
        Err(e) => {
            logger.log(0, "error", &format!("回环上报服务启动失败：{e}"));
            std::process::exit(1);
        }
    };
    logger.log(
        0,
        "sys",
        &format!("回环上报服务已监听 {}:{}（控制页 http://<host>:<映射端口>/）", cfg.bind, port),
    );

    let _engine = engine::spawn(cfg.clone(), port, logger.clone(), shared.clone(), ctrl_rx);

    // —— 冒烟模式：跑 N 秒按指标退出（CI 端到端验证）——
    if cfg.smoke {
        let deadline = Instant::now() + Duration::from_secs(cfg.smoke_seconds.max(10));
        while Instant::now() < deadline && !SHUTDOWN.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(500));
        }
        let h = shared.snapshot();
        let ok = h.browser == "running" && h.ticks >= 5;
        logger.log(
            0,
            "sys",
            &format!(
                "smoke 结束：browser={} page={} ticks={} clicks={} url={} → {}",
                h.browser,
                h.page,
                h.ticks,
                h.clicks,
                h.page_url,
                if ok { "PASS" } else { "FAIL" }
            ),
        );
        shared.request_stop();
        thread::sleep(Duration::from_secs(3)); // 留时间优雅停 Chromium
        std::process::exit(if ok { 0 } else { 1 });
    }

    // —— 常驻：等终止信号 ——
    loop {
        thread::sleep(Duration::from_millis(200));
        if SHUTDOWN.load(Ordering::SeqCst) {
            logger.log(0, "sys", "收到终止信号，优雅退出");
            shared.request_stop();
            thread::sleep(Duration::from_secs(3));
            break;
        }
    }
    std::process::exit(0);
}

/// 自检：脚本生成 + 回环服务可用性（无浏览器环境可跑，CI 快速回归）
fn selftest(cfg: &config::Config, logger: &Arc<Logger>) -> i32 {
    let script = keepalive::build_init_script(cfg, cfg.report_port);
    let mut checks: Vec<(&str, bool)> = vec![
        (
            "script: 配置注入",
            script.contains(&format!("\"platform\":\"{}\"", cfg.platform))
                && script.contains(&format!("\"port\":{}", cfg.report_port)),
        ),
        ("script: 占位符已替换", !script.contains("__CPK_CFG__") && !script.contains("__CPK_CURSOR__")),
        ("script: tick 暴露", script.contains("__CPK_TICK__")),
        ("script: 状态暴露", script.contains("__CPK_STATE__")),
        ("script: 诊断缓冲", script.contains("__CPK_DRAIN__")),
        ("script: 移动选择器", script.contains(".unlocked") && script.contains("#tabbar")),
        ("script: 联通选择器", script.contains(".try-content") && script.contains(".van-dialog__confirm")),
        ("script: 触摸模拟", script.contains("touchstart")),
        ("script: 外部驱动开关", script.contains("pageTimer")),
    ];
    let shared = SharedState::new(cfg);
    let (ctrl_tx, _ctrl_rx) = mpsc::channel();
    let rcfg = report_server::ReportCfg {
        bind: "127.0.0.1".into(),
        port: 0,
        control_token: String::new(),
    };
    let p = match report_server::start(rcfg, logger.clone(), shared, ctrl_tx) {
        Ok(p) => p,
        Err(e) => {
            logger.log(0, "error", &format!("selftest 回环服务失败：{e}"));
            return 1;
        }
    };
    match util::http_get(p, "/healthz", 3000) {
        Ok((st, body)) if st == 200 || st == 503 => {
            let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            checks.push((
                "server: healthz JSON",
                v.get("platform").and_then(|x| x.as_str()) == Some(cfg.platform.as_str()),
            ));
        }
        other => {
            logger.log(0, "error", &format!("selftest healthz 异常：{other:?}"));
            checks.push(("server: healthz JSON", false));
        }
    }
    let mut failed = 0;
    for (name, ok) in &checks {
        logger.log(0, "sys", &format!("selftest {} {}", if *ok { "PASS" } else { "FAIL" }, name));
        if !ok {
            failed += 1;
        }
    }
    if failed > 0 {
        logger.log(0, "error", &format!("selftest {failed} 项失败"));
        1
    } else {
        logger.log(0, "sys", "selftest 全部通过");
        0
    }
}
