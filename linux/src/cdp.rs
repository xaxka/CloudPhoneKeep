//! 极简 CDP（Chrome DevTools Protocol）客户端：跑在自研 WsClient 上。
//!  - call()：发命令并等待应答（按 id 匹配；等待期间持续分发事件）
//!  - 事件内联处理：Page.javascriptDialogOpening → 自动 Page.handleJavaScriptDialog
//!    （无头环境无人可点，不处理 alert/confirm 会冻住页面与 Runtime.evaluate）
//!  - 传输类错误统一以「WS:」前缀返回，调用方据此区分「重连」与「页面级恢复」
//!  - fire()：发后即忘（对话框应答等，响应到达时静默丢弃）

use crate::util;
use crate::ws::{WsClient, WsMessage};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// 订阅 id 进程级单调递增：CDP 会话重建后陈旧的 Detach 不会误删新订阅
static SINK_ID: AtomicU32 = AtomicU32::new(1);

pub struct Cdp {
    ws: WsClient,
    next_id: u64,
    /// 浏览器标识（/json/version 的 Browser）
    pub browser: String,
    /// 原始 UA（/json/version 的 User-Agent，headless shell 带 HeadlessChrome 字样）
    pub raw_ua: String,
    pub dialog_count: u32,
    pub last_dialog: Option<String>,
    /// 实时画面帧订阅者（每个 /stream.mjpg 连接一信箱：只存最新帧，丢旧保新）
    sinks: Vec<(u32, FrameBox)>,
    /// Page.startScreencast 是否在跑（首个订阅者开启，最后一个离开自动关，
    /// 无人观看不消耗 JPEG 编码 CPU）
    screencast_active: bool,
    /// 当前 screencast 目标帧率（控制面板 /fps 可运行时调整；重建 CDP 会话时
    /// 从 SharedState 恢复，设置跨重连存活）
    screencast_fps: u32,
    /// 软件限帧基准：上次向订阅者推帧时刻。
    /// 实测 Chrome 152 的 Page.startScreencast maxFrameRate 参数无效
    /// （设 5 仍以合成器帧率 ~30fps 发帧），帧率上限由本侧丢帧实现：
    /// 距上次推送不足 1000/fps ms 的帧直接丢弃（ack 照发，Chrome 不受影响）
    last_frame_push: Option<Instant>,
    /// 在按触点状态（id → 最近坐标）。move 只回放已跟踪触点：导航/CDP
    /// 重建后控制页续发的游离 move 若凭空重建触点，Chrome 会出现幽灵第二
    /// 触点（单指拖动被判成双指——「拖动放大页面」的来源）；BTreeMap 顺序
    /// 确定可重现。
    /// 协议规定（CDP Input 文档）：touchEnd/touchCancel 的 touchPoints 必须
    /// 为空（整组释放）、touchStart/touchMove 至少一点。曾按「带点释放」
    /// 实现，实测该形态违反协议约束被 Chrome 拒绝——错误应答在 fire 路径
    /// 被静默丢弃，页面收不到 touchend 的 tap 收尾，表现为「点击没反应」；
    /// 现回归 puppeteer 同款规范形态：end/cancel 恒空点整组释放。
    touch_active: BTreeMap<i64, (f64, f64)>,
    /// 最近一次收到 screencastFrame 事件时刻。订阅时判定 cast 是否真在出帧
    /// （startScreencast 失败被静默吞掉后 screencast_active 恒真，流重连
    /// 只会干等）：超阈值未出帧即重新发起 startScreencast。
    last_cast_frame: Option<Instant>,
    /// 已发未答的命令（id → method）：fire 类命令的应答按 id 找回 method，
    /// 错误应答落诊断记录（此前被当「无关应答」静默丢弃——
    /// startScreencast/Input.dispatchTouchEvent 被 Chrome 拒绝时无任何线索）
    outstanding: HashMap<u64, String>,
    /// fire 类命令被 Chrome 拒绝的记录（引擎每监督周期取走落日志，上限 32 条防膨胀）
    error_replies: Vec<String>,
}

/// 「最新帧信箱」：生产者覆盖写入（旧帧直接作废，观看端永远拿到最新画面），
/// 条件变量唤醒等待中的流线程。弱机/弱网下宁可跳帧也不排队——排队的旧帧
/// 只会带来“画面滞后于操作”的卡顿感，跳帧则始终实时。
/// alive：生产侧（引擎事件泵）每个泵周期置位一次；消费侧超时醒来后读取并复位，
/// 连续 2 个窗口无心跳 = CDP 已重建/浏览器重启（旧信箱不会再有帧）→ 消费方关流重连。
/// （旧 mpsc 模型靠 Disconnected 感知，信箱模型无法被动感知，改用此主动心跳。）
pub struct FrameSlot {
    frame: Mutex<Option<Vec<u8>>>,
    cv: Condvar,
    alive: AtomicU8,
    /// 连续无心跳窗口数（Mutex 内部字段，随帧槽一起加锁）
    missed: Mutex<u32>,
}

/// 流线程持有的信箱句柄
pub type FrameBox = Arc<FrameSlot>;

/// 消费侧每次 poll 的结果
pub enum FramePoll {
    /// 拿到最新帧（信箱内已清空）
    Frame(Vec<u8>),
    /// 窗口内无新帧但生产侧活着（静态页：走 2s 心跳重发）
    Idle,
    /// 连续 2 个窗口无生产心跳：CDP 已重建，关流重连
    Dead,
}

impl FrameSlot {
    /// 创建信箱（生产侧/测试模拟引擎用；消费侧拿到的是 FrameBox 克隆）
    pub fn new() -> FrameBox {
        Arc::new(FrameSlot {
            frame: Mutex::new(None),
            cv: Condvar::new(),
            alive: AtomicU8::new(1),
            missed: Mutex::new(0),
        })
    }

    /// 生产侧：覆盖写入最新帧并唤醒（旧帧作废）
    pub fn post(&self, f: Vec<u8>) {
        let mut g = self.frame.lock().unwrap();
        *g = Some(f);
        self.alive.store(1, Ordering::Release);
        self.cv.notify_all();
    }

    /// 消费侧：等待至多 `to` 取一帧（取到即清空信箱）。
    /// 有新帧 → Frame；超时且生产侧活着 → Idle；连续无心跳 → Dead。
    pub fn poll(&self, to: Duration) -> FramePoll {
        let mut g = self.frame.lock().unwrap();
        if let Some(f) = g.take() {
            *self.missed.lock().unwrap() = 0;
            return FramePoll::Frame(f);
        }
        // 生产心跳检查：上次醒来后引擎是否泵过/推过帧
        if self.alive.swap(0, Ordering::AcqRel) == 1 {
            *self.missed.lock().unwrap() = 0;
        }
        let (g2, _timeout) = self
            .cv
            .wait_timeout(g, to)
            .expect("FrameSlot 锁中毒（引擎 panic 后由重建恢复，不致命）");
        let mut g = g2;
        if let Some(f) = g.take() {
            *self.missed.lock().unwrap() = 0;
            return FramePoll::Frame(f);
        }
        let mut missed = self.missed.lock().unwrap();
        if self.alive.swap(0, Ordering::AcqRel) == 1 {
            *missed = 0;
            return FramePoll::Idle;
        }
        *missed += 1;
        if *missed >= 2 {
            FramePoll::Dead
        } else {
            FramePoll::Idle
        }
    }

    /// 生产侧心跳：引擎事件泵每周期 touch（订阅通道活着）
    pub fn touch_alive(&self) {
        self.alive.store(1, Ordering::Release);
    }
}

/// 拉取 http://127.0.0.1:port/json/version（带重试；Chromium 启动期端口渐次可用）
pub fn fetch_version(port: u16, attempts: u32, interval_ms: u64) -> Result<Value, String> {
    let mut last = String::new();
    for i in 0..attempts {
        match util::http_get(port, "/json/version", 3000) {
            Ok((200, body)) => {
                return serde_json::from_str(&body).map_err(|e| format!("解析 /json/version 失败: {e}"))
            }
            Ok((s, _)) => last = format!("HTTP {s}"),
            Err(e) => last = e,
        }
        if i + 1 < attempts {
            std::thread::sleep(Duration::from_millis(interval_ms));
        }
    }
    Err(format!("DevTools HTTP 端点不可达(127.0.0.1:{port}): {last}"))
}

impl Cdp {
    pub fn connect(port: u16) -> Result<Cdp, String> {
        let v = fetch_version(port, 2, 500)?;
        let ws_url = v
            .get("webSocketDebuggerUrl")
            .and_then(|x| x.as_str())
            .ok_or("版本信息缺 webSocketDebuggerUrl")?
            .to_string();
        let rest = ws_url.strip_prefix("ws://").ok_or("仅支持 ws://（本地回环）")?;
        let (hp, path) = match rest.split_once('/') {
            Some((hp, p)) => (hp.to_string(), format!("/{p}")),
            None => (rest.to_string(), "/".to_string()),
        };
        let ws = WsClient::connect(&hp, &path).map_err(|e| format!("WS 握手失败({hp}{path}): {e:?}"))?;
        Ok(Cdp {
            ws,
            next_id: 1,
            browser: v.get("Browser").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            raw_ua: v.get("User-Agent").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            dialog_count: 0,
            last_dialog: None,
            sinks: Vec::new(),
            screencast_active: false,
            screencast_fps: 25,
            last_frame_push: None,
            touch_active: BTreeMap::new(),
            last_cast_frame: None,
            outstanding: HashMap::new(),
            error_replies: Vec::new(),
        })
    }

    /// 连接后设置默认帧率（引擎装配时从 SharedState 读取——运行时改过的值
    /// 跨 CDP 重建保留）
    pub fn set_default_fps(&mut self, fps: u32) {
        self.screencast_fps = fps.clamp(1, 60);
    }

    fn build_msg(&mut self, method: &str, params: Value, session: Option<&str>) -> (u64, String) {
        let id = self.next_id;
        self.next_id += 1;
        let mut m = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            m["sessionId"] = Value::String(s.to_string());
        }
        self.outstanding.insert(id, method.to_string());
        (id, m.to_string())
    }

    /// fire 类命令的错误应答记录（按 id 找回 method）：
    /// startScreencast 在导航换档期被拒、dispatchTouchEvent 点列表违反
    /// 协议约束被拒等——这些错误以前被当「无关应答」静默丢弃，故障排查
    /// 无任何线索。引擎每个监督周期取走（take_error_replies）落日志。
    fn note_reply_error(&mut self, method: Option<&str>, err: Option<&Value>) {
        if let (Some(m), Some(e)) = (method, err) {
            if self.error_replies.len() < 32 {
                self.error_replies.push(format!("{m} 命令被 Chrome 拒绝（fire 路径）: {e}"));
            }
        }
    }

    /// 取走 fire 路径累积的错误应答记录（引擎监督循环每周期调用）
    pub fn take_error_replies(&mut self) -> Vec<String> {
        std::mem::take(&mut self.error_replies)
    }

    /// 发后即忘（不等待应答；响应到达时按「无关 id」静默丢弃）
    pub fn fire(&mut self, method: &str, params: Value, session: Option<&str>) {
        let _ = self.fire_checked(method, params, session);
    }

    /// 发后即忘但检查发送结果：WS 断裂（写失败）立刻可见——控制类命令
    /// （导航/输入事件）用它在毫秒级拿到传输层错误，而不是等下轮 tick 才发现。
    /// 错误以「WS:」前缀返回，调用方据此走重连路径。
    pub fn fire_checked(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<(), String> {
        let (_, text) = self.build_msg(method, params, session);
        self.ws
            .send_text(&text)
            .map_err(|e| format!("WS: 发送 {method} 失败({e:?})"))
    }

    /// 发送命令并等待应答。等待期间分发事件（含对话框自动确认）。
    /// 错误约定：以「WS:」开头 = 传输层断裂（调用方应重连）；
    /// 其余（超时/协议错误）= 命令级失败（页面级恢复路径处理）。
    pub fn call(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
        timeout_ms: u64,
    ) -> Result<Value, String> {
        self.call_pumped(method, params, session, timeout_ms, &mut |_c| {})
    }

    /// call() 的可泵变体：等待应答的每个 200ms 空窗期回调 `pump`——
    /// 引擎用它把控制通道里的输入事件（fire 即发，不占等待）即时分发出去，
    /// 消灭「页面慢 eval 阻塞引擎线程 → 触摸/点击排队秒级」的输入锁死：
    /// 真实云机页（WebRTC 视频）上 tick/采样 eval 常态秒级，旧结构下
    /// 控制请求最长早 13s 无人应答（点击「没用」/ /fps 超时的根因）。
    /// 回调在等待空窗内独占 &mut Cdp（发 fire 类消息不影响 id 配对），
    /// 绝不在读到消息的周期里回调（避免与事件分发重入）。
    pub fn call_pumped(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
        timeout_ms: u64,
        pump: &mut dyn FnMut(&mut Cdp),
    ) -> Result<Value, String> {
        let (id, text) = self.build_msg(method, params, session);
        self.ws
            .send_text(&text)
            .map_err(|e| format!("WS: 发送 {method} 失败({e:?})"))?;
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            if Instant::now() >= deadline {
                return Err(format!("{method} 命令超时({timeout_ms}ms)"));
            }
            let poll = Instant::now() + Duration::from_millis(200);
            match self.ws.read_message(poll) {
                Ok(WsMessage::Text(t)) => {
                    let v: Value = match serde_json::from_str(&t) {
                        Ok(v) => v,
                        Err(_) => continue, // 非 JSON 帧（不应出现）：丢弃
                    };
                    if let Some(rid) = v.get("id").and_then(|x| x.as_u64()) {
                        let fire_method = self.outstanding.remove(&rid);
                        if rid != id {
                            // fire() 的回执等：无关应答——但错误应答要留痕
                            self.note_reply_error(fire_method.as_deref(), v.get("error"));
                            continue;
                        }
                        if let Some(err) = v.get("error") {
                            return Err(format!("{method} 协议错误: {err}"));
                        }
                        return Ok(v.get("result").cloned().unwrap_or(Value::Null));
                    }
                    if let Some(m) = v.get("method").and_then(|x| x.as_str()) {
                        let sess = v.get("sessionId").and_then(|x| x.as_str()).map(|s| s.to_string());
                        self.on_event(m, v.get("params").cloned().unwrap_or(Value::Null), sess.as_deref());
                    }
                }
                Ok(WsMessage::Close) => return Err(format!("WS: 连接已关闭({method})")),
                Err(crate::ws::WsError::Timeout) => {
                    // 本轮 poll 无消息：等待空窗——泵入输入事件（fire 类，不占等待）
                    pump(self);
                    continue;
                }
                Err(e) => return Err(format!("WS: 读取错误({method}): {e:?}")),
            }
        }
    }

    fn on_event(&mut self, method: &str, params: Value, session: Option<&str>) {
        if method == "Page.javascriptDialogOpening" {
            self.dialog_count += 1;
            self.last_dialog = params
                .get("message")
                .and_then(|m| m.as_str())
                .map(|s| s.chars().take(120).collect());
            self.fire("Page.handleJavaScriptDialog", json!({ "accept": true }), session);
        }
        if method == "Page.screencastFrame" {
            self.last_cast_frame = Some(Instant::now());
            self.on_screencast_frame(params, session);
        }
        if method == "Page.frameStartedLoading" {
            // 导航换档：渲染器即将换成新文档，在按触摸一律按取消收尾
            // （跨文档的 touch 队列语义未定义；残留触点会让后续手势错乱）。
            // touchCancel 空点 = 整组取消（协议规定 end/cancel 不得携带触点）
            self.cancel_touches(session);
        }
        // 其余事件（Target/Page/Runtime 通知类）无需处理：状态以快照采样为准
    }

    /// 导航开始：取消全部在按触摸并清空跟踪。
    /// 发后即忘（此刻正处于事件读取循环内，仅 send-only 操作安全）。
    fn cancel_touches(&mut self, session: Option<&str>) {
        if !self.touch_active.is_empty() {
            let _ = self.fire_checked(
                "Input.dispatchTouchEvent",
                json!({ "type": "touchCancel", "touchPoints": [] }),
                session,
            );
            self.touch_active.clear();
        }
    }

    /// screencast 帧：先 ack（不 ack Chromium 会停发帧），再分发给订阅者。
    /// 订阅者信箱覆盖写入（丢旧保新）。
    /// ⚠️ ack 的 sessionId 必须原样透传事件 params 里的值：Chromium 各版本
    /// 类型不一（实测 152 为 int，协议文档写 string）——as_str() 会静默丢 ack，
    /// Chrome 发完首批帧后无限等待 → 实时画面掉到 ~1fps（曾长期误判为弱机性能）
    fn on_screencast_frame(&mut self, params: Value, session: Option<&str>) {
        if let Some(fs) = params.get("sessionId").cloned() {
            self.fire("Page.screencastFrameAck", json!({ "sessionId": fs }), session);
        }
        if let Some(b64) = params.get("data").and_then(|x| x.as_str()) {
            let frame = util::base64_decode(b64);
            self.push_frame(frame);
        }
        if let Some(s) = session {
            self.maybe_stop_screencast(s);
        }
    }

    /// 向所有订阅者推送一帧（screencast 事件与首帧兜底共用）：
    /// 覆盖写入信箱（丢旧保新）：观看端永远拿到最新画面，旧帧作废不排队。
    /// 软件限帧（Chrome 152 maxFrameRate 无效，实测设 5 仍 ~30fps 发帧）：
    /// 距上次推送不足 1000/fps ms 的帧直接丢弃——ack 已先行（Chromium 不受
    /// 影响，合成器照常出帧），观看端帧率精确受限；无人观看不记账。
    pub fn push_frame(&mut self, frame: Vec<u8>) {
        if self.sinks.is_empty() {
            return;
        }
        if frame_throttled(self.last_frame_push, Instant::now(), self.screencast_fps) {
            return;
        }
        self.last_frame_push = Some(Instant::now());
        for (_, box_) in &self.sinks {
            box_.post(frame.clone());
        }
    }

    /// 订阅实时画面帧（/stream.mjpg 用）。
    /// startScreencast 改为「等应答 + 瞬态错误退避重试」：实测页面导航换档期
    /// （渲染器切换）Chrome 会拒绝 Page.startScreencast/captureScreenshot
    /// （"Not attached to an active page"，日志实证）。旧实现 fire 后即忘——
    /// 错误应答被静默丢弃而 screencast_active 照样置真，此后流重连也永不
    /// 重发 startScreencast → 全程无帧，画面卡「等待首帧」直到 30s 超时
    /// 转截图模式（用户「选平台后画面一直卡在等待、需手动刷新」的根因）。
    /// 现在：成功才置 screencast_active；瞬态错误按 200/500/1200/3000ms
    /// 退避重试跨过导航窗口（最长约 4.9s）；持续失败返回 Err → 流即刻
    /// 500，页面立即转截图轮询并稍后重连，而不是干等 30 秒。
    /// 另：cast 开启后长时间无任何帧事件（失败被吞的兜底）→ 重新发起。
    /// 所有等待经 pump 继续分发输入事件（不占输入通道延迟）。
    /// jpeg 50% 逐合成器帧 + maxFrameRate（默认 25，/fps 运行时可调）；
    /// 帧尺寸 = 视口像素，与触摸坐标同坐标系。
    pub fn screencast_subscribe_pumped(
        &mut self,
        session: &str,
        pump: &mut dyn FnMut(&mut Cdp),
    ) -> Result<(u32, FrameBox), String> {
        let cast_stale = self
            .last_cast_frame
            .map(|t| t.elapsed() > Duration::from_secs(6))
            .unwrap_or(true);
        if !self.screencast_active || cast_stale {
            let backoffs = [200u64, 500, 1200, 3000];
            let mut last_err = String::new();
            let mut ok = false;
            for (i, b) in backoffs.iter().enumerate() {
                match self.call_pumped(
                    "Page.startScreencast",
                    json!({
                        "format": "jpeg",
                        "quality": 50,
                        "everyNthFrame": 1,
                        "maxFrameRate": self.screencast_fps
                    }),
                    Some(session),
                    3000,
                    &mut *pump, // 重借用：循环多轮传递（按值 move 会耗尽 &mut）
                ) {
                    Ok(_) => {
                        ok = true;
                        break;
                    }
                    Err(e) => {
                        if e.starts_with("WS:") {
                            return Err(e);
                        }
                        last_err = e;
                        if !is_transient_cast_error(&last_err) {
                            break; // 非瞬态错误：立即放弃（流 500 → 页面转截图轮询）
                        }
                        if i + 1 < backoffs.len() {
                            // 退避等待切片泵事件（期间新到帧照常分发；WS 断裂上抛）
                            let deadline = Instant::now() + Duration::from_millis(*b);
                            loop {
                                let left = deadline.saturating_duration_since(Instant::now());
                                if left.is_zero() {
                                    break;
                                }
                                self.pump_events(left.min(Duration::from_millis(100)))?;
                            }
                        }
                    }
                }
            }
            if ok {
                self.screencast_active = true;
                self.last_cast_frame = Some(Instant::now());
            } else if !self.screencast_active {
                return Err(format!("Page.startScreencast 失败: {last_err}"));
            }
            // cast_stale 重发失败但 cast 仍标记在播：保留现状（可能仍在出帧），
            // 信箱照常订阅，帧事件恢复即推送；流侧 30s 无帧自会转截图重连
        }
        let id = SINK_ID.fetch_add(1, Ordering::Relaxed);
        let box_ = FrameSlot::new();
        self.sinks.push((id, box_.clone()));
        Ok((id, box_))
    }

    /// 兼容包装（诊断探针测试等无泵场景）：无回调订阅
    pub fn screencast_subscribe(&mut self, session: &str) -> Result<(u32, FrameBox), String> {
        self.screencast_subscribe_pumped(session, &mut |_c| {})
    }

    /// 取消订阅；最后一个订阅者离开时自动 Page.stopScreencast。
    pub fn screencast_unsubscribe(&mut self, id: u32, session: &str) {
        self.sinks.retain(|(sid, _)| *sid != id);
        self.maybe_stop_screencast(session);
    }

    fn maybe_stop_screencast(&mut self, session: &str) {
        if self.screencast_active && self.sinks.is_empty() {
            self.screencast_active = false;
            self.fire("Page.stopScreencast", json!({}), Some(session));
        }
    }

    /// 运行时调整实时画面帧率（控制面板「设置 → 帧率」）。
    /// 软件限帧（见 push_frame）：只更新目标值即刻生效，无需重启 cast
    /// （实测 Chrome 152 stop+start 重建也不改变发帧频率——maxFrameRate
    /// 参数本身无效，重启反而白白造成一次流空窗）。
    pub fn set_screencast_fps(&mut self, fps: u32, _session: &str) {
        self.screencast_fps = fps.clamp(1, 60);
    }

    /// 触摸事件统一分发（引擎侧触点跟踪）。所有 /touch 输入与 tap/swipe
    /// 手势都走这里。协议规定（CDP Input 文档）：touchEnd/touchCancel 的
    /// touchPoints 必须为空（整组释放）、touchStart/touchMove 至少一点——
    /// puppeteer 同款形态。move 只回放已跟踪触点（导航/CDP 重建后控制页
    /// 续发的游离 move 丢弃，不凭空重建幽灵触点）。发后即忘（错误应答由
    /// outstanding/error_replies 通道留痕，不再静默）。
    pub fn dispatch_touch(
        &mut self,
        phase: &str,
        points: &[(f64, f64, i64)],
        session: &str,
    ) -> Result<(), String> {
        let typ = match phase {
            "start" => "touchStart",
            "move" => "touchMove",
            "end" => "touchEnd",
            _ => "touchCancel",
        };
        let tracked = track_touch_points(phase, points, &mut self.touch_active);
        if phase == "move" && tracked.is_empty() {
            return Ok(()); // 全部为游离触点（导航后的残留手势）：丢弃
        }
        let pts: Vec<Value> = tracked
            .iter()
            .map(|(x, y, id)| json!({ "x": x, "y": y, "id": id }))
            .collect();
        self.fire_checked("Input.dispatchTouchEvent", json!({ "type": typ, "touchPoints": pts }), Some(session))
    }

    /// 稳态循环空闲期泵取并分发 WS 消息（实时画面帧/事件）——
    /// 空闲睡眠的替代：预算内持续处理到达的消息，poll 空窗即返回。
    /// 每个泵周期向订阅信箱发心跳（消费侧据此判活/关流重连）。
    /// 传输层断裂返回「WS:」前缀错误（调用方据此走重连路径）。
    pub fn pump_events(&mut self, budget: Duration) -> Result<(), String> {
        for (_, box_) in &self.sinks {
            box_.touch_alive();
        }
        let deadline = Instant::now() + budget;
        loop {
            if Instant::now() >= deadline {
                return Ok(());
            }
            let poll = (Instant::now() + Duration::from_millis(50)).min(deadline);
            match self.ws.read_message(poll) {
                Ok(WsMessage::Text(t)) => {
                    let v: Value = match serde_json::from_str(&t) {
                        Ok(v) => v,
                        Err(_) => continue, // 非 JSON 帧（不应出现）：丢弃
                    };
                    if let Some(rid) = v.get("id").and_then(|x| x.as_u64()) {
                        let fire_method = self.outstanding.remove(&rid);
                        self.note_reply_error(fire_method.as_deref(), v.get("error"));
                        continue; // fire() 的回执等：无关应答（错误已留痕）
                    }
                    if let Some(m) = v.get("method").and_then(|x| x.as_str()) {
                        let sess = v.get("sessionId").and_then(|x| x.as_str()).map(|s| s.to_string());
                        self.on_event(m, v.get("params").cloned().unwrap_or(Value::Null), sess.as_deref());
                    }
                }
                Ok(WsMessage::Close) => return Err("WS: 连接已关闭(pump)".into()),
                Err(crate::ws::WsError::Timeout) => return Ok(()),
                Err(e) => return Err(format!("WS: 读取错误(pump): {e:?}")),
            }
        }
    }

    pub fn close(&mut self) {
        let _ = self.ws.send_close();
    }

    /// 实时流自愈（稳态循环每周期调用）：cast 标记在播且有订阅者，但距最近
    /// 一帧已超 6s——导航换档/渲染器切换后 Chromium 会单方面停止发帧
    /// （startScreencast 的订阅不跨 renderer 存活，实测「选平台后卡等待
    /// 需手动刷新」「回首页后画面冻结拖不动」的根因）。此前只在【新订阅】
    /// 时才重发 startScreencast：已建立的死流只能等消费侧超时关流重连。
    /// 现在引擎侧主动重发拉活：fire 即发（错误走 error_replies 留痕，导航
    /// 窗口的瞬态拒拒下周期自动再试；last_cast_frame 预置续期限流重试风暴）。
    /// 返回 true = 本次发出了重发（供上层落日志）。
    pub fn cast_rescue(&mut self, session: &str) -> bool {
        if !self.screencast_active || self.sinks.is_empty() {
            return false;
        }
        let stale = self
            .last_cast_frame
            .map(|t| t.elapsed() > Duration::from_secs(6))
            .unwrap_or(true);
        if !stale {
            return false;
        }
        // 先续期再重发：若重发被拒（Not attached），下个监督周期（~秒级）
        // 再试——每 6s 最多一次，不会打搭 CDP 通道
        self.last_cast_frame = Some(Instant::now());
        self.fire_checked(
            "Page.startScreencast",
            json!({
                "format": "jpeg",
                "quality": 50,
                "everyNthFrame": 1,
                "maxFrameRate": self.screencast_fps
            }),
            Some(session),
        )
        .is_ok()
    }
}

/// 在页面上执行 JS 表达式并取回返回值（returnByValue）。
pub fn evaluate(cdp: &mut Cdp, session: &str, expr: &str, timeout_ms: u64) -> Result<Value, String> {
    cdp.call(
        "Runtime.evaluate",
        json!({ "expression": expr, "returnByValue": true, "awaitPromise": false }),
        Some(session),
        timeout_ms,
    )
}

/// evaluate 结果里的返回值（字符串）。
/// 注意：call() 已解包响应外层 result —— Runtime.evaluate 的应答结构为
/// {"result": {"result": {type,value}, "exceptionDetails": …}}，故此处取
/// v["result"]["value"]（单层），exceptionDetails 与 result 平级。
pub fn eval_string(cdp: &mut Cdp, session: &str, expr: &str, timeout_ms: u64) -> Result<String, String> {
    let v = evaluate(cdp, session, expr, timeout_ms)?;
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

/// 软件限帧判定（纯函数，可单测）：距上次推送不足 1000/fps ms → 丢帧。
/// last=None（首次/重新订阅）永不丢。
fn frame_throttled(last: Option<Instant>, now: Instant, fps: u32) -> bool {
    let min_gap = Duration::from_millis(1000 / fps.clamp(1, 60) as u64);
    match last {
        Some(t) => now.duration_since(t) < min_gap,
        None => false,
    }
}

/// startScreencast/captureScreenshot 的瞬态错误判定（纯函数，可单测；引擎
/// 侧的兜底截图重试也用它）：页面导航换档期（渲染器切换，目标暂无活动
/// 页面）Chrome 会拒绝这类页面级命令（实测 chrome-headless-shell：
/// "Not attached to an active page"，导航完成后重试即成）——这类错误
/// 值得退避重试而非放弃。
pub fn is_transient_cast_error(e: &str) -> bool {
    e.contains("Not attached to an active page")
}

/// 触点跟踪（纯函数，可单测）。协议语义（CDP Input 文档：touchEnd/
/// touchCancel 不得携带触点 → 整组释放）：
/// - start：记录触点（已存在则覆盖坐标）
/// - move：只回放已跟踪触点（坐标更新）；未跟踪的游离去掉——导航/
///   CDP 重建后控制页续发的 move 若凭空重建，Chrome 会出现幽灵第二触点
///   （单指拖动被判成双指缩放，「拖动放大页面」的来源）
/// - end/cancel：清空全部跟踪触点，返回空列表（整组释放）
fn track_touch_points(
    phase: &str,
    points: &[(f64, f64, i64)],
    active: &mut BTreeMap<i64, (f64, f64)>,
) -> Vec<(f64, f64, i64)> {
    match phase {
        "start" => {
            for (x, y, id) in points {
                active.insert(*id, (*x, *y));
            }
            points.to_vec()
        }
        "move" => {
            // 只回放已跟踪触点（坐标更新）。用显式循环而非 filter+map 链：
            // 两个闭包同时捕获 active 会被借用检查拒绝（E0500）
            let mut out = Vec::new();
            for (x, y, id) in points {
                if active.contains_key(id) {
                    active.insert(*id, (*x, *y));
                    out.push((*x, *y, *id));
                }
            }
            out
        }
        _ => {
            active.clear();
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{frame_throttled, is_transient_cast_error, track_touch_points, FramePoll, FrameSlot, Cdp};
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    #[test]
    fn ws_error_prefix_convention() {
        // 传输错误统一 WS: 前缀——engine 依赖该约定做「重连/页面恢复」分流
        assert!("WS: 发送 x 失败".starts_with("WS:"));
        assert!("WS: 连接已关闭(x)".starts_with("WS:"));
        assert!(!"Runtime.evaluate 命令超时(5000ms)".starts_with("WS:"));
        assert!(!"Page.navigate 协议错误: {}".starts_with("WS:"));
    }

    /// Rust 版实时流探测器（诊断用，非回归）：用本 crate 的 ws.rs/cdp.rs 连
    /// 真实 chrome-headless-shell，复刻引擎稳态循环（每 1s tick evaluate +
    /// pump(50ms)），8s 数帧。python 客户端同环境实测 60fps；
    /// 若本测试 <60 帧 → 瓶颈在 ws.rs/cdp.rs；≥60 → 在 engine 主循环。
    /// 跑法：scripts/rust_probe.sh（起页面 server + chrome 后 cargo test -- --ignored）
    #[test]
    #[ignore = "需 scripts/rust_probe.sh 前置（页面 server + chrome 9336）"]
    fn probe_screencast_via_own_ws() {
        let mut cdp = Cdp::connect(9336).expect("连接 chrome 9336 失败");
        let targets = cdp.call("Target.getTargets", json!({}), None, 10000).unwrap();
        let tid = targets["targetInfos"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["type"] == "page")
            .unwrap()["targetId"]
            .as_str()
            .unwrap()
            .to_string();
        let attach = cdp
            .call("Target.attachToTarget", json!({ "targetId": tid, "flatten": true }), None, 10000)
            .unwrap();
        let session = attach["sessionId"].as_str().unwrap().to_string();
        cdp.call("Page.enable", json!({}), Some(&session), 10000).unwrap();
        cdp.call(
            "Page.navigate",
            json!({ "url": "http://127.0.0.1:18097/t.html" }),
            Some(&session),
            20000,
        )
        .unwrap();
        std::thread::sleep(Duration::from_secs(2));
        let (_sid, box_) = cdp.screencast_subscribe(&session).unwrap();

        let t0 = std::time::Instant::now();
        let mut next_tick = t0 + Duration::from_secs(1);
        let mut frames = 0u64;
        let mut per_sec = [0u64; 8];
        while t0.elapsed() < Duration::from_secs(8) {
            if std::time::Instant::now() >= next_tick {
                next_tick += Duration::from_secs(1);
                let _ = cdp.call(
                    "Runtime.evaluate",
                    json!({ "expression": "(function(){window.__CPK_TICK__&&window.__CPK_TICK__();return 'ok'})()", "returnByValue": true }),
                    Some(&session),
                    5000,
                );
            }
            let _ = cdp.pump_events(Duration::from_millis(50));
            while let FramePoll::Frame(_f) = box_.poll(Duration::from_millis(1)) {
                frames += 1;
                let s = (t0.elapsed().as_secs() as usize).min(7);
                per_sec[s] += 1;
            }
        }
        eprintln!("Rust 客户端 8s 帧数={frames} 每秒={:?}", per_sec);
        cdp.close();
        assert!(frames >= 60, "Rust 客户端帧率过低：{frames}（python 同环境 60fps）→ 瓶颈在 ws.rs/cdp.rs");
    }

    /// 软件限帧判定：5fps → 200ms 内的连发第二帧丢弃；超窗通过；首次不丢；
    /// fps 乱序值（0/999）clamp 到安全区间不 panic。
    #[test]
    fn frame_throttle_decision() {
        let t0 = Instant::now();
        assert!(!frame_throttled(None, t0, 5), "首次推送永不丢");
        assert!(frame_throttled(Some(t0), t0 + Duration::from_millis(50), 5), "200ms 窗内应丢");
        assert!(frame_throttled(Some(t0), t0 + Duration::from_millis(199), 5), "接近窗口仍丢");
        assert!(!frame_throttled(Some(t0), t0 + Duration::from_millis(201), 5), "超窗应过");
        // 60fps 窗口 16ms：快速连发仍受限，但 20ms 间隔应通过
        assert!(frame_throttled(Some(t0), t0 + Duration::from_millis(10), 60));
        assert!(!frame_throttled(Some(t0), t0 + Duration::from_millis(20), 60));
        // 边界 fps：0 clamp→1（窗口 1000ms），999 clamp→60（窗口 16ms），不 panic
        assert!(frame_throttled(Some(t0), t0 + Duration::from_millis(300), 0));
        assert!(!frame_throttled(Some(t0), t0 + Duration::from_millis(1100), 0));
        assert!(!frame_throttled(Some(t0), t0 + Duration::from_millis(300), 999));
    }

    /// 触点跟踪语义（协议规定 touchEnd/touchCancel 不得携带触点）：
    /// start 记录；move 只回放已跟踪触点（游离 move 丢弃，不重建幽灵触点）；
    /// end/cancel 恒空点整组释放。
    #[test]
    fn touch_point_tracking_semantics() {
        let mut active = BTreeMap::new();
        // start 记录触点
        let out = track_touch_points("start", &[(207.0, 680.0, 1)], &mut active);
        assert_eq!(out, vec![(207.0, 680.0, 1)]);
        assert_eq!(active.get(&1), Some(&(207.0, 680.0)));
        // move 更新已跟踪触点坐标
        let out = track_touch_points("move", &[(200.0, 500.0, 1)], &mut active);
        assert_eq!(out, vec![(200.0, 500.0, 1)]);
        assert_eq!(active.get(&1), Some(&(200.0, 500.0)));
        // 游离 move（未跟踪触点）丢弃：导航/会话重建后续发的残留手势
        // 不得凭空重建触点（幽灵第二触点 = 非预期双指缩放）
        let out = track_touch_points("move", &[(9.0, 9.0, 7)], &mut active);
        assert!(out.is_empty(), "游离 move 应丢弃");
        assert!(!active.contains_key(&7), "不得凭空重建跟踪");
        // 带点 end：同样空点整组释放（带点形态违反协议点列表约束会被 Chrome 拒绝）
        let out = track_touch_points("end", &[(200.0, 500.0, 1)], &mut active);
        assert!(out.is_empty(), "end 必须空点（协议规定）");
        assert!(active.is_empty(), "end 整组释放清空跟踪");
        // 双点在按 → cancel 空点整组取消
        track_touch_points("start", &[(10.0, 20.0, 1), (50.0, 60.0, 2)], &mut active);
        let out = track_touch_points("cancel", &[], &mut active);
        assert!(out.is_empty());
        assert!(active.is_empty());
    }

    /// 瞬态 cast 错误判定：导航换档期的 "Not attached to an active page"
    /// 值得退避重试；其他错误（超时/别的协议错误）立即放弃
    #[test]
    fn transient_cast_error_detection() {
        assert!(is_transient_cast_error(
            "Page.startScreencast 协议错误: {\"code\":-32000,\"message\":\"Not attached to an active page\"}"
        ));
        assert!(!is_transient_cast_error("Page.startScreencast 命令超时(3000ms)"));
        assert!(!is_transient_cast_error(
            "Page.startScreencast 协议错误: {\"code\":-32000,\"message\":\"Something else\"}"
        ));
        assert!(!is_transient_cast_error(""));
    }

    #[test]
    fn frame_slot_latest_wins_and_dead_detection() {
        // 信箱语义：连发两帧 → 消费侧只拿到最新帧（丢旧保新，不排队）
        let box_ = FrameSlot::new();
        box_.post(vec![1, 2]);
        box_.post(vec![3, 4]);
        match box_.poll(Duration::from_millis(10)) {
            FramePoll::Frame(f) => assert_eq!(f, vec![3, 4], "应覆盖旧帧只留最新"),
            _ => panic!("应取到帧"),
        }
        // 有心跳的空窗口 → Idle（静态页：流线程走 2s 心跳重发）
        box_.touch_alive();
        match box_.poll(Duration::from_millis(5)) {
            FramePoll::Idle => {}
            _ => panic!("心跳窗口内的空轮询应报 Idle"),
        }
        // 无心跳：入出口两次检查间无新 touch → 累计 missed≥2 → Dead
        // （首窗口已耗掉入口心跳，此处即第二个无心跳窗口）
        match box_.poll(Duration::from_millis(5)) {
            FramePoll::Dead => {}
            FramePoll::Idle => panic!("连续无心跳应报 Dead 而非 Idle"),
            FramePoll::Frame(_) => panic!("不应有帧"),
        }
    }
}
