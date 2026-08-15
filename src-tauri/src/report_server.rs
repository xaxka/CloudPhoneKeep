use crate::state::{now_ms, SlotState, ACTION_STATUSES};
use crate::AppState;
use tauri::Emitter;
use tauri::Manager;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// 启动 127.0.0.1 回环 HTTP 服务，接收页面内保活脚本的状态上报。
/// Chromium 将环回地址视为可信来源，HTTPS 页面内 fetch http://127.0.0.1 不会被混合内容策略拦截。
pub fn spawn(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let listener = match TcpListener::bind("127.0.0.1:0").await {
            Ok(l) => l,
            Err(_) => return,
        };
        let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
        if port == 0 {
            return;
        }
        {
            let state: tauri::State<AppState> = app.state();
            *state.port.lock().unwrap() = port;
        }
        crate::logger::log(
            &app,
            0,
            "sys",
            &format!("状态回环服务已监听 http://127.0.0.1:{port}/report"),
        );

        loop {
            if let Ok((stream, _)) = listener.accept().await {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    handle_conn(app, stream).await;
                });
            }
        }
    });
}

async fn handle_conn(app: tauri::AppHandle, mut stream: TcpStream) {
    // 请求可能分多次到达：先读满 header，再按 Content-Length 读 body
    let mut buf = vec![0u8; 8192];
    let mut req = String::new();
    let mut header_end = None;
    for _ in 0..8 {
        let n = match stream.read(&mut buf).await {
            Ok(n) if n > 0 => n,
            _ => break,
        };
        req.push_str(&String::from_utf8_lossy(&buf[..n]));
        if let Some(pos) = req.find("\r\n\r\n") {
            header_end = Some(pos + 4);
            break;
        }
    }
    let Some(hend) = header_end else {
        let _ = stream.shutdown().await;
        return;
    };

    // 需要继续读取的 body 长度
    let content_len = req
        .headers_like_content_length()
        .unwrap_or(0);
    while (req.len() - hend) < content_len {
        let n = match stream.read(&mut buf).await {
            Ok(n) if n > 0 => n,
            _ => break,
        };
        req.push_str(&String::from_utf8_lossy(&buf[..n]));
    }

    let first_line = req.lines().next().unwrap_or_default().to_string();
    let body = req[hend.min(req.len())..].to_string();

    // GET/POST /report?slot=1&status=alive  或  /log?slot=N&level=xx&msg=xx
    let (path, query) = match first_line.split(' ').nth(1) {
        Some(u) => match u.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (u.to_string(), String::new()),
        },
        None => (String::new(), String::new()),
    };

    let params = parse_params(&query, &body);

    if path == "/log" {
        let slot = params.get("slot").and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
        let level = params.get("level").cloned().unwrap_or_else(|| "info".into());
        if let Some(msg) = params.get("msg") {
            crate::logger::log(&app, slot, &level, msg);
        }
    } else if path == "/report" {
        let slot = params.get("slot").and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
        let status = params.get("status").cloned().unwrap_or_default();
        if (1..=9).contains(&slot) && !status.is_empty() {
            on_report(&app, slot, &status);
        }
    }

    let _ = stream
        .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .await;
    let _ = stream.shutdown().await;
}

trait ContentLength {
    fn headers_like_content_length(&self) -> Option<usize>;
}

impl ContentLength for String {
    fn headers_like_content_length(&self) -> Option<usize> {
        for line in self.lines() {
            let l = line.to_ascii_lowercase();
            if let Some(v) = l.strip_prefix("content-length:") {
                return v.trim().parse::<usize>().ok();
            }
        }
        None
    }
}

/// 合并解析 query string 与 urlencoded body
fn parse_params(query: &str, body: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for src in [query, body] {
        if src.is_empty() {
            continue;
        }
        for pair in src.split('&') {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            out.entry(urldecode(k)).or_insert_with(|| urldecode(v));
        }
    }
    out
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(b);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// 状态到达：更新槽位状态、统计点击、必要时发系统通知并推送给前端
fn on_report(app: &tauri::AppHandle, slot: u32, status: &str) {
    let mut notify: Option<(String, String)> = None;
    let snapshot = {
        let state: tauri::State<AppState> = app.state();
        let mut states = state.states.lock().unwrap();
        let s = states.entry(slot).or_insert_with(|| SlotState::new(slot));

        // 点击动作计数
        if ACTION_STATUSES.contains(&status) {
            s.clicks += 1;
        }

        // 状态迁移时通知：退出云机 / 到期
        let key = status.to_string();
        let prev = s.last_status.clone();
        if key != prev {
            match status {
                "exited" => {
                    if prev != "exited" {
                        notify = Some((
                            "已退出云手机".into(),
                            format!("帐号槽位 {slot} 已退回云手机首页，请检查会话"),
                        ));
                    }
                }
                "expired" => {
                    notify = Some((
                        "时间已到期".into(),
                        format!("帐号槽位 {slot} 云手机使用时间已到期"),
                    ));
                }
                _ => {}
            }
        }

        s.last_status = key;
        s.last_at = now_ms();
        s.clone()
    };

    if let Some((title, body)) = notify {
        use tauri_plugin_notification::NotificationExt;
        let _ = app
            .notification()
            .builder()
            .title(title)
            .body(body)
            .show();
    }

    let _ = app.emit("cpk://status", &snapshot);
}

#[cfg(test)]
mod tests {
    #[test]
    fn urldecode_basic() {
        assert_eq!(super::urldecode("alive"), "alive");
        assert_eq!(super::urldecode("a%20b"), "a b");
    }
}
