//! 保活初始化脚本构建器：内嵌 keepalive.inject.js（Windows 版
//! src-tauri/src/keepalive.rs::build_init_script 的逐行移植），替换占位符。
//! 与 Windows 差异：
//!  - customCursor 恒为 false（无头模式没有可见光标，占位符替换为空串）
//!  - 新增 pageTimer: false（tick 由宿主 Rust 看门狗经 CDP 驱动，见注入文件头注释）
//!  - slot 恒为 1（一个容器一个账号）

use crate::config::Config;

const TEMPLATE: &str = include_str!("keepalive.inject.js");

pub fn build_init_script(cfg: &Config, port: u16) -> String {
    let inject = serde_json::json!({
        "slot": 1,
        "port": port,
        "platform": cfg.platform,
        "homeUri": cfg.url,
        "keepAlive": cfg.keep_alive,
        "intervalMs": cfg.interval_ms,
        "simulateActivity": cfg.simulate_activity,
        "customCursor": false,
        "blockContextMenu": cfg.block_context_menu,
        "pageTimer": cfg.page_timer,
    });
    let json = serde_json::to_string(&inject).unwrap_or_default();
    TEMPLATE
        .replace("var CFG = __CPK_CFG__;", &format!("var CFG = {};", json))
        .replace("data:image/png;base64,__CPK_CURSOR__", "data:image/png;base64,")
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
            report_port: 8080,
            bind: "0.0.0.0".into(),
            control_token: String::new(),
            cdp_port: 0,
            chrome_bin: "chrome-headless-shell".into(),
            headless: false,
            no_sandbox: true,
            ua_mode: "windows".into(),
            lang: "zh-CN".into(),
            tz: "Asia/Shanghai".into(),
            extra_chrome_args: String::new(),
            tick_fail_reload: 10,
            frozen_reload: 3,
            beat_stale_sec: 180,
            selftest: false,
            smoke: false,
            smoke_seconds: 60,
        }
    }

    #[test]
    fn placeholders_replaced() {
        let s = build_init_script(&cfg(), 8080);
        assert!(!s.contains("__CPK_CFG__"), "CFG 占位符应被替换");
        assert!(!s.contains("__CPK_CURSOR__"), "光标占位符应被替换");
        assert!(s.contains("\"platform\":\"mobile\""));
        assert!(s.contains("\"port\":8080"));
        assert!(s.contains("\"pageTimer\":false"));
        // 注入的 JSON 含 $ 序列时不得被误展开（Rust replace 是字面替换，天然满足；
        // 显式断言防回归）
        // 注：serde_json 默认按字母序输出键（BTreeMap），逐字段断言而非依赖键序
        assert!(s.contains("var CFG = {"));
        assert!(s.contains("\"slot\":1"));
        assert!(s.contains("\"homeUri\":\"https://cloudphoneh5.buy.139.com\""));
        assert!(s.contains("\"intervalMs\":5000"));
        assert!(s.contains("\"keepAlive\":true"));
        assert!(s.contains("\"customCursor\":false"));
    }

    #[test]
    fn port_injected() {
        let s = build_init_script(&cfg(), 1234);
        assert!(s.contains("\"port\":1234"));
    }

    #[test]
    fn keepalive_semantics_fully_ported() {
        let s = build_init_script(&cfg(), 8080);
        // 双定时器语义（stopCheck 1s / actionTick 5s）
        assert!(s.contains("state.n >= every"));
        assert!(s.contains("stopCheck"));
        assert!(s.contains("actionTick"));
        // 移动平台选择器
        assert!(s.contains(".unlocked"));
        assert!(s.contains("#tabbar"));
        // 联通平台选择器
        assert!(s.contains(".try-content"));
        assert!(s.contains(".phone-dialog-wrap"));
        assert!(s.contains(".van-dialog__confirm"));
        assert!(s.contains(".title-bar"));
        // 触摸模拟 + 回环上报 + CDP 外部驱动开关
        assert!(s.contains("touchstart"));
        assert!(s.contains("/report?"));
        assert!(s.contains("127.0.0.1:' + PORT"));
        assert!(s.contains("pageTimer"));
        // 无 Rust format! 转义残留（模板是纯 JS，非 Rust format 字符串）
        assert!(!s.contains("{{"));
    }
}
