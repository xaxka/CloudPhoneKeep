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
    let mut buf = vec![0u8; 4096];
    let n = match stream.read(&mut buf).await {
        Ok(n) if n > 0 => n,
        _ => return,
    };
    let req = String::from_utf8_lossy(&buf[..n]).to_string();
    let first_line = req.lines().next().unwrap_or_default().to_string();

    // 解析 GET /report?slot=1&status=alive&t=...
    if let Some(query) = first_line.split('?').nth(1) {
        let mut slot: u32 = 0;
        let mut status = String::new();
        for pair in query.split('&') {
            let mut it = pair.splitn(2, '=');
            let k = it.next().unwrap_or("");
            let v = it.next().unwrap_or("");
            let v = urldecode(v);
            match k {
                "slot" => slot = v.parse().unwrap_or(0),
                "status" => status = v,
                _ => {}
            }
        }
        if slot >= 1 && slot <= 9 && !status.is_empty() {
            on_report(&app, slot, &status);
        }
    }

    let _ = stream
        .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .await;
    let _ = stream.shutdown().await;
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
                "expired-confirm" => {
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
