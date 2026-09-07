//! 引擎：Chromium 进程监管 + CDP 会话 + 看门狗 + 分级自动恢复。
//! ---------------------------------------------------------------------------
//! 线程模型（刻意单引擎线程——CDP 命令天然串行，无锁竞争，内存最低）：
//!   [引擎线程]  启动 Chromium → CDP 连接/页面装配/注入 → 稳态监督循环
//!               （活跃态每 1s 经 CDP 驱动页面 __CPK_TICK__（stopCheck 每 tick
//!                 + actionTick 墙钟门控 ≈ intervalMs，与 Windows 隐藏态看门狗
//!                 同一模型）；每 5s 采样 __CPK_STATE__ + 取走 __CPK_DRAIN__ 诊断
//!                 缓冲。空闲自适应降频：无观看且无操作 CPK_IDLE_AFTER_SEC（默认
//!                 60s）后 tick→CPK_IDLE_TICK_SEC、采样同步放缓——保活动作周期/
//!                 心跳/自动恢复语义不变，任一操作或打开画面流即时恢复 1s/5s）
//!   [HTTP 线程] 回环上报/控制端点；控制请求经 channel 交给引擎线程执行：
//!               快通道（触摸/鼠标/键盘/文本/导航/限帧）fire 即发——在 eval
//!               等待空窗（200ms 节拍）由 InputPump 即时分发（延迟 ≤200ms，
//!               真实云机页 eval 常态秒级也不锁死输入）；慢通道（截图/剪贴
//!               板/流订阅/平台切换）由稳态循环空闲期处理，阻塞等待期间继续泵
//!
//! 恢复分级（对齐 Windows 版思路）：
//!   tick 连续失败 / 状态冻结 / 脚本缺失 → 页面导航回首页
//!   → 10 分钟窗口 3 次无效 / 传输断裂 → 重建 CDP 会话（进程保留，页面不重载）
//!   → 重连无效 / Chromium 退出 / 心跳超龄 → 重启 Chromium（指数退避 5s→300s）

use crate::cdp::{self, Cdp, FrameBox};
use crate::config::{self, Config};
use crate::keepalive;
use crate::logger::Logger;
use crate::util;
use serde_json::{json, Value};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::collections::VecDeque;
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
/// 剪贴板读取：云机页面当前选中文本（含输入框选区）——/copy（云机 → 本机）数据源
pub const CLIP_EXPR: &str = "(function(){try{var s='';try{s=String(document.getSelection())}catch(e){}if(!s){var a=document.activeElement;try{if(a&&(/^(INPUT|TEXTAREA)$/.test(a.tagName))&&('value'in a)&&a.selectionStart!=null){s=String(a.value).slice(a.selectionStart,a.selectionEnd)}}catch(e){}}return JSON.stringify({t:s})}catch(e){return JSON.stringify({t:''})}})()";

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
    /// 视口尺寸（控制页触摸坐标映射与画面宽高比的数据源）
    pub vw: u32,
    pub vh: u32,
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
    /// 当前实时画面帧率（控制面板 /fps 运行时可调）
    pub fps: u32,
    /// 当前画质（控制面板 /quality 运行时可调，10..90）
    pub quality: u32,
    /// 当前采集分辨率百分比（控制面板 /scale 运行时可调，30..100）
    pub scale: u32,
    /// 页面标题（采样周期回读；对齐 Windows 版窗口标题/标题变化日志的可见性）
    pub title: String,
    /// 页面上报的最近一次状态（alive/retry/enter/…/exited/expired）；
    /// 状态迁移供控制页发通知（对齐 Windows 版系统通知）
    pub last_status: String,
    /// 当前监督周期是否处于空闲降频态（tick/采样 eval 已放缓）。
    /// 远程验证降频是否生效：curl /healthz 看 tickIdle
    pub tick_idle: bool,
}

/// 运行时平台全貌（控制面板 /platform 切换后由 SharedState 持有；
/// 引擎的重启/重注入/导航全部以此为单一事实源，cfg 仅提供初值）
#[derive(Clone)]
pub struct PlatformInfo {
    pub platform: String,
    pub label: String,
    pub url: String,
    pub vw: u32,
    pub vh: u32,
}

pub struct SharedState {
    health: Mutex<Health>,
    last_beat_ms: AtomicI64,
    exited: AtomicBool,
    stop: AtomicBool,
    beat_stale_sec: u64,
    fps: AtomicU32,
    quality: AtomicU32,
    scale: AtomicU32,
    /// 最近一次用户活动（控制请求/流订阅）：tick 自适应降频的判定源。
    /// 「活动」定义＝有人在用：触摸/键鼠/导航/设置调整/画面流订阅；
    /// 页面自发事件（心跳/保活动作/弹窗）不算——那正是空闲态要省的开销
    last_activity: Mutex<Instant>,
    /// 空闲降频态回显（healthz tickIdle）
    tick_idle: AtomicBool,
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
            vw: cfg.width,
            vh: cfg.height,
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
            fps: cfg.fps.clamp(1, 60),
            quality: cfg.jpeg_quality.clamp(10, 90),
            scale: cfg.stream_scale_pct.clamp(30, 100),
            title: String::new(),
            last_status: String::new(),
            tick_idle: false,
        };
        Arc::new(SharedState {
            health: Mutex::new(health),
            last_beat_ms: AtomicI64::new(0),
            exited: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            beat_stale_sec: cfg.beat_stale_sec,
            fps: AtomicU32::new(cfg.fps.clamp(1, 60)),
            quality: AtomicU32::new(cfg.jpeg_quality.clamp(10, 90)),
            scale: AtomicU32::new(cfg.stream_scale_pct.clamp(30, 100)),
            last_activity: Mutex::new(Instant::now()),
            tick_idle: AtomicBool::new(false),
        })
    }

    pub fn snapshot(&self) -> Health {
        let mut h = self.health.lock().unwrap().clone();
        h.last_beat_ms = self.last_beat_ms.load(Ordering::Relaxed);
        h.exited = self.exited.load(Ordering::Relaxed);
        h.fps = self.fps.load(Ordering::Relaxed);
        h.quality = self.quality.load(Ordering::Relaxed);
        h.scale = self.scale.load(Ordering::Relaxed);
        h.tick_idle = self.tick_idle.load(Ordering::Relaxed);
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

    /// 当前文档是否为 Chrome 网络错误页（chrome-error://）——
    /// 导航失败探测线程据此丢弃过期结论（探测期间页面已恢复则不覆盖 lastError）
    pub fn page_is_error(&self) -> bool {
        self.health
            .lock()
            .map(|g| g.page_url.starts_with("chrome-error://"))
            .unwrap_or(false)
    }
    pub fn set_restarts(&self, n: u32) { self.update(|h| h.restarts = n); }
    /// 运行时调整实时画面帧率（控制面板 /fps；跨 CDP 重建保留）
    pub fn set_fps(&self, n: u32) { self.fps.store(n.clamp(1, 60), Ordering::Relaxed); }
    pub fn fps(&self) -> u32 { self.fps.load(Ordering::Relaxed) }
    /// 运行时调整画质/采集缩放（控制面板 /quality /scale；跨 CDP 重建保留）
    pub fn set_quality(&self, n: u32) { self.quality.store(n.clamp(10, 90), Ordering::Relaxed); }
    pub fn quality(&self) -> u32 { self.quality.load(Ordering::Relaxed) }
    pub fn set_scale(&self, n: u32) { self.scale.store(n.clamp(30, 100), Ordering::Relaxed); }
    pub fn scale(&self) -> u32 { self.scale.load(Ordering::Relaxed) }
    pub fn bump_reloads(&self) { self.update(|h| h.reloads += 1); }
    pub fn set_dialogs(&self, n: u32) { self.update(|h| h.dialogs = n); }
    pub fn set_chrome_version(&self, v: &str) { self.update(|h| h.chrome_version = v.into()); }
    pub fn set_last_error(&self, e: &str) { self.update(|h| h.last_error = e.into()); }
    /// 页面标题更新（采样周期回读；控制页状态显示）
    pub fn set_title(&self, s: &str) { self.update(|h| if h.title != s { h.title = s.into(); }); }
    /// 当前平台全貌（首页 URL/视口随切换实时生效）
    pub fn platform(&self) -> PlatformInfo {
        self.health
            .lock()
            .map(|g| PlatformInfo {
                platform: g.platform.clone(),
                label: g.platform_label.clone(),
                url: g.home_uri.clone(),
                vw: g.vw,
                vh: g.vh,
            })
            .unwrap_or_else(|_| PlatformInfo {
                platform: "mobile".into(),
                label: config::PLATFORM_MOBILE_LABEL.into(),
                url: config::PLATFORM_MOBILE_URI.into(),
                vw: config::PLATFORM_MOBILE_W,
                vh: config::PLATFORM_MOBILE_H,
            })
    }
    /// 运行时切换平台（/platform）：更新 Health 平台字段（healthz 即刻可见）。
    /// 未知平台返回 false（HTTP 层已校验，此处兑底）
    pub fn set_platform(&self, platform: &str) -> bool {
        match config::platform_profile(platform) {
            Some((label, url, vw, vh)) => {
                self.update(|h| {
                    h.platform = platform.to_string();
                    h.platform_label = label.to_string();
                    h.home_uri = url.to_string();
                    h.vw = vw;
                    h.vh = vh;
                });
                true
            }
            None => false,
        }
    }
    /// 页面上报状态记录（/report；控制页据此检测状态迁移并发通知）
    pub fn set_status(&self, s: &str) { self.update(|h| h.last_status = s.into()); }
    pub fn touch_beat(&self) { self.last_beat_ms.store(util::now_ms(), Ordering::Relaxed); }
    pub fn mark_exited(&self) { self.exited.store(true, Ordering::Relaxed); }
    pub fn request_stop(&self) { self.stop.store(true, Ordering::Relaxed); }
    pub fn stopping(&self) -> bool { self.stop.load(Ordering::Relaxed) }
    // —— tick 自适应降频 ——
    /// 用户活动登记（触摸/键鼠/导航/设置/流订阅都会调用；引擎循环判定空闲用）
    pub fn touch_activity(&self) {
        if let Ok(mut g) = self.last_activity.lock() { *g = Instant::now(); }
    }
    /// 距最近一次用户活动的时长
    pub fn activity_age(&self) -> Duration {
        self.last_activity.lock().map(|g| g.elapsed()).unwrap_or_default()
    }
    pub fn set_tick_idle(&self, v: bool) { self.tick_idle.store(v, Ordering::Relaxed); }
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
        "vw": h.vw,
        "vh": h.vh,
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
        "fps": h.fps,
        "quality": h.quality,
        "scale": h.scale,
        "title": h.title,
        "lastStatus": h.last_status,
        "tickIdle": h.tick_idle,
    })
}

// ---------------------------------------------------------------------------
// 控制请求（HTTP 控制端点 → 引擎线程串行执行）
// ---------------------------------------------------------------------------

pub enum ControlRequest {
    Screenshot { reply: Sender<Result<Vec<u8>, String>> },
    Tap { x: f64, y: f64, reply: Sender<Result<(), String>> },
    Swipe { x1: f64, y1: f64, x2: f64, y2: f64, reply: Sender<Result<(), String>> },
    /// 实时触摸流（/touch）：按下/移动/抬起/取消逐点直通 CDP Input.dispatchTouchEvent
    /// ——页面拖动跟手（不再「松手才补发整段滑动」）；move 高频，仅按下留日志、
    /// 超时收紧防积压（渲染卡顿时移动点丢弃链路继续，不占引擎 5s）。
    /// points = 本次变动的触点（Chromium 逐点语义：start 新增 / move 移动 /
    /// end 列出释放的触点，空=整组释放）——支撑双指缩放等多点手势
    Touch {
        phase: String,
        points: Vec<TouchPoint>,
        reply: Sender<Result<(), String>>,
    },
    /// 真实鼠标事件（/mouse）：move/press/release/wheel（全键位/拖动/双击/滚轮）
    Mouse {
        action: String,
        x: f64,
        y: f64,
        button: String,
        buttons: u32,
        click_count: u32,
        dx: f64,
        dy: f64,
        modifiers: u32,
        reply: Sender<Result<(), String>>,
    },
    /// 键盘事件（/kbd）：rawKeyDown/keyDown/keyUp 全字段直通
    /// （t=down 且有 text → keyDown，否则 rawKeyDown；t=up → keyUp）
    KeyEvent {
        typ: String,
        key: String,
        code: String,
        vk: u32,
        text: String,
        modifiers: u32,
        location: u32,
        auto_repeat: bool,
        reply: Sender<Result<(), String>>,
    },
    /// 读取云机选中文本（/clip：云机 → 本机剪贴板的数据源）
    ClipGet { reply: Sender<Result<String, String>> },
    /// 运行时调整实时画面帧率（控制面板「设置 → 帧率」）
    SetFps { fps: u32, reply: Sender<Result<(), String>> },
    /// 运行时调整实时画面 JPEG 画质（控制面板「设置 → 画质」；quality 是
    /// startScreencast 参数，cast 活动且有观众时 stop+start 重建生效）
    SetQuality { quality: u32, reply: Sender<Result<(), String>> },
    /// 运行时调整采集分辨率百分比（控制面板「设置 → 分辨率」；同上重建生效）
    SetScale { scale_pct: u32, reply: Sender<Result<(), String>> },
    /// 切换平台（/platform）：更新共享状态（healthz 即刻回显）+ 重启云机实例
    /// （重注入平台对应 CFG 的保活脚本 + 新视口 + 导航新首页——与冷启动同路径，
    /// 登录态在同一 Profile 里两平台共存，切换后已登过的平台无需重登）
    SetPlatform { platform: String, reply: Sender<Result<(), String>> },
    TypeText { text: String, reply: Sender<Result<(), String>> },
    Key { key: String, reply: Sender<Result<(), String>> },
    Navigate { url: String, reply: Sender<Result<(), String>> },
    Reload { reply: Sender<Result<(), String>> },
    /// 实时画面流订阅（/stream.mjpg → 引擎交出帧信箱 + 开启 Page.startScreencast）
    ScreencastAttach { reply: Sender<Result<(u32, FrameBox), String>> },
    /// 取消订阅（最后一个订阅者离开时引擎自动 Page.stopScreencast）
    ScreencastDetach { id: u32, reply: Sender<Result<(), String>> },
}

/// 触摸点（id 由控制页分配，1..=10）
#[derive(Clone, Copy)]
pub struct TouchPoint {
    pub x: f64,
    pub y: f64,
    pub id: i64,
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
    /// 平台切换（/platform）：重启 Chromium 并按新平台重注入/导航/视口
    PlatformRestart,
    /// 收到停止信号
    Stop,
}

/// 引擎重建期（CDP 重连/浏览器重启）快速失败积压的控制请求：
/// 立刻回 Err 而不是让 HTTP 层干等 20s 超时——控制页输入通道毫秒级感知
/// 「引擎忙」并提示，而不是整页「点不动」。
/// 请求本就无法送达（WS 已断/进程已死），快速失败才是正确语义；
/// 旧版在这些阶段不清空队列，触摸/导航请求全部压到 20s 超时，
/// 表现为「点了回首页后再也点不动」。
fn drain_ctrl_fail(ctrl_rx: &Receiver<ControlRequest>, reason: &str) {
    while let Ok(req) = ctrl_rx.try_recv() {
        fail_request(req, reason);
    }
}

/// 单个控制请求快速失败（drain 与待机循环共用）：按变体回 Err
fn fail_request(req: ControlRequest, reason: &str) {
    let r = reason.to_string();
    match req {
        ControlRequest::Screenshot { reply } => { let _ = reply.send(Err(r)); }
        ControlRequest::Tap { reply, .. } => { let _ = reply.send(Err(r)); }
        ControlRequest::Swipe { reply, .. } => { let _ = reply.send(Err(r)); }
        ControlRequest::Touch { reply, .. } => { let _ = reply.send(Err(r)); }
        ControlRequest::Mouse { reply, .. } => { let _ = reply.send(Err(r)); }
        ControlRequest::KeyEvent { reply, .. } => { let _ = reply.send(Err(r)); }
        ControlRequest::ClipGet { reply } => { let _ = reply.send(Err(r)); }
        ControlRequest::SetFps { reply, .. } => { let _ = reply.send(Err(r)); }
        ControlRequest::SetQuality { reply, .. } => { let _ = reply.send(Err(r)); }
        ControlRequest::SetScale { reply, .. } => { let _ = reply.send(Err(r)); }
        ControlRequest::SetPlatform { reply, .. } => { let _ = reply.send(Err(r)); }
        ControlRequest::TypeText { reply, .. } => { let _ = reply.send(Err(r)); }
        ControlRequest::Key { reply, .. } => { let _ = reply.send(Err(r)); }
        ControlRequest::Navigate { reply, .. } => { let _ = reply.send(Err(r)); }
        ControlRequest::Reload { reply } => { let _ = reply.send(Err(r)); }
        ControlRequest::ScreencastAttach { reply } => { let _ = reply.send(Err(r)); }
        ControlRequest::ScreencastDetach { reply, .. } => { let _ = reply.send(Err(r)); }
    }
}

/// 退避/重试睡眠 + 持续快速失败积压请求（200ms 切片，睡眠期间请求不积压）
fn sleep_drain(dur: Duration, ctrl_rx: &Receiver<ControlRequest>, reason: &str) {
    let deadline = Instant::now() + dur;
    loop {
        drain_ctrl_fail(ctrl_rx, reason);
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        thread::sleep(left.min(Duration::from_millis(200)));
    }
    drain_ctrl_fail(ctrl_rx, reason);
}

/// 平台待机等待（启动时平台留空）：不启动 Chromium，直到控制页经 /platform
/// 选择平台。期间其他控制请求（截图/触摸/流订阅…）快速失败并明确提示
/// 先选平台，而不是挂 20s 超时。
/// 返回 true = 平台已选择（外层重启循环按新平台启动）；false = 停止信号。
fn wait_platform(
    shared: &Arc<SharedState>,
    ctrl_rx: &Receiver<ControlRequest>,
    logger: &Arc<Logger>,
) -> bool {
    loop {
        if shared.stopping() {
            return false;
        }
        while let Ok(req) = ctrl_rx.try_recv() {
            match req {
                ControlRequest::SetPlatform { platform, reply } => {
                    if shared.set_platform(&platform) {
                        logger.log(0, "sys", &format!("平台已选择 → {platform}，启动云机实例"));
                        let _ = reply.send(Ok(()));
                        return true;
                    }
                    let _ = reply.send(Err(format!("未知平台：{platform}")));
                }
                ControlRequest::SetFps { fps, reply } => {
                    // 待机期同样接受帧率设置：值入共享状态（下次 CDP 装配
                    // 恢复 + 稳态循环每周期同步）——不再因引擎待机被拒
                    shared.set_fps(fps);
                    logger.log(1, "sys", &format!("实时画面帧率设为 {fps}（引擎待机，启动后生效）"));
                    let _ = reply.send(Ok(()));
                }
                ControlRequest::SetQuality { quality, reply } => {
                    // 待机期同样接受：值入共享状态，下次 CDP 装配自然用新参数
                    shared.set_quality(quality);
                    logger.log(1, "sys", &format!("实时画面画质设为 {quality}（引擎待机，启动后生效）"));
                    let _ = reply.send(Ok(()));
                }
                ControlRequest::SetScale { scale_pct, reply } => {
                    shared.set_scale(scale_pct);
                    logger.log(1, "sys", &format!("实时画面采集分辨率设为 {scale_pct}%（引擎待机，启动后生效）"));
                    let _ = reply.send(Ok(()));
                }
                other => {
                    fail_request(other, "平台未选择：请先在控制页「设置→平台」选择移动/联通");
                }
            }
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn engine_loop(cfg: &Config, report_port: u16, logger: Arc<Logger>, shared: Arc<SharedState>, ctrl_rx: Receiver<ControlRequest>) {
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
        // 每轮取当前平台全貌：/platform 切换后重启路径据此换视口/脚本/首页
        // （cfg 仅提供启动初值；SharedState 为运行时单一事实源）
        let cur = shared.platform();

        // —— 0. 平台未选择：待机（不启动 Chromium；控制页「设置→平台」
        //    选择后经 /platform 唤醒 → 回到循环顶按新平台全貌启动）——
        if cur.platform.is_empty() {
            shared.set_browser("idle");
            shared.set_page("");
            logger.log(
                0,
                "sys",
                "平台未选择，引擎待机（不加载页面；控制页「设置→平台」选择移动/联通后自动启动）",
            );
            if wait_platform(&shared, &ctrl_rx, &logger) {
                continue 'outer;
            }
            kill_child(&mut child, &logger);
            return;
        }

        let script = keepalive::build_init_script_for(&cur.platform, &cur.url, cfg, report_port);

        // —— 1. Chromium 进程 ——
        if child.is_none() {
            shared.set_browser("starting");
            match launch_chrome(cfg, cur.vw, cur.vh, &logger) {
                Ok(c) => {
                    child = Some(c);
                }
                Err(e) => {
                    logger.log(0, "error", &format!("Chromium 启动失败：{e}"));
                    shared.set_last_error(&e);
                    shared.set_browser("failed");
                    sleep_drain(Duration::from_secs(backoff), &ctrl_rx, "浏览器启动失败，重试中");
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
                    sleep_drain(Duration::from_secs(backoff), &ctrl_rx, "DevTools 未就绪，重启中");
                    backoff = (backoff * 2).min(300);
                    continue 'outer;
                }
            };
            cdp_port = port;
            match attach_all(cfg, port, &script, &cur.url, &shared, &logger, true) {
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
                    sleep_drain(Duration::from_secs(backoff), &ctrl_rx, "CDP 装配失败，重启中");
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
            SteadyOutcome::PlatformRestart => {
                // /platform 切换：重启 Chromium（新视口）+ 新平台脚本重注入 +
                // 导航新首页（外层循环顶部重读 shared.platform() 全部生效）。
                // 不退避：用户主动操作，立即重启；不给 backoff 累加
                if let Some(mut c) = cdp.take() {
                    c.close();
                }
                kill_child(&mut child, &logger);
                shared.set_browser("stopped");
                shared.set_page("loading");
                shared.set_last_error("");
                sleep_drain(Duration::from_millis(300), &ctrl_rx, "平台切换中，云机实例重启");
                continue 'outer;
            }
            SteadyOutcome::Reattach => {
                logger.log(0, "sys", "CDP 传输断裂，重建会话（Chromium 进程保留，页面不重载）");
                // 立刻清空积压：触摸/导航/订阅请求全部快速失败（毫秒级反馈），
                // 不让任何请求挂着 20s 超时卡死控制页输入通道
                drain_ctrl_fail(&ctrl_rx, "引擎重建 CDP 会话中，稍后自动恢复");
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
                    match attach_all(cfg, cdp_port, &script, &cur.url, &shared, &logger, false) {
                        Ok((mut c, s)) => {
                            // 探测当前文档是否已有脚本：无则导航（新文档经 addScript 自动注入）
                            let has = cdp::eval_string(&mut c, &s, PROBE_EXPR, 5000)
                                .map(|v| v == "y")
                                .unwrap_or(false);
                            if !has {
                                logger.log(1, "nav", "重连后当前文档无保活脚本，导航回首页");
                                // 发后即忘：不给引擎线程挂 20s 阻塞调用，恢复由采样回看
                                c.fire("Page.navigate", json!({ "url": cur.url }), Some(&s));
                            }
                            cdp = Some(c);
                            session = s;
                            ok = true;
                            break;
                        }
                        Err(_) => {}
                    }
                    sleep_drain(Duration::from_secs(3), &ctrl_rx, "引擎重建 CDP 会话中，稍后自动恢复");
                }
                if !ok {
                    logger.log(0, "sys", "CDP 会话重建耗尽，重启 Chromium");
                    kill_child(&mut child, &logger);
                    shared.set_browser("stopped");
                    sleep_drain(Duration::from_secs(backoff), &ctrl_rx, "浏览器重启中，稍后自动恢复");
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
                sleep_drain(Duration::from_secs(backoff), &ctrl_rx, "浏览器重启中，稍后自动恢复");
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
    /// chrome-error 错误页（首页导航失败）退避重试状态。
    /// 生命周期独立于 reloads 恢复窗口：重启浏览器修不了网络，绝不升级重启
    nav_err_active: bool,
    nav_backoff: Duration,
    nav_next_retry: Option<Instant>,
    /// 帧流统计窗口（30s）：起点 + 期初计数（收帧/字节/解码）。
    /// 诊断「传输画面 CPU/帧率」用：收帧 fps ≈ 合成器内容实际出帧率
    /// （ack 门控生效时 ≤ 目标帧率，远低于目标＝页面内容变化慢），
    /// 解码 fps ≈ 推送给观看端的帧率。
    cast_stat_at: Instant,
    cast_stat_prev: (u64, u64, u64),
}

/// tick 自适应降频决策（纯函数）：空闲态的 (tick 周期, 采样周期)。
/// Some = 允许降频；None = 该配置下降频被安全规则否决（维持活跃 1s/5s）。
///
/// 空闲 CPU 的两个常驻源是每 1s 的 tick eval 与每 5s 的采样 eval（每次都要
/// 唤醒 Chrome 渲染主线程跑 JS + JSON 往返）。降频规则：
///  - tick → idle_tick_sec（CPK_IDLE_TICK_SEC，默认 5）
///  - 采样 → 3 × 动作周期（动作周期 = max(interval_ms, tick)——注入脚本
///    actionTick 墙钟门控下 ticks 每 ≈动作周期前进一次；采样窗 ≥3 倍保证
///    每窗必见 ticks 前进，frozen 冻结检测不误报）
///  - 安全钳：页面级冻结恢复阶梯（frozen_reload × 采样周期）必须先于心跳
///    硬重启（beat_stale_sec）触发，否则整体放弃降频——绝不为省 CPU 打破
///    「先页面级恢复、再硬重启」的分级语义
/// 保活语义不变：保活动作周期恒 ≈ interval_ms（墙钟门控）、心跳照发、
/// 自动恢复照常（恢复在途时循环侧会暂退回活跃节奏，见 steady_loop）。
fn idle_periods(cfg: &Config) -> Option<(Duration, Duration)> {
    if cfg.idle_after_sec == 0 {
        return None; // 显式关闭
    }
    let tick_s = cfg.idle_tick_sec.max(1);
    let interval_s = ((cfg.interval_ms as u64) / 1000).max(1);
    let action_s = tick_s.max(interval_s);
    let sample_s = (action_s * 3).max(5);
    // 恢复阶梯安全：frozen_reload × 采样 ≥ 心跳硬重启阈值 → 否决降频
    if (cfg.frozen_reload as u64) * sample_s >= cfg.beat_stale_sec {
        return None;
    }
    Some((Duration::from_secs(tick_s), Duration::from_secs(sample_s)))
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
        nav_err_active: false,
        nav_backoff: Duration::from_secs(5),
        nav_next_retry: None,
        cast_stat_at: Instant::now(),
        cast_stat_prev: (0, 0, 0),
    };
    // 输入泵：慢 eval（tick 5s 超时/采样 8s 超时）的等待空窗里即时分发
    // 快通道请求（触摸/鼠标/键盘/导航/限帧），慢通道暂存由下方循环顶处理。
    // 真实云机页（WebRTC/重 JS）eval 常态秒级——没有这个泵，点击/限帧
    // 请求要等 eval 结束才被看一眼（最长 ~13s），用户侧即「点击没用」
    let mut pump = InputPump {
        ctrl_rx,
        shared,
        logger,
        session,
        pending: VecDeque::new(),
        tap_probe: None,
    };
    let mut next_tick = Instant::now();
    let mut next_sample = Instant::now() + Duration::from_secs(5);
    let mut last_progress = Instant::now();
    let mut last_dialog_count = 0u32;
    let mut rescue_log_at: Option<Instant> = None; // 自愈日志限流（静态页无帧属正常）
    // —— tick 自适应降频（空闲 CPU 治理）——
    // 空闲判定＝无画面订阅 && 无用户操作 ≥ idle_after && 无恢复在途；
    // 恢复在途（not_installed/tick_fails/frozen/nav_err 任一非零）＝页面正被
    // 救治，维持活跃节奏直到恢复完成；空闲中页面出状况 → 恢复计数增长
    // 自动暂退活跃节奏（下一循环即恢复密集监督，自愈闭环）
    let idle_plan = idle_periods(cfg);
    let mut tick_idle_mode = false;
    shared.set_page("loading");

    loop {
        if shared.stopping() {
            return SteadyOutcome::Stop;
        }
        // 软限帧值同步：/fps 现由 HTTP 层直写共享状态，引擎每周期拉齐
        // CDP（即使通知请求在待机/重启窗口丢失，也在 1 个周期内生效）
        cdp.set_screencast_fps(shared.fps(), session);
        // 画质/采集分辨率周期拉齐（与 /fps 同模式）：SetQuality/SetScale
        // 的 ControlRequest 转发在引擎忙/重启窗口丢失时，HTTP 层直写的
        // SharedState 值在这里 1 个周期内自愈——值变化且 cast 在播有观众
        // 则 restart_cast 重建（首帧即按新画质/尺寸编码，画面闪动可见）；
        // 幂等：值未变时 sync 返回 false，不会每周期空转重建
        {
            let p = shared.platform();
            if cdp.sync_cast_tuning(shared.quality(), cast_max_for(shared.scale(), p.vw, p.vh))
                && cdp.cast_active()
                && cdp.has_sinks()
            {
                cdp.restart_cast(session);
                logger.log(
                    1,
                    "sys",
                    &format!("cast 参数周期同步：画质 {} / 分辨率 {}% 重建生效", shared.quality(), shared.scale()),
                );
            }
        }
        // fire 类命令被 Chrome 拒绝的留痕（此前被当「无关应答」静默丢弃：
        // startScreencast 导航窗口被拒、dispatchTouchEvent 点列表违规被拒等）
        for e in cdp.take_error_replies() {
            logger.log(0, "error", &e);
        }
        // 实时流自愈：cast 在播且有订阅者但 6s 无帧（导航换档/渲染器切换
        // 后 Chromium 单方面停发帧）→ 主动重发 startScreencast 拉活。
        // 此前死流只能等消费侧 30s 超时关流重连（「选平台后卡等待」「回
        // 首页后画面冻结拖不动」）——现在引擎侧秒级自愈，无需用户刷新
        if cdp.cast_rescue(session) {
            // 静态页正常不发帧：重发本身无害（重订阅），但每 6s 一条日志会刷屏
            // ——限流 60s 一条
            let quiet = rescue_log_at
                .map(|t| t.elapsed() < Duration::from_secs(60))
                .unwrap_or(false);
            if !quiet {
                rescue_log_at = Some(Instant::now());
                logger.log(1, "sys", "实时流 6s 无帧，已 stop+start 重发 screencast 自愈（静态页无帧属正常）");
            }
        }
        // —— 帧流统计（30s 窗口，仅观看中采样；静默窗口不刷日志）——
        // 收帧 = Chrome 实际采集+编码数（CPU 正相关；收帧≈页面内容变化率：
        // ack 门控生效时收帧≤目标帧率，远低于目标＝内容本身变化慢，
        // 非「帧数上不去」故障）；解码推送 = 观看端帧率。
        if cdp.has_sinks() && stats.cast_stat_at.elapsed() >= Duration::from_secs(30) {
            let (recv, bytes, decoded) = cdp.cast_stats();
            let (pr, pb, pd) = stats.cast_stat_prev;
            let secs = stats.cast_stat_at.elapsed().as_secs_f64().max(0.001);
            if recv > pr {
                logger.log(
                    1,
                    "sys",
                    &format!(
                        "实时流统计({:.0}s): Chrome 出帧 {:.1}/s {:.0}KB/s → 解码推送 {:.1}/s",
                        secs,
                        (recv - pr) as f64 / secs,
                        (bytes - pb) as f64 / secs / 1024.0,
                        (decoded - pd) as f64 / secs,
                    ),
                );
            }
            stats.cast_stat_at = Instant::now();
            stats.cast_stat_prev = (recv, bytes, decoded);
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
        // 控制请求（每个监督周期清空一次）：先处理 eval 等待期间暂存的慢请求，
        // 再捞新请求。快请求也走 handle_control（快通道 arm 是 fire 即发，
        // 不阻塞）；返回 true = 平台切换 → 重启实例生效
        let mut platform_switched = false;
        loop {
            let req = if let Some(r) = pump.pending.pop_front() {
                r
            } else {
                match ctrl_rx.try_recv() {
                    Ok(r) => r,
                    Err(_) => break,
                }
            };
            match handle_control(cdp, session, shared, req, logger, &mut pump) {
                Ok(true) => platform_switched = true,
                Ok(false) => {}
                Err(e) => {
                    logger.log(0, "error", &format!("控制请求处理失败：{e}"));
                    if e.starts_with("WS:") {
                        return SteadyOutcome::Reattach;
                    }
                }
            }
        }
        if platform_switched {
            return SteadyOutcome::PlatformRestart;
        }

        // —— 点击命中探针（诊断）：最近一次触摸按下坐标 → elementFromPoint
        //    探落点元素落日志。「点击没反应」时日志直接给出命中目标（骨架
        //    屏/透明弹层/真实按钮），不再盲猜。连点合并为最新一次；eval
        //    慢（真实云机页常态秒级）时失败静默（系统性故障另有错误留痕）——
        //    等待期间继续泵输入，不占触摸通道 ——
        if let Some((px, py)) = pump.tap_probe.take() {
            let expr = String::from("(function(){try{var e=document.elementFromPoint(")
                + &px.to_string()
                + ","
                + &py.to_string()
                + ");var s=e?e.tagName+(e.id?'#'+e.id:'')+'.'+String(e.className).slice(0,70):'null';return 'hit:'+s}catch(_){return 'hit:err'}})()";
            if let Ok(h) = eval_string_pumped(cdp, session, &expr, 3000, &mut pump) {
                if !h.is_empty() {
                    logger.log(1, "click", &format!("({:.0},{:.0}) → {}", px, py, h));
                }
            }
        }

        // —— tick 自适应降频决策（每周期重估：任一活动源出现即退出空闲）——
        let recovering = stats.not_installed > 0
            || stats.tick_fails > 0
            || stats.frozen > 0
            || stats.nav_err_active;
        let want_idle = idle_plan.is_some()
            && !cdp.has_sinks()
            && !recovering
            && shared.activity_age() >= Duration::from_secs(cfg.idle_after_sec);
        if want_idle != tick_idle_mode {
            tick_idle_mode = want_idle;
            shared.set_tick_idle(want_idle);
            if want_idle {
                let (t, s) = idle_plan.unwrap_or((Duration::from_secs(1), Duration::from_secs(5)));
                logger.log(
                    1,
                    "sys",
                    &format!(
                        "无观看/无操作 {}s，tick 降频 1s→{}s、采样 5s→{}s（保活动作周期不变，操作/观看即时恢复）",
                        cfg.idle_after_sec, t.as_secs(), s.as_secs()
                    ),
                );
            } else {
                logger.log(1, "sys", "检测到观看/操作，tick 恢复 1s、采样 5s");
            }
            // 周期立即重排：恢复态马上 tick+采样确认页面状况，降频态按新周期起步
            next_tick = Instant::now();
            next_sample = Instant::now();
        }
        let (tick_period, sample_period) = if tick_idle_mode {
            idle_plan.unwrap_or((Duration::from_secs(1), Duration::from_secs(5)))
        } else {
            (Duration::from_secs(1), Duration::from_secs(5))
        };

        let now = Instant::now();
        // —— tick：驱动页面双定时器（活跃 1s / 空闲 idle_tick_sec） ——
        if now >= next_tick {
            next_tick = now + tick_period;
            let t0 = Instant::now();
            let r = eval_string_pumped(cdp, session, TICK_EXPR, 5000, &mut pump);
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

        // —— 采样：读状态 + 取走诊断缓冲（活跃 5s / 空闲 3×动作周期） ——
        let now = Instant::now();
        if now >= next_sample {
            next_sample = now + sample_period;
            let t1 = Instant::now();
            let r2 = eval_string_pumped(cdp, session, SNAPSHOT_EXPR, 8000, &mut pump);
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
                            // 页面标题回读（控制页显示；对齐 Windows 版标题可见性）
                            shared.set_title(snap.get("title").and_then(|x| x.as_str()).unwrap_or(""));
                            if snap.get("wasExited").and_then(|x| x.as_bool()) == Some(true) {
                                shared.mark_exited();
                            }
                            // —— chrome-error 错误页识别：注入脚本在错误页上照常 tick 且
                            //    readyState=complete，常规监督项全部「正常」——不显式
                            //    识别则 page 恒报 ok，导航失败被白屏掩盖 ——
                            let on_err_page = url.starts_with("chrome-error://");
                            if on_err_page {
                                if let Err(e) = nav_error_step(cdp, session, cfg, shared, &mut stats, logger) {
                                    if e.starts_with("WS:") {
                                        return SteadyOutcome::Reattach;
                                    }
                                }
                            } else if stats.nav_err_active {
                                stats.nav_err_active = false;
                                stats.nav_backoff = Duration::from_secs(5);
                                stats.nav_next_retry = None;
                                shared.set_last_error("");
                                logger.log(1, "nav", "导航恢复：页面已离开 chrome-error 错误页");
                            }
                            // tick 前进即渲染进程存活（错误页上脚本照常 tick）：
                            // 心跳/进度照续——导航失败≠进程死亡，不许误触发硬重启
                            if ticks != stats.last_ticks && ticks >= 0 {
                                stats.last_ticks = ticks;
                                stats.frozen = 0;
                                stats.not_installed = 0;
                                shared.touch_beat();
                                last_progress = Instant::now();
                                if !on_err_page && snap.get("ready").and_then(|x| x.as_str()) == Some("complete") {
                                    shared.set_page("ok");
                                }
                            } else if !on_err_page {
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

        // —— 空闲期改睡为泵：分发实时画面帧（screencast）/事件，断流走重连 ——
        // 泵预算扩到「距下个 tick 的剩余时间」（旧版固定 100ms 是弱机 1fps 瓶颈
        // 之一：帧读取窗口仅占循环节拍 ~10%）；泵内 poll 50ms 无消息即提前返回
        // 交还循环顶——控制命令延迟仍 ≤50ms。
        // 上限 200ms：帧密集（60fps）时每 200ms 强制回循环顶捞控制命令——
        // 触摸/导航不被帧处理洪流（base64+JSON 解码）挤到秒级延迟
        let pump_budget = next_tick
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(200))
            .max(Duration::from_millis(50));
        if let Err(e) = cdp.pump_events(pump_budget) {
            logger.log(0, "sys", &format!("CDP 事件泵传输失败：{e}"));
            return SteadyOutcome::Reattach;
        }
    }
}

/// chrome-error 错误页处理（首页导航失败的自动恢复）：
///  - page 状态如实报 nav-error（错误页上脚本照常 tick，常规监督项不可见故障）
///  - 独立退避重导航 5s→10s→…→60s 封顶（不进 reloads 恢复窗口：
///    重启浏览器修不了网络，升级只会白白重建会话）
///  - 每次重试顺带异步 DNS/TCP 探测（独立线程，getaddrinfo 可阻塞 ~20s，
///    绝不挡引擎监督循环），结论写 lastError 供控制页/healthz 直接可见
fn nav_error_step(
    cdp: &mut Cdp,
    session: &str,
    _cfg: &Config,
    shared: &Arc<SharedState>,
    stats: &mut Stats,
    logger: &Arc<Logger>,
) -> Result<(), String> {
    let url = shared.platform().url;
    shared.set_page("nav-error");
    if !stats.nav_err_active {
        stats.nav_err_active = true;
        logger.log(
            1,
            "nav",
            "页面停在 chrome-error 错误页：首页导航失败（网络/DNS），进入退避自动重试",
        );
    }
    let now = Instant::now();
    if stats.nav_next_retry.map(|t| now >= t).unwrap_or(true) {
        stats.nav_next_retry = Some(now + stats.nav_backoff);
        stats.nav_backoff = (stats.nav_backoff * 2).min(Duration::from_secs(60));
        logger.log(
            1,
            "nav",
            &format!("导航重试（退避 {}s）：{}", stats.nav_backoff.as_secs(), url),
        );
        spawn_nav_probe(&url, shared, logger);
        // 发后即忘：导航命令已投递；失败与否由下轮采样看 URL 判定（同步等应答
        // 会在 DNS 不通时占住引擎线程最长 15s，把实时流订阅/触摸全部压在队尾）
        cdp.fire("Page.navigate", json!({ "url": url }), Some(session));
        Ok(())
    } else {
        Ok(())
    }
}

/// 网络层探测（独立线程）：区分容器 DNS 不通 / TCP 不通 / 均正常但站点层拒绝。
/// 探测期间页面已恢复（page_is_error=false）则丢弃结论，不覆盖恢复态。
fn spawn_nav_probe(url: &str, shared: &Arc<SharedState>, logger: &Arc<Logger>) {
    let url = url.to_string();
    let shared = shared.clone();
    let logger = logger.clone();
    let _ = thread::Builder::new()
        .name("cpk-net-probe".into())
        .spawn(move || {
            let msg = match util::host_port_of(&url) {
                Some((host, port)) => util::net_probe(&host, port),
                None => "CPK_URL 无法解析出主机（非法 URL？）".into(),
            };
            if shared.page_is_error() {
                shared.set_last_error(&format!("导航失败：{msg}"));
                logger.log(1, "probe", &format!("导航失败网络探测：{msg}"));
            }
        });
}

fn nav_home(
    cdp: &mut Cdp,
    session: &str,
    _cfg: &Config,
    shared: &Arc<SharedState>,
    stats: &mut Stats,
    logger: &Arc<Logger>,
) -> Result<(), String> {
    let url = shared.platform().url;
    stats.reloads_window += 1;
    shared.bump_reloads();
    shared.set_page("reloading");
    logger.log(1, "nav", &format!("恢复性导航 {url}"));
    // 发后即忘：不等应答（理由同 nav_error_step），成败由采样周期回看 URL
    cdp.fire("Page.navigate", json!({ "url": url }), Some(session));
    Ok(())
}

// ---------------------------------------------------------------------------
// 控制请求执行：快通道（fire 即发）与慢通道（需等 CDP 应答）分离。
// 真实云机页（WebRTC 视频/重 JS）上 tick/采样 eval 常态秒级——旧结构在
// eval 等待期间完全不服务控制通道，点击/限帧请求最长 ~13s 无人应答
// （点击「没用」、/fps 超时的共同根因）。现在 eval 等待空窗（200ms 节拍）
// 由 InputPump 即时分发快通道请求；慢通道暂存待稳态循环空闲期处理，
// 其自身的阻塞等待也经 call_pumped 继续泵入新到的快通道请求。
// ---------------------------------------------------------------------------

/// 快通道判别：fire 即发（不等 CDP 应答，占用 <0.1ms）的请求类型。
/// 在 Cdp::call_pumped 等待空窗里穿插发送不影响应答 id 配对。
fn is_fast(req: &ControlRequest) -> bool {
    matches!(
        req,
        ControlRequest::Touch { .. }
            | ControlRequest::Mouse { .. }
            | ControlRequest::KeyEvent { .. }
            | ControlRequest::SetFps { .. }
            | ControlRequest::SetQuality { .. }
            | ControlRequest::SetScale { .. }
            | ControlRequest::TypeText { .. }
            | ControlRequest::Key { .. }
            | ControlRequest::Navigate { .. }
            | ControlRequest::Reload { .. }
            | ControlRequest::ScreencastDetach { .. }
    )
}

/// eval 等待期间的输入泵：快通道即时分发（延迟 ≤200ms），慢通道暂存
struct InputPump<'a> {
    ctrl_rx: &'a Receiver<ControlRequest>,
    shared: &'a Arc<SharedState>,
    logger: &'a Arc<Logger>,
    session: &'a str,
    pending: VecDeque<ControlRequest>,
    /// 待探针的最近触摸按下坐标（诊断：elementFromPoint 看点击命中元素，
    /// 稳态循环采样落日志——「点击没反应」时直接给出落点目标）
    tap_probe: Option<(f64, f64)>,
}

impl<'a> InputPump<'a> {
    /// 排空控制通道：快通道即时经 fire 分发，慢通道入暂存队列。
    /// 只在 Cdp::call_pumped 的等待空窗内被调用（此时 WS 无消息在途读取，
    /// 穿插发送 fire 类消息不影响应答 id 配对）。WS 断裂仅记日志——
    /// 外层 eval 的读循环会撞到同一断连并统一走重建路径。
    fn drain(&mut self, cdp: &mut Cdp) {
        while let Ok(req) = self.ctrl_rx.try_recv() {
            if !is_fast(&req) {
                self.pending.push_back(req);
                continue;
            }
            if let Err(e) = dispatch_input(cdp, self.session, self.shared, req, self.logger, &mut self.tap_probe) {
                self.logger.log(0, "error", &format!("输入泵分发失败：{e}"));
            }
        }
    }
}

/// 可泵 eval（与 cdp::eval_string 同语义，但等待应答的空窗期持续泵入
/// 快通道控制请求——输入延迟 ≤200ms 而非最长 13s）
fn eval_string_pumped(
    cdp: &mut Cdp,
    session: &str,
    expr: &str,
    timeout_ms: u64,
    pump: &mut InputPump,
) -> Result<String, String> {
    let v = cdp.call_pumped(
        "Runtime.evaluate",
        json!({ "expression": expr, "returnByValue": true, "awaitPromise": false }),
        Some(session),
        timeout_ms,
        &mut |c| pump.drain(c),
    )?;
    if let Some(d) = v.get("exceptionDetails") {
        return Err(format!("evaluate 异常: {d}"));
    }
    Ok(v
        .get("result")
        .and_then(|r| r.get("value"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string())
}

/// 慢通道请求处理（需等 CDP 应答）：阻塞等待期间经 call_pumped 继续
/// 服务快通道请求。返回 true = 平台已切换，引擎需重启实例生效。
fn handle_control(
    cdp: &mut Cdp,
    session: &str,
    shared: &Arc<SharedState>,
    req: ControlRequest,
    logger: &Arc<Logger>,
    pump: &mut InputPump,
) -> Result<bool, String> {
    // 用户活动登记：截图/剪贴板/流订阅/设置调整等慢通道请求同样算「有人在用」
    // （快通道在 dispatch_input 登记，覆盖 eval 等待空窗的泵入路径）
    shared.touch_activity();
    // 快通道分流：fire 即发（稳态循环直接调用与 eval 等待空窗泵入共用本入口）
    if is_fast(&req) {
        dispatch_input(cdp, session, shared, req, logger, &mut pump.tap_probe)?;
        return Ok(false);
    }
    match req {
        ControlRequest::Screenshot { reply } => {
            let v = cdp.call_pumped(
                "Page.captureScreenshot",
                json!({ "format": "jpeg", "quality": 70 }),
                Some(session),
                15000,
                &mut |c| pump.drain(c),
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
        ControlRequest::ClipGet { reply } => {
            // 云机选区 → 控制页（控制页写入本机剪贴板）：页面普通选区 +
            // 输入框内选区，最多 64KB（防异常超大选区拖垮 HTTP 层）
            let r = eval_string_pumped(cdp, session, CLIP_EXPR, 5000, pump);
            let out = match r {
                Ok(s) => serde_json::from_str::<Value>(&s)
                    .ok()
                    .and_then(|v| v.get("t").and_then(|x| x.as_str()).map(|t| t.to_string()))
                    .unwrap_or_default(),
                Err(e) => {
                    if e.starts_with("WS:") {
                        return Err(e);
                    }
                    String::new()
                }
            };
            let cut: String = out.chars().take(65536).collect();
            let _ = reply.send(Ok(cut));
        }
        ControlRequest::ScreencastAttach { reply } => {
            match cdp.screencast_subscribe_pumped(session, &mut |c| pump.drain(c)) {
                Ok(sub) => {
                    // 先应答后补帧：HTTP 层零等待；弱机/引擎忙时截图再慢也只影响首帧
                    // 到达时刻，不影响连接建立（TTFB）
                    let _ = reply.send(Ok(sub));
                    // 首帧兜底：静态页/错误页合成器无更新，screencast 可能长期不发帧 →
                    // 立即截一帧推给所有订阅者，保证流打开就有画面（也盖住重连空窗）。
                    // 「Not attached to an active page」（导航换档的瞬态拒）退避重试
                    // 跨过导航窗口——此前只试一次即弃，选平台后流长期无首帧的成因
                    // 之一；重试期间持续泵入快通道输入请求
                    let mut first_frame_sent = false;
                    let mut last_err = String::new();
                    for attempt in 0..3u32 {
                        match cdp.call_pumped(
                            "Page.captureScreenshot",
                            json!({ "format": "jpeg", "quality": 60 }),
                            Some(session),
                            8000,
                            &mut |c| pump.drain(c),
                        ) {
                            Ok(v) => {
                                if let Some(b64) = v.get("data").and_then(|x| x.as_str()) {
                                    cdp.push_frame(util::base64_decode(b64));
                                    first_frame_sent = true;
                                }
                                break;
                            }
                            Err(e) => {
                                if e.starts_with("WS:") {
                                    return Err(e);
                                }
                                last_err = e;
                                if !cdp::is_transient_cast_error(&last_err) {
                                    break; // 非瞬态（页面忙等）：screencast 事件帧照常会到
                                }
                                if attempt + 1 < 3 {
                                    // 退避切片：期间泵事件（新帧照常分发；WS 断裂上抛）
                                    let deadline =
                                        Instant::now() + Duration::from_millis(300 * (attempt as u64 + 1));
                                    loop {
                                        let left = deadline.saturating_duration_since(Instant::now());
                                        if left.is_zero() {
                                            break;
                                        }
                                        if let Err(e2) = cdp.pump_events(left.min(Duration::from_millis(100))) {
                                            if e2.starts_with("WS:") {
                                                return Err(e2);
                                            }
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if !first_frame_sent {
                        // 命令级失败仅记日志（screencast 事件帧照常会到；稳态循环
                        // 的 cast_rescue 6s 自愈也会拉活）
                        logger.log(1, "sys", &format!("实时流首帧兜底截图未成：{last_err}"));
                    }
                }
                Err(e) => {
                    logger.log(0, "error", &format!("实时画面流开启失败：{e}"));
                    let _ = reply.send(Err(e));
                }
            }
        }
        ControlRequest::SetPlatform { platform, reply } => {
            // 平台切换：共享状态即刻更新（healthz 马上回显新平台/视口/首页），
            // 引擎重启实例后按新平台重注入脚本 + 新视口 + 导航新首页。
            // Profile 不变：两平台登录态共存，已登过的平台切换后无需重登
            if shared.set_platform(&platform) {
                logger.log(0, "sys", &format!("平台切换 → {platform}，重启云机实例"));
                let _ = reply.send(Ok(()));
                return Ok(true);
            }
            let _ = reply.send(Err(format!("未知平台：{platform}")));
        }
        // 快通道由 dispatch_input 处理（稳态循环直接调用时也先经 is_fast 分流）
        ControlRequest::Touch { .. }
        | ControlRequest::Mouse { .. }
        | ControlRequest::KeyEvent { .. }
        | ControlRequest::SetFps { .. }
        | ControlRequest::SetQuality { .. }
        | ControlRequest::SetScale { .. }
        | ControlRequest::TypeText { .. }
        | ControlRequest::Key { .. }
        | ControlRequest::Navigate { .. }
        | ControlRequest::Reload { .. }
        | ControlRequest::ScreencastDetach { .. } => {
            unreachable!("快通道请求不应进入慢通道处理")
        }
    }
    Ok(false)
}

/// 快通道分发（fire 即发，无等待；eval 等待空窗里可安全穿插）
fn dispatch_input(
    cdp: &mut Cdp,
    session: &str,
    shared: &Arc<SharedState>,
    req: ControlRequest,
    logger: &Arc<Logger>,
    tap_probe: &mut Option<(f64, f64)>,
) -> Result<(), String> {
    // 用户活动登记：触摸/键鼠/导航类请求出现＝有人在用，tick 降频即时退出
    // （稳态循环下一周期 ≤200ms 重估；eval 等待空窗的泵入路径同样经过这里）
    shared.touch_activity();
    match req {
        ControlRequest::Touch { phase, points, reply } => {
            // 协议规定 touchEnd/touchCancel 不得携带触点（整组释放，引擎侧
            // track_touch_points 保证）；move 只回放已跟踪触点。fire 即发即答；
            // move 高频仅按下留日志（错误应答由 error_replies 通道留痕）
            if phase == "start" && !points.is_empty() {
                logger.log(1, "click", &format!("触摸按下 ({:.0},{:.0}) id={}", points[0].x, points[0].y, points[0].id));
                // 诊断探针：记下按下坐标，稳态循环用 elementFromPoint
                // 探落点元素（点击无反应时日志直接给出命中目标）
                *tap_probe = Some((points[0].x, points[0].y));
            }
            // 抬起/取消同样留痕：此前 end 无日志，「点击没反应」时无法区分
            // 「end 未到达引擎」与「页面收到但不响应」——观测盲区消除
            if phase == "end" {
                logger.log(1, "click", "触摸抬起（整组释放）");
            } else if phase == "cancel" {
                logger.log(1, "click", "触摸取消（整组释放）");
            }
            let pts: Vec<(f64, f64, i64)> = points.iter().map(|p| (p.x, p.y, p.id)).collect();
            cdp.dispatch_touch(&phase, &pts, session)?;
            let _ = reply.send(Ok(()));
        }
        ControlRequest::Mouse { action, x, y, button, buttons, click_count, dx, dy, modifiers, reply } => {
            // 真实鼠标事件同样 fire 即发（应答无信息量；与触摸同链路同理由）。
            // 全键位：left/right/middle；clickCount 2/3 → 远端合成 dblclick/三击
            let mut params = json!({
                "type": match action.as_str() {
                    "down" => "mousePressed",
                    "up" => "mouseReleased",
                    "wheel" => "mouseWheel",
                    _ => "mouseMoved",
                },
                "x": x,
                "y": y,
                "button": button,
                "buttons": buttons,
                "modifiers": modifiers,
            });
            if action == "down" || action == "up" {
                params["clickCount"] = Value::from(click_count.max(1));
            }
            if action == "wheel" {
                params["deltaX"] = Value::from(dx);
                params["deltaY"] = Value::from(dy);
            }
            if action == "down" {
                logger.log(1, "click", &format!("鼠标按下 {button} ({x:.0},{y:.0})×{click_count}"));
            }
            cdp.fire_checked("Input.dispatchMouseEvent", params, Some(session))?;
            let _ = reply.send(Ok(()));
        }
        ControlRequest::KeyEvent { typ, key, code, vk, text, modifiers, location, auto_repeat, reply } => {
            // t=down 且带文本 → keyDown（Chrome 生成字符输入）；其余 down →
            // rawKeyDown；t=up → keyUp。修饰键位图：Alt=1 Ctrl=2 Meta=4 Shift=8
            let mut params = json!({
                "type": if typ == "up" { "keyUp" } else if text.is_empty() { "rawKeyDown" } else { "keyDown" },
                "key": key,
                "code": code,
                "windowsVirtualKeyCode": vk,
                "nativeVirtualKeyCode": vk,
                "modifiers": modifiers,
            });
            if location > 0 {
                params["location"] = Value::from(location);
            }
            if auto_repeat {
                params["autoRepeat"] = Value::Bool(true);
            }
            if typ == "down" && !text.is_empty() {
                params["text"] = Value::String(text.clone());
                params["unmodifiedText"] = Value::String(text.clone());
            }
            if typ == "down" {
                logger.log(1, "click", &format!("按键 {key}{}", if text.is_empty() { String::new() } else { format!("（{text}）") }));
            }
            cdp.fire_checked("Input.dispatchKeyEvent", params, Some(session))?;
            let _ = reply.send(Ok(()));
        }
        ControlRequest::SetFps { fps, reply } => {
            // 软件限帧（Chrome 152 maxFrameRate 无效）：只记目标值即刻生效；
            // 值存 SharedState——CDP 重建后沿用（用户设置不因重连丢失）
            shared.set_fps(fps);
            cdp.set_screencast_fps(fps, session);
            logger.log(1, "sys", &format!("实时画面帧率设为 {fps}"));
            let _ = reply.send(Ok(()));
        }
        ControlRequest::SetQuality { quality, reply } => {
            // 画质是 startScreencast 参数：更新值；cast 活动且有观众时
            // stop+start 重建生效（毫秒级空窗由信箱心跳掩盖），无人观看则
            // 下次订阅自然用新值。值存 SharedState 跨重建保留
            shared.set_quality(quality);
            let p = shared.platform();
            cdp.set_cast_tuning(shared.quality(), cast_max_for(shared.scale(), p.vw, p.vh));
            if cdp.cast_active() && cdp.has_sinks() {
                cdp.restart_cast(session);
            }
            logger.log(1, "sys", &format!("实时画面画质设为 {quality}"));
            let _ = reply.send(Ok(()));
        }
        ControlRequest::SetScale { scale_pct, reply } => {
            // 采集分辨率缩放同为 startScreencast 参数（按当前平台视口计算
            // maxWidth/maxHeight；触摸坐标是 CSS 系不受影响）
            shared.set_scale(scale_pct);
            let p = shared.platform();
            cdp.set_cast_tuning(shared.quality(), cast_max_for(shared.scale(), p.vw, p.vh));
            if cdp.cast_active() && cdp.has_sinks() {
                cdp.restart_cast(session);
            }
            logger.log(1, "sys", &format!("实时画面采集分辨率设为 {scale_pct}%"));
            let _ = reply.send(Ok(()));
        }
        ControlRequest::TypeText { text, reply } => {
            logger.log(1, "click", &format!("输入文本（{} 字符）", text.chars().count()));
            // fire 即发：insertText 应答无信息量；页面忙时同步等曾在弱机占 5s，
            // 输入法打字逐字卡顿——事件在 WS 管道保序，Chrome 输入线程照常消化
            cdp.fire_checked("Input.insertText", json!({ "text": text }), Some(session))?;
            let _ = reply.send(Ok(()));
        }
        ControlRequest::Key { key, reply } => {
            logger.log(1, "click", &format!("按键 {key}"));
            key_event(cdp, session, &key)?;
            let _ = reply.send(Ok(()));
        }
        ControlRequest::Navigate { url, reply } => {
            logger.log(1, "nav", &format!("控制页导航 {url}"));
            // 发后即忘：导航本身可费时数十秒（弱网/慢站），同步等完会把引擎线程
            // 占住最多 20s——期间触摸/输入全部压队（表现为「回首页后无法操作」）；
            // 结果由实时画面流直接看到，传输断裂则立刻报错走重连
            cdp.fire_checked("Page.navigate", json!({ "url": url }), Some(session))?;
            let _ = reply.send(Ok(()));
        }
        ControlRequest::Reload { reply } => {
            logger.log(1, "nav", "控制页重载");
            cdp.fire_checked("Page.reload", json!({ "ignoreCache": true }), Some(session))?;
            let _ = reply.send(Ok(()));
        }
        ControlRequest::ScreencastDetach { id, reply } => {
            cdp.screencast_unsubscribe(id, session);
            let _ = reply.send(Ok(()));
        }
        // 慢通道由 handle_control 处理
        ControlRequest::Screenshot { .. }
        | ControlRequest::Tap { .. }
        | ControlRequest::Swipe { .. }
        | ControlRequest::ClipGet { .. }
        | ControlRequest::ScreencastAttach { .. }
        | ControlRequest::SetPlatform { .. } => {
            unreachable!("慢通道请求不应进入快通道分发")
        }
    }
    Ok(())
}


fn tap(cdp: &mut Cdp, session: &str, x: f64, y: f64) -> Result<(), String> {
    // 规范形态（puppeteer 同款）：start 带点、短按、end 空点整组释放
    // （协议规定 touchEnd/touchCancel 不得携带触点）
    cdp.dispatch_touch("start", &[(x, y, 1)], session)?;
    thread::sleep(Duration::from_millis(80));
    cdp.dispatch_touch("end", &[], session)
}

fn swipe(cdp: &mut Cdp, session: &str, x1: f64, y1: f64, x2: f64, y2: f64) -> Result<(), String> {
    cdp.dispatch_touch("start", &[(x1, y1, 1)], session)?;
    for i in 1..=8 {
        let t = i as f64 / 8.0;
        let xi = x1 + (x2 - x1) * t;
        let yi = y1 + (y2 - y1) * t;
        cdp.dispatch_touch("move", &[(xi, yi, 1)], session)?;
        thread::sleep(Duration::from_millis(16));
    }
    cdp.dispatch_touch("end", &[], session)
}

fn key_event(cdp: &mut Cdp, session: &str, key: &str) -> Result<(), String> {
    // /key 兼容端点（控制页现已用 /kbd 全键位直通；此处保留常用控制键）
    let (code, vk, text): (&str, u32, &str) = match key {
        "Backspace" => ("Backspace", 8, ""),
        "Tab" => ("Tab", 9, "\t"),
        "Escape" => ("Escape", 27, ""),
        "Delete" => ("Delete", 46, ""),
        "Insert" => ("Insert", 45, ""),
        "Home" => ("Home", 36, ""),
        "End" => ("End", 35, ""),
        "PageUp" => ("PageUp", 33, ""),
        "PageDown" => ("PageDown", 34, ""),
        "ArrowLeft" => ("ArrowLeft", 37, ""),
        "ArrowUp" => ("ArrowUp", 38, ""),
        "ArrowRight" => ("ArrowRight", 39, ""),
        "ArrowDown" => ("ArrowDown", 40, ""),
        "Space" | " " => ("Space", 32, " "),
        _ => ("Enter", 13, "\r"),
    };
    let mut kd = json!({
        "type": if text.is_empty() { "rawKeyDown" } else { "keyDown" },
        "key": key,
        "code": code,
        "windowsVirtualKeyCode": vk,
        "nativeVirtualKeyCode": vk,
    });
    if !text.is_empty() {
        kd["text"] = Value::String(text.to_string());
    }
    // fire 即发：按键事件应答无信息量，绝不占引擎线程
    cdp.fire_checked("Input.dispatchKeyEvent", kd, Some(session))?;
    cdp.fire_checked(
        "Input.dispatchKeyEvent",
        json!({
            "type": "keyUp",
            "key": key,
            "code": code,
            "windowsVirtualKeyCode": vk,
            "nativeVirtualKeyCode": vk,
        }),
        Some(session),
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Chromium 进程管理
// ---------------------------------------------------------------------------

/// 构造 Chromium 启动参数（低内存 + 保活语义 + WebRTC 可用；
/// 与 Node 版 buildArgs 逐项一致）
pub fn build_args(cfg: &Config) -> Vec<String> {
    build_args_with(cfg, cfg.width, cfg.height)
}

/// 平台运行时切换（/platform）需要按 SharedState 当前视口重建参数：
/// cfg.width/height 只是启动初值，切换后以本变体传入实时值
pub fn build_args_with(cfg: &Config, vw: u32, vh: u32) -> Vec<String> {
    let mut a: Vec<String> = vec![
        format!("--user-data-dir={}", cfg.profile_dir.display()),
        format!("--remote-debugging-port={}", cfg.cdp_port), // 0 = 自动分配（读 DevToolsActivePort）
        "--remote-debugging-address=127.0.0.1".into(),       // DevTools 只在回环暴露
        "--remote-allow-origins=*".into(), // 允许外部 DevTools 一次性登录（仅回环暴露）
        format!("--window-size={},{}", vw, vh),
        "--force-device-scale-factor=1".into(),
        "--hide-scrollbars".into(), // 截图/实时画面无滚动条，视口与触摸坐标严格对齐
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
        "--disable-pinch".into(), // 禁页面捏合缩放：云机画面 1:1（拖动误触缩放/缩放后坐标错位的根治）
        format!("--lang={}", cfg.lang),
    ];
    if cfg.no_sandbox {
        a.push("--no-sandbox".into());
        a.push("--disable-setuid-sandbox".into());
    }
    // 注：镜像固定 chrome-headless-shell（本身就无头，不需要 --headless=new）。
    // 极少数换用完整 Chromium 的场景，经 CPK_EXTRA_CHROME_ARGS 自行追加参数。
    if !cfg.extra_chrome_args.is_empty() {
        for p in cfg.extra_chrome_args.split_whitespace() {
            a.push(p.to_string());
        }
    }
    a.push("about:blank".into());
    a
}

fn launch_chrome(cfg: &Config, vw: u32, vh: u32, logger: &Arc<Logger>) -> Result<Child, String> {
    let args = build_args_with(cfg, vw, vh);
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

/// 采集分辨率百分比 → startScreencast 的 maxWidth/maxHeight（按平台视口）。
/// 100 = 原画不传（Chrome 按视口采集）；<100 时 Chrome 编码前先缩小，
/// 编码 CPU 与带宽按像素数近线性下降
fn cast_max_for(scale_pct: u32, vw: u32, vh: u32) -> Option<(u32, u32)> {
    if scale_pct >= 100 {
        return None;
    }
    let w = ((vw as u64 * scale_pct as u64) / 100).max(1) as u32;
    let h = ((vh as u64 * scale_pct as u64) / 100).max(1) as u32;
    Some((w, h))
}

/// CDP 装配：复用/新建页面目标 → attach → enable → UA 对齐 → 注入保活脚本
/// （navigate=true 时导航到云手机首页；重连场景 navigate=false）
fn attach_all(
    cfg: &Config,
    port: u16,
    script: &str,
    url: &str,
    shared: &Arc<SharedState>,
    logger: &Arc<Logger>,
    navigate: bool,
) -> Result<(Cdp, String), String> {
    let mut cdp = Cdp::connect(port)?;
    // 帧率沿用 SharedState 当前值：控制面板改过的帧率跨 CDP 重建保留
    cdp.set_default_fps(shared.fps());
    // cast 调优同样以 SharedState 为单一事实源（/quality /scale 运行时改过的值
    // 跨重建保留）；缩放按当前平台视口计算（触摸坐标是 CSS 系不受影响）
    let plat = shared.platform();
    cdp.set_cast_tuning(shared.quality(), cast_max_for(shared.scale(), plat.vw, plat.vh));
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
    } else if cfg.ua_mode == "mobile" {
        // navigator.platform 同步 Android（部分 H5 以此判分支）
        ua_params["platform"] = Value::String("Linux armv8l".into());
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
        let r = cdp.call("Page.navigate", json!({ "url": url }), Some(&session), 20000)?;
        // 同步期失败（DNS/连接/TLS）以 errorText 回报：留在结果字段而非协议错误。
        // 留痕 + 写 lastError；错误页停留由稳态采样的 chrome-error 检测接管重试
        match r.get("errorText").and_then(|x| x.as_str()) {
            Some(et) if !et.is_empty() => {
                logger.log(0, "nav", &format!("初始导航失败：{et}"));
                shared.set_last_error(&format!("导航失败：{et}"));
            }
            _ => logger.log(1, "nav", &format!("导航 {url}")),
        }
    }
    Ok((cdp, session))
}

/// UA 规范化（与 Node 版 normalizeUserAgent 一致）：
/// mobile=重建为 Android Chrome 移动 UA（默认）：云机 H5 按 UA 分手机/桌面
/// 分支，桌面 UA 下页面渲染 no-phone-layout 桌面布局（首见采样类名实证），
/// 414 视口与桌面布局错位 → 点击坐标命错目标（「去登陆/秒开点了没反应」
/// 的根因）。正常用户 100% 移动 UA——移动 UA 才是与真实云机用户一致的
/// 环境指纹；windows=重建为 Windows Chrome UA；auto=仅去 Headless 字样；
/// none=原样
pub fn normalize_user_agent(raw: &str, mode: &str) -> String {
    if mode == "none" {
        return raw.to_string();
    }
    if mode == "auto" {
        return raw.replace("HeadlessChrome", "Chrome");
    }
    let ver = regex_chrome_version(raw).unwrap_or_else(|| "138.0.0.0".into());
    if mode == "mobile" {
        return format!(
            "Mozilla/5.0 (Linux; Android 13; Pixel 7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{ver} Mobile Safari/537.36"
        );
    }
    // windows：提取 Chrome 版本号重建
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

    fn test_cfg() -> crate::config::Config {
        crate::config::Config {
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
            report_port: 8088,
            bind: "0.0.0.0".into(),
            control_token: String::new(),
            auth_user: String::new(),
            auth_pass: String::new(),
            cdp_port: 0,
            chrome_bin: "chrome-headless-shell".into(),
            no_sandbox: true,
            ua_mode: "windows".into(),
            lang: "zh-CN".into(),
            tz: "Asia/Shanghai".into(),
            extra_chrome_args: String::new(),
            tick_fail_reload: 10,
            frozen_reload: 3,
            beat_stale_sec: 180,
            fps: 25,
            jpeg_quality: 50,
            stream_scale_pct: 100,
            idle_after_sec: 60,
            idle_tick_sec: 5,
            selftest: false,
            smoke: false,
            smoke_seconds: 60,
        }
    }

    #[test]
    fn idle_periods_decision_table() {
        // 默认：60s 无活动进入空闲，tick 1s→5s、采样 5s→15s（3×动作周期）
        assert_eq!(
            idle_periods(&test_cfg()),
            Some((Duration::from_secs(5), Duration::from_secs(15)))
        );
        // 显式关闭（CPK_IDLE_AFTER_SEC=0）
        let mut c = test_cfg();
        c.idle_after_sec = 0;
        assert_eq!(idle_periods(&c), None, "idle_after=0 应关闭空闲降频");
        // tick=1（仅采样放缓）：动作周期取 interval=5s → 采样 15s
        let mut c = test_cfg();
        c.idle_tick_sec = 1;
        assert_eq!(idle_periods(&c), Some((Duration::from_secs(1), Duration::from_secs(15))));
        // tick < interval：动作周期取 interval（墙钟门控下 ticks 每 8s 前进）
        let mut c = test_cfg();
        c.interval_ms = 8000;
        c.idle_tick_sec = 2;
        assert_eq!(idle_periods(&c), Some((Duration::from_secs(2), Duration::from_secs(24))));
        // tick > interval：动作周期取 tick
        let mut c = test_cfg();
        c.idle_tick_sec = 10;
        assert_eq!(idle_periods(&c), Some((Duration::from_secs(10), Duration::from_secs(30))));
        // 安全钳：冻结恢复阶梯 ≥ 心跳硬重启 → 否决降频（默认 beat_stale=180：
        // 3×采样 ≥180 即采样 ≥60 → 动作周期 ≥20 → tick ≥20 否决）
        let mut c = test_cfg();
        c.idle_tick_sec = 20;
        assert_eq!(idle_periods(&c), None, "阶梯 3×60=180 ≥ beat_stale 180 应否决");
        let mut c = test_cfg();
        c.beat_stale_sec = 44; // 3×15=45 ≥ 44 → 默认周期也被否决
        assert_eq!(idle_periods(&c), None);
        // 采样下限 5s：interval 极小（1s）时采样不快于活跃态
        let mut c = test_cfg();
        c.interval_ms = 1000;
        c.idle_tick_sec = 1;
        assert_eq!(idle_periods(&c), Some((Duration::from_secs(1), Duration::from_secs(5))));
    }

    #[test]
    fn shared_state_activity_tracking() {
        // 活动登记/读取：touch_activity 后年龄归零；空闲态回显写入 healthz
        let shared = SharedState::new(&test_cfg());
        assert!(shared.activity_age() < Duration::from_secs(1), "新建即最近活动");
        assert!(!shared.snapshot().tick_idle, "初始应为活跃态");
        shared.set_tick_idle(true);
        let j = health_json(&shared.snapshot());
        assert_eq!(j.get("tickIdle").and_then(|x| x.as_bool()), Some(true), "healthz 应回显 tickIdle");
        shared.set_tick_idle(false);
        assert_eq!(health_json(&shared.snapshot()).get("tickIdle").and_then(|x| x.as_bool()), Some(false));
    }
    #[test]
    fn wait_platform_accepts_fps_and_wakes_on_platform() {
        // 平台待机期（启动时平台留空）：
        // - SetFps 被接受（不再快速失败）：值入共享状态，下次装配恢复 +
        //   稳态周期同步——「引擎待机时设帧率被拒但控制页报成功」的修复回归
        // - 其他控制请求仍快速失败并提示先选平台
        // - SetPlatform 唤醒待机循环（返回 true = 引擎按新平台启动）
        let cfg = test_cfg();
        let shared = SharedState::new(&cfg);
        assert_eq!(shared.fps(), 25);
        let (tx, rx) = std::sync::mpsc::channel();
        let sh = shared.clone();
        let log_dir = std::env::temp_dir().join(format!("cpk-wp-{}", std::process::id()));
        let logger = std::sync::Arc::new(crate::logger::Logger::new(log_dir.clone()));
        let h = std::thread::spawn(move || wait_platform(&sh, &rx, &logger));
        // 待机期设帧率：被接受且生效
        let (rtx, rrx) = std::sync::mpsc::channel();
        tx.send(ControlRequest::SetFps { fps: 10, reply: rtx }).unwrap();
        assert!(rrx.recv_timeout(std::time::Duration::from_secs(2)).unwrap().is_ok(), "待机期 SetFps 应被接受");
        assert_eq!(shared.fps(), 10, "帧率应写入共享状态");
        // 其他控制请求：快速失败 + 明确提示
        let (rtx2, rrx2) = std::sync::mpsc::channel();
        tx.send(ControlRequest::Reload { reply: rtx2 }).unwrap();
        let e = rrx2.recv_timeout(std::time::Duration::from_secs(2)).unwrap().unwrap_err();
        assert!(e.contains("平台未选择"), "提示应含平台未选择：{e}");
        // 平台选择 → 唤醒
        let (rtx3, rrx3) = std::sync::mpsc::channel();
        tx.send(ControlRequest::SetPlatform { platform: "mobile".into(), reply: rtx3 }).unwrap();
        assert!(rrx3.recv_timeout(std::time::Duration::from_secs(2)).unwrap().is_ok());
        assert!(h.join().unwrap(), "平台选择后待机循环应唤醒");
        assert_eq!(shared.platform().platform, "mobile");
        let _ = std::fs::remove_dir_all(&log_dir);
    }
    #[test]
    fn health_json_exposes_title_and_last_status() {
        // 对齐 Windows 版：页面标题 + 上报状态暴露给控制页（标题显示/状态迁移通知）
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
            report_port: 8088,
            bind: "0.0.0.0".into(),
            control_token: String::new(),
            auth_user: String::new(),
            auth_pass: String::new(),
            cdp_port: 0,
            chrome_bin: "chrome-headless-shell".into(),
            no_sandbox: true,
            ua_mode: "windows".into(),
            lang: "zh-CN".into(),
            tz: "Asia/Shanghai".into(),
            extra_chrome_args: String::new(),
            tick_fail_reload: 10,
            frozen_reload: 3,
            beat_stale_sec: 180,
            fps: 25,
            jpeg_quality: 50,
            stream_scale_pct: 100,
            idle_after_sec: 60,
            idle_tick_sec: 5,
            selftest: false,
            smoke: false,
            smoke_seconds: 60,
        };
        let shared = SharedState::new(&cfg);
        assert!(shared.snapshot().title.is_empty());
        assert!(shared.snapshot().last_status.is_empty());
        shared.set_title("移动云手机");
        shared.set_status("exited");
        let j = health_json(&shared.snapshot());
        assert_eq!(j["title"].as_str(), Some("移动云手机"));
        assert_eq!(j["lastStatus"].as_str(), Some("exited"));
        // 标题未变时不重复写入（set_title 幂等判断分支回归）
        shared.set_title("移动云手机");
        assert_eq!(health_json(&shared.snapshot())["title"].as_str(), Some("移动云手机"));
    }

    #[test]
    fn platform_runtime_switch_updates_health() {
        // /platform 运行时切换：healthz 平台/标签/首页/视口即刻切换；未知平台拒
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
            report_port: 8088,
            bind: "0.0.0.0".into(),
            control_token: String::new(),
            auth_user: String::new(),
            auth_pass: String::new(),
            cdp_port: 0,
            chrome_bin: "chrome-headless-shell".into(),
            no_sandbox: true,
            ua_mode: "windows".into(),
            lang: "zh-CN".into(),
            tz: "Asia/Shanghai".into(),
            extra_chrome_args: String::new(),
            tick_fail_reload: 10,
            frozen_reload: 3,
            beat_stale_sec: 180,
            fps: 25,
            jpeg_quality: 50,
            stream_scale_pct: 100,
            idle_after_sec: 60,
            idle_tick_sec: 5,
            selftest: false,
            smoke: false,
            smoke_seconds: 60,
        };
        let shared = SharedState::new(&cfg);
        // 初始：mobile 414x896
        let j = health_json(&shared.snapshot());
        assert_eq!(j["platform"].as_str(), Some("mobile"));
        assert_eq!(j["platformLabel"].as_str(), Some("移动云手机"));
        assert_eq!(j["vw"].as_u64(), Some(414));
        assert_eq!(j["vh"].as_u64(), Some(896));
        // 切联通：label/url/视口全切换
        assert!(shared.set_platform("unicom"));
        let j = health_json(&shared.snapshot());
        assert_eq!(j["platform"].as_str(), Some("unicom"));
        assert_eq!(j["platformLabel"].as_str(), Some("联通云手机"));
        assert_eq!(j["homeUri"].as_str(), Some(crate::config::PLATFORM_UNICOM_URI));
        assert_eq!(j["vw"].as_u64(), Some(405));
        assert_eq!(j["vh"].as_u64(), Some(720));
        // 未知平台：拒绝且状态不变
        assert!(!shared.set_platform("telecom"));
        let j = health_json(&shared.snapshot());
        assert_eq!(j["platform"].as_str(), Some("unicom"));
        // 切回移动
        assert!(shared.set_platform("mobile"));
        assert_eq!(health_json(&shared.snapshot())["vw"].as_u64(), Some(414));
    }

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
        // mobile：Android 移动 UA（云机 H5 手机布局的正确环境）
        let mb = normalize_user_agent(raw, "mobile");
        assert!(mb.starts_with("Mozilla/5.0 (Linux; Android 13; Pixel 7)"));
        assert!(mb.contains("Chrome/152.0.7977.82 Mobile Safari"));
        assert!(!mb.contains("HeadlessChrome"));
        // 无版本号兜底
        let win2 = normalize_user_agent("Mozilla/5.0 HeadlessChrome", "windows");
        assert!(win2.contains("Chrome/138.0.0.0"));
        let mb2 = normalize_user_agent("Mozilla/5.0 HeadlessChrome", "mobile");
        assert!(mb2.contains("Chrome/138.0.0.0 Mobile"));
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
            report_port: 8088,
            bind: "0.0.0.0".into(),
            control_token: String::new(),
            auth_user: String::new(),
            auth_pass: String::new(),
            cdp_port: 0,
            chrome_bin: "chrome-headless-shell".into(),
            no_sandbox: true,
            ua_mode: "windows".into(),
            lang: "zh-CN".into(),
            tz: "Asia/Shanghai".into(),
            extra_chrome_args: "--js-flags=--max-old-space-size=512".into(),
            tick_fail_reload: 10,
            frozen_reload: 3,
            beat_stale_sec: 180,
            fps: 25,
            jpeg_quality: 50,
            stream_scale_pct: 100,
            idle_after_sec: 60,
            idle_tick_sec: 5,
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
        // 禁页面捏合缩放（拖动误触缩放/缩放后坐标错位的根治）
        assert!(args.contains(&"--disable-pinch".to_string()));
        // 恒无头：镜像固定 chrome-headless-shell，不添加 --headless=new
        assert!(!args.contains(&"--headless=new".to_string()));
    }
}
