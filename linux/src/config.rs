//! 环境变量配置（一个容器 = 一个账号）。
//! 字段语义与 Windows 版 src-tauri/src/config.rs 的 SlotConfig/platforms 对齐：
//!   mobile  → https://cloudphoneh5.buy.139.com       414×896
//!   unicom  → https://uphone.wo-adv.cn/cloudphone/#/home  405×720

use std::env;
use std::path::PathBuf;

pub const PLATFORM_MOBILE_URI: &str = "https://cloudphoneh5.buy.139.com";
pub const PLATFORM_UNICOM_URI: &str = "https://uphone.wo-adv.cn/cloudphone/#/home";

#[derive(Clone)]
pub struct Config {
    pub account: String,
    pub platform: String,
    pub platform_label: String,
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub data_dir: PathBuf,
    pub profile_dir: PathBuf,
    pub log_dir: PathBuf,
    pub keep_alive: bool,
    pub interval_ms: u32,
    pub simulate_activity: bool,
    pub block_context_menu: bool,
    /// 页内 setInterval 驱动（默认 false：由 Rust 看门狗经 CDP 驱动 __CPK_TICK__）
    pub page_timer: bool,
    pub report_port: u16,
    pub bind: String,
    pub control_token: String,
    pub cdp_port: u16,
    pub chrome_bin: String,
    /// 完整 Chromium 需要 --headless=new；chrome-headless-shell 本身即无头，无需该参数
    /// （Alpine 版镜像用发行版 chromium 包 → CPK_HEADLESS=1）
    pub headless: bool,
    pub no_sandbox: bool,
    pub ua_mode: String,
    pub lang: String,
    pub tz: String,
    pub extra_chrome_args: String,
    pub tick_fail_reload: u32,
    pub frozen_reload: u32,
    pub beat_stale_sec: u64,
    pub selftest: bool,
    pub smoke: bool,
    pub smoke_seconds: u64,
}

fn envs(key: &str) -> Option<String> {
    env::var(key).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn bool_env(key: &str, default: bool) -> bool {
    match envs(key) {
        Some(v) => !matches!(v.to_ascii_lowercase().as_str(), "0" | "false" | "no" | "off"),
        None => default,
    }
}

fn i64_env(key: &str, default: i64, min: i64, max: i64) -> i64 {
    envs(key)
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(default)
        .clamp(min, max)
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

impl Config {
    pub fn from_env() -> Config {
        let raw_platform = envs("CPK_PLATFORM").unwrap_or_else(|| "mobile".into());
        let (platform, platform_label, default_url, w, h) = match raw_platform.as_str() {
            "unicom" => (
                "unicom".to_string(),
                "联通云手机".to_string(),
                PLATFORM_UNICOM_URI.to_string(),
                405i64,
                720i64,
            ),
            // 未知平台回退 mobile（与 Windows 默认一致）
            _ => (
                "mobile".to_string(),
                "移动云手机".to_string(),
                PLATFORM_MOBILE_URI.to_string(),
                414i64,
                896i64,
            ),
        };
        let account = envs("CPK_ACCOUNT").unwrap_or_else(|| "account1".into());
        let data_dir = PathBuf::from(envs("CPK_DATA_DIR").unwrap_or_else(|| "/data".into()));
        let profile_dir = match envs("CPK_PROFILE_DIR") {
            Some(p) => PathBuf::from(p),
            None => data_dir.join(format!("profile-{}", sanitize(&account))),
        };
        let log_dir = data_dir.join("logs");
        Config {
            account,
            platform,
            platform_label,
            url: envs("CPK_URL").unwrap_or(default_url),
            width: i64_env("CPK_WIDTH", w, 200, 4096) as u32,
            height: i64_env("CPK_HEIGHT", h, 200, 8192) as u32,
            data_dir,
            profile_dir,
            log_dir,
            keep_alive: bool_env("CPK_KEEP_ALIVE", true),
            interval_ms: i64_env("CPK_INTERVAL_MS", 5000, 1000, 600_000) as u32,
            simulate_activity: bool_env("CPK_SIMULATE_ACTIVITY", true),
            block_context_menu: bool_env("CPK_BLOCK_CONTEXT_MENU", true),
            page_timer: bool_env("CPK_PAGE_TIMER", false),
            report_port: i64_env("CPK_REPORT_PORT", 8080, 0, 65535) as u16,
            bind: envs("CPK_BIND").unwrap_or_else(|| "0.0.0.0".into()),
            control_token: envs("CPK_CONTROL_TOKEN").unwrap_or_default(),
            cdp_port: i64_env("CPK_CDP_PORT", 0, 0, 65535) as u16,
            chrome_bin: envs("CPK_CHROME_BIN").unwrap_or_else(|| "chrome-headless-shell".into()),
            headless: bool_env("CPK_HEADLESS", false),
            no_sandbox: bool_env("CPK_NO_SANDBOX", true), // Docker 默认无 user-namespace 特权
            ua_mode: envs("CPK_UA_MODE")
                .filter(|m| ["windows", "auto", "none"].contains(&m.as_str()))
                .unwrap_or_else(|| "windows".into()),
            lang: envs("CPK_LANG").unwrap_or_else(|| "zh-CN".into()),
            tz: envs("TZ").unwrap_or_else(|| "Asia/Shanghai".into()),
            extra_chrome_args: envs("CPK_EXTRA_CHROME_ARGS").unwrap_or_default(),
            tick_fail_reload: i64_env("CPK_TICK_FAIL_RELOAD", 10, 3, 600) as u32,
            frozen_reload: i64_env("CPK_FROZEN_RELOAD", 3, 1, 100) as u32,
            beat_stale_sec: i64_env("CPK_BEAT_STALE_SEC", 180, 30, 3600) as u64,
            selftest: bool_env("CPK_SELFTEST", false),
            smoke: bool_env("CPK_SMOKE", false),
            smoke_seconds: i64_env("CPK_SMOKE_SECONDS", 60, 10, 3600) as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // env 是进程级全局——所有环境用例合并在单个测试函数内串行执行
    #[test]
    fn env_parsing_and_defaults() {
        let k = std::env::var("CPK_PLATFORM");
        // 默认：mobile + 414x896 + 5s 周期
        for name in ["CPK_PLATFORM", "CPK_DATA_DIR", "CPK_ACCOUNT", "CPK_URL"] {
            std::env::remove_var(name);
        }
        let cfg = Config::from_env();
        assert_eq!(cfg.platform, "mobile");
        assert_eq!(cfg.platform_label, "移动云手机");
        assert_eq!(cfg.url, PLATFORM_MOBILE_URI);
        assert_eq!(cfg.width, 414);
        assert_eq!(cfg.height, 896);
        assert_eq!(cfg.interval_ms, 5000);
        assert_eq!(cfg.report_port, 8080);
        assert_eq!(cfg.bind, "0.0.0.0");
        assert_eq!(cfg.ua_mode, "windows");
        assert_eq!(cfg.page_timer, false);
        assert_eq!(cfg.chrome_bin, "chrome-headless-shell");
        assert!(cfg.profile_dir.to_string_lossy().contains("profile-account1"));

        // 覆盖：unicom + 自定义 URL + 分辨率 + 周期
        std::env::set_var("CPK_PLATFORM", "unicom");
        std::env::set_var("CPK_ACCOUNT", "18612341234");
        std::env::set_var("CPK_URL", "https://example.com/h5");
        std::env::set_var("CPK_WIDTH", "405");
        std::env::set_var("CPK_HEIGHT", "720");
        std::env::set_var("CPK_INTERVAL_MS", "8000");
        std::env::set_var("CPK_KEEP_ALIVE", "no");
        std::env::set_var("CPK_REPORT_PORT", "9090");
        let cfg = Config::from_env();
        assert_eq!(cfg.platform, "unicom");
        assert_eq!(cfg.url, "https://example.com/h5");
        assert_eq!(cfg.width, 405);
        assert_eq!(cfg.interval_ms, 8000);
        assert_eq!(cfg.keep_alive, false);
        assert_eq!(cfg.report_port, 9090);
        assert!(cfg.profile_dir.to_string_lossy().contains("18612341234"));

        // 非法平台回退 mobile
        std::env::set_var("CPK_PLATFORM", "telecom");
        let cfg = Config::from_env();
        assert_eq!(cfg.platform, "mobile");

        // bool/int 解析健壮性：非数字回默认、越界截断
        std::env::set_var("CPK_PLATFORM", "mobile");
        std::env::set_var("CPK_INTERVAL_MS", "abc");
        std::env::set_var("CPK_TICK_FAIL_RELOAD", "99999");
        std::env::set_var("CPK_UA_MODE", "bogus");
        let cfg = Config::from_env();
        assert_eq!(cfg.interval_ms, 5000);
        assert_eq!(cfg.tick_fail_reload, 600);
        assert_eq!(cfg.ua_mode, "windows");

        std::env::remove_var("CPK_PLATFORM");
        std::env::remove_var("CPK_ACCOUNT");
        std::env::remove_var("CPK_URL");
        std::env::remove_var("CPK_WIDTH");
        std::env::remove_var("CPK_HEIGHT");
        std::env::remove_var("CPK_INTERVAL_MS");
        std::env::remove_var("CPK_KEEP_ALIVE");
        std::env::remove_var("CPK_REPORT_PORT");
        std::env::remove_var("CPK_TICK_FAIL_RELOAD");
        std::env::remove_var("CPK_UA_MODE");
        let _ = k; // 保留原值避免 unused 警告
    }
}
