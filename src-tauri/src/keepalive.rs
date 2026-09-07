use crate::config::SlotConfig;
use cloudphonekeep_shared::keepalive::{build_init_script as shared_build, InjectParams};

/// 内嵌的触点光标 PNG（26×26，热点居中 13,13）。安卓官方风格触点指示器：
/// Android 品牌绿(#3DDC84)主圆环 + 外层淡绿光晕 + 白色半透明触点面，
/// 本地程序化绘制（抗锯齿），零第三方网络依赖。
/// v1.7.2 起默认关闭（custom_cursor=false，使用系统默认鼠标指针），资源保留备用。
/// Tauri 专属资源（CLI 无头端不需要），故留在 src-tauri/assets 而不进 shared。
const CURSOR_PNG_B64: &str = include_str!("../assets/cursor.b64");

/// 生成注入到云手机页面的保活初始化脚本。
///
/// 脚本本体与占位符替换逻辑的唯一源在 `shared/keepalive.inject.js` +
/// `cloudphonekeep_shared::keepalive`（CLI/Tauri 双端同源，改保活规则、
/// 选择器、弹窗处理逻辑只需改那一份，双端构建后同时生效）。
///
/// Tauri 端策略（与 CLI 端的差异，见 shared::keepalive 模块注释）：
///   - slot / platform / homeUri / 各开关取槽位配置（SlotConfig）
///   - platform 空值回退 "unicom"（历史行为）
///   - pageTimer 恒 true（窗口可见时页内 setInterval 驱动，隐藏时看门狗接管）
///   - customCursor 取用户配置，光标 b64 用本端内嵌资源
///
/// 双定时器结构忠实还原原版 web.aardio：
///   stopTimer 1000ms → 脚本 stopCheck()：退出检测(#tabbar/.title-bar) + 到期「知道了」
///   runTimer  5000ms → 脚本 actionTick()：重连/进入/确认弹窗点击 + 解锁区/进入云机
/// （窗口隐藏时由 Rust 看门狗每 1 秒 eval __CPK_TICK__ 驱动，tick 内自行按周期分流）
///
/// 按槽位配置的 platform 分流（unicom 联通 / mobile 移动）：
/// 联通：试用弹窗(.try-content/.try-btn)、无法连接(.phone-dialog-wrap，
///       v1.9.0 起按钮宽松匹配 + miss 时记录弹窗全文/按钮清单 + 持续失败分级兜底)、
///       详情页进入云机(.detail-info-container/.enter-intance)、到期(.van-dialog__confirm)、
///       退回首页检测(.title-bar)
/// 移动：解锁区进入云机(.unlocked/.enter-intance)、重连/进入/确认按钮按文字包含匹配、
///       到期「知道了」(.van-dialog__confirm)、退回 H5 首页检测(#tabbar)；
///       未知文字的确认弹窗 miss 附弹窗全文与按钮清单，持续 3 分钟未识别
///       自动整页重载（对齐 v1.9.0 联通同款分级兜底，改版不再静默失效）
/// 通用：触点光标（默认关闭，系统默认指针）、屏蔽右键、鼠标→触摸操控模拟
///       （WebView2 里页面自带的模拟器不加载，鼠标拖不动云机——移植页面同款 TouchEmulator 补上）、
///       空闲鼠标活动模拟、
///       状态通过 127.0.0.1 回环 HTTP 上报给 Rust 侧（绕过跨域与远程 IPC 限制）
pub fn build_init_script(cfg: &SlotConfig, port: u16) -> String {
    let platform = if cfg.platform.trim().is_empty() {
        "unicom".to_string()
    } else {
        cfg.platform.trim().to_string()
    };

    shared_build(
        &InjectParams {
            slot: cfg.slot,
            platform: &platform,
            home_uri: &cfg.web_uri,
            keep_alive: cfg.keep_alive,
            interval_ms: cfg.interval_ms,
            simulate_activity: cfg.simulate_activity,
            custom_cursor: cfg.custom_cursor,
            block_context_menu: cfg.block_context_menu,
            // Windows：窗口可见时由页内 setInterval 驱动（隐藏时看门狗接管）。
            // CLI 无头端传 false（宿主 CDP 看门狗驱动），见 shared 脚本头注释
            page_timer: true,
        },
        port,
        CURSOR_PNG_B64.trim(),
    )
}
