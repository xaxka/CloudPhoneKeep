//! 诊断日志：对齐 Windows 版 src-tauri/src/logger.rs 的行为
//!  - 按天滚动：{data}/logs/cpk-YYYYMMDD.log，保留最近 7 天
//!  - 行格式：HH:mm:ss.SSS [pid=N] [slot=1|sys] [level] message
//!  - level 约定与 Windows 一致：nav/beat/click/miss/exit/probe/error/sys
//!  - 逐条「打开-追加-关闭」，不缓存句柄（宿主可实时 tail；docker logs 同步镜像）
//!  - 目录不可写：静默降级（日志绝不打断保活）
//! 与 Windows 差异：单账号容器无槽位区分，页面侧上报的 slot 原样透传。

use crate::util::{day_str, now_local};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

const KEEP_DAYS: i64 = 7;

pub struct Logger {
    dir: PathBuf,
    broken: bool,
    /// 上次触发过期清理的日期（天数）
    state: Mutex<i64>,
}

impl Logger {
    pub fn new(dir: PathBuf) -> Logger {
        let broken = fs::create_dir_all(&dir).is_err();
        Logger { dir, broken, state: Mutex::new(i64::MIN) }
    }

    pub fn log(&self, slot: u32, level: &str, msg: &str) {
        let st = now_local();
        let day = day_str(st.day);
        let msg: String = msg.chars().take(4000).collect();
        let tag = if slot == 0 { "sys".to_string() } else { format!("slot={slot}") };
        let line = format!(
            "{:02}:{:02}:{:02}.{:03} [pid={}] [{}] [{}] {}",
            st.hh, st.mm, st.ss, st.ms, std::process::id(), tag, level, msg
        );
        // stdout 镜像（容器唯一出口：docker logs -f 即诊断台）
        println!("{line}");
        if self.broken {
            return;
        }
        {
            let mut last = match self.state.lock() {
                Ok(g) => g,
                Err(_) => return, // 锁中毒：放弃本条（绝不让日志拖垮业务）
            };
            if *last != st.day {
                *last = st.day;
                self.cleanup(&day);
            }
        }
        let file = self.dir.join(format!("cpk-{day}.log"));
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&file) {
            let _ = writeln!(f, "{line}");
        }
        // 写入失败（磁盘满/目录被删）：静默
    }

    fn cleanup(&self, today: &str) {
        let today_n: i64 = match today.parse() {
            Ok(v) => v,
            Err(_) => return,
        };
        if let Ok(entries) = fs::read_dir(&self.dir) {
            for e in entries.flatten() {
                let name = e.file_name();
                let name = name.to_string_lossy();
                if let Some(core) = name.strip_prefix("cpk-").and_then(|s| s.strip_suffix(".log")) {
                    if let Ok(d) = core.parse::<i64>() {
                        if d < today_n - KEEP_DAYS {
                            let _ = fs::remove_file(e.path());
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_and_line_format() {
        let dir = std::env::temp_dir().join(format!("cpk-log-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let log = Logger::new(dir.clone());
        log.log(0, "sys", "你好 world");
        log.log(1, "beat", "tick=3");
        let files: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(files.len(), 1);
        assert!(files[0].starts_with("cpk-") && files[0].ends_with(".log"), "{}", files[0]);
        let text = fs::read_to_string(dir.join(&files[0])).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("[sys] [sys] 你好 world"), "{}", lines[0]);
        assert!(lines[1].contains("[slot=1] [beat] tick=3"), "{}", lines[1]);
        // 行首时间格式 HH:mm:ss.SSS
        assert!(lines[0].starts_with(&"0".repeat(0)) && {
            let mut cs = lines[0].char_indices();
            let head: String = lines[0][..cs.nth(12).map(|(i, _)| i).unwrap_or(0)].to_string();
            head.chars().filter(|c| *c == ':').count() == 2
        });
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cleanup_old_days() {
        let dir = std::env::temp_dir().join(format!("cpk-log-clean-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("cpk-20200101.log"), "old\n").unwrap();
        let log = Logger::new(dir.clone());
        log.log(0, "sys", "trigger"); // 当天首条触发清理
        assert!(!dir.join("cpk-20200101.log").exists(), "过期日志应被删除");
        let n = fs::read_dir(&dir).unwrap().count();
        assert_eq!(n, 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn broken_dir_silent() {
        // 用「已存在的普通文件」充当目录路径：mkdir 必然 ENOTDIR，瞬时失败
        let blocker = std::env::temp_dir().join(format!("cpk-blocker-{}.txt", std::process::id()));
        fs::write(&blocker, "x").unwrap();
        let log = Logger::new(blocker.join("sub"));
        log.log(0, "sys", "no crash"); // 不 panic 即通过
        fs::remove_file(&blocker).unwrap();
    }
}
