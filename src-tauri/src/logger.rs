use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// 诊断日志（按目录分流）：
/// 1. 帐号级（slot=N）：写到该帐号数据目录 AppData\LocalLow\CloudPhoneKeep\<目录名>\，
///    与 WebView2 数据同目录——一个帐号一个目录，日志跟着数据走
/// 2. 程序级（slot=0/sys）：写到数据根目录 AppData\LocalLow\CloudPhoneKeep\
/// 3. 按天滚动，保留最近 7 天；每行日志带 [pid=N] 前缀，多实例混写也能按进程过滤
/// 4. 控制台模式（--console / CPK_CONSOLE=1）下同步镜像到终端
///
/// 格式：`HH:mm:ss.SSS [pid=N] [slot=N|sys] [level] message`
/// level 约定：
///   nav    页面 URL/路由变化（SPA 路由切换是改版定位的第一线索）
///   beat   保活心跳采样（含各选择器命中摘要；全 0 是否异常由上下文标注：iframe/云机内为正常态）
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

/// 每个目录一个缓存句柄（slot=0 → 数据根目录；slot=N → 帐号 N 的数据目录）。
/// HashMap::new() 不是 const fn，static 里不能直接构造 → OnceLock 惰性初始化
fn sinks() -> &'static Mutex<std::collections::HashMap<u32, Sink>> {
    static SINKS: OnceLock<Mutex<std::collections::HashMap<u32, Sink>>> = OnceLock::new();
    SINKS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

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

/// 某条日志的落盘目录：帐号级 → 该帐号数据目录；程序级(slot=0) → 数据根目录。
/// 目录来自启动时的登记表（register_slot_dir），logger 不读 AppState/config 锁——
/// 任何「持着配置锁写日志」的调用路径都不会死锁。
/// 同上：HashMap 不能 const 构造，OnceLock 惰性初始化
fn slot_dirs() -> &'static Mutex<std::collections::HashMap<u32, PathBuf>> {
    static SLOT_DIRS: OnceLock<Mutex<std::collections::HashMap<u32, PathBuf>>> = OnceLock::new();
    SLOT_DIRS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// 登记某槽位的日志目录（= 数据目录）。启动载入配置后登记全部已配置槽位，
/// 窗口启动 / 数据目录兜底切换（-r2/-r3）时更新
pub fn register_slot_dir(slot: u32, dir: PathBuf) {
    if slot == 0 {
        return;
    }
    slot_dirs().lock().unwrap().insert(slot, dir);
}

/// 查询某槽位当前登记的日志目录（= 该帐号窗口实际使用的数据目录，
/// 可能是 -r2 兜底目录）。托盘「打开数据目录」用它，保证打开的目录
/// 与日志/WebView2 数据真正所在一致（按配置名重算会算回原目录，兜底场景下打开错目录）
pub fn slot_dir(slot: u32) -> Option<PathBuf> {
    if slot == 0 {
        return None;
    }
    slot_dirs().lock().unwrap().get(&slot).cloned()
}

fn log_dir(slot: u32) -> PathBuf {
    if slot == 0 {
        return crate::config::base_dir();
    }
    slot_dirs()
        .lock()
        .unwrap()
        .get(&slot)
        .cloned()
        .unwrap_or_else(crate::config::base_dir)
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

/// 追加一行日志（线程安全，每个目录缓存一个句柄）
fn append(slot: u32, line: &str) {
    let dir = log_dir(slot);
    let (day, _, days) = now_parts();
    let path = dir.join(format!("cpk-{day}.log"));

    let mut sinks = sinks().lock().unwrap();
    let need_reopen = match sinks.get(&slot) {
        Some(s) => s.day != day,
        None => true,
    };
    if need_reopen {
        if let Ok(f) = OpenOptions::new().create(true).append(true).open(&path) {
            sinks.insert(slot, Sink { file: f, day: day.clone() });
            cleanup(&dir, days);
        }
    }
    if let Some(s) = sinks.get_mut(&slot) {
        let _ = s.file.write_all(line.as_bytes());
    }
}

/// 统一入口：写文件（+ 控制台模式时镜像到终端）。
/// app 参数仅为兼容既有调用点保留（落盘目录由登记表决定），不读任何状态锁
pub fn log(_app: &tauri::AppHandle, slot: u32, level: &str, msg: &str) {
    let msg: String = msg.chars().take(4000).collect();
    let (_, ts, _) = now_parts();
    let pid = std::process::id();
    let tag = if slot == 0 { "sys".into() } else { format!("slot={slot}") };
    let line = format!("{ts} [pid={pid}] [{tag}] [{level}] {msg}");
    if CONSOLE_MIRROR.load(Ordering::Relaxed) {
        eprintln!("{line}");
    }
    append(slot, &format!("{line}\n"));
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
