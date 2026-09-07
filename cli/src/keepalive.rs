//! 保活初始化脚本构建器的 CLI 适配层：脚本本体与占位符替换逻辑的唯一源在
//! `shared/keepalive.inject.js` + `cloudphonekeep_shared::keepalive`
//! （Windows/Linux 双端同源，改保活规则只需改那一份）。
//!
//! CLI 端策略（与 shared 构建器的约定，见其模块注释）：
//!  - `customCursor` 恒为 false（无头模式没有可见光标，占位符替换为空负载）
//!  - `pageTimer` 恒取 cfg.page_timer（默认 false：tick 由宿主 Rust 看门狗
//!    经 CDP 驱动；Tauri 侧传 true 保持页内定时器）
//!  - `slot` 恒为 1（一个容器/一个进程一个账号）

use crate::config::Config;
use cloudphonekeep_shared::keepalive::{build_init_script as shared_build, InjectParams};

pub fn build_init_script(cfg: &Config, port: u16) -> String {
    build_init_script_for(&cfg.platform, &cfg.url, cfg, port)
}

/// 平台运行时可切换（控制面板 /platform）：以 (platform, url) 直建注入脚本，
/// 其余开关仍取 cfg。保持 build_init_script(cfg) 兼容签名供 selftest/测试用。
pub fn build_init_script_for(platform: &str, url: &str, cfg: &Config, port: u16) -> String {
    shared_build(
        &InjectParams {
            slot: 1,
            platform,
            home_uri: url,
            keep_alive: cfg.keep_alive,
            interval_ms: cfg.interval_ms as u64,
            simulate_activity: cfg.simulate_activity,
            custom_cursor: false,
            block_context_menu: cfg.block_context_menu,
            page_timer: cfg.page_timer,
        },
        port,
        "", // CLI 无头无可见光标：空负载（页面侧不加载图片）
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config {
            account: "t".into(),
            platform: "mobile".into(),
            platform_label: "移动云手机".into(),
            url: "https://cloudphoneh5.buy.139.com".into(),
            width: 414,
            height: 896,
            data_dir: "/tmp".into(),
            profile_dir: "/tmp/p".into(),
            log_dir: "/tmp/logs".into(),
            keep_alive: true,
            interval_ms: 5000,
            simulate_activity: true,
            block_context_menu: true,
            page_timer: false,
            report_port: 8088,
            bind: "0.0.0.0".into(),
            control_token: String::new(),
            cdp_port: 0,
            chrome_bin: "chrome-headless-shell".into(),
            no_sandbox: true,
            ua_mode: "windows".into(),
            lang: "zh-CN".into(),
            tz: "Asia/Shanghai".into(),
            extra_chrome_args: String::new(),
            tick_fail_reload: 10,
            frozen_reload: 3,
            beat_stale_sec: 180,
            fps: 25,
            jpeg_quality: 50,
            stream_scale_pct: 100,
            idle_after_sec: 60,
            idle_tick_sec: 5,
            selftest: false,
            smoke: false,
            smoke_seconds: 60,
        }
    }

    #[test]
    fn cli_policy_applied() {
        // CLI 端策略：slot=1 / customCursor=false / pageTimer 取 cfg /
        // 光标空负载（占位符替换后前缀保留、负载为空）
        let s = build_init_script(&cfg(), 8088);
        assert!(s.contains("\"slot\":1"));
        assert!(s.contains("\"customCursor\":false"));
        assert!(s.contains("\"pageTimer\":false"));
        assert!(s.contains("\"platform\":\"mobile\""));
        assert!(s.contains("\"port\":8088"));
        assert!(s.contains("var CFG = {"));
        assert!(s.contains("\"homeUri\":\"https://cloudphoneh5.buy.139.com\""));
        assert!(s.contains("\"intervalMs\":5000"));
        assert!(s.contains("\"keepAlive\":true"));
    }

    #[test]
    fn platform_variant_builds_unicom_cfg() {
        let s = build_init_script_for("unicom", "https://uphone.wo-adv.cn/cloudphone/#/home", &cfg(), 8088);
        assert!(s.contains("\"platform\":\"unicom\""));
        assert!(s.contains("\"homeUri\":\"https://uphone.wo-adv.cn/cloudphone/#/home\""));
        // 联通选择器在脚本本体里（平台分支由 CFG.platform 运行时选择）
        assert!(s.contains(".try-content"));
    }
}
