//! 时间 / 编码 / HTTP 工具（纯 std，零第三方依赖）。
//! SHA-1 与 base64 手写实现仅服务于 WebSocket 握手（RFC 6455 accept 计算）
//! 与 CDP 截图 base64 解码——刻意不引入 crate，单账号低内存优先。

use std::fs::File;
use std::io::Read;
use std::net::SocketAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 日期（Howard Hinnant civil 日期算法，纯整数；对齐 Windows 版 logger.rs）
// ---------------------------------------------------------------------------

pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// yyyymmdd（对齐 Windows 版日志文件名）
pub fn day_str(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}{m:02}{d:02}")
}

// ---------------------------------------------------------------------------
// 本地时间（glibc localtime_r FFI，TZ 环境变量由 glibc 解释；失败回退 UTC）
// ---------------------------------------------------------------------------

#[repr(C)]
struct CTm {
    sec: i32,
    min: i32,
    hour: i32,
    mday: i32,
    mon: i32,
    year: i32,
    wday: i32,
    yday: i32,
    isdst: i32,
    gmtoff: i64,
    zone: *const i8,
}

impl CTm {
    fn zeroed() -> Self {
        CTm {
            sec: 0,
            min: 0,
            hour: 0,
            mday: 0,
            mon: 0,
            year: 0,
            wday: 0,
            yday: 0,
            isdst: 0,
            gmtoff: 0,
            zone: std::ptr::null(),
        }
    }
}

extern "C" {
    fn localtime_r(timep: *const i64, result: *mut CTm) -> *mut CTm;
}

pub struct LocalStamp {
    pub day: i64, // epoch 起的天数（本地时区）
    pub hh: u32,
    pub mm: u32,
    pub ss: u32,
    pub ms: u32,
}

pub fn now_local() -> LocalStamp {
    let ms = now_ms();
    let secs = ms.div_euclid(1000);
    let ms_part = ms.rem_euclid(1000) as u32;
    unsafe {
        let mut tm = CTm::zeroed();
        let t = secs;
        if !localtime_r(&t as *const i64, &mut tm as *mut CTm).is_null() {
            let day = days_from_civil(tm.year as i64 + 1900, tm.mon as i64 + 1, tm.mday as i64);
            return LocalStamp { day, hh: tm.hour as u32, mm: tm.min as u32, ss: tm.sec as u32, ms: ms_part };
        }
    }
    // UTC 兜底（localtime_r 失败几乎不可能发生）
    let day = secs.div_euclid(86400);
    let sod = secs.rem_euclid(86400);
    LocalStamp { day, hh: (sod / 3600) as u32, mm: ((sod % 3600) / 60) as u32, ss: (sod % 60) as u32, ms: ms_part }
}

// ---------------------------------------------------------------------------
// SHA-1（RFC 3174）——仅用于 WebSocket 握手 accept
// ---------------------------------------------------------------------------

pub fn sha1(msg: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0];
    let mut data = Vec::with_capacity(msg.len() + 72);
    data.extend_from_slice(msg);
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&((msg.len() as u64) * 8).to_be_bytes());
    for block in data.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[4 * i], block[4 * i + 1], block[4 * i + 2], block[4 * i + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for i in 0..80 {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1u32),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDCu32),
                _ => (b ^ c ^ d, 0xCA62_C1D6u32),
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w[i]);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for i in 0..5 {
        out[4 * i..4 * i + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

// ---------------------------------------------------------------------------
// base64（标准字母表 + 填充；decode 宽容空白/非法字符）
// ---------------------------------------------------------------------------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(B64[((n >> 18) & 63) as usize] as char);
        out.push(B64[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { B64[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[(n & 63) as usize] as char } else { '=' });
    }
    out
}

pub fn base64_decode(s: &str) -> Vec<u8> {
    let mut vals: Vec<u8> = Vec::new();
    'outer: for c in s.bytes() {
        if c == b'=' {
            break;
        }
        if c.is_ascii_whitespace() {
            continue;
        }
        for (i, &x) in B64.iter().enumerate() {
            if x == c {
                vals.push(i as u8);
                continue 'outer;
            }
        }
        // 非法字符忽略
    }
    let mut out = Vec::with_capacity(vals.len() * 3 / 4);
    for chunk in vals.chunks(4) {
        let cnt = chunk.len();
        let mut n: u32 = 0;
        for &v in chunk {
            n = (n << 6) | v as u32;
        }
        n <<= 6 * (4 - cnt) as u32;
        out.push((n >> 16) as u8);
        if cnt >= 3 {
            out.push((n >> 8) as u8);
        }
        if cnt >= 4 {
            out.push(n as u8);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// WS 随机（掩码 / 握手 nonce，非安全用途；优先 /dev/urandom）
// ---------------------------------------------------------------------------

pub fn rand_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    if let Ok(mut f) = File::open("/dev/urandom") {
        if f.read_exact(&mut buf).is_ok() {
            return buf;
        }
    }
    // 兜底：时间 + pid 混合的 LCG（仅用于协议随机位，不承载安全性）
    let mut seed = (now_ms() as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ ((std::process::id() as u64) << 32);
    for b in buf.iter_mut() {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *b = (seed >> 33) as u8;
    }
    buf
}

// ---------------------------------------------------------------------------
// URL 解码（对齐 Windows 版 report_server.rs 的宽容实现）
// ---------------------------------------------------------------------------

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub fn urldecode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                if let (Some(h), Some(l)) = (hex_val(b[i + 1]), hex_val(b[i + 2])) {
                    out.push(h << 4 | l);
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
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------------------
// 极简 HTTP GET（仅 127.0.0.1；用于 DevTools /json/version 与自检）
// ---------------------------------------------------------------------------

pub fn http_get(port: u16, path: &str, timeout_ms: u64) -> Result<(u16, String), String> {
    use std::io::Write;
    use std::net::TcpStream;
    let sa: SocketAddr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream =
        TcpStream::connect_timeout(&sa, Duration::from_millis(5000)).map_err(|e| format!("连接 127.0.0.1:{port} 失败: {e}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(timeout_ms.min(10_000))));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let req = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: */*\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).map_err(|e| format!("发送失败: {e}"))?;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        if Instant::now() > deadline {
            break;
        }
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.len() > 4 * 1024 * 1024 {
                    break;
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                if Instant::now() > deadline {
                    break;
                }
            }
            Err(e) => return Err(format!("读取失败: {e}")),
        }
    }
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = match text.find("\r\n\r\n") {
        Some(p) => (&text[..p], text[p + 4..].to_string()),
        None => return Err("HTTP 响应不完整（无头体分隔）".into()),
    };
    let status: u16 = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Ok((status, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn sha1_known_vectors() {
        assert_eq!(hex(&sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(
            hex(&sha1(b"The quick brown fox jumps over the lazy dog")),
            "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12"
        );
    }

    #[test]
    fn base64_roundtrip() {
        assert_eq!(base64_encode(b"abc"), "YWJj");
        assert_eq!(base64_encode(b"ab"), "YWI=");
        assert_eq!(base64_encode(b"abcd"), "YWJjZA==");
        assert_eq!(base64_decode("YWJj"), b"abc".to_vec());
        assert_eq!(base64_decode("YWJjZA=="), b"abcd".to_vec());
        let big = rand_bytes(200_000);
        assert_eq!(base64_decode(&base64_encode(&big)), big);
    }

    #[test]
    fn urldecode_windows_parity() {
        assert_eq!(urldecode("alive"), "alive");
        assert_eq!(urldecode("a%20b"), "a b");
        assert_eq!(urldecode("%"), "%");
        assert_eq!(urldecode("%4"), "%4");
        assert_eq!(urldecode("%ZZ"), "%ZZ");
        assert_eq!(urldecode("%é%41"), "%éA"); // 多字节边界：残缺序列原样保留
        assert_eq!(urldecode("%41"), "A");
        assert_eq!(urldecode("a+b"), "a b");
    }

    #[test]
    fn civil_roundtrip() {
        for d in [0i64, 20680, 15000, -100, 20_000, 1_000_000] {
            let (y, m, dd) = civil_from_days(d);
            assert_eq!(days_from_civil(y, m, dd), d, "roundtrip {d}");
        }
        assert_eq!(day_str(0), "19700101");
        assert_eq!(day_str(20680), "20260815");
    }
}
