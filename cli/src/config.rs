//! 环境变量配置（一个容器 = 一个账号，裸机亦可直跑）。
//! 字段语义与 Tauri 版 src-tauri/src/config.rs 的 SlotConfig/platforms 对齐：
//!   mobile  → https://cloudphoneh5.buy.139.com       414×896
//!   unicom  → https://uphone.wo-adv.cn/cloudphone/#/home  405×720
//! 平台预设常量与查表函数的唯一源在 shared（cloudphonekeep-shared::platform），
//! 此处再导出保持全仓调用点（engine.rs 等 `config::PLATFORM_*`）零改动。

use std::env;
use std::path::{Path, PathBuf};

// 平台预设家族整体再导出（engine/测试用其中一部分；bin crate 无外部
// 消费者故显式豁免 unused 检查，保持 config::PLATFORM_* 口径完整）
#[allow(unused_imports)]
pub use cloudphonekeep_shared::platform::{
    platform_profile, PLATFORM_MOBILE_H, PLATFORM_MOBILE_LABEL, PLATFORM_MOBILE_URI,
    PLATFORM_MOBILE_W, PLATFORM_UNICOM_H, PLATFORM_UNICOM_LABEL, PLATFORM_UNICOM_URI,
    PLATFORM_UNICOM_W,
};

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
    pub no_sandbox: bool,
    pub ua_mode: String,
    pub lang: String,
    pub tz: String,
    pub extra_chrome_args: String,
    pub tick_fail_reload: u32,
    pub frozen_reload: u32,
    pub beat_stale_sec: u64,
    /// 实时画面目标帧率上限（控制面板 /fps 可运行时调整；此为初始值。
    /// 默认 10：云机页面内容变化率普遍 5-10fps，10 已足额且传输 CPU 低）
    pub fps: u32,
    /// 实时画面 JPEG 质量（10..90，默认 50；控制面板 /quality 可运行时调整）
    pub jpeg_quality: u32,
    /// 实时画面采集分辨率百分比（30..100，默认 100；控制面板 /scale 可运行时
    /// 调整；<100 时 Chrome 编码前先缩小，编码 CPU 与带宽按像素数近线性下降，
    /// 触摸坐标不受影响）
    pub stream_scale_pct: u32,
    /// tick 自适应降频：连续无观看/无操作该时长（秒）后进入空闲态
    /// （CPK_IDLE_AFTER_SEC，默认 60；0 = 关闭空闲降频）。
    /// 空闲态下 tick eval 降频到 idle_tick_sec、采样 eval 降频到 3 倍动作周期；
    /// 保活动作周期/心跳/自动恢复全部不变；任一操作或打开画面流立即恢复
    pub idle_after_sec: u64,
    /// 空闲态 tick 周期（秒；CPK_IDLE_TICK_SEC，默认 5，1..60）。
    /// 采样周期同步放缓保证冻结检测不误报；若该配置会打破「页面级恢复
    /// 先于心跳硬重启」的分级安全，引擎自动放弃降频（维持 1s/5s）
    pub idle_tick_sec: u64,
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

/// 未设 CPK_DATA_DIR 时的默认数据目录（裸机直跑友好）：
/// - `/data` 已存在（Docker VOLUME 场景）→ 沿用 `/data`（容器行为不变）；
/// - 否则落 XDG 数据目录 `~/.local/share/cloudphonekeep`（普通用户可写）；
/// - 无 HOME 时兜底当前目录 `./data`。
/// Docker 镜像同时显式设置 ENV CPK_DATA_DIR=/data，双保险不走此分支。
fn default_data_dir() -> PathBuf {
    pick_data_dir(Path::new("/data").exists(), env::var_os("HOME"))
}

/// default_data_dir 的决策核心（纯函数，便于单测）
fn pick_data_dir(data_root_exists: bool, home: Option<std::ffi::OsString>) -> PathBuf {
    if data_root_exists {
        PathBuf::from("/data")
    } else if let Some(h) = home {
        PathBuf::from(h).join(".local/share/cloudphonekeep")
    } else {
        PathBuf::from("./data")
    }
}

impl Config {
    pub fn from_env() -> Config {
        // 平台三态启动（还原 CPK_PLATFORM 启动参数）：
        //   ① CPK_URL 显式 → 自动启动（CI 冒烟/自定义 H5），按 mobile 视口起
        //   ② CPK_PLATFORM=mobile/unicom 显式 → 自动启动该平台（环境变量
        //      直达：compose/CLI 部署免开控制页选择；非法值忽略走待机，
        //      不会静默起错平台）
        //   ③ 两者都无 → 平台留空待机（控制页「设置→平台」选择后启动）。
        //      Profile 保留双平台登录态，切回已登过的平台无需重登
        let explicit_url = envs("CPK_URL");
        let platform_env = envs("CPK_PLATFORM")
            .filter(|p| p == "mobile" || p == "unicom");
        let (platform, platform_label, default_url, w, h) = if let Some(u) = &explicit_url {
            (
                "mobile".to_string(),
                PLATFORM_MOBILE_LABEL.to_string(),
                u.clone(),
                PLATFORM_MOBILE_W as i64,
                PLATFORM_MOBILE_H as i64,
            )
        } else if let Some(p) = &platform_env {
            let (label, url, pw, ph) = platform_profile(p).expect("已过滤合法平台");
            (p.clone(), label.to_string(), url.to_string(), pw as i64, ph as i64)
        } else {
            (
                String::new(),
                "未选择".to_string(),
                String::new(),
                PLATFORM_MOBILE_W as i64,
                PLATFORM_MOBILE_H as i64,
            )
        };
        let account = envs("CPK_ACCOUNT").unwrap_or_else(|| "1".into());
        let data_dir = match envs("CPK_DATA_DIR") {
            Some(p) => PathBuf::from(p),
            None => default_data_dir(),
        };
        let profile_dir = match envs("CPK_PROFILE_DIR") {
            Some(p) => PathBuf::from(p),
            None => data_dir.join(format!("profile-{}", sanitize(&account))),
        };
        let log_dir = data_dir.join("logs");
        Config {
            account,
            platform,
            platform_label,
            url: default_url,
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
            report_port: i64_env("CPK_REPORT_PORT", 8088, 0, 65535) as u16,
            bind: envs("CPK_BIND").unwrap_or_else(|| "0.0.0.0".into()),
            control_token: envs("CPK_CONTROL_TOKEN").unwrap_or_default(),
            cdp_port: i64_env("CPK_CDP_PORT", 0, 0, 65535) as u16,
            chrome_bin: envs("CPK_CHROME_BIN").unwrap_or_else(|| "chrome-headless-shell".into()),
            no_sandbox: bool_env("CPK_NO_SANDBOX", true), // Docker 默认无 user-namespace 特权
            ua_mode: envs("CPK_UA_MODE")
                .filter(|m| ["mobile", "windows", "auto", "none"].contains(&m.as_str()))
                .unwrap_or_else(|| "mobile".into()),
            lang: envs("CPK_LANG").unwrap_or_else(|| "zh-CN".into()),
            tz: envs("TZ").unwrap_or_else(|| "Asia/Shanghai".into()),
            extra_chrome_args: envs("CPK_EXTRA_CHROME_ARGS").unwrap_or_default(),
            tick_fail_reload: i64_env("CPK_TICK_FAIL_RELOAD", 10, 3, 600) as u32,
            frozen_reload: i64_env("CPK_FROZEN_RELOAD", 3, 1, 100) as u32,
            beat_stale_sec: i64_env("CPK_BEAT_STALE_SEC", 180, 30, 3600) as u64,
            fps: i64_env("CPK_FPS", 10, 1, 60) as u32,
            jpeg_quality: i64_env("CPK_JPEG_QUALITY", 50, 10, 90) as u32,
            stream_scale_pct: i64_env("CPK_STREAM_SCALE", 100, 30, 100) as u32,
            idle_after_sec: i64_env("CPK_IDLE_AFTER_SEC", 60, 0, 3600) as u64,
            idle_tick_sec: i64_env("CPK_IDLE_TICK_SEC", 5, 1, 60) as u64,
            selftest: bool_env("CPK_SELFTEST", false),
            smoke: bool_env("CPK_SMOKE", false),
            smoke_seconds: i64_env("CPK_SMOKE_SECONDS", 60, 10, 3600) as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_data_dir_decision() {
        // Docker 场景：/data 存在 → 沿用（容器行为不变）
        assert_eq!(
            pick_data_dir(true, Some("/home/u".into())),
            PathBuf::from("/data")
        );
        // 裸机场景：/data 不存在 → XDG 数据目录（普通用户可写）
        assert_eq!(
            pick_data_dir(false, Some("/home/u".into())),
            PathBuf::from("/home/u/.local/share/cloudphonekeep")
        );
        // 极端兜底：无 HOME → 当前目录 ./data
        assert_eq!(pick_data_dir(false, None), PathBuf::from("./data"));
    }

    // env 是进程级全局——所有环境用例合并在单个测试函数内串行执行
    #[test]
    fn env_parsing_and_defaults() {
        let k = std::env::var("CPK_PLATFORM");
        // 默认：平台留空（待机，控制页选择后启动）+ 414x896 + 5s 周期
        for name in ["CPK_PLATFORM", "CPK_DATA_DIR", "CPK_ACCOUNT", "CPK_URL"] {
            std::env::remove_var(name);
        }
        let cfg = Config::from_env();
        assert_eq!(cfg.platform, "", "默认平台应留空（待机待选）");
        assert_eq!(cfg.platform_label, "未选择");
        assert_eq!(cfg.url, "");
        assert_eq!(cfg.width, 414);
        assert_eq!(cfg.height, 896);
        assert_eq!(cfg.interval_ms, 5000);
        assert_eq!(cfg.report_port, 8088);
        assert_eq!(cfg.bind, "0.0.0.0");
        assert_eq!(cfg.ua_mode, "mobile", "默认 UA 应为移动（云机 H5 手机布局）");
        assert_eq!(cfg.page_timer, false);
        assert_eq!(cfg.chrome_bin, "chrome-headless-shell");
        assert!(cfg.profile_dir.to_string_lossy().contains("profile-1"));
        // 空闲降频默认：60s 无活动进入空闲，tick 1s→5s
        assert_eq!(cfg.idle_after_sec, 60);
        assert_eq!(cfg.idle_tick_sec, 5);

        // 覆盖：显式 CPK_URL → 自动启动（mobile 视口）；自定义分辨率/周期
        std::env::set_var("CPK_ACCOUNT", "18612341234");
        std::env::set_var("CPK_URL", "https://example.com/h5");
        std::env::set_var("CPK_WIDTH", "405");
        std::env::set_var("CPK_HEIGHT", "720");
        std::env::set_var("CPK_INTERVAL_MS", "8000");
        std::env::set_var("CPK_KEEP_ALIVE", "no");
        std::env::set_var("CPK_REPORT_PORT", "9090");
        let cfg = Config::from_env();
        assert_eq!(cfg.platform, "mobile", "显式 CPK_URL 应自动启动");
        assert_eq!(cfg.platform_label, "移动云手机");
        assert_eq!(cfg.url, "https://example.com/h5");
        assert_eq!(cfg.width, 405);
        assert_eq!(cfg.interval_ms, 8000);
        assert_eq!(cfg.keep_alive, false);
        assert_eq!(cfg.report_port, 9090);
        assert!(cfg.profile_dir.to_string_lossy().contains("18612341234"));

        // CPK_PLATFORM 还原：显式平台直接启动（URL 未设时生效）
        std::env::remove_var("CPK_URL");
        std::env::set_var("CPK_PLATFORM", "unicom");
        let cfg = Config::from_env();
        assert_eq!(cfg.platform, "unicom", "CPK_PLATFORM=unicom 应自动启动联通");
        assert_eq!(cfg.platform_label, "联通云手机");
        assert_eq!(cfg.url, "https://uphone.wo-adv.cn/cloudphone/#/home");
        assert_eq!(cfg.width, 405, "联通视口 405x720");
        assert_eq!(cfg.height, 720);
        // CPK_URL 优先级高于 CPK_PLATFORM（自定义 H5 明确意图）
        std::env::set_var("CPK_URL", "https://example.com/h5");
        let cfg = Config::from_env();
        assert_eq!(cfg.platform, "mobile", "CPK_URL 优先于 CPK_PLATFORM");
        assert_eq!(cfg.url, "https://example.com/h5");
        // 非法平台值忽略 → 待机（不静默起错平台）
        std::env::remove_var("CPK_URL");
        std::env::set_var("CPK_PLATFORM", "telecom");
        let cfg = Config::from_env();
        assert_eq!(cfg.platform, "", "非法 CPK_PLATFORM 应忽略走待机");
        // mobile 显式 → 启动
        std::env::set_var("CPK_PLATFORM", "mobile");
        let cfg = Config::from_env();
        assert_eq!(cfg.platform, "mobile", "CPK_PLATFORM=mobile 应自动启动");
        assert_eq!(cfg.url, "https://cloudphoneh5.buy.139.com");

        // bool/int 解析健壮性：非数字回默认、越界截断
        std::env::set_var("CPK_PLATFORM", "mobile");
        std::env::set_var("CPK_INTERVAL_MS", "abc");
        std::env::set_var("CPK_TICK_FAIL_RELOAD", "99999");
        std::env::set_var("CPK_UA_MODE", "bogus");
        std::env::set_var("CPK_IDLE_AFTER_SEC", "0");
        std::env::set_var("CPK_IDLE_TICK_SEC", "abc");
        let cfg = Config::from_env();
        assert_eq!(cfg.interval_ms, 5000);
        assert_eq!(cfg.tick_fail_reload, 600);
        assert_eq!(cfg.ua_mode, "mobile", "非法 UA 模式应回退默认 mobile");
        // 空闲旋钮：0 = 显式关闭；非法值回默认
        assert_eq!(cfg.idle_after_sec, 0, "CPK_IDLE_AFTER_SEC=0 应关闭空闲降频");
        assert_eq!(cfg.idle_tick_sec, 5, "非法 CPK_IDLE_TICK_SEC 应回默认 5");
        std::env::set_var("CPK_IDLE_TICK_SEC", "30");
        let cfg = Config::from_env();
        assert_eq!(cfg.idle_tick_sec, 30);
        // 合法覆盘：windows 仍可选（旧部署兼容）
        std::env::set_var("CPK_UA_MODE", "windows");
        let cfg = Config::from_env();
        assert_eq!(cfg.ua_mode, "windows");

        // 去掉 CPK_URL 后：CPK_PLATFORM=mobile 仍在 → 保持启动（平台变量独立生效）
        std::env::remove_var("CPK_URL");
        let cfg = Config::from_env();
        assert_eq!(cfg.platform, "mobile", "无 CPK_URL 但 CPK_PLATFORM=mobile 应保持启动");
        // 平台也清空 → 回到待机默认（防回归）
        std::env::remove_var("CPK_PLATFORM");
        let cfg = Config::from_env();
        assert_eq!(cfg.platform, "", "URL 与平台都未设时应回到待机");

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
        std::env::remove_var("CPK_IDLE_AFTER_SEC");
        std::env::remove_var("CPK_IDLE_TICK_SEC");
        let _ = k; // 保留原值避免 unused 警告
    }
}
