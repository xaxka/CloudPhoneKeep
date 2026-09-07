//! 保活注入脚本构建器——CLI 与 Tauri 双端唯一源。
//! 脚本本体在 `shared/keepalive.inject.js`（`include_str!` 编译期内嵌，
//! 改保活规则只需改那一份，双端构建后同时生效）。
//!
//! 占位符替换约定（此前两端平行实现、约定一致，现收敛为本模块）：
//! - `var CFG = __CPK_CFG__;` → 注入参数 JSON（serde_json 序列化，键按字母序）
//! - `data:image/png;base64,__CPK_CURSOR__` → 触点光标 PNG 的 base64。
//!   CLI 无头模式无可见光标 → 传空串（保留前缀、空负载，页面侧不加载图片）；
//!   Tauri 传 `src-tauri/assets/cursor.b64`（Tauri 专属资源，不进 shared）。
//!
//! 各端策略差异由调用方传参决定（本模块零平台假设）：
//! - `slot`：CLI 恒 1（一容器一账号）；Tauri 取槽位 1..9
//! - `custom_cursor`：CLI 恒 false；Tauri 取用户配置
//! - `page_timer`：CLI 默认 false（tick 由宿主 CDP 看门狗驱动）；
//!   Tauri 恒 true（窗口可见时页内 setInterval 驱动，隐藏时看门狗接管）
//! - `platform` 空值兜底：Tauri 端回退 "unicom"；CLI 端平台留空属待机语义
//!
//! 注入 JSON 键序说明：serde_json 默认按字母序输出键（BTreeMap），
//! 测试断言按「键值对包含」而非整串比对。

/// 注入脚本模板（shared/keepalive.inject.js，双端唯一源）
pub const TEMPLATE: &str = include_str!("../keepalive.inject.js");

/// 注入参数：模板 `var CFG = __CPK_CFG__;` 占位符的替换内容。
/// 生命周期跟随调用方持有的配置字符串（CLI 的 Config / Tauri 的 SlotConfig）
#[derive(Clone, Copy)]
pub struct InjectParams<'a> {
    /// 槽位编号（CLI：恒 1；Tauri：1..9，同时是老板键 Ctrl+N 的 N）
    pub slot: u32,
    /// 平台 id：mobile / unicom（空值兜底策略由调用方决定）
    pub platform: &'a str,
    /// 云机首页 URL（重连/回退目标）
    pub home_uri: &'a str,
    /// 保活引擎开关
    pub keep_alive: bool,
    /// actionTick 周期（毫秒）
    pub interval_ms: u64,
    /// 空闲时模拟鼠标活动防掉线
    pub simulate_activity: bool,
    /// 注入云手机触点光标（CLI 无头恒 false；Tauri 取用户配置）
    pub custom_cursor: bool,
    /// 屏蔽页面右键菜单
    pub block_context_menu: bool,
    /// 页内 setInterval 驱动开关（CLI false：宿主看门狗经 CDP 驱动 __CPK_TICK__）
    pub page_timer: bool,
}

/// 生成注入到云手机页面的保活初始化脚本。
///
/// `cursor_b64`：触点光标 PNG 的 base64（去空白）。CLI 传空串 =
/// 光标占位符替换为空负载（页面侧不加载图片）；Tauri 传
/// `CURSOR_PNG_B64.trim()`（Android 风格触点指示器，默认关闭仅备用）。
pub fn build_init_script(params: &InjectParams, port: u16, cursor_b64: &str) -> String {
    let inject = serde_json::json!({
        "slot": params.slot,
        "port": port,
        "platform": params.platform,
        "homeUri": params.home_uri,
        "keepAlive": params.keep_alive,
        "intervalMs": params.interval_ms,
        "simulateActivity": params.simulate_activity,
        "customCursor": params.custom_cursor,
        "blockContextMenu": params.block_context_menu,
        "pageTimer": params.page_timer,
    });
    let json = serde_json::to_string(&inject).unwrap_or_default();
    TEMPLATE
        .replace("var CFG = __CPK_CFG__;", &format!("var CFG = {};", json))
        .replace(
            "data:image/png;base64,__CPK_CURSOR__",
            &format!("data:image/png;base64,{cursor_b64}"),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> InjectParams<'static> {
        InjectParams {
            slot: 1,
            platform: "mobile",
            home_uri: "https://cloudphoneh5.buy.139.com",
            keep_alive: true,
            interval_ms: 5000,
            simulate_activity: true,
            custom_cursor: false,
            block_context_menu: true,
            page_timer: false,
        }
    }

    #[test]
    fn placeholders_replaced() {
        let s = build_init_script(&base(), 8088, "");
        // 注：脚本头部文档注释会提及占位符名，断言只针对实际代码行
        assert!(!s.contains("var CFG = __CPK_CFG__;"), "CFG 占位符应被替换");
        assert!(!s.contains("base64,__CPK_CURSOR__"), "光标占位符应被替换");
        assert!(s.contains("\"platform\":\"mobile\""));
        assert!(s.contains("\"port\":8088"));
        assert!(s.contains("\"pageTimer\":false"));
        // 注入的 JSON 含 $ 序列时不得被误展开（Rust replace 是字面替换，天然满足；
        // 显式断言防回归）
        assert!(s.contains("var CFG = {"));
        assert!(s.contains("\"slot\":1"));
        assert!(s.contains("\"homeUri\":\"https://cloudphoneh5.buy.139.com\""));
        assert!(s.contains("\"intervalMs\":5000"));
        assert!(s.contains("\"keepAlive\":true"));
        assert!(s.contains("\"customCursor\":false"));
        // 无 Rust format! 转义残留（模板是纯 JS，非 Rust format 字符串）
        assert!(!s.contains("{{"));
    }

    #[test]
    fn port_injected() {
        let s = build_init_script(&base(), 1234, "");
        assert!(s.contains("\"port\":1234"));
    }

    #[test]
    fn cursor_payload_modes() {
        // CLI 模式：空串 → 前缀保留、负载为空
        let cli = build_init_script(&base(), 8088, "");
        assert!(cli.contains("data:image/png;base64,\""));
        // Tauri 模式：b64 负载原样落位
        let tauri = build_init_script(&base(), 8088, "QUJDREVGRw==");
        assert!(tauri.contains("data:image/png;base64,QUJDREVGRw=="));
        assert!(!tauri.contains("base64,__CPK_CURSOR__"));
    }

    #[test]
    fn platform_variant_builds_unicom_cfg() {
        let mut p = base();
        p.platform = "unicom";
        p.home_uri = "https://uphone.wo-adv.cn/cloudphone/#/home";
        let s = build_init_script(&p, 8088, "");
        assert!(s.contains("\"platform\":\"unicom\""));
        assert!(s.contains("\"homeUri\":\"https://uphone.wo-adv.cn/cloudphone/#/home\""));
        // 联通选择器在脚本本体里（平台分支由 CFG.platform 运行时选择）
        assert!(s.contains(".try-content"));
    }

    #[test]
    fn params_flow_into_cfg() {
        // Tauri 侧参数形态：槽位 3 / interval 8000 / 光标开 / 页内定时器开
        let p = InjectParams {
            slot: 3,
            platform: "unicom",
            home_uri: "https://uphone.wo-adv.cn/cloudphone/#/home",
            keep_alive: false,
            interval_ms: 8000,
            simulate_activity: false,
            custom_cursor: true,
            block_context_menu: false,
            page_timer: true,
        };
        let s = build_init_script(&p, 9090, "eHg=");
        assert!(s.contains("\"slot\":3"));
        assert!(s.contains("\"port\":9090"));
        assert!(s.contains("\"keepAlive\":false"));
        assert!(s.contains("\"intervalMs\":8000"));
        assert!(s.contains("\"simulateActivity\":false"));
        assert!(s.contains("\"customCursor\":true"));
        assert!(s.contains("\"blockContextMenu\":false"));
        assert!(s.contains("\"pageTimer\":true"));
        assert!(s.contains("data:image/png;base64,eHg="));
    }

    #[test]
    fn keepalive_semantics_fully_ported() {
        let s = build_init_script(&base(), 8088, "");
        // 双定时器语义（stopCheck 每 tick / actionTick 墙钟门控 ≈ intervalMs）
        assert!(s.contains("state.nextActionAt"));
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
    }

    #[test]
    fn touch_double_fire_guard_present() {
        // 防双发守卫（Linux 点击无反应的另一半根因）：真实触摸轻点后
        // Chrome 合成的 mousedown/mouseup 不得再被模拟器转一轮触摸
        let s = build_init_script(&base(), 8088, "");
        assert!(s.contains("tsFromTouchSynth"), "缺防双发判定函数");
        assert!(s.contains("ev.isTrusted"), "缺 isTrusted 真实事件判别");
        assert!(s.contains("tsLastRealEnd"), "缺真实触摸时间戳跟踪");
    }

    #[test]
    fn tap_click_synth_fallback_present() {
        // 触摸轻点 → 合成 click 兜底（chrome-headless-shell 无 touch→mouse 合成链）
        let s = build_init_script(&base(), 8088, "");
        assert!(s.contains("tkSynthing"), "缺合成 click 兜底（防再转触摸标记）");
        assert!(s.contains("tkMouseSeen"), "缺 Chrome 合成链探测（防双发）");
        assert!(s.contains("document.elementFromPoint(st.x, st.y)"), "缺落点元素解析");
        assert!(s.contains("el.dispatchEvent(mk('click', 0))"), "缺合成 click 派发");
        assert!(s.contains("if (tkSynthing) return;"), "缺转换器跳过守卫");
    }
}
