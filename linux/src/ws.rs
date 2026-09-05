//! 极简 RFC 6455 WebSocket 客户端（纯 std）：
//!  - 仅客户端方向：文本帧收发、ping 自动回 pong、close 语义
//!  - 客户端帧强制掩码（协议要求）；服务端帧支持分片（continuation）
//!  - 握手严格校验 Sec-WebSocket-Accept（SHA-1 + base64 由 util 提供）
//!  - 刻刻不引第三方 crate：CDP 只用文本帧 + 本地回环，无 TLS / 压缩扩展协商
//!  - read_message(poll_deadline)：Timeout 仅表示「本轮 poll 无完整消息」，
//!    半帧状态绝不丢失（消息级缓冲 + 60s 不完整超时升级为错误）

use crate::util;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

const READ_POLL: Duration = Duration::from_millis(250);
const FRAME_STALL_LIMIT: Duration = Duration::from_secs(60);
const MAX_FRAME: usize = 64 * 1024 * 1024;

#[derive(Debug)]
pub enum WsError {
    Timeout,
    Closed,
    Handshake(String),
    Io(String),
}

#[derive(Debug)]
pub enum WsMessage {
    Text(String),
    Close,
}

pub struct WsClient {
    stream: TcpStream,
    rbuf: Vec<u8>,
}

/// RFC 6455 握手 accept：base64(SHA1(key + GUID))
pub fn accept_key(key: &str) -> String {
    const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    let mut buf = Vec::with_capacity(key.len() + GUID.len());
    buf.extend_from_slice(key.as_bytes());
    buf.extend_from_slice(GUID.as_bytes());
    util::base64_encode(&util::sha1(&buf))
}

impl WsClient {
    /// 连接 host:port 并完成 WebSocket 握手（path 形如 /devtools/browser/<uuid>）
    pub fn connect(host_port: &str, path: &str) -> Result<WsClient, WsError> {
        let addr = host_port
            .to_socket_addrs()
            .map_err(|e| WsError::Io(format!("解析 {host_port}: {e}")))?
            .next()
            .ok_or_else(|| WsError::Io(format!("无有效地址: {host_port}")))?;
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(10))
            .map_err(|e| WsError::Io(format!("TCP 连接失败: {e}")))?;
        let _ = stream.set_nodelay(true);
        let _ = stream.set_read_timeout(Some(READ_POLL));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
        let mut c = WsClient { stream, rbuf: Vec::with_capacity(16384) };
        c.handshake(host_port, path)?;
        Ok(c)
    }

    fn handshake(&mut self, host_port: &str, path: &str) -> Result<(), WsError> {
        let key = util::base64_encode(&util::rand_bytes(16));
        let req = format!(
            "GET {path} HTTP/1.1\r\n\
             Host: {host_port}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: {key}\r\n\
             Sec-WebSocket-Version: 13\r\n\
             User-Agent: cpk-cdp/1.0\r\n\
             \r\n"
        );
        self.stream.write_all(req.as_bytes()).map_err(|e| WsError::Handshake(format!("发送握手请求失败: {e}")))?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut hdr = Vec::new();
        while !hdr.ends_with(b"\r\n\r\n") {
            if Instant::now() > deadline {
                return Err(WsError::Handshake("握手响应超时".into()));
            }
            if hdr.len() > 16384 {
                return Err(WsError::Handshake("响应头过大".into()));
            }
            let mut chunk = [0u8; 2048];
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err(WsError::Handshake("连接在握手期间关闭".into())),
                Ok(n) => hdr.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => continue,
                Err(e) => return Err(WsError::Handshake(format!("读取握手响应失败: {e}"))),
            }
        }
        let text = String::from_utf8_lossy(&hdr).into_owned();
        let status_line = text.lines().next().unwrap_or("");
        if !status_line.contains("101") {
            return Err(WsError::Handshake(format!("非 101 响应: {status_line}")));
        }
        let expected = accept_key(&key);
        let mut accept_ok = false;
        for line in text.lines().skip(1) {
            if let Some((k, v)) = line.split_once(':') {
                if k.trim().eq_ignore_ascii_case("sec-websocket-accept") && v.trim() == expected {
                    accept_ok = true;
                }
            }
        }
        if !accept_ok {
            return Err(WsError::Handshake("Sec-WebSocket-Accept 校验失败".into()));
        }
        Ok(())
    }

    fn write_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), WsError> {
        let mask = util::rand_bytes(4);
        let mut frame = Vec::with_capacity(payload.len() + 14);
        frame.push(0x80 | opcode); // FIN=1 + opcode
        let len = payload.len();
        if len < 126 {
            frame.push(0x80 | len as u8);
        } else if len <= 0xFFFF {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }
        frame.extend_from_slice(&mask);
        for (i, b) in payload.iter().enumerate() {
            frame.push(b ^ mask[i & 3]);
        }
        self.stream
            .write_all(&frame)
            .map_err(|e| WsError::Io(format!("写帧失败: {e}")))
    }

    pub fn send_text(&mut self, text: &str) -> Result<(), WsError> {
        self.write_frame(0x1, text.as_bytes())
    }

    pub fn send_close(&mut self) -> Result<(), WsError> {
        self.write_frame(0x8, &[])
    }

    fn fill(&mut self) -> Result<usize, WsError> {
        let mut chunk = [0u8; 65536];
        match self.stream.read(&mut chunk) {
            Ok(0) => Err(WsError::Closed),
            Ok(n) => {
                self.rbuf.extend_from_slice(&chunk[..n]);
                Ok(n)
            }
            Err(e)
                if e.kind() == ErrorKind::WouldBlock
                    || e.kind() == ErrorKind::TimedOut
                    || e.kind() == ErrorKind::Interrupted =>
            {
                Err(WsError::Timeout) // poll 窗口内无数据
            }
            Err(e) => Err(WsError::Io(format!("{e}"))),
        }
    }

    /// 读取下一条完整消息。
    /// poll_deadline：本轮无数据时的等待上限（超时返回 WsError::Timeout，调用方
    /// 可借机处理其他事务后重试）；半帧/分片状态内部保留，绝不丢失。
    pub fn read_message(&mut self, poll_deadline: Instant) -> Result<WsMessage, WsError> {
        let mut frag: Vec<u8> = Vec::new();
        let mut frag_is_text = true;
        loop {
            // —— 1. 尝试从缓冲解析一帧 ——
            if self.rbuf.len() >= 2 {
                let b0 = self.rbuf[0];
                let b1 = self.rbuf[1];
                let fin = b0 & 0x80 != 0;
                let opcode = b0 & 0x0F;
                let masked = b1 & 0x80 != 0;
                let len7 = (b1 & 0x7F) as u64;
                let head = match len7 {
                    126 => 4,
                    127 => 10,
                    _ => 2,
                };
                if self.rbuf.len() >= head {
                    let len: usize = match len7 {
                        126 => u16::from_be_bytes([self.rbuf[2], self.rbuf[3]]) as usize,
                        127 => {
                            let mut b = [0u8; 8];
                            b.copy_from_slice(&self.rbuf[2..10]);
                            u64::from_be_bytes(b) as usize
                        }
                        n => n as usize,
                    };
                    if len > MAX_FRAME {
                        return Err(WsError::Io("帧过大".into()));
                    }
                    let mask_len = if masked { 4 } else { 0 };
                    let total = head + mask_len + len;
                    if self.rbuf.len() >= total {
                        let data: Vec<u8> = self.rbuf.drain(..total).collect();
                        let payload_start = head + mask_len;
                        let mut payload = data[payload_start..payload_start + len].to_vec();
                        if masked {
                            let mask = [data[head], data[head + 1], data[head + 2], data[head + 3]];
                            for (i, b) in payload.iter_mut().enumerate() {
                                *b ^= mask[i & 3];
                            }
                        }
                        // 控制帧（可在分片序列中间出现）
                        match opcode {
                            0x8 => {
                                let _ = self.send_close();
                                return Ok(WsMessage::Close);
                            }
                            0x9 => {
                                let _ = self.write_frame(0xA, &payload); // ping → pong
                            }
                            0xA => {} // pong 忽略
                            0x1 | 0x2 => {
                                frag_is_text = opcode == 0x1;
                                frag.clear();
                                frag.extend_from_slice(&payload);
                            }
                            0x0 => {
                                frag.extend_from_slice(&payload);
                            }
                            _ => {} // 未知 opcode 忽略
                        }
                        if !matches!(opcode, 0x8 | 0x9 | 0xA) && fin && !frag.is_empty() && frag_is_text {
                            return Ok(WsMessage::Text(String::from_utf8_lossy(&frag).into_owned()));
                        }
                        continue; // 处理缓冲中的下一帧
                    }
                }
            }
            // —— 2. 缓冲不足，读取更多 ——
            let mid_frame = !self.rbuf.is_empty() || !frag.is_empty();
            let deadline = if mid_frame {
                Instant::now() + FRAME_STALL_LIMIT
            } else {
                poll_deadline
            };
            match self.fill() {
                Ok(_) => {}
                Err(WsError::Timeout) => {
                    if Instant::now() >= deadline {
                        if mid_frame {
                            return Err(WsError::Io("帧数据不完整超时".into()));
                        }
                        return Err(WsError::Timeout);
                    }
                }
                Err(e) => return Err(e),
            }
            if Instant::now() >= deadline {
                if mid_frame {
                    return Err(WsError::Io("帧数据不完整超时".into()));
                }
                if self.rbuf.is_empty() && frag.is_empty() {
                    return Err(WsError::Timeout);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn accept_key_rfc6455_example() {
        // RFC 6455 §1.3 示例：key "dGhlIHNhbXBsZSBub25jZQ==" → accept "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    /// 测试用服务端：完成握手回包 + 读一帧（去掩码）+ 回写一帧（不掩码）
    fn echo_server(listener: TcpListener, expected: &str, reply: &str) {
        let expected = expected.to_string();
        let reply = reply.to_string();
        thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let _ = s.set_read_timeout(Some(Duration::from_secs(30)));
            let _ = s.set_write_timeout(Some(Duration::from_secs(30)));
            // 读握手头
            let mut buf = Vec::new();
            let mut chunk = [0u8; 2048];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = s.read(&mut chunk).unwrap();
                if n == 0 {
                    return;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let text = String::from_utf8_lossy(&buf).into_owned();
            let key = text
                .lines()
                .find(|l| l.to_ascii_lowercase().starts_with("sec-websocket-key:"))
                .and_then(|l| l.split_once(':'))
                .map(|(_, v)| v.trim().to_string())
                .unwrap();
            let resp = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
                accept_key(&key)
            );
            s.write_all(resp.as_bytes()).unwrap();
            // 读一帧（客户端必带掩码）
            let (payload, opcode) = read_frame_raw(&mut s);
            assert_eq!(opcode, 0x1);
            assert_eq!(String::from_utf8_lossy(&payload), expected);
            // 回一帧（服务端不掩码）
            write_frame_raw(&mut s, 0x1, reply.as_bytes());
        });
    }

    fn read_frame_raw(s: &mut TcpStream) -> (Vec<u8>, u8) {
        let mut head = [0u8; 2];
        s.read_exact(&mut head).unwrap();
        let opcode = head[0] & 0x0F;
        let masked = head[1] & 0x80 != 0;
        let len7 = (head[1] & 0x7F) as usize;
        let len = match len7 {
            126 => {
                let mut b = [0u8; 2];
                s.read_exact(&mut b).unwrap();
                u16::from_be_bytes(b) as usize
            }
            127 => {
                let mut b = [0u8; 8];
                s.read_exact(&mut b).unwrap();
                u64::from_be_bytes(b) as usize
            }
            n => n,
        };
        let mask = if masked {
            let mut m = [0u8; 4];
            s.read_exact(&mut m).unwrap();
            Some(m)
        } else {
            None
        };
        let mut payload = vec![0u8; len];
        s.read_exact(&mut payload).unwrap();
        if let Some(m) = mask {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= m[i & 3];
            }
        }
        (payload, opcode)
    }

    fn write_frame_raw(s: &mut TcpStream, opcode: u8, payload: &[u8]) {
        let mut f = vec![0x80 | opcode];
        let len = payload.len();
        if len < 126 {
            f.push(len as u8);
        } else if len <= 0xFFFF {
            f.push(126);
            f.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            f.push(127);
            f.extend_from_slice(&(len as u64).to_be_bytes());
        }
        f.extend_from_slice(payload);
        s.write_all(&f).unwrap();
    }

    #[test]
    fn handshake_and_roundtrip() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        echo_server(listener, "hello cdp", "hi from server");
        let mut c = WsClient::connect(&format!("127.0.0.1:{port}"), "/devtools/browser/test-uuid").unwrap();
        c.send_text("hello cdp").unwrap();
        let msg = c.read_message(Instant::now() + Duration::from_secs(10)).unwrap();
        assert!(matches!(msg, WsMessage::Text(t) if t == "hi from server"));
    }

    #[test]
    fn large_payload_roundtrip() {
        // 200KB 文本：触发 64 位长度编码 + 分块写
        let big = "x".repeat(200_000);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let big2 = big.clone();
        echo_server(listener, &big, "ok");
        let mut c = WsClient::connect(&format!("127.0.0.1:{port}"), "/x").unwrap();
        c.send_text(&big2).unwrap();
        let msg = c.read_message(Instant::now() + Duration::from_secs(30)).unwrap();
        assert!(matches!(msg, WsMessage::Text(t) if t == "ok"));
    }

    #[test]
    fn poll_timeout_when_idle() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        echo_server(listener, "x", "y"); // 服务端不主动发消息直到收到一帧
        let mut c = WsClient::connect(&format!("127.0.0.1:{port}"), "/x").unwrap();
        let r = c.read_message(Instant::now() + Duration::from_millis(300));
        assert!(matches!(r, Err(WsError::Timeout)));
    }
}
