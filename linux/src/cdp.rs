//! 极简 CDP（Chrome DevTools Protocol）客户端：跑在自研 WsClient 上。
//!  - call()：发命令并等待应答（按 id 匹配；等待期间持续分发事件）
//!  - 事件内联处理：Page.javascriptDialogOpening → 自动 Page.handleJavaScriptDialog
//!    （无头环境无人可点，不处理 alert/confirm 会冻住页面与 Runtime.evaluate）
//!  - 传输类错误统一以「WS:」前缀返回，调用方据此区分「重连」与「页面级恢复」
//!  - fire()：发后即忘（对话框应答等，响应到达时静默丢弃）

use crate::util;
use crate::ws::{WsClient, WsMessage};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
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
    /// 实时画面帧订阅者（每个 /stream.mjpg 连接一通道，容量 2 满即丢帧保实时）
    sinks: Vec<(u32, SyncSender<Vec<u8>>)>,
    /// Page.startScreencast 是否在跑（首个订阅者开启，最后一个离开自动关，
    /// 无人观看不消耗 JPEG 编码 CPU）
    screencast_active: bool,
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
        })
    }

    fn build_msg(&mut self, method: &str, params: Value, session: Option<&str>) -> (u64, String) {
        let id = self.next_id;
        self.next_id += 1;
        let mut m = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            m["sessionId"] = Value::String(s.to_string());
        }
        (id, m.to_string())
    }

    /// 发后即忘（不等待应答；响应到达时按「无关 id」静默丢弃）
    pub fn fire(&mut self, method: &str, params: Value, session: Option<&str>) {
        let (_, text) = self.build_msg(method, params, session);
        let _ = self.ws.send_text(&text);
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
                        if rid != id {
                            continue; // fire() 的回执等：无关应答
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
                Err(crate::ws::WsError::Timeout) => continue, // 本轮 poll 无消息
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
            self.on_screencast_frame(params, session);
        }
        // 其余事件（Target/Page/Runtime 通知类）无需处理：状态以快照采样为准
    }

    /// screencast 帧：先 ack（不 ack Chromium 会停发帧），再分发给订阅者。
    /// 订阅者通道满 → 丢该帧（实时优先于完整）；接收端已断开 → 移除该订阅者。
    fn on_screencast_frame(&mut self, params: Value, session: Option<&str>) {
        if let Some(fs) = params.get("sessionId").and_then(|x| x.as_str()) {
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
    /// 通道满 → 丢帧保实时；接收端已断开 → 移除该订阅者（下次 stopScreencast 机会触发）
    pub fn push_frame(&mut self, frame: Vec<u8>) {
        if self.sinks.is_empty() {
            return;
        }
        self.sinks.retain(|(_, tx)| match tx.try_send(frame.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Disconnected(_)) => false,
        });
    }

    /// 订阅实时画面帧（/stream.mjpg 用）。
    /// Page.startScreencast 发后即忘（fire）：同步等待应答曾在引擎忙/弱机场景
    /// 占满 10s 超时——把 ScreencastAttach 应答压在队尾，表现为「连接实时画面…」
    /// 10 秒。首帧由调用方补一帧 captureScreenshot 兜底（静态页/错误页合成器
    /// 无更新时 screencast 可能长期不发帧）。
    /// jpeg 60% 逐合成器帧；帧尺寸 = 视口像素，与触摸坐标同坐标系。
    pub fn screencast_subscribe(&mut self, session: &str) -> Result<(u32, Receiver<Vec<u8>>), String> {
        if !self.screencast_active {
            self.fire(
                "Page.startScreencast",
                json!({ "format": "jpeg", "quality": 60, "everyNthFrame": 1 }),
                Some(session),
            );
            self.screencast_active = true;
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        let id = SINK_ID.fetch_add(1, Ordering::Relaxed);
        self.sinks.push((id, tx));
        Ok((id, rx))
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

    /// 稳态循环空闲期泵取并分发 WS 消息（实时画面帧/事件）——
    /// 空闲睡眠的替代：预算内持续处理到达的消息，poll 空窗即返回。
    /// 传输层断裂返回「WS:」前缀错误（调用方据此走重连路径）。
    pub fn pump_events(&mut self, budget: Duration) -> Result<(), String> {
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
                    if v.get("id").is_some() {
                        continue; // fire() 的回执等：无关应答
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

#[cfg(test)]
mod tests {
    #[test]
    fn ws_error_prefix_convention() {
        // 传输错误统一 WS: 前缀——engine 依赖该约定做「重连/页面恢复」分流
        assert!("WS: 发送 x 失败".starts_with("WS:"));
        assert!("WS: 连接已关闭(x)".starts_with("WS:"));
        assert!(!"Runtime.evaluate 命令超时(5000ms)".starts_with("WS:"));
        assert!(!"Page.navigate 协议错误: {}".starts_with("WS:"));
    }
}
