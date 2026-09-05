//! 127.0.0.1/0.0.0.0 回环 HTTP 服务（对齐 Windows 版 report_server.rs 语义）：
//!  页面把 127.0.0.1 视为可信来源，HTTPS 页面 fetch http://127.0.0.1
//!  不受混合内容策略拦截 —— 同一机制在 Chromium Headless 下继续生效。
//!
//!  - GET  /report?status=…   页面状态上报（alive/retry/enter/confirm/try-enable/
//!                            expired/exited/paused/error/installed）
//!  - POST /log               页面诊断日志落盘（level+msg 表单）
//!  - GET  /healthz /status   健康检查（JSON；不健康 503）
//!  - GET  /                  极简控制页：截图 + 触摸/滑动/输入（首次登录用）
//!  - GET  /shot.jpg          页面截图（JPEG，控制页可访问）
//!  - POST /tap /swipe /type /key /nav /reload  控制端点（token 可选保护）
//!
//!  说明：控制端点经 channel 由引擎线程用 CDP Input 域执行 = 内核级触摸模拟，
//!        无需桌面/VNC；/report 与 /log 即使被 Chromium 专用网络访问(PNA)策略
//!        拦截也不影响保活（诊断另有 CDP __CPK_DRAIN__ 通道兜底，双保险）。

use crate::engine::{health_json, ControlRequest, SharedState};
use crate::logger::Logger;
use crate::util::urldecode;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// 状态转换提示（对齐 Windows report_server.rs 的通知语义，容器内落日志）
fn notify_text(status: &str) -> Option<&'static str> {
    match status {
        "exited" => Some("已退出云机"),
        "expired" => Some("云手机到期弹窗已确认"),
        "entered" => Some("已自动进入云机"),
        _ => None,
    }
}

#[derive(Clone)]
pub struct ReportCfg {
    pub bind: String,
    pub port: u16,
    pub control_token: String,
}

impl ReportCfg {
    pub fn from(cfg: &crate::config::Config) -> ReportCfg {
        ReportCfg {
            bind: cfg.bind.clone(),
            port: cfg.report_port,
            control_token: cfg.control_token.clone(),
        }
    }
}

const CONTROL_PAGE_HTML: &str = r#"<!doctype html>
<html lang="zh"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>CloudPhoneKeep 控制台</title>
<style>
body{margin:0;background:#1e293b;color:#e2e8f0;font:14px/1.5 system-ui,sans-serif;
display:flex;flex-direction:column;align-items:center;min-height:100vh}
h1{font-size:16px;margin:10px 0 2px;font-weight:600}
#meta{color:#94a3b8;font-size:12px;margin-bottom:8px}
#shot{max-width:420px;width:100%;border-radius:10px;box-shadow:0 4px 24px #0008;
cursor:pointer;background:#0f172a;min-height:200px}
.bar{display:flex;gap:6px;margin:10px 0;flex-wrap:wrap;justify-content:center}
input[type=text]{background:#0f172a;color:#e2e8f0;border:1px solid #475569;
border-radius:6px;padding:6px 8px;width:220px}
button{background:#334155;color:#e2e8f0;border:0;border-radius:6px;padding:6px 12px;cursor:pointer}
.note{color:#94a3b8;font-size:12px;max-width:420px;padding:0 12px 20px;text-align:center}
#toast{position:fixed;bottom:14px;background:#334155;padding:6px 14px;border-radius:8px;
opacity:0;transition:opacity .3s;pointer-events:none}
</style></head><body>
<h1>CloudPhoneKeep</h1><div id="meta">载入中…</div><img id="shot" alt="页面截图">
<div class="bar">
<input type="text" id="text" placeholder="输入文本后回车发送">
<button onclick="sendKey('Enter')">Enter</button>
<button onclick="sendKey('Backspace')">⌫</button>
<button onclick="reload()">刷新</button>
<button onclick="home()">回首页</button>
</div>
<div class="note">点击截图 = 触摸该位置（真实 TouchEvent）。文本框回车 = 输入并提交。
首次登录：在截图中点登录 → 手机号输入框点一下 → 输入手机号 → 收短信验证码后输入 → 登录态自动持久化到 Profile。</div>
<div id="toast"></div>
<script>
var QS = location.search;
function ping(t){ var el=document.getElementById('toast'); el.textContent=t; el.style.opacity=1;
  setTimeout(function(){ el.style.opacity=0; }, 1500); }
function refresh(){
  document.getElementById('shot').src = '/shot.jpg?_=' + Date.now() + QS;
  fetch('/healthz' + QS).then(r=>r.json()).then(function(j){
    document.getElementById('meta').textContent =
      j.account + ' · ' + j.platformLabel + ' · 浏览器 ' + j.browser + ' · 页面 ' + j.page +
      ' · ticks ' + j.ticks + ' · clicks ' + j.clicks + (j.exited ? ' · 已退出云机!' : '');
  }).catch(function(){});
}
document.getElementById('shot').addEventListener('click', function(ev){
  var img = this, r = img.getBoundingClientRect();
  var x = (ev.clientX - r.left) / r.width * (window.__vw || 414);
  var y = (ev.clientY - r.top) / r.height * (window.__vh || 896);
  fetch('/tap' + QS, {method:'POST', headers:{'content-type':'application/x-www-form-urlencoded'},
    body:'x=' + x + '&y=' + y}).then(function(){ ping('已触摸 ' + Math.round(x) + ',' + Math.round(y)); refresh(); });
});
async function sendKey(k){ await fetch('/key' + QS, {method:'POST',
  headers:{'content-type':'application/x-www-form-urlencoded'}, body:'key=' + encodeURIComponent(k)}); ping('按键 ' + k); }
async function reload(){ await fetch('/reload' + QS, {method:'POST'}); ping('已重载'); refresh(); }
async function home(){ await fetch('/nav' + QS, {method:'POST',
  headers:{'content-type':'application/x-www-form-urlencoded'}, body:'url=' + encodeURIComponent(jHome)}); }
document.getElementById('text').addEventListener('keydown', async function(ev){
  if (ev.key !== 'Enter') return;
  var v = this.value; this.value = '';
  await fetch('/type' + QS, {method:'POST',
    headers:{'content-type':'application/x-www-form-urlencoded'}, body:'text=' + encodeURIComponent(v)});
  await sendKey('Enter'); ping('已输入');
});
var jHome = location.origin;
fetch('/status' + QS).then(r=>r.json()).then(function(j){
  jHome = j.homeUri || jHome; window.__vw = 414; window.__vh = 896;
}).catch(function(){});
refresh(); setInterval(refresh, 2500);
</script></body></html>"#;

/// 启动服务（端口绑定必须在保活脚本注入前完成——脚本里写死了端口号）
pub fn start(
    cfg: ReportCfg,
    logger: Arc<Logger>,
    shared: Arc<SharedState>,
    ctrl: Sender<ControlRequest>,
) -> Result<u16, String> {
    let listener = TcpListener::bind((cfg.bind.as_str(), cfg.port))
        .map_err(|e| format!("绑定 {}:{} 失败：{e}", cfg.bind, cfg.port))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    thread::spawn(move || {
        for conn in listener.incoming() {
            match conn {
                Ok(stream) => {
                    let cfg = cfg.clone();
                    let logger = logger.clone();
                    let shared = shared.clone();
                    let ctrl = ctrl.clone();
                    thread::spawn(move || {
                        let _ = handle_conn(stream, &cfg, &logger, &shared, &ctrl);
                    });
                }
                Err(_) => thread::sleep(Duration::from_millis(200)),
            }
        }
    });
    Ok(port)
}

struct Req {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    form: HashMap<String, String>,
}

fn handle_conn(
    mut stream: TcpStream,
    cfg: &ReportCfg,
    logger: &Arc<Logger>,
    shared: &Arc<SharedState>,
    ctrl: &Sender<ControlRequest>,
) -> Result<(), String> {
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(30))).ok();
    let req = parse_request(&mut stream)?;
    let (status, ctype, body) = route(&req, cfg, logger, shared, ctrl);
    respond(&mut stream, status, &ctype, &body);
    let _ = stream.shutdown(std::net::Shutdown::Both);
    Ok(())
}

fn parse_request(stream: &mut TcpStream) -> Result<Req, String> {
    let mut raw: Vec<u8> = Vec::new();
    let mut buf = [0u8; 2048];
    // 读到请求头结束
    let head_end = loop {
        if let Some(p) = find(&raw, b"\r\n\r\n") {
            break p + 4;
        }
        if raw.len() > 32768 {
            return Err("请求头过大".into());
        }
        match stream.read(&mut buf) {
            Ok(0) => return Err("连接过早关闭".into()),
            Ok(n) => raw.extend_from_slice(&buf[..n]),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Err("读取请求超时".into())
            }
            Err(e) => return Err(format!("读取失败：{e}")),
        }
    };
    let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_uppercase();
    let target = parts.next().unwrap_or("/");
    let (path, query_str) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.to_string(), String::new()),
    };
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    // body（POST 表单，上限 64KB）
    let content_len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut body: Vec<u8> = raw[head_end..].to_vec();
    while body.len() < content_len && body.len() < 65536 {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }
    }
    let query = parse_form(&query_str);
    let form = if method == "POST" {
        parse_form(&String::from_utf8_lossy(&body))
    } else {
        HashMap::new()
    };
    Ok(Req { method, path, query, headers, form })
}

fn parse_form(s: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in s.split('&') {
        if pair.is_empty() {
            continue;
        }
        match pair.split_once('=') {
            Some((k, v)) => {
                map.insert(urldecode(k), urldecode(v));
            }
            None => {
                map.insert(urldecode(pair), String::new());
            }
        }
    }
    map
}

fn route(
    req: &Req,
    cfg: &ReportCfg,
    logger: &Arc<Logger>,
    shared: &Arc<SharedState>,
    ctrl: &Sender<ControlRequest>,
) -> (u16, String, Vec<u8>) {
    let p = req.path.as_str();
    // —— 页面上报通道（无鉴权：内容仅为状态/诊断，绑定地址由 CPK_BIND 控制）——
    if p == "/report" && req.method == "GET" {
        let status: String = req.query.get("status").cloned().unwrap_or_default().chars().take(40).collect();
        if !status.is_empty() {
            shared.touch_beat();
            if status == "exited" {
                shared.mark_exited();
            }
            if let Some(text) = notify_text(&status) {
                logger.log(1, "sys", &format!("页面状态：{status}（{text}）"));
            }
        }
        return (204, "text/plain".into(), Vec::new());
    }
    if p == "/log" && req.method == "POST" {
        let level = req.form.get("level").cloned().unwrap_or_else(|| "sys".into());
        let msg = req.form.get("msg").cloned().unwrap_or_default();
        if !msg.is_empty() {
            logger.log(1, &level, &msg);
        }
        return (204, "text/plain".into(), Vec::new());
    }
    if p == "/healthz" || p == "/status" {
        let h = shared.snapshot();
        let ok = h.ok;
        let body = health_json(&h).to_string();
        return (if ok { 200 } else { 503 }, "application/json".into(), body.into_bytes());
    }
    // —— 控制通道（token 可选保护）——
    if !token_ok(req, cfg) {
        return (403, "text/plain".into(), b"forbidden: token mismatch".to_vec());
    }
    match p {
        "/" | "/index.html" => (
            200,
            "text/html; charset=utf-8".into(),
            CONTROL_PAGE_HTML.as_bytes().to_vec(),
        ),
        "/shot.jpg" => match screenshot(ctrl) {
            Ok(jpg) => (200, "image/jpeg".into(), jpg),
            Err(e) => (500, "text/plain; charset=utf-8".into(), e.into_bytes()),
        },
        "/tap" => {
            let x = num(&req.query, &req.form, "x");
            let y = num(&req.query, &req.form, "y");
            control_void(ctrl, move |reply| ControlRequest::Tap { x, y, reply })
        }
        "/swipe" => {
            let x1 = num(&req.query, &req.form, "x1");
            let y1 = num(&req.query, &req.form, "y1");
            let x2 = num(&req.query, &req.form, "x2");
            let y2 = num(&req.query, &req.form, "y2");
            control_void(ctrl, move |reply| ControlRequest::Swipe { x1, y1, x2, y2, reply })
        }
        "/type" => {
            let text = req
                .query
                .get("text")
                .or_else(|| req.form.get("text"))
                .cloned()
                .unwrap_or_default();
            control_void(ctrl, move |reply| ControlRequest::TypeText { text, reply })
        }
        "/key" => {
            let key = req
                .query
                .get("key")
                .or_else(|| req.form.get("key"))
                .cloned()
                .unwrap_or_else(|| "Enter".into());
            control_void(ctrl, move |reply| ControlRequest::Key { key, reply })
        }
        "/nav" => {
            let url = req
                .query
                .get("url")
                .or_else(|| req.form.get("url"))
                .cloned()
                .unwrap_or_default();
            control_void(ctrl, move |reply| ControlRequest::Navigate { url, reply })
        }
        "/reload" => control_void(ctrl, |reply| ControlRequest::Reload { reply }),
        _ => (404, "text/plain".into(), b"not found".to_vec()),
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

fn token_ok(req: &Req, cfg: &ReportCfg) -> bool {
    if cfg.control_token.is_empty() {
        return true;
    }
    let q = req.query.get("token").map(|s| s.as_str()).unwrap_or("");
    let h = req.headers.get("x-cpk-token").map(|s| s.as_str()).unwrap_or("");
    q == cfg.control_token || h == cfg.control_token
}

fn num(query: &HashMap<String, String>, form: &HashMap<String, String>, key: &str) -> f64 {
    query
        .get(key)
        .or_else(|| form.get(key))
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0)
}

fn screenshot(ctrl: &Sender<ControlRequest>) -> Result<Vec<u8>, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    ctrl.send(ControlRequest::Screenshot { reply: tx })
        .map_err(|_| "引擎不可用".to_string())?;
    match rx.recv_timeout(Duration::from_secs(20)) {
        Ok(r) => r,
        Err(_) => Err("截图超时（引擎忙或浏览器未就绪）".into()),
    }
}

fn control_void(
    ctrl: &Sender<ControlRequest>,
    build: impl FnOnce(Sender<Result<(), String>>) -> ControlRequest,
) -> (u16, String, Vec<u8>) {
    let (tx, rx) = std::sync::mpsc::channel();
    if ctrl.send(build(tx)).is_err() {
        return (500, "text/plain".into(), b"engine unavailable".to_vec());
    }
    match rx.recv_timeout(Duration::from_secs(20)) {
        Ok(Ok(())) => (200, "text/plain".into(), b"ok".to_vec()),
        Ok(Err(e)) => (500, "text/plain; charset=utf-8".into(), e.into_bytes()),
        Err(_) => (504, "text/plain".into(), b"engine busy / timeout".to_vec()),
    }
}

fn respond(stream: &mut TcpStream, status: u16, ctype: &str, body: &[u8]) {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "OK",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn start_server(token: &str) -> (u16, Arc<SharedState>, Sender<ControlRequest>) {
        let cfg = Config::from_env();
        let shared = SharedState::new(&cfg);
        let (tx, _rx) = std::sync::mpsc::channel();
        let rcfg = ReportCfg { bind: "127.0.0.1".into(), port: 0, control_token: token.into() };
        let port = start(rcfg, Arc::new(Logger::new(cfg.log_dir.clone())), shared.clone(), tx.clone()).unwrap();
        (port, shared, tx)
    }

    fn http(port: u16, req: &str) -> (u16, String) {
        use std::io::{Read, Write};
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        let mut buf = [0u8; 4096];
        loop {
            match s.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => out.push_str(&String::from_utf8_lossy(&buf[..n])),
            }
        }
        let status: u16 = out.split_whitespace().nth(1).and_then(|v| v.parse().ok()).unwrap_or(0);
        let body = out.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
        (status, body)
    }

    #[test]
    fn report_updates_beat_and_status_endpoint() {
        let (port, shared, _tx) = start_server("");
        let (st, _) = http(port, "GET /report?status=alive&slot=1 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 204);
        assert!(shared.snapshot().last_beat_ms > 0);
        let (st, _) = http(port, "GET /report?status=exited HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 204);
        assert!(shared.snapshot().exited);
        // healthz：未运行浏览器 → 503 + JSON
        let (st, body) = http(port, "GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 503);
        assert!(body.contains("\"platform\""));
        let (st, body) = http(port, "GET /status HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 503);
        assert!(body.contains("\"homeUri\""));
    }

    #[test]
    fn token_protection() {
        let (port, _shared, _tx) = start_server("s3cret");
        // 无 token → 403
        let (st, _) = http(port, "GET /shot.jpg HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 403);
        // 带 token → 引擎侧无接收者时 500（证明已过鉴权）
        let (st, body) = http(port, "GET /shot.jpg?token=s3cret HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 500);
        assert!(body.contains("引擎不可用"));
        // 控制页带 token 可访问
        let (st, body) = http(port, "GET /?token=s3cret HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 200);
        assert!(body.contains("CloudPhoneKeep"));
        // /report 与 /healthz 不要求 token（页面脚本无法携带）
        let (st, _) = http(port, "GET /report?status=alive HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 204);
        let (st, _) = http(port, "GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 503);
    }

    #[test]
    fn post_log_and_form_parsing() {
        let dir = std::env::temp_dir().join(format!("cpk-srv-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = Config::from_env();
        let shared = SharedState::new(&cfg);
        let (tx, _rx) = std::sync::mpsc::channel();
        let rcfg = ReportCfg { bind: "127.0.0.1".into(), port: 0, control_token: String::new() };
        let logger = Arc::new(Logger::new(dir.clone()));
        let port = start(rcfg, logger, shared, tx).unwrap();
        let body = "level=beat&msg=tick%3D3%20url%3D%2Fhome";
        let req = format!(
            "POST /log HTTP/1.1\r\nHost: x\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let (st, _) = http(port, &req);
        assert_eq!(st, 204);
        // 落盘验证
        let files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect();
        assert_eq!(files.len(), 1);
        let text = std::fs::read_to_string(&files[0]).unwrap();
        assert!(text.contains("[slot=1] [beat] tick=3 url=/home"), "{text}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
