//! 127.0.0.1/0.0.0.0 回环 HTTP 服务（对齐 Windows 版 report_server.rs 语义）：
//!  页面把 127.0.0.1 视为可信来源，HTTPS 页面 fetch http://127.0.0.1
//!  不受混合内容策略拦截 —— 同一机制在 Chromium Headless 下继续生效。
//!
//!  - GET  /report?status=…   页面状态上报（alive/retry/enter/confirm/try-enable/
//!                            expired/exited/paused/error/installed）
//!  - POST /log               页面诊断日志落盘（level+msg 表单）
//!  - GET  /healthz /status   健康检查（JSON；不健康 503）
//!  - GET  /                  控制页：左实时画面 + 右操作栏（自适应布局）
//!  - GET  /stream.mjpg       实时画面流（MJPEG multipart，Page.startScreencast 帧直推，
//!                            断流自动重连；页面侧不可用时退化 /shot.jpg 轮询）
//!  - GET  /shot.jpg          单帧页面截图（JPEG，兼容/兜底用）
//!  - POST /touch             实时触摸流（phase=start/move/end/cancel；多点 ps=x,y,id;…）
//!  - POST /mouse             真实鼠标事件（action=move/down/up/wheel，全键位/滚轮）
//!  - POST /kbd               键盘事件全字段直通（t=down/up，key/code/vk/text/mods）
//!  - POST /type              文本插入（Input.insertText；输入法/粘贴整段发送）
//!  - GET  /clip              读取云机选中文本（云机 → 本机剪贴板）
//!  - POST /fps               运行时帧率上限（引擎侧软件限帧，实测 Chrome 152
//!                            maxFrameRate 参数无效）
//!  - POST /quality           运行时 JPEG 画质 10..90（cast 活动时重建即刻生效）
//!  - POST /scale             运行时采集分辨率百分比 30..100（<100 编码前缩小）
//!  - POST /platform          平台选择/切换（mobile/unicom；打开控制页时首选，
//!                            之后可随时切换：换首页/视口/保活脚本，云机实例自动
//!                            重启，Profile 保留双平台登录态）
//!  - POST /tap /swipe /key /nav /reload  控制端点（token 可选保护；兼容保留）
//!
//!  说明：控制端点经 channel 由引擎线程用 CDP Input 域执行 = 内核级触摸模拟，
//!        无需桌面/VNC；/report 与 /log 即使被 Chromium 专用网络访问(PNA)策略
//!        拦截也不影响保活（诊断另有 CDP __CPK_DRAIN__ 通道兜底，双保险）。

use crate::cdp::FramePoll;
use crate::engine::{health_json, ControlRequest, SharedState, TouchPoint};
use crate::logger::Logger;
use crate::util::urldecode;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

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

const CONTROL_PAGE_HTML: &str = include_str!("../../shared/control_page.html");

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
    // 实时画面流：响应体无界（multipart），绕过 route/respond 一次性模型直写 socket
    if req.method == "GET" && req.path == "/stream.mjpg" {
        if !token_ok(&req, cfg) {
            respond(&mut stream, 403, "text/plain", "forbidden: token mismatch".as_bytes());
        } else {
            stream_mjpeg(&mut stream, ctrl, logger);
        }
        let _ = stream.shutdown(std::net::Shutdown::Both);
        return Ok(());
    }
    let (status, ctype, body) = route(&req, cfg, logger, shared, ctrl);
    respond(&mut stream, status, &ctype, &body);
    let _ = stream.shutdown(std::net::Shutdown::Both);
    Ok(())
}

/// 实时画面流：向引擎订阅 screencast 帧信箱（只存最新帧），以 multipart/x-mixed-replace
/// 推送（MJPEG）。退出条件：客户端断开（写失败）/ 生产侧心跳丢失（CDP 重建、
/// 浏览器重启：连续两个窗口无引擎泵心跳）/ 首帧 12s 未至（引擎极端繁忙）。
/// 关流后页面侧自动重连。静态页面合成器无更新 → screencast 不发新帧：以 2s
/// 心跳重发上一帧维持连接。
fn stream_mjpeg(stream: &mut TcpStream, ctrl: &Sender<ControlRequest>, logger: &Arc<Logger>) {
    const BOUNDARY: &str = "cpkframe";
    // 1) 订阅引擎实时画面：引擎线程可能正在慢 eval（弱机 tick/采样可达数秒）/
    //    启动浏览器，宽限 8s 再判超时；引擎彻底不可用会立刻 500
    let (tx, rx) = std::sync::mpsc::channel();
    if ctrl.send(ControlRequest::ScreencastAttach { reply: tx }).is_err() {
        respond(stream, 500, "text/plain; charset=utf-8", "引擎不可用".as_bytes());
        return;
    }
    // 平台冷启动全程 ~13s（Chromium 启动 + CDP 装配 + 导航），等待窗放宽到
    // 20s：8s 会在引擎刚到稳态前掐掉订阅，多走一轮「截图模式 → 8s 后重连」
    let (sub_id, frame_box) = match rx.recv_timeout(Duration::from_secs(20)) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            respond(
                stream,
                500,
                "text/plain; charset=utf-8",
                format!("画面流建立失败：{e}").as_bytes(),
            );
            return;
        }
        Err(_) => {
            respond(
                stream,
                504,
                "text/plain; charset=utf-8",
                "画面流建立超时（引擎忙或浏览器启动中）".as_bytes(),
            );
            return;
        }
    };
    // 2) 流头 + 帧循环（multipart；每次写失败即客户端已断开）。
    //    帧信箱只存最新帧：引擎覆盖写入（丢旧保新），此处 poll 等待/取帧
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary={BOUNDARY}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(head.as_bytes()).is_err() {
        screencast_detach(ctrl, sub_id);
        return;
    }
    let mut last: Option<Vec<u8>> = None;
    let mut last_push = Instant::now();
    let opened = Instant::now();
    loop {
        match frame_box.poll(Duration::from_secs(2)) {
            FramePoll::Frame(frame) => {
                if !write_part(stream, BOUNDARY, &frame) {
                    break;
                }
                last = Some(frame);
                last_push = Instant::now();
            }
            FramePoll::Idle => {
                // 静态页：生产侧活着但无新帧 → 心跳重发上一帧维持连接
                // （路由器/代理不掐空闲连接，页面也不闪重连循环）
                if let Some(f) = &last {
                    if last_push.elapsed() >= Duration::from_secs(2) {
                        if !write_part(stream, BOUNDARY, f) {
                            break;
                        }
                        last_push = Instant::now();
                    }
                } else if opened.elapsed() > Duration::from_secs(12) {
                    // 首帧 12s 未至（引擎极端繁忙/浏览器启动中）→ 关流，页面转截图兜底
                    // （30s→12s：死流期间引擎侧 cast_rescue 已在 6s 级重发拉活，
                    // 12s 仍无帧说明流路彻底不可用——早转截图让用户见到画面，
                    // 不再干等需手动刷新）
                    logger.log(1, "sys", "实时画面流首帧 12s 未至，关流（页面自动转截图轮询并重连）");
                    break;
                }
            }
            // 生产侧心跳丢失：CDP 会话重建/浏览器重启，旧信箱不会再有帧
            // → 立即关流，页面 1.5s 重连新订阅（替代旧 mpsc 的 Disconnected 信号）
            FramePoll::Dead => {
                logger.log(1, "sys", "实时画面流生产侧心跳丢失，关流重连（CDP 会话重建/浏览器重启）");
                break;
            }
        }
    }
    screencast_detach(ctrl, sub_id);
}

/// 写一帧 multipart part；任何写失败 = 客户端已断开。
fn write_part(stream: &mut TcpStream, boundary: &str, frame: &[u8]) -> bool {
    let part = format!(
        "--{boundary}\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
        frame.len()
    );
    stream.write_all(part.as_bytes()).is_ok()
        && stream.write_all(frame).is_ok()
        && stream.write_all(b"\r\n").is_ok()
}

/// 发后即忘的取消订阅：应答接收端立即丢弃（引擎应答时发送失败被静默忽略），
/// 引擎在下一监督周期执行；最后一个订阅者离开时自动 Page.stopScreencast。
fn screencast_detach(ctrl: &Sender<ControlRequest>, id: u32) {
    let (tx, _rx) = std::sync::mpsc::channel();
    let _ = ctrl.send(ControlRequest::ScreencastDetach { id, reply: tx });
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
            // 状态记录（对齐 Windows 版 on_report 状态迁移语义）：控制页
            // 据此检测 exited/expired 转换并发通知（Windows 版发系统通知）
            shared.set_status(&status);
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
        "/touch" => {
            // 实时触摸流：phase/ps 在 HTTP 层校验（非法 400，不占引擎）；坐标与 /tap 同坐标系。
            // 单点兼容：phase + x/y；多点：ps="x1,y1,id1;x2,y2,id2"（Chromium 逐点语义）
            let phase = req
                .query
                .get("phase")
                .or_else(|| req.form.get("phase"))
                .cloned()
                .unwrap_or_default();
            if !matches!(phase.as_str(), "start" | "move" | "end" | "cancel") {
                return (
                    400,
                    "text/plain; charset=utf-8".into(),
                    b"phase must be start/move/end/cancel".to_vec(),
                );
            }
            let ps = req
                .query
                .get("ps")
                .or_else(|| req.form.get("ps"))
                .cloned()
                .unwrap_or_default();
            let points: Vec<TouchPoint> = if matches!(phase.as_str(), "end" | "cancel") {
                // 协议规定 touchEnd/touchCancel 不得携带触点（整组释放）：
                // 忽略 ps——带点释放形态违反协议点列表约束会被 Chrome 拒绝，
                // 页面收不到 touchend 的 tap 收尾（轻点「没反应」的直接根源）
                Vec::new()
            } else if !ps.is_empty() {
                match parse_touch_points(&ps) {
                    Some(v) => v,
                    None => {
                        return (
                            400,
                            "text/plain; charset=utf-8".into(),
                            b"ps must be x,y,id triples joined by ; (id 1..=10)".to_vec(),
                        )
                    }
                }
            } else {
                // start/move 单点兼容：phase + x/y
                let x = num(&req.query, &req.form, "x");
                let y = num(&req.query, &req.form, "y");
                vec![TouchPoint { x, y, id: 1 }]
            };
            control_void(ctrl, move |reply| ControlRequest::Touch { phase, points, reply })
        }
        "/mouse" => {
            // 真实鼠标事件：action/action 按钮在 HTTP 层校验；坐标与 /touch 同坐标系。
            // move=悬停/拖动，down/up=按下/抬起（clickCount 支撑双击/三击），
            // wheel=滚轮（dx/dy 像素）。修饰键位图：Alt=1 Ctrl=2 Meta=4 Shift=8
            let action = req
                .query
                .get("action")
                .or_else(|| req.form.get("action"))
                .cloned()
                .unwrap_or_default();
            if !matches!(action.as_str(), "move" | "down" | "up" | "wheel") {
                return (
                    400,
                    "text/plain; charset=utf-8".into(),
                    b"action must be move/down/up/wheel".to_vec(),
                );
            }
            let button = req
                .query
                .get("b")
                .or_else(|| req.form.get("b"))
                .cloned()
                .unwrap_or_else(|| "left".into());
            if !matches!(button.as_str(), "none" | "left" | "right" | "middle") {
                return (
                    400,
                    "text/plain; charset=utf-8".into(),
                    b"b must be none/left/right/middle".to_vec(),
                );
            }
            let x = num(&req.query, &req.form, "x");
            let y = num(&req.query, &req.form, "y");
            let dx = num(&req.query, &req.form, "dx");
            let dy = num(&req.query, &req.form, "dy");
            let buttons = unum(&req.query, &req.form, "bb") as u32;
            let click_count = (unum(&req.query, &req.form, "n") as u32).clamp(1, 3);
            let modifiers = (unum(&req.query, &req.form, "m") as u32).min(31);
            control_void(ctrl, move |reply| ControlRequest::Mouse {
                action, x, y, button, buttons, click_count, dx, dy, modifiers, reply,
            })
        }
        "/kbd" => {
            // 键盘事件全字段直通：t=down/up；key/code 限长防滥用；vk 0..=255；
            // text ≤16 字符（长文本/粘贴走 /type insertText）
            let typ = req
                .query
                .get("t")
                .or_else(|| req.form.get("t"))
                .cloned()
                .unwrap_or_default();
            if !matches!(typ.as_str(), "down" | "up") {
                return (
                    400,
                    "text/plain; charset=utf-8".into(),
                    b"t must be down/up".to_vec(),
                );
            }
            let key = take(&req, "key", 32);
            let code = take(&req, "code", 32);
            let vk = (unum(&req.query, &req.form, "vk") as u32).min(65535);
            let text = take(&req, "text", 16);
            let modifiers = (unum(&req.query, &req.form, "m") as u32).min(31);
            let location = (unum(&req.query, &req.form, "l") as u32).min(3);
            let auto_repeat = matches!(
                req.query.get("r").or_else(|| req.form.get("r")).map(|s| s.as_str()),
                Some("1" | "true")
            );
            control_void(ctrl, move |reply| ControlRequest::KeyEvent {
                typ, key, code, vk, text, modifiers, location, auto_repeat, reply,
            })
        }
        "/clip" => {
            // 云机选区 → 控制页（控制页写入本机剪贴板）：复制按钮/Ctrl+C 数据源
            let (tx, rx) = std::sync::mpsc::channel();
            if ctrl.send(ControlRequest::ClipGet { reply: tx }).is_err() {
                return (500, "text/plain".into(), b"engine unavailable".to_vec());
            }
            match rx.recv_timeout(Duration::from_secs(20)) {
                Ok(Ok(t)) => (200, "text/plain; charset=utf-8".into(), t.into_bytes()),
                Ok(Err(e)) => (500, "text/plain; charset=utf-8".into(), e.into_bytes()),
                Err(_) => (504, "text/plain".into(), b"engine busy / timeout".to_vec()),
            }
        }
        "/platform" => {
            // 平台运行时切换：mobile/unicom（HTTP 层校验）。引擎更新共享状态后
            // 重启云机实例（新视口/新保活脚本 CFG/新首页导航）
            let p = req
                .query
                .get("value")
                .or_else(|| req.form.get("value"))
                .cloned()
                .unwrap_or_default();
            if !matches!(p.as_str(), "mobile" | "unicom") {
                return (
                    400,
                    "text/plain; charset=utf-8".into(),
                    b"value must be mobile/unicom".to_vec(),
                );
            }
            control_void(ctrl, move |reply| ControlRequest::SetPlatform { platform: p, reply })
        }
        "/fps" => {
            // 帧率上限：1..=60；引擎侧软件限帧（Chrome 152 的
            // startScreencast maxFrameRate 参数实测无效，见 cdp.rs push_frame）。
            // 共享状态由 HTTP 层直写：引擎待机/重启窗口期设置同样立即生效
            // （下次 CDP 装配恢复 + 稳态循环每周期同步），不再依赖引擎控制
            // 通道存活——此前待机期设帧率被引擎快速失败，控制页却报
            // 「已设为 N」（post 不检查 r.ok 的谎报），healthz 仍回旧值
            // （用户实测「设置 10 显示上限 25」的根因）。
            // 引擎在线时的即刻生效通知仍尽力转发（失败由稳态同步兜底）。
            let fps = unum(&req.query, &req.form, "value");
            if !(1..=60).contains(&fps) {
                return (
                    400,
                    "text/plain; charset=utf-8".into(),
                    b"value must be 1..=60".to_vec(),
                );
            }
            let fps = fps as u32;
            shared.set_fps(fps);
            let (tx, _rx) = std::sync::mpsc::channel();
            let _ = ctrl.send(ControlRequest::SetFps { fps, reply: tx });
            (200, "text/plain".into(), b"ok".to_vec())
        }
        "/quality" => {
            // 实时画面 JPEG 画质（10..=90）：与 /fps 同模式——共享状态由 HTTP 层
            // 直写（引擎待机/重启窗口期设置同样立即生效），引擎在线时尽力转发
            // （cast 活动且有观众时 stop+start 重建让新画质即刻可见）
            let q = unum(&req.query, &req.form, "value");
            if !(10..=90).contains(&q) {
                return (400, "text/plain; charset=utf-8".into(), b"value must be 10..=90".to_vec());
            }
            let q = q as u32;
            shared.set_quality(q);
            let (tx, _rx) = std::sync::mpsc::channel();
            let _ = ctrl.send(ControlRequest::SetQuality { quality: q, reply: tx });
            (200, "text/plain".into(), b"ok".to_vec())
        }
        "/scale" => {
            // 采集分辨率百分比（30..=100）：同 /quality 模式；<100 时 Chrome
            // 编码前按当前平台视口等比缩小（触摸坐标是 CSS 系不受影响）
            let s = unum(&req.query, &req.form, "value");
            if !(30..=100).contains(&s) {
                return (400, "text/plain; charset=utf-8".into(), b"value must be 30..=100".to_vec());
            }
            let s = s as u32;
            shared.set_scale(s);
            let (tx, _rx) = std::sync::mpsc::channel();
            let _ = ctrl.send(ControlRequest::SetScale { scale_pct: s, reply: tx });
            (200, "text/plain".into(), b"ok".to_vec())
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

/// 无符号整数参数（非法/缺失 → 0）
fn unum(query: &HashMap<String, String>, form: &HashMap<String, String>, key: &str) -> u64 {
    query
        .get(key)
        .or_else(|| form.get(key))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
}

/// 字符串参数截断（限长防滥用）
fn take(req: &Req, key: &str, max_chars: usize) -> String {
    req.query
        .get(key)
        .or_else(|| req.form.get(key))
        .cloned()
        .unwrap_or_default()
        .chars()
        .take(max_chars)
        .collect()
}

/// 多点触控参数解析："x1,y1,id1;x2,y2,id2"（id 1..=10，最多 10 点）
fn parse_touch_points(s: &str) -> Option<Vec<TouchPoint>> {
    let mut v = Vec::new();
    for part in s.split(';') {
        if part.is_empty() {
            continue;
        }
        let f: Vec<&str> = part.split(',').collect();
        if f.len() != 3 {
            return None;
        }
        let x: f64 = f[0].trim().parse().ok()?;
        let y: f64 = f[1].trim().parse().ok()?;
        let id: i64 = f[2].trim().parse().ok()?;
        if !(1..=10).contains(&id) {
            return None;
        }
        v.push(TouchPoint { x, y, id });
        if v.len() > 10 {
            return None;
        }
    }
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
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
        // healthz：未运行浏览器 → 503 + JSON；状态记录（对齐 Windows 版 on_report
        // 状态迁移语义）：/report 后 lastStatus 如实回显，title 字段在位
        let (st, body) = http(port, "GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 503);
        assert!(body.contains("\"platform\""));
        assert!(body.contains("\"lastStatus\":\"exited\""), "exited 未记录：{body}");
        assert!(body.contains("\"title\""), "healthz 缺 title 字段：{body}");
        let (st, body) = http(port, "GET /status HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 503);
        assert!(body.contains("\"homeUri\""));
    }

    #[test]
    fn report_status_field_updates() {
        // lastStatus 随 /report 逐次覆盖：alive → expired → healthz 如实回显
        let (port, _shared, _tx) = start_server("");
        http(port, "GET /report?status=alive HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        let (st, body) = http(port, "GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 503);
        assert!(body.contains("\"lastStatus\":\"alive\""), "{body}");
        http(port, "GET /report?status=expired HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        let (_, body) = http(port, "GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert!(body.contains("\"lastStatus\":\"expired\""), "{body}");
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
    fn stream_endpoint_guards() {
        // token 保护与 /shot.jpg 同策略：无 token → 403
        let (port, _shared, _tx) = start_server("s3cret");
        let (st, _) = http(port, "GET /stream.mjpg HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 403);
        // 引擎不可用（控制通道接收端已随测试辅助函数返回被丢弃）→ 500
        let (port2, _shared2, _tx2) = start_server("");
        let (st2, body2) = http(port2, "GET /stream.mjpg HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st2, 500);
        assert!(body2.contains("引擎不可用"), "{body2}");
        // 带 token：同样引擎不可用，但已过鉴权
        let (port3, _shared3, _tx3) = start_server("s3cret");
        let (st3, body3) = http(
            port3,
            "GET /stream.mjpg?token=s3cret HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st3, 500);
        assert!(body3.contains("引擎不可用"), "{body3}");
        // 控制页：新布局（左画面右操作栏 + 实时流）
        let (st4, body4) = http(port3, "GET /?token=s3cret HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st4, 200);
        assert!(body4.contains("stream.mjpg"));
        assert!(body4.contains("CloudPhoneKeep"));
    }

    #[test]
    fn stream_mjpeg_frames_keepalive_and_close() {
        // 自建控制通道（start_server 辅助会丢弃接收端）：模拟引擎应答
        // ScreencastAttach 并交出帧信箱（FrameBox）；随后按引擎节奏推帧/泵心跳，
        // 收到 Detach 后收尾退出
        let cfg = Config::from_env();
        let shared = SharedState::new(&cfg);
        let (tx, engine_rx) = std::sync::mpsc::channel();
        let rcfg = ReportCfg { bind: "127.0.0.1".into(), port: 0, control_token: String::new() };
        let port = start(
            rcfg,
            Arc::new(Logger::new(cfg.log_dir.clone())),
            shared,
            tx.clone(),
        )
        .unwrap();
        // 模拟引擎的帧信箱：attach 时交给流线程；推帧=post，泵周期=touch_alive
        let box_ = crate::cdp::FrameSlot::new();
        let box_tx = box_.clone();
        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let running2 = running.clone();
        thread::spawn(move || {
            let mut pending = Some(box_tx);
            while let Ok(req) = engine_rx.recv() {
                match req {
                    ControlRequest::ScreencastAttach { reply } => {
                        if let Some(b) = pending.take() {
                            let _ = reply.send(Ok((1u32, b)));
                        }
                    }
                    ControlRequest::ScreencastDetach { reply, .. } => {
                        let _ = reply.send(Ok(()));
                        break;
                    }
                    _ => {}
                }
            }
            running2.store(false, std::sync::atomic::Ordering::Release);
        });
        // 模拟引擎泵心跳（每 300ms touch，直到 Detach 收尾）
        let heartbeat = box_.clone();
        thread::spawn(move || {
            while running.load(std::sync::atomic::Ordering::Acquire) {
                heartbeat.touch_alive();
                thread::sleep(Duration::from_millis(300));
            }
        });
        // 引擎推一帧（5 字节假帧：SOI+EOI+尾部，客户端只看 multipart 语义）
        box_.post(vec![0xFF, 0xD8, 0xFF, 0xD9, 0x01]);

        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        s.write_all(b"GET /stream.mjpg HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        let t0 = Instant::now();
        // 读到首个 JPEG 帧为止（订阅应答即时；防抖上限 10s）
        while !out.windows(2).any(|w| w == [0xFF, 0xD8]) && t0.elapsed() < Duration::from_secs(10) {
            match s.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(_) => {}
            }
        }
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
        assert!(text.contains("multipart/x-mixed-replace"), "{text}");
        assert!(text.contains("Content-Length: 5"), "{text}");
        // 静态页心跳：无新帧 2s 后重发上一帧 → 字节继续增长（连接保持活性，
        // 生产侧 touch_alive 心跳在 → 不触发 Dead 关流）
        let before = out.len();
        let t1 = Instant::now();
        while out.len() == before && t1.elapsed() < Duration::from_secs(6) {
            match s.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(_) => {}
            }
        }
        assert!(out.len() > before, "2s 心跳未重发上一帧");
        // 客户端断开 → 服务端写失败退出并发 Detach（引擎线程收尾，测试可退出）
        drop(s);
        thread::sleep(Duration::from_millis(300));
    }

    #[test]
    fn touch_endpoint_guards() {
        // phase 非法 → HTTP 层直接 400（不占引擎，无需浏览器）
        let (port, _shared, _tx) = start_server("");
        let (st, body) = http(
            port,
            "GET /touch?phase=poke&x=1&y=2 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 400);
        assert!(body.contains("phase"), "{body}");
        // phase 合法但引擎不可用（控制通道无接收者）→ 500（已过参数校验，进入控制通道）
        let (st2, body2) = http(
            port,
            "GET /touch?phase=move&x=1.5&y=2.5 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st2, 500);
        assert!(body2.contains("engine unavailable"), "{body2}");
        // token 保护与其它控制端点同策略
        let (port2, _shared2, _tx2) = start_server("s3cret");
        let (st3, _) = http(
            port2,
            "GET /touch?phase=start&x=1&y=2 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st3, 403);
        // 控制页已接线实时触摸流（按下/移动/抬起）+ 移动端圆点抽屉 + 触摸反馈
        let (st4, body4) = http(port2, "GET /?token=s3cret HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st4, 200);
        assert!(body4.contains("/touch"), "控制页缺 /touch 接线");
        assert!(body4.contains("pointermove"), "控制页缺实时拖动接线");
        assert!(body4.contains("visibilitychange"), "控制页缺后台暂停接线");
        assert!(body4.contains("homei"), "控制页缺移动端圆点菜单");
        assert!(body4.contains("id=\"pstat\""), "控制页缺状态面板（fps 收纳处）");
        assert!(body4.contains("id=\"fpsel\""), "控制页缺帧率设置");
        // 画质/采集分辨率运行时可调（面板三旋钮：帧率/画质/分辨率）
        assert!(body4.contains("id=\"qsel\""), "控制页缺画质设置");
        assert!(body4.contains("id=\"sssel\""), "控制页缺采集分辨率设置");
        assert!(body4.contains("'/quality'"), "控制页缺画质设置接线");
        assert!(body4.contains("'/scale'"), "控制页缺分辨率设置接线");
        // fps 状态行实测+上限双指标（静止页实测远低于上限不再误读为设置失效）
        assert!(body4.contains("实测 "), "状态行缺实测帧率");
        assert!(body4.contains("上限 "), "状态行缺帧率上限");
        // fps 回读验证（绝不谎报：设 N 后 healthz.fps≠N 如实报错并解锁下拉）
        assert!(body4.contains("帧率设置未生效"), "fps 设置缺回读验证接线");
        // 触摸整组释放恒空点（CDP 协议规定 touchEnd/touchCancel 不得携带触点，
        // 带点形态会被 Chrome 拒绝——页面收不到 tap 收尾，「点击没反应」
        // 的直接根源）——控制页与引擎双保险
        assert!(body4.contains("tSend(isEnd?'end':'cancel','')"), "整组释放应恒空点");
        // 键盘 UI 按需求移除：物理键盘直通常开，输入只留复制/粘贴
        // （云机 H5 自带软键盘 + /type 粘贴；旧「键盘」开关与输入框不再提供）
        assert!(!body4.contains("id=\"kbin\""), "键盘输入框应已移除（只留复制粘贴）");
        assert!(!body4.contains("id=\"kbt\""), "键盘开关按钮应已移除");
        assert!(!body4.contains("kbOn("), "键盘开关逻辑应已移除");
        assert!(body4.contains("doCopy()"), "复制按钮应保留");
        assert!(body4.contains("doPaste()"), "粘贴按钮应保留");
        assert!(!body4.contains("id=\"imode\""), "触控模式选择器应已移除（与 Windows 版一致）");
        assert!(!body4.contains("cpk_imode"), "触控模式 localStorage 残留应已移除");
        assert!(!body4.contains("id=\"fpsb\""), "fps 悬浮徽标应已移入状态面板");
        // 面板精简（用户要求）：诊断明细 stats 区与「提示」说明行不再展示——
        // ticks/clicks/浏览器版本/URL 等是开发诊断数据，对使用者零价值；
        // 状态信息由 pstat 单行收纳（色点+页面状态+实测/上限帧率）
        assert!(!body4.contains("id=\"stats\""), "诊断明细区应已移除（pstat 单行收纳）");
        assert!(!body4.contains("ticks"), "内部计数器展示应已移除");
        assert!(!body4.contains("帧率越高 CPU 越高"), "设置区说明文字应已移除");
        assert!(!body4.contains("CPK_JPEG_QUALITY / CPK_STREAM_SCALE"), "环境变量说明应已移除");
        assert!(body4.contains("id=\"pstat\""), "状态单行应保留");
        assert!(body4.contains("pointer-events:none"), "提示层不应挡触摸");
        assert!(!body4.contains("上滑"), "方向滑动按钮应已删除");
        assert!(!body4.contains("sendKey"), "旧按键按钮应已删除");
        assert!(!body4.contains("<header"), "顶栏应已删除");
    }

    #[test]
    fn status_notify_wiring_and_no_addr_bar() {
        // 地址栏已按需求移除（Linux 版不需要）：端点下线 → 404
        let (port, _shared, _tx) = start_server("");
        let (st, _) = http(port, "POST /addr HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 404);
        // 控制页不再有地址栏接线（防回归）；状态转换通知保留
        let (port2, _shared2, _tx2) = start_server("s3cret");
        let (st3, body3) = http(port2, "GET /?token=s3cret HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st3, 200);
        assert!(!body3.contains("doAddr()"), "地址栏接线应已移除");
        assert!(!body3.contains(">地址</button>"), "地址按钮应已移除");
        assert!(!body3.contains("lk==='u'"), "Ctrl+U 拦截应已移除");
        assert!(body3.contains("statNotify"), "控制页缺状态转换通知");
        assert!(body3.contains("Notification.permission"), "控制页缺系统通知权限申请");
        assert!(body3.contains("lastStatus"), "控制页未消费 lastStatus");
        // 页面标题/账号等诊断明细已随 stats 区移除（画面本身可见页面内容）
        assert!(!body3.contains("j.title"), "页面标题展示应已移除");
    }

    #[test]
    fn platform_endpoint_guards_and_page_wiring() {
        // /platform：非法值 → 400（HTTP 层校验）；合法值但引擎不可用 → 500
        let (port, _shared, _tx) = start_server("");
        let (st, body) = http(
            port,
            "POST /platform HTTP/1.1\r\nHost: x\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 13\r\nConnection: close\r\n\r\nvalue=telecom",
        );
        assert_eq!(st, 400);
        assert!(body.contains("mobile/unicom"), "{body}");
        let (st, _) = http(
            port,
            "POST /platform HTTP/1.1\r\nHost: x\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 12\r\nConnection: close\r\n\r\nvalue=unicom",
        );
        assert_eq!(st, 500);
        // token 保护与其它控制端点同策略
        let (port2, _shared2, _tx2) = start_server("s3cret");
        let (st2, _) = http(
            port2,
            "POST /platform HTTP/1.1\r\nHost: x\r\nContent-Length: 12\r\nConnection: close\r\n\r\nvalue=unicom",
        );
        assert_eq!(st2, 403);
        // 控制页接线：平台选择器（留空占位项）+ healthz 同步 + POST
        let (st3, body3) = http(port2, "GET /?token=s3cret HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st3, 200);
        assert!(body3.contains("id=\"psel\""), "控制页缺平台选择器");
        assert!(body3.contains(">联通云手机</option>"), "控制页缺联通选项");
        assert!(body3.contains("选择平台…</option>"), "控制页缺留空占位项");
        assert!(body3.contains("syncPlatSel"), "控制页缺平台同步逻辑");
        assert!(body3.contains("/platform"), "控制页缺平台切换接线");
        // 启动无弹窗（平台留空待选）：无首选层/无 localStorage 记忆/无自动切回
        assert!(!body3.contains("id=\"pick\""), "平台首选弹窗应已移除（启动无弹窗）");
        assert!(!body3.contains("cpk_platform"), "平台 localStorage 记忆应已移除");
        // 待选平台模式：引擎待机时画面区提示选择，选好后自动开流
        assert!(body3.contains("idle-plat"), "控制页缺待选平台分支");
        assert!(body3.contains("请选择云手机平台"), "控制页缺待选提示");
    }

    #[test]
    fn mouse_kbd_fps_clip_endpoint_guards() {
        // /mouse：action/按钮非法 → 400；合法但引擎不可用 → 500（已过参数校验）
        let (port, _shared, _tx) = start_server("");
        let (st, body) = http(
            port,
            "GET /mouse?action=poke&x=1&y=2 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 400);
        assert!(body.contains("action"), "{body}");
        let (st, _) = http(
            port,
            "GET /mouse?action=down&x=1&y=2&b=left&n=1&m=0&bb=1 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 500);
        let (st, _) = http(
            port,
            "GET /mouse?action=wheel&x=1&y=2&dx=0&dy=120 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 500);
        // /kbd：t 非法 → 400；合法 → 500（引擎不可用）
        let (st, body) = http(
            port,
            "GET /kbd?t=press&key=a HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 400);
        assert!(body.contains("down/up"), "{body}");
        let (st, _) = http(
            port,
            "GET /kbd?t=down&key=a&code=KeyA&vk=65&text=a&m=0 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 500);
        // /fps：越界 → 400；合法 → 200 且直写共享状态（引擎不在线也生效——
        // 待机/重启窗口期设帧率不再丢失，healthz 立即回显新值）
        let (port, shared, _tx) = start_server("");
        let (st, _) = http(
            port,
            "GET /fps?value=99 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 400);
        let (st, _) = http(
            port,
            "GET /fps?value=10 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 200, "fps 应由 HTTP 层直写共享状态，引擎不在线也成功");
        assert_eq!(shared.snapshot().fps, 10, "帧率应写入共享状态并回显 healthz");
        // /quality：越界 → 400；合法 → 200 直写共享状态（与 /fps 同模式：
        // 引擎待机/重启窗口期设置不丢，下次 CDP 装配恢复）
        let (st, _) = http(
            port,
            "GET /quality?value=95 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 400);
        let (st, _) = http(
            port,
            "GET /quality?value=70 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 200, "画质应由 HTTP 层直写共享状态");
        assert_eq!(shared.snapshot().quality, 70, "画质应写入共享状态并回显 healthz");
        // /scale：越界 → 400；合法 → 200 直写共享状态
        let (st, _) = http(
            port,
            "GET /scale?value=20 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 400);
        let (st, _) = http(
            port,
            "GET /scale?value=75 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 200, "采集分辨率应由 HTTP 层直写共享状态");
        assert_eq!(shared.snapshot().scale, 75, "采集分辨率应写入共享状态并回显 healthz");
        // /clip：引擎不可用 → 500
        let (st, body) = http(
            port,
            "GET /clip HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 500);
        assert!(body.contains("engine unavailable"), "{body}");
        // /touch 多点：ps 非法 → 400；合法多点 → 500（已过参数校验）
        let (st, _) = http(
            port,
            "GET /touch?phase=start&ps=1,2,3;bad HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 400);
        let (st, _) = http(
            port,
            "GET /touch?phase=start&ps=100,200,1;400,600,2 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 500);
    }

    #[test]
    fn touch_points_parsing() {
        // 多点解析："x,y,id;…"，id 1..=10，最多 10 点
        let v = parse_touch_points("100.5,200.5,1;400.0,600.0,2").unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].x, 100.5);
        assert_eq!(v[1].y, 600.0);
        assert_eq!(v[1].id, 2);
        assert!(parse_touch_points("").is_none());
        assert!(parse_touch_points("1,2").is_none());
        assert!(parse_touch_points("1,2,3,4").is_none());
        assert!(parse_touch_points("1,2,0").is_none()); // id 超下界
        assert!(parse_touch_points("1,2,11").is_none()); // id 超上界
        assert!(parse_touch_points("a,b,1").is_none());
        let many = "1,1,1;2,2,2;3,3,3;4,4,4;5,5,5;6,6,6;7,7,7;8,8,8;9,9,9;10,10,10;11,11,11";
        assert!(parse_touch_points(many).is_none()); // 超 10 点
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
