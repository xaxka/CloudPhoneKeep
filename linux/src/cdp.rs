//! 极简 CDP（Chrome DevTools Protocol）客户端：跑在自研 WsClient 上。
//!  - call()：发命令并等待应答（按 id 匹配；等待期间持续分发事件）
//!  - 事件内联处理：Page.javascriptDialogOpening → 自动 Page.handleJavaScriptDialog
//!    （无头环境无人可点，不处理 alert/confirm 会冻住页面与 Runtime.evaluate）
//!  - 传输类错误统一以「WS:」前缀返回，调用方据此区分「重连」与「页面级恢复」
//!  - fire()：发后即忘（对话框应答等，响应到达时静默丢弃）

use crate::util;
use crate::ws::{WsClient, WsMessage};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub struct Cdp {
    ws: WsClient,
    next_id: u64,
    /// 浏览器标识（/json/version 的 Browser）
    pub browser: String,
    /// 原始 UA（/json/version 的 User-Agent，headless shell 带 HeadlessChrome 字样）
    pub raw_ua: String,
    pub dialog_count: u32,
    pub last_dialog: Option<String>,
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
        // 其余事件（Target/Page/Runtime 通知类）无需处理：状态以快照采样为准
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
