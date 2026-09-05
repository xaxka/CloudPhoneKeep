//! 引擎：Chromium 进程监管 + CDP 会话 + 看门狗 + 分级自动恢复。
//! ---------------------------------------------------------------------------
//! 线程模型（刻意单引擎线程——CDP 命令天然串行，无锁竞争，内存最低）：
//!   [引擎线程]  启动 Chromium → CDP 连接/页面装配/注入 → 稳态监督循环
//!               （每 1s 经 CDP 驱动页面 __CPK_TICK__（stopCheck 1s + actionTick
//!                 intervalMs 双定时器语义，与 Windows 隐藏态看门狗同一模型）；
//!                每 5s 采样 __CPK_STATE__ + 取走 __CPK_DRAIN__ 诊断缓冲）
//!   [HTTP 线程] 回环上报/控制端点；控制请求（截图/触摸/输入）经 channel
//!               交给引擎线程串行执行（每 tick 周期清空一次，延迟 ≤1s）
//!
//! 恢复分级（对齐 Windows 版思路）：
//!   tick 连续失败 / 状态冻结 / 脚本缺失 → 页面导航回首页
//!   → 10 分钟窗口 3 次无效 / 传输断裂 → 重建 CDP 会话（进程保留，页面不重载）
//!   → 重连无效 / Chromium 退出 / 心跳超龄 → 重启 Chromium（指数退避 5s→300s）

use crate::cdp::{self, Cdp};
use crate::config::Config;
use crate::keepalive;
use crate::logger::Logger;
use crate::util;
use serde_json::{json, Value};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

// —— 注入脚本暴露的驱动/采样表达式（与 keepalive.inject.js 的约定一致）——
/// tick：驱动页面双定时器；返回 ok / noscript / err:<msg>
pub const TICK_EXPR: &str = "(function(){try{if(!window.__CPK_TICK__)return 'noscript';window.__CPK_TICK__();return 'ok'}catch(e){return 'err:'+String(e&&e.message)}})()";
/// 快照：__CPK_STATE__ + 取走 __CPK_DRAIN__（诊断环形缓冲，元素 {t,l,m}）
pub const SNAPSHOT_EXPR: &str = "(function(){try{var s=window.__CPK_STATE__;var d=window.__CPK_DRAIN__?window.__CPK_DRAIN__():[];if(!s)return JSON.stringify({no:1,d:d});return JSON.stringify({ticks:s.ticks,clicks:s.clicks,last:s.last,wasExited:s.wasExited,stopDone:s.stopDone,entered:s.entered,url:location.href.slice(0,200),title:(document.title||'').slice(0,60),ready:document.readyState,d:d})}catch(e){return JSON.stringify({err:String(e&&e.message)})}})()";
/// 重连探测：当前文档是否已装保活脚本（返回 'y'/'n' 字符串便于 evaluate 读取）
pub const PROBE_EXPR: &str = "window.__CPK_INSTALLED__===true?'y':'n'";

extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

// ---------------------------------------------------------------------------
// 共享状态（healthz / 控制页数据源）
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Health {
    pub ok: bool,
    pub browser: String,
    pub page: String,
    pub platform: String,
    pub platform_label: String,
    pub account: String,
    pub home_uri: String,
    pub version: String,
    pub ticks: u64,
    pub clicks: u64,
    pub last_action: String,
    pub page_url: String,
    pub exited: bool,
    pub last_beat_ms: i64,
    pub last_beat_age: Option<u64>,
    pub restarts: u32,
    pub reloads: u32,
    pub dialogs: u32,
    pub chrome_version: String,
    pub last_error: String,
    pub started_at_ms: i64,
}

pub struct SharedState {
    health: Mutex<Health>,
    last_beat_ms: AtomicI64,
    exited: AtomicBool,
    stop: AtomicBool,
    beat_stale_sec: u64,
}

impl SharedState {
    pub fn new(cfg: &Config) -> Arc<SharedState> {
        let health = Health {
            ok: false,
            browser: "stopped".into(),
            page: "none".into(),
            platform: cfg.platform.clone(),
            platform_label: cfg.platform_label.clone(),
            account: cfg.account.clone(),
            home_uri: cfg.url.clone(),
            version: format!("{} (win {})", crate::VERSION, crate::WIN_VERSION),
            ticks: 0,
            clicks: 0,
            last_action: String::new(),
            page_url: String::new(),
            exited: false,
            last_beat_ms: 0,
            last_beat_age: None,
            restarts: 0,
            reloads: 0,
            dialogs: 0,
            chrome_version: String::new(),
            last_error: String::new(),
            started_at_ms: util::now_ms(),
        };
        Arc::new(SharedState {
            health: Mutex::new(health),
            last_beat_ms: AtomicI64::new(0),
            exited: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            beat_stale_sec: cfg.beat_stale_sec,
        })
    }

    pub fn snapshot(&self) -> Health {
        let mut h = self.health.lock().unwrap().clone();
        h.last_beat_ms = self.last_beat_ms.load(Ordering::Relaxed);
        h.exited = self.exited.load(Ordering::Relaxed);
        let age = if h.last_beat_ms > 0 {
            Some(((util::now_ms() - h.last_beat_ms).max(0) / 1000) as u64)
        } else {
            None
        };
        h.last_beat_age = age;
        h.ok = h.browser == "running" && !h.exited && age.map(|a| a < self.beat_stale_sec).unwrap_or(false);
        h
    }

    fn update(&self, f: impl FnOnce(&mut Health)) {
        if let Ok(mut g) = self.health.lock() {
            f(&mut g);
        }
    }

    pub fn set_browser(&self, s: &str) { self.update(|h| h.browser = s.into()); }
    pub fn set_page(&self, s: &str) { self.update(|h| h.page = s.into()); }
    pub fn set_page_stats(&self, ticks: u64, clicks: u64, last_action: &str, url: &str) {
        self.update(|h| {
            h.ticks = ticks;
            h.clicks = clicks;
            h.last_action = last_action.into();
            h.page_url = url.into();
        });
    }
    pub fn set_restarts(&self, n: u32) { self.update(|h| h.restarts = n); }
    pub fn bump_reloads(&self) { self.update(|h| h.reloads += 1); }
    pub fn set_dialogs(&self, n: u32) { self.update(|h| h.dialogs = n); }
    pub fn set_chrome_version(&self, v: &str) { self.update(|h| h.chrome_version = v.into()); }
    pub fn set_last_error(&self, e: &str) { self.update(|h| h.last_error = e.into()); }
    pub fn touch_beat(&self) { self.last_beat_ms.store(util::now_ms(), Ordering::Relaxed); }
    pub fn mark_exited(&self) { self.exited.store(true, Ordering::Relaxed); }
    pub fn request_stop(&self) { self.stop.store(true, Ordering::Relaxed); }
    pub fn stopping(&self) -> bool { self.stop.load(Ordering::Relaxed) }
}

pub fn health_json(h: &Health) -> Value {
    json!({
        "ok": h.ok,
        "browser": h.browser,
        "page": h.page,
        "platform": h.platform,
        "platformLabel": h.platform_label,
        "account": h.account,
        "homeUri": h.home_uri,
        "version": h.version,
        "ticks": h.ticks,
        "clicks": h.clicks,
        "lastAction": h.last_action,
        "pageUrl": h.page_url,
        "exited": h.exited,
        "lastBeatAge": h.last_beat_age,
        "restarts": h.restarts,
        "reloads": h.reloads,
        "dialogs": h.dialogs,
        "chromeVersion": h.chrome_version,
        "lastError": h.last_error,
    })
}

// ---------------------------------------------------------------------------
// 控制请求（HTTP 控制端点 → 引擎线程串行执行）
// ---------------------------------------------------------------------------

pub enum ControlRequest {
    Screenshot { reply: Sender<Result<Vec<u8>, String>> },
    Tap { x: f64, y: f64, reply: Sender<Result<(), String>> },
    Swipe { x1: f64, y1: f64, x2: f64, y2: f64, reply: Sender<Result<(), String>> },
    TypeText { text: String, reply: Sender<Result<(), String>> },
    Key { key: String, reply: Sender<Result<(), String>> },
    Navigate { url: String, reply: Sender<Result<(), String>> },
    Reload { reply: Sender<Result<(), String>> },
}

// ---------------------------------------------------------------------------
// 启动入口
// ---------------------------------------------------------------------------

pub fn spawn(
    cfg: Config,
    report_port: u16,
    logger: Arc<Logger>,
    shared: Arc<SharedState>,
    ctrl_rx: Receiver<ControlRequest>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("cpk-engine".into())
        .spawn(move || engine_loop(&cfg, report_port, logger, shared, ctrl_rx))
        .expect("引擎线程启动失败")
}

enum SteadyOutcome {
    /// 传输断裂但 Chromium 可能还活着：仅重建 CDP 会话（页面不重载）
    Reattach,
    /// 升级/进程退出/心跳超龄：重启 Chromium
    Restart,
    /// 收到停止信号
    Stop,
}

fn engine_loop(cfg: &Config, report_port: u16, logger: Arc<Logger>, shared: Arc<SharedState>, ctrl_rx: Receiver<ControlRequest>) {
    let script = keepalive::build_init_script(cfg, report_port);
    let mut backoff: u64 = 5;
    let mut restarts: u32 = 0;
    let mut child: Option<Child> = None;
    let mut cdp: Option<Cdp> = None;
    let mut cdp_port: u16 = 0;
    let mut session: String = String::new();

    'outer: loop {
        if shared.stopping() {
            kill_child(&mut child, &logger);
            return;
        }

        // —— 1. Chromium 进程 ——
        if child.is_none() {
            shared.set_browser("starting");
            match launch_chrome(cfg, &logger) {
                Ok(c) => {
                    child = Some(c);
                }
                Err(e) => {
                    logger.log(0, "error", &format!("Chromium 启动失败：{e}"));
                    shared.set_last_error(&e);
                    shared.set_browser("failed");
                    thread::sleep(Duration::from_secs(backoff));
                    backoff = (backoff * 2).min(300);
                    continue 'outer;
                }
            }
        }

        // —— 2. DevTools 就绪 + CDP 装配 ——
        if cdp.is_none() {
            let ch = child.as_mut().expect("child");
            let port = match wait_devtools(ch, cfg, 30_000) {
                Ok(p) => p,
                Err(e) => {
                    logger.log(0, "error", &format!("DevTools 就绪失败：{e}"));
                    shared.set_last_error(&e);
                    kill_child(&mut child, &logger);
                    shared.set_browser("stopped");
                    thread::sleep(Duration::from_secs(backoff));
                    backoff = (backoff * 2).min(300);
                    continue 'outer;
                }
            };
            cdp_port = port;
            match attach_all(cfg, port, &script, &logger, true) {
                Ok((c, s)) => {
                    shared.set_chrome_version(&c.browser);
                    cdp = Some(c);
                    session = s;
                    restarts += 1;
                    shared.set_restarts(restarts);
                    shared.set_browser("running");
                }
                Err(e) => {
                    logger.log(0, "error", &format!("CDP 装配失败：{e}"));
                    shared.set_last_error(&e);
                    kill_child(&mut child, &logger);
                    shared.set_browser("stopped");
                    thread::sleep(Duration::from_secs(backoff));
                    backoff = (backoff * 2).min(300);
                    continue 'outer;
                }
            }
        }

        // —— 3. 稳态监督 ——
        let steady_started = Instant::now();
        let ch = child.as_mut().expect("child");
        let c = cdp.as_mut().expect("cdp");
        let outcome = steady_loop(c, &session, ch, cfg, &shared, &ctrl_rx, &logger);
        // 上一轮稳定运行超过 10 分钟 → 重置退避（非崩溃循环，是偶发故障）
        if steady_started.elapsed() > Duration::from_secs(600) {
            backoff = 5;
        }
        match outcome {
            SteadyOutcome::Stop => {
                if let Some(mut c) = cdp.take() {
                    c.close();
                }
                kill_child(&mut child, &logger);
                return;
            }
            SteadyOutcome::Reattach => {
                logger.log(0, "sys", "CDP 传输断裂，重建会话（Chromium 进程保留，页面不重载）");
                if let Some(mut c) = cdp.take() {
                    c.close();
                }
                let mut ok = false;
                for _ in 0..10 {
                    if shared.stopping() {
                        kill_child(&mut child, &logger);
                        return;
                    }
                    // 进程已死：直接走重启路径
                    if let Some(ch) = child.as_mut() {
                        if matches!(ch.try_wait(), Ok(Some(_))) {
                            break;
                        }
                    }
                    match attach_all(cfg, cdp_port, &script, &logger, false) {
                        Ok((mut c, s)) => {
                            // 探测当前文档是否已有脚本：无则导航（新文档经 addScript 自动注入）
                            let has = cdp::eval_string(&mut c, &s, PROBE_EXPR, 5000)
                                .map(|v| v == "y")
                                .unwrap_or(false);
                            if !has {
                                logger.log(1, "nav", "重连后当前文档无保活脚本，导航回首页");
                                let _ = c.call("Page.navigate", json!({ "url": cfg.url }), Some(&s), 20000);
                            }
                            cdp = Some(c);
                            session = s;
                            ok = true;
                            break;
                        }
                        Err(_) => {}
                    }
                    thread::sleep(Duration::from_secs(3));
                }
                if !ok {
                    logger.log(0, "sys", "CDP 会话重建耗尽，重启 Chromium");
                    kill_child(&mut child, &logger);
                    shared.set_browser("stopped");
                    thread::sleep(Duration::from_secs(backoff));
                    backoff = (backoff * 2).min(300);
                    continue 'outer;
                }
                // 重建成功 → 回到稳态（自然进入下一轮循环体）
                continue 'outer;
            }
            SteadyOutcome::Restart => {
                logger.log(0, "sys", "重启 Chromium（分级恢复升级/心跳超龄/进程退出）");
                if let Some(mut c) = cdp.take() {
                    c.close();
                }
                kill_child(&mut child, &logger);
                shared.set_browser("stopped");
                thread::sleep(Duration::from_secs(backoff));
                backoff = (backoff * 2).min(300);
                continue 'outer;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 稳态监督循环
// ---------------------------------------------------------------------------

struct Stats {
    tick_fails: u32,
    not_installed: u32,
    frozen: u32,
    last_ticks: i64,
    reloads_window: u32,
    reload_window_start: Instant,
}

fn steady_loop(
    cdp: &mut Cdp,
    session: &str,
    child: &mut Child,
    cfg: &Config,
    shared: &Arc<SharedState>,
    ctrl_rx: &Receiver<ControlRequest>,
    logger: &Arc<Logger>,
) -> SteadyOutcome {
    let mut stats = Stats {
        tick_fails: 0,
        not_installed: 0,
        frozen: 0,
        last_ticks: -1,
        reloads_window: 0,
        reload_window_start: Instant::now(),
    };
    let mut next_tick = Instant::now();
    let mut next_sample = Instant::now() + Duration::from_secs(5);
    let mut last_progress = Instant::now();
    let mut last_dialog_count = 0u32;
    shared.set_page("loading");

    loop {
        if shared.stopping() {
            return SteadyOutcome::Stop;
        }
        // Chromium 进程退出
        match child.try_wait() {
            Ok(Some(st)) => {
                logger.log(0, "sys", &format!("Chromium 退出：{st}"));
                return SteadyOutcome::Restart;
            }
            Ok(None) => {}
            Err(e) => {
                logger.log(0, "error", &format!("检查 Chromium 进程失败：{e}"));
                return SteadyOutcome::Restart;
            }
        }
        // 控制请求（每个监督周期清空一次；单请求超时上限 15s）
        while let Ok(req) = ctrl_rx.try_recv() {
            if let Err(e) = handle_control(cdp, session, req, logger) {
                logger.log(0, "error", &format!("控制请求处理失败：{e}"));
                if e.starts_with("WS:") {
                    return SteadyOutcome::Reattach;
                }
            }
        }

        let now = Instant::now();
        // —— tick：每 1 秒驱动页面双定时器 ——
        if now >= next_tick {
            next_tick = now + Duration::from_secs(1);
            let t0 = Instant::now();
            let r = cdp::eval_string(cdp, session, TICK_EXPR, 5000);
            let dt = t0.elapsed();
            if dt > Duration::from_millis(300) {
                logger.log(0, "sys", &format!("慢调用诊断: tick eval {dt:?}"));
            }
            match r {
                Ok(val) => {
                    if val == "ok" {
                        stats.tick_fails = 0;
                    } else if val == "noscript" {
                        // 当前文档没有保活脚本（重连后旧文档/异常导航目标）
                        stats.tick_fails = 0;
                        stats.not_installed += 1;
                        if stats.not_installed == 3 {
                            logger.log(1, "nav", "当前文档无保活脚本，导航回首页");
                            if let Err(e) = nav_home(cdp, session, cfg, shared, &mut stats, logger) {
                                if e.starts_with("WS:") {
                                    return SteadyOutcome::Reattach;
                                }
                            }
                        } else if stats.not_installed >= 6 {
                            logger.log(1, "error", "保活脚本持续缺失，重建 CDP 会话");
                            return SteadyOutcome::Reattach;
                        }
                    } else {
                        // err:xxx —— 页面脚本异常（少见，恢复路径与失败计数共用）
                        stats.tick_fails += 1;
                        if val.starts_with("err:") {
                            logger.log(1, "error", &format!("tick 异常：{val}"));
                        }
                        if stats.tick_fails >= cfg.tick_fail_reload {
                            stats.tick_fails = 0;
                            logger.log(1, "sys", &format!("tick 连续失败 {} 次，导航回首页", cfg.tick_fail_reload));
                            if let Err(e) = nav_home(cdp, session, cfg, shared, &mut stats, logger) {
                                if e.starts_with("WS:") {
                                    return SteadyOutcome::Reattach;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    stats.tick_fails += 1;
                    if e.starts_with("WS:") {
                        logger.log(0, "sys", &format!("tick 传输失败：{e}"));
                        return SteadyOutcome::Reattach;
                    }
                    if stats.tick_fails >= cfg.tick_fail_reload {
                        stats.tick_fails = 0;
                        logger.log(1, "sys", &format!("tick 连续失败 {} 次，导航回首页", cfg.tick_fail_reload));
                        if let Err(e2) = nav_home(cdp, session, cfg, shared, &mut stats, logger) {
                            if e2.starts_with("WS:") {
                                return SteadyOutcome::Reattach;
                            }
                        }
                    }
                }
            }
        }

        // —— 采样：每 5 秒读状态 + 取走诊断缓冲 ——
        let now = Instant::now();
        if now >= next_sample {
            next_sample = now + Duration::from_secs(5);
            let t1 = Instant::now();
            let r2 = cdp::eval_string(cdp, session, SNAPSHOT_EXPR, 8000);
            let dt2 = t1.elapsed();
            if dt2 > Duration::from_millis(300) {
                logger.log(0, "sys", &format!("慢调用诊断: sample eval {dt2:?}"));
            }
            match r2 {
                Ok(s) => match serde_json::from_str::<Value>(&s) {
                    Ok(snap) => {
                        // 诊断环形缓冲（元素 {t,l,m}）→ 落盘
                        if let Some(arr) = snap.get("d").and_then(|x| x.as_array()) {
                            for item in arr {
                                let l = item.get("l").and_then(|x| x.as_str()).unwrap_or("sys");
                                let m = item.get("m").and_then(|x| x.as_str()).unwrap_or("");
                                if !m.is_empty() {
                                    logger.log(1, l, m);
                                }
                            }
                        }
                        if snap.get("no").and_then(|x| x.as_i64()) == Some(1) {
                            // 无脚本：tick 侧 not_installed 路径已处理导航
                        } else if let Some(err) = snap.get("err").and_then(|x| x.as_str()) {
                            logger.log(1, "error", &format!("采样异常：{err}"));
                        } else {
                            let ticks = snap.get("ticks").and_then(|x| x.as_i64()).unwrap_or(-1);
                            let clicks = snap.get("clicks").and_then(|x| x.as_i64()).unwrap_or(0);
                            let last = snap.get("last").and_then(|x| x.as_str()).unwrap_or("");
                            let url = snap.get("url").and_then(|x| x.as_str()).unwrap_or("");
                            shared.set_page_stats(ticks.max(0) as u64, clicks.max(0) as u64, last, url);
                            if snap.get("wasExited").and_then(|x| x.as_bool()) == Some(true) {
                                shared.mark_exited();
                            }
                            if ticks != stats.last_ticks && ticks >= 0 {
                                stats.last_ticks = ticks;
                                stats.frozen = 0;
                                stats.not_installed = 0;
                                shared.touch_beat();
                                last_progress = Instant::now();
                                if snap.get("ready").and_then(|x| x.as_str()) == Some("complete") {
                                    shared.set_page("ok");
                                }
                            } else {
                                stats.frozen += 1;
                                if stats.frozen >= cfg.frozen_reload {
                                    stats.frozen = 0;
                                    logger.log(
                                        1,
                                        "sys",
                                        &format!("页面状态冻结 {} 个采样周期（ticks 停滞），导航回首页", cfg.frozen_reload),
                                    );
                                    if let Err(e) = nav_home(cdp, session, cfg, shared, &mut stats, logger) {
                                        if e.starts_with("WS:") {
                                            return SteadyOutcome::Reattach;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => logger.log(1, "error", &format!("采样解析失败：{e}")),
                },
                Err(e) => {
                    if e.starts_with("WS:") {
                        return SteadyOutcome::Reattach;
                    }
                    // 命令级失败（超时等）：tick 侧失败计数已覆盖恢复路径
                }
            }
            // 对话框自动确认留痕
            if cdp.dialog_count != last_dialog_count {
                last_dialog_count = cdp.dialog_count;
                if let Some(d) = cdp.last_dialog.clone() {
                    logger.log(1, "sys", &format!("原生弹窗已自动确认：{d}"));
                }
                shared.set_dialogs(cdp.dialog_count);
            }
        }

        // —— 心跳超龄：渲染进程半死不活的兜底（tick 不再前进 + /report 丢失）——
        if last_progress.elapsed().as_secs() >= cfg.beat_stale_sec {
            logger.log(
                0,
                "error",
                &format!("心跳丢失 {}s，硬重启浏览器", last_progress.elapsed().as_secs()),
            );
            return SteadyOutcome::Restart;
        }

        // —— 恢复窗口：10 分钟内页面级恢复超 3 次 → 升级 ——
        if stats.reload_window_start.elapsed() >= Duration::from_secs(600) {
            stats.reload_window_start = Instant::now();
            stats.reloads_window = 0;
        }
        if stats.reloads_window > 3 {
            logger.log(0, "sys", "10 分钟内页面级恢复超 3 次无效，升级重启浏览器");
            return SteadyOutcome::Restart;
        }

        thread::sleep(Duration::from_millis(100));
    }
}

fn nav_home(
    cdp: &mut Cdp,
    session: &str,
    cfg: &Config,
    shared: &Arc<SharedState>,
    stats: &mut Stats,
    logger: &Arc<Logger>,
) -> Result<(), String> {
    stats.reloads_window += 1;
    shared.bump_reloads();
    shared.set_page("reloading");
    logger.log(1, "nav", &format!("恢复性导航 {}", cfg.url));
    cdp.call("Page.navigate", json!({ "url": cfg.url }), Some(session), 15000)
        .map(|_| ())
}

// ---------------------------------------------------------------------------
// 控制请求执行（CDP Input 域 = 内核级触摸/输入模拟）
// ---------------------------------------------------------------------------

fn handle_control(cdp: &mut Cdp, session: &str, req: ControlRequest, logger: &Arc<Logger>) -> Result<(), String> {
    match req {
        ControlRequest::Screenshot { reply } => {
            let v = cdp.call(
                "Page.captureScreenshot",
                json!({ "format": "jpeg", "quality": 70 }),
                Some(session),
                15000,
            )?;
            let b64 = v.get("data").and_then(|x| x.as_str()).ok_or("截图无 data")?;
            let bytes = util::base64_decode(b64);
            let _ = reply.send(Ok(bytes));
        }
        ControlRequest::Tap { x, y, reply } => {
            logger.log(1, "click", &format!("触摸 ({x:.0},{y:.0})"));
            tap(cdp, session, x, y)?;
            let _ = reply.send(Ok(()));
        }
        ControlRequest::Swipe { x1, y1, x2, y2, reply } => {
            logger.log(1, "click", &format!("滑动 ({x1:.0},{y1:.0})→({x2:.0},{y2:.0})"));
            swipe(cdp, session, x1, y1, x2, y2)?;
            let _ = reply.send(Ok(()));
        }
        ControlRequest::TypeText { text, reply } => {
            logger.log(1, "click", &format!("输入文本（{} 字符）", text.chars().count()));
            cdp.call("Input.insertText", json!({ "text": text }), Some(session), 5000)?;
            let _ = reply.send(Ok(()));
        }
        ControlRequest::Key { key, reply } => {
            logger.log(1, "click", &format!("按键 {key}"));
            key_event(cdp, session, &key)?;
            let _ = reply.send(Ok(()));
        }
        ControlRequest::Navigate { url, reply } => {
            logger.log(1, "nav", &format!("控制页导航 {url}"));
            cdp.call("Page.navigate", json!({ "url": url }), Some(session), 20000)?;
            let _ = reply.send(Ok(()));
        }
        ControlRequest::Reload { reply } => {
            logger.log(1, "nav", "控制页重载");
            cdp.call("Page.reload", json!({ "ignoreCache": true }), Some(session), 20000)?;
            let _ = reply.send(Ok(()));
        }
    }
    Ok(())
}

fn touch_event(cdp: &mut Cdp, session: &str, typ: &str, points: Value) -> Result<(), String> {
    cdp.call(
        "Input.dispatchTouchEvent",
        json!({ "type": typ, "touchPoints": points }),
        Some(session),
        5000,
    )
    .map(|_| ())
}

fn tap(cdp: &mut Cdp, session: &str, x: f64, y: f64) -> Result<(), String> {
    touch_event(cdp, session, "touchStart", json!([{ "x": x, "y": y, "id": 1 }]))?;
    thread::sleep(Duration::from_millis(80));
    touch_event(cdp, session, "touchEnd", json!([]))
}

fn swipe(cdp: &mut Cdp, session: &str, x1: f64, y1: f64, x2: f64, y2: f64) -> Result<(), String> {
    touch_event(cdp, session, "touchStart", json!([{ "x": x1, "y": y1, "id": 1 }]))?;
    for i in 1..=8 {
        let t = i as f64 / 8.0;
        let xi = x1 + (x2 - x1) * t;
        let yi = y1 + (y2 - y1) * t;
        touch_event(cdp, session, "touchMove", json!([{ "x": xi, "y": yi, "id": 1 }]))?;
        thread::sleep(Duration::from_millis(16));
    }
    touch_event(cdp, session, "touchEnd", json!([]))
}

fn key_event(cdp: &mut Cdp, session: &str, key: &str) -> Result<(), String> {
    let (code, vk, text): (&str, u32, &str) = match key {
        "Backspace" => ("Backspace", 8, ""),
        "Tab" => ("Tab", 9, "\t"),
        "Escape" => ("Escape", 27, ""),
        _ => ("Enter", 13, "\r"),
    };
    let mut kd = json!({
        "type": "keyDown",
        "key": key,
        "code": code,
        "windowsVirtualKeyCode": vk,
        "nativeVirtualKeyCode": vk,
    });
    if !text.is_empty() {
        kd["text"] = Value::String(text.to_string());
    }
    cdp.call("Input.dispatchKeyEvent", kd, Some(session), 5000)?;
    cdp.call(
        "Input.dispatchKeyEvent",
        json!({
            "type": "keyUp",
            "key": key,
            "code": code,
            "windowsVirtualKeyCode": vk,
            "nativeVirtualKeyCode": vk,
        }),
        Some(session),
        5000,
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Chromium 进程管理
// ---------------------------------------------------------------------------

/// 构造 Chromium 启动参数（低内存 + 保活语义 + WebRTC 可用；
/// 与 Node 版 buildArgs 逐项一致）
pub fn build_args(cfg: &Config) -> Vec<String> {
    let mut a: Vec<String> = vec![
        format!("--user-data-dir={}", cfg.profile_dir.display()),
        format!("--remote-debugging-port={}", cfg.cdp_port), // 0 = 自动分配（读 DevToolsActivePort）
        "--remote-debugging-address=127.0.0.1".into(),       // DevTools 只在回环暴露
        "--remote-allow-origins=*".into(), // 允许外部 DevTools 一次性登录（仅回环暴露）
        format!("--window-size={},{}", cfg.width, cfg.height),
        "--force-device-scale-factor=1".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-gpu".into(),                          // 容器内无 GPU；WebRTC 走软件编解码
        "--disable-dev-shm-usage".into(),                // Docker 默认 /dev/shm 64MB 偏小
        "--disable-crash-reporter".into(),
        "--disable-background-timer-throttling".into(),  // 保活三件套：任何情况下不节流页面定时器
        "--disable-backgrounding-occluded-windows".into(),
        "--disable-renderer-backgrounding".into(),
        "--disable-background-networking".into(),
        "--disable-component-update".into(),
        "--disable-sync".into(),
        "--disable-features=Translate,MediaRouter,OptimizationHints".into(),
        "--mute-audio".into(),
        "--autoplay-policy=no-user-gesture-required".into(), // 云机视频流自动播放
        format!("--lang={}", cfg.lang),
    ];
    if cfg.no_sandbox {
        a.push("--no-sandbox".into());
        a.push("--disable-setuid-sandbox".into());
    }
    // 完整 Chromium（发行版 chromium 包）需要显式无头模式；
    // chrome-headless-shell 本身即无头，该开关对其无副作用
    if cfg.headless {
        a.push("--headless=new".into());
    }
    if !cfg.extra_chrome_args.is_empty() {
        for p in cfg.extra_chrome_args.split_whitespace() {
            a.push(p.to_string());
        }
    }
    a.push("about:blank".into());
    a
}

fn launch_chrome(cfg: &Config, logger: &Arc<Logger>) -> Result<Child, String> {
    let args = build_args(cfg);
    logger.log(
        0,
        "sys",
        &format!("启动 {}（参数：{}）", cfg.chrome_bin, args.join(" ")),
    );
    let mut child = Command::new(&cfg.chrome_bin)
        .args(&args)
        .env("TZ", &cfg.tz)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("无法启动 {}：{e}（二进制缺失/权限？）", cfg.chrome_bin))?;
    // stderr 过滤线程：只留错误级（缺共享库/崩溃在此可见），stdout 直接丢弃
    if let Some(stderr) = child.stderr.take() {
        let lg = logger.clone();
        thread::spawn(move || {
            use std::io::BufRead;
            let reader = std::io::BufReader::new(stderr);
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        let t = l.trim();
                        if !t.is_empty()
                            && (t.contains("rror") || t.contains("atal") || t.contains("issing") || t.contains("annot"))
                        {
                            let cut: String = t.chars().take(500).collect();
                            lg.log(0, "error", &format!("chrome: {cut}"));
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }
    Ok(child)
}

/// 等待 DevTools 就绪（固定端口直探 / 自动端口读 DevToolsActivePort 文件）
fn wait_devtools(child: &mut Child, cfg: &Config, timeout_ms: u64) -> Result<u16, String> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let port_file = cfg.profile_dir.join("DevToolsActivePort");
    loop {
        if Instant::now() > deadline {
            return Err("等待 DevTools 端口超时(30s)".into());
        }
        if let Ok(Some(st)) = child.try_wait() {
            return Err(format!("Chromium 启动后立即退出：{st}（见上方 chrome 错误行）"));
        }
        if cfg.cdp_port > 0 {
            if cdp::fetch_version(cfg.cdp_port, 1, 0).is_ok() {
                return Ok(cfg.cdp_port);
            }
        } else if let Ok(text) = std::fs::read_to_string(&port_file) {
            if let Some(first) = text.lines().next() {
                if let Ok(p) = first.trim().parse::<u16>() {
                    if p > 0 && cdp::fetch_version(p, 1, 0).is_ok() {
                        return Ok(p);
                    }
                }
            }
        }
        thread::sleep(Duration::from_millis(300));
    }
}

/// CDP 装配：复用/新建页面目标 → attach → enable → UA 对齐 → 注入保活脚本
/// （navigate=true 时导航到云手机首页；重连场景 navigate=false）
fn attach_all(cfg: &Config, port: u16, script: &str, logger: &Arc<Logger>, navigate: bool) -> Result<(Cdp, String), String> {
    let mut cdp = Cdp::connect(port)?;
    // 复用已有 page 目标（chrome-headless-shell 启动自带一个 about:blank）
    let targets = cdp.call("Target.getTargets", json!({}), None, 10000)?;
    let existing = targets
        .get("targetInfos")
        .and_then(|x| x.as_array())
        .and_then(|infos| {
            infos
                .iter()
                .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
        })
        .and_then(|t| t.get("targetId").and_then(|v| v.as_str()).map(|s| s.to_string()));
    let target_id = match existing {
        Some(t) => t,
        None => {
            let r = cdp.call("Target.createTarget", json!({ "url": "about:blank" }), None, 10000)?;
            r.get("targetId")
                .and_then(|v| v.as_str())
                .ok_or("createTarget 无 targetId")?
                .to_string()
        }
    };
    let attach = cdp.call(
        "Target.attachToTarget",
        json!({ "targetId": target_id, "flatten": true }),
        None,
        10000,
    )?;
    let session = attach
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or("attachToTarget 无 sessionId")?
        .to_string();
    cdp.call("Page.enable", json!({}), Some(&session), 10000)?;
    cdp.call("Runtime.enable", json!({}), Some(&session), 10000)?;
    // UA 对齐（best-effort：实现差异不影响保活，仅环境指纹）
    let ua = normalize_user_agent(&cdp.raw_ua, &cfg.ua_mode);
    let mut ua_params = json!({ "userAgent": ua });
    if cfg.ua_mode == "windows" {
        ua_params["platform"] = Value::String("Windows".into());
    }
    if cdp.call("Emulation.setUserAgentOverride", ua_params, Some(&session), 5000).is_err() {
        logger.log(1, "sys", "UA 覆盖未生效（不影响保活，仅环境指纹差异）");
    }
    // 保活脚本：document_start 注入，所有新文档自动重装——
    // 与 Tauri initialization_script / WebView2 AddScriptToExecuteOnDocumentCreated 同语义
    cdp.call(
        "Page.addScriptToEvaluateOnNewDocument",
        json!({ "source": script, "runImmediately": true }),
        Some(&session),
        10000,
    )?;
    if navigate {
        cdp.call("Page.navigate", json!({ "url": cfg.url }), Some(&session), 20000)?;
        logger.log(1, "nav", &format!("导航 {}", cfg.url));
    }
    Ok((cdp, session))
}

/// UA 规范化（与 Node 版 normalizeUserAgent 一致）：
/// windows=重建为 Windows Chrome UA；auto=仅去 Headless 字样；none=原样
pub fn normalize_user_agent(raw: &str, mode: &str) -> String {
    if mode == "none" {
        return raw.to_string();
    }
    if mode == "auto" {
        return raw.replace("HeadlessChrome", "Chrome");
    }
    // windows（默认）：提取 Chrome 版本号重建
    let ver = regex_chrome_version(raw).unwrap_or_else(|| "138.0.0.0".into());
    format!(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{ver} Safari/537.36"
    )
}

fn regex_chrome_version(ua: &str) -> Option<String> {
    // 手写最小匹配：Chrome/<digits(.digits)*>（避免引入 regex crate）
    let key = "Chrome/";
    let start = ua.rfind(key)? + key.len();
    let end = ua[start..]
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|i| start + i)
        .unwrap_or(ua.len());
    if end > start {
        Some(ua[start..end].to_string())
    } else {
        None
    }
}

fn kill_child(child: &mut Option<Child>, logger: &Arc<Logger>) {
    if let Some(mut c) = child.take() {
        // 先 SIGTERM（Chromium 优雅退出清理 Profile 锁），3 秒后 SIGKILL
        let pid = c.id() as i32;
        unsafe {
            libc_kill(pid, 15);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match c.try_wait() {
                Ok(Some(_)) => break,
                _ if Instant::now() < deadline => thread::sleep(Duration::from_millis(100)),
                _ => {
                    let _ = c.kill();
                    break;
                }
            }
        }
        let _ = c.wait();
        logger.log(0, "sys", "Chromium 进程已终止");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ua_normalization() {
        let raw = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) HeadlessChrome/152.0.7977.82 Safari/537.36";
        let win = normalize_user_agent(raw, "windows");
        assert!(win.starts_with("Mozilla/5.0 (Windows NT 10.0; Win64; x64)"));
        assert!(win.contains("Chrome/152.0.7977.82"));
        assert!(!win.contains("HeadlessChrome"));
        assert!(!win.contains("X11"));
        let auto = normalize_user_agent(raw, "auto");
        assert!(auto.contains("Chrome/152.0.7977.82"));
        assert!(!auto.contains("HeadlessChrome"));
        assert!(auto.contains("X11; Linux x86_64"));
        let none = normalize_user_agent(raw, "none");
        assert_eq!(none, raw);
        // 无版本号兜底
        let win2 = normalize_user_agent("Mozilla/5.0 HeadlessChrome", "windows");
        assert!(win2.contains("Chrome/138.0.0.0"));
    }

    #[test]
    fn chrome_args_basic() {
        let cfg = crate::config::Config {
            account: "t".into(),
            platform: "mobile".into(),
            platform_label: "移动云手机".into(),
            url: "https://x".into(),
            width: 414,
            height: 896,
            data_dir: "/data".into(),
            profile_dir: "/data/profile-t".into(),
            log_dir: "/data/logs".into(),
            keep_alive: true,
            interval_ms: 5000,
            simulate_activity: true,
            block_context_menu: true,
            page_timer: false,
            report_port: 8080,
            bind: "0.0.0.0".into(),
            control_token: String::new(),
            cdp_port: 0,
            chrome_bin: "chrome-headless-shell".into(),
            headless: false,
            no_sandbox: true,
            ua_mode: "windows".into(),
            lang: "zh-CN".into(),
            tz: "Asia/Shanghai".into(),
            extra_chrome_args: "--js-flags=--max-old-space-size=512".into(),
            tick_fail_reload: 10,
            frozen_reload: 3,
            beat_stale_sec: 180,
            selftest: false,
            smoke: false,
            smoke_seconds: 60,
        };
        let args = build_args(&cfg);
        assert!(args.contains(&"--user-data-dir=/data/profile-t".to_string()));
        assert!(args.contains(&"--remote-debugging-port=0".to_string()));
        assert!(args.contains(&"--window-size=414,896".to_string()));
        assert!(args.contains(&"--no-sandbox".to_string()));
        // 保活三件套
        assert!(args.contains(&"--disable-background-timer-throttling".to_string()));
        assert!(args.contains(&"--disable-backgrounding-occluded-windows".to_string()));
        assert!(args.contains(&"--disable-renderer-backgrounding".to_string()));
        // WebRTC 相关不禁止
        assert!(!args.iter().any(|a| a.contains("webrtc")));
        // 额外参数透传
        assert!(args.contains(&"--js-flags=--max-old-space-size=512".to_string()));
        // 触摸事件需要 autoplay 策略放开
        assert!(args.contains(&"--autoplay-policy=no-user-gesture-required".to_string()));
        // headless 开关：chrome-headless-shell 不需要，完整 Chromium 需要
        assert!(!args.contains(&"--headless=new".to_string()));
        let mut cfg2 = cfg;
        cfg2.headless = true;
        assert!(build_args(&cfg2).contains(&"--headless=new".to_string()));
    }
}
