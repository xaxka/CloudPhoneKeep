use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// 诊断日志（按归属分两处落盘）：
/// 1. 程序级日志（slot=0/sys：启动、配置、回环服务、设置窗口等）写数据根目录
///    AppData\LocalLow\CloudPhoneKeep\cpk-YYYYMMDD.log
/// 2. 帐号级日志（slot=N：保活心跳/点击/退出检测/窗口生命周期等）写该帐号
///    【实际运行的数据目录】里的同名文件（登记表中定位；数据目录被锁换用
///    -r2/-r3 兜底目录时，日志跟着新目录走）——每个目录里每天一个文件
/// 3. 追加写入；每行带 [pid=N] [slot=N|sys] 前缀，多实例混写也能按来源过滤
/// 4. 按天滚动，每个目录各自保留最近 7 天
/// 5. 逐条「打开-追加-关闭」，不缓存句柄：句柄常驻时 Windows 资源管理器
///    对追加写入的文件常一直显示 0 KB（句柄关闭后才刷新）；逐条开关
///    既让文件大小实时可见，也绝不产生「建了文件却没写入内容」的 0 字节空文件
/// 6. 控制台模式（--console / CPK_CONSOLE=1）下同步镜像到终端
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

/// 最近一次清理日志的（目录 → 天序号）：每目录每天首条日志触发一次清理
fn last_clean() -> &'static Mutex<std::collections::HashMap<PathBuf, i64>> {
    static LAST_CLEAN: OnceLock<Mutex<std::collections::HashMap<PathBuf, i64>>> = OnceLock::new();
    LAST_CLEAN.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
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

/// 各帐号「实际运行数据目录」登记表：
/// 1. 决定帐号级日志落盘位置（slot=N 的日志写进该帐号目录）
/// 2. 托盘「打开数据目录」据此定位（数据目录被锁换 -r2 兜底目录时，
///    按配置名重算会打开旧目录）
fn slot_dirs() -> &'static Mutex<std::collections::HashMap<u32, PathBuf>> {
    static SLOT_DIRS: OnceLock<Mutex<std::collections::HashMap<u32, PathBuf>>> = OnceLock::new();
    SLOT_DIRS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// 登记某槽位实际运行的数据目录。启动载入配置后登记全部已配置槽位，
/// 窗口启动 / 数据目录兜底切换（-r2/-r3）时更新。
/// 帐号级日志的落盘位置与托盘「打开数据目录」都由这张表决定
pub fn register_slot_dir(slot: u32, dir: PathBuf) {
    if slot == 0 {
        return;
    }
    slot_dirs().lock().unwrap().insert(slot, dir);
}

/// 查询某槽位当前登记的数据目录（可能是 -r2 兜底目录）。
/// 托盘「打开数据目录」用它，保证打开的目录与 WebView2 数据真正所在一致
pub fn slot_dir(slot: u32) -> Option<PathBuf> {
    if slot == 0 {
        return None;
    }
    slot_dirs().lock().unwrap().get(&slot).cloned()
}

/// 清理过期日志（每天一次；当天文件由逐条开关写入，无常驻句柄，可安全删除）。
/// 文件名两种格式都认：cpk-YYYYMMDD.log（现行）与 cpk-YYYYMMDD-pPID.log（历史版本遗留）
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

/// 追加一行日志（线程安全）。
/// 帐号级（slot≠0）写该帐号实际运行的数据目录；程序级（slot=0）与
/// 尚未登记的帐号日志落数据根目录。每条「打开-追加-关闭」，不缓存句柄——
/// 避免 Windows 资源管理器常显 0 KB 与 0 字节空文件
fn append(slot: u32, line: &str) {
    let dir = if slot == 0 {
        crate::config::base_dir()
    } else {
        // 登记表未命中（极少：窗口创建前的极早日志）退回数据根目录兜底
        slot_dir(slot).unwrap_or_else(crate::config::base_dir)
    };
    let (day, _, days) = now_parts();
    let path = dir.join(format!("cpk-{day}.log"));

    // 每目录每天首条日志触发一次过期清理（HashMap 抢占：同目录当天仅一次）
    let need_clean = {
        let mut last = last_clean().lock().unwrap();
        last.insert(dir.clone(), days) != Some(days)
    };
    if need_clean {
        cleanup(&dir, days);
    }

    // 追加写入，失败静默（日志永不打断业务；失败多为磁盘满/目录被删）
    let _ = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(line.as_bytes()));
}

/// 统一入口：写文件（+ 控制台模式时镜像到终端）。
/// 落盘位置：帐号级（slot≠0）写该帐号实际运行的数据目录，
/// 程序级（slot=0）写数据根目录。app 参数仅为兼容既有调用点保留，不读任何状态锁
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
