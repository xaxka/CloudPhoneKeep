use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// 诊断日志：
/// 1. 写入 exe目录/logs/cpk-YYYYMMDD-p{pid}.log，按天滚动，保留最近 7 天；
///    文件名带进程号——同时启动多个程序时各写各的文件，互不混淆
/// 2. 每行日志带 [pid=N] 前缀，即使合并查看也能按实例过滤
/// 3. 控制台模式（--console / CPK_CONSOLE=1）下同步镜像到终端
///
/// 格式：`HH:mm:ss.SSS [pid=N] [slot=N|sys] [level] message`
/// level 约定：
///   nav    页面 URL/路由变化（SPA 路由切换是改版定位的第一线索）
///   beat   保活心跳采样（含各选择器命中摘要，全 0 即疑似改版）
///   click  自动点击命中（含目标元素 tag/class/text）
///   miss   容器可见但按钮未找到（改版的最直接证据）
///   exit   云机退出事件（附 DOM class 采样）
///   probe  手动 DOM 采样
///   error  脚本异常（含 message）
///   sys    Rust 侧窗口/看门狗生命周期事件

const KEEP_DAYS: u64 = 7;

/// 终端镜像开关：--console / CPK_CONSOLE=1 启动时置位，
/// 之后每条日志在写文件的同时也打到启动它的终端里
static CONSOLE_MIRROR: AtomicBool = AtomicBool::new(false);

pub fn set_console_mirror(on: bool) {
    CONSOLE_MIRROR.store(on, Ordering::Relaxed);
}

struct Sink {
    file: File,
    day: String,
}

static SINK: Mutex<Option<Sink>> = Mutex::new(None);

/// UTC+8 毫秒时间 → (日期 YYYYMMDD, HH:mm:ss.SSS, 天序号)
fn now_parts() -> (String, String, i64) {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
        + 8 * 3600 * 1000; // 北京时间
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let (h, m, s, msec) = (
        rem / 3_600_000,
        rem % 3_600_000 / 60_000,
        rem % 60_000 / 1000,
        rem % 1000,
    );
    (day_str(days), format!("{h:02}:{m:02}:{s:02}.{msec:03}"), days)
}

/// 天序号 → YYYYMMDD（Howard Hinnant civil_from_days）
fn day_str(z: i64) -> String {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}{m:02}{d:02}")
}

fn log_dir(_app: &tauri::AppHandle) -> PathBuf {
    // 便携化：日志固定保存在 exe 目录下的 logs/（与原版程序数据目录行为一致）
    let dir = crate::config::base_dir().join("logs");
    let _ = fs::create_dir_all(&dir);
    dir
}

/// 清理过期日志（仅在天切换时触发一次）。
/// 文件名两种格式都认：cpk-YYYYMMDD.log（旧版）与 cpk-YYYYMMDD-pPID.log（现行）
fn cleanup(dir: &PathBuf, today_days: i64) {
    let cutoff = today_days - KEEP_DAYS as i64;
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(stem) = name
                .strip_prefix("cpk-")
                .and_then(|s| s.strip_suffix(".log"))
            {
                // 取前 8 位日期（YYYYMMDD）解析为天序号
                let day_part: String = stem.chars().take(8).collect();
                if let Ok(d) = date_to_days(&day_part) {
                    if d < cutoff {
                        let _ = fs::remove_file(e.path());
                    }
                }
            }
        }
    }
}

/// YYYYMMDD → 天序号（Howard Hinnant days_from_civil，与 day_str 互逆）
fn date_to_days(s: &str) -> Result<i64, ()> {
    let b = s.as_bytes();
    if b.len() != 8 || !b.iter().all(u8::is_ascii_digit) {
        return Err(());
    }
    let y: i64 = s[0..4].parse().map_err(|_| ())?;
    let m: i64 = s[4..6].parse().map_err(|_| ())?;
    let d: i64 = s[6..8].parse().map_err(|_| ())?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(());
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Ok(era * 146_097 + doe - 719_468)
}

/// 追加一行日志（线程安全，文件句柄缓存复用）
fn append(app: &tauri::AppHandle, line: &str) {
    let dir = log_dir(app);
    let (day, _, days) = now_parts();
    let path = dir.join(format!("cpk-{day}-p{}.log", std::process::id()));

    let mut guard = SINK.lock().unwrap();
    let need_reopen = match guard.as_ref() {
        Some(s) => s.day != day,
        None => true,
    };
    if need_reopen {
        if let Ok(f) = OpenOptions::new().create(true).append(true).open(&path) {
            *guard = Some(Sink { file: f, day: day.clone() });
            cleanup(&dir, days);
        }
    }
    if let Some(s) = guard.as_mut() {
        let _ = s.file.write_all(line.as_bytes());
    }
}

/// 统一入口：写文件（+ 控制台模式时镜像到终端）
pub fn log(app: &tauri::AppHandle, slot: u32, level: &str, msg: &str) {
    let msg: String = msg.chars().take(4000).collect();
    let (_, ts, _) = now_parts();
    let pid = std::process::id();
    let tag = if slot == 0 { "sys".into() } else { format!("slot={slot}") };
    let line = format!("{ts} [pid={pid}] [{tag}] [{level}] {msg}");
    if CONSOLE_MIRROR.load(Ordering::Relaxed) {
        eprintln!("{line}");
    }
    append(app, &format!("{line}\n"));
}

#[cfg(test)]
mod tests {
    #[test]
    fn day_str_known_dates() {
        assert_eq!(super::day_str(0), "19700101");
        assert_eq!(super::day_str(20680), "20260815");
    }

    #[test]
    fn date_to_days_inverse() {
        for d in [0i64, 20680, 15000, -100] {
            let s = super::day_str(d);
            assert_eq!(super::date_to_days(&s), Ok(d), "roundtrip {d}");
        }
    }
}
