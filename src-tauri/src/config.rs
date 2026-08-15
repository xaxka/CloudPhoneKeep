use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_WEB_URI: &str = "https://uphone.wo-adv.cn/cloudphone/#/home";
/// 移动云手机 H5 入口
pub const MOBILE_WEB_URI: &str = "https://cloudphoneh5.buy.139.com";

/// 支持的平台预设
pub struct PlatformPreset {
    pub id: &'static str,
    pub label: &'static str,
    pub web_uri: &'static str,
    pub width: f64,
    pub height: f64,
}

pub const PLATFORMS: [PlatformPreset; 2] = [
    PlatformPreset {
        id: "mobile",
        label: "移动云手机",
        web_uri: MOBILE_WEB_URI,
        width: 414.0,
        height: 896.0,
    },
    PlatformPreset {
        id: "unicom",
        label: "联通云手机",
        web_uri: DEFAULT_WEB_URI,
        width: 405.0,
        height: 720.0,
    },
];

pub fn platform_preset(id: &str) -> &'static PlatformPreset {
    PLATFORMS
        .iter()
        .find(|p| p.id == id)
        .unwrap_or(&PLATFORMS[0])
}

/// Cookie 注入域（还原原版 CDP Network.setCookies 的 domain 参数）
/// 移动 .139.com（源码明确指定）；联通 .wo-adv.cn（REVERSE.md 逆向结论）
pub fn platform_cookie_domain(platform: &str) -> &'static str {
    if platform == "unicom" {
        ".wo-adv.cn"
    } else {
        ".139.com"
    }
}

// ---------------------------------------------------------------------------
// 便携化：全部数据保存在 exe 所在目录（与原版 aardio 程序一致）
//   config.json        总配置
//   data/slot-N-名字   每个帐号的浏览器数据目录（Cookie/缓存隔离）
//   logs/cpk-*.log     诊断日志
// ---------------------------------------------------------------------------

/// exe 所在目录（便携根目录）
pub fn base_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn config_path() -> PathBuf {
    base_dir().join("config.json")
}

/// 帐号名 → 安全目录名（只保留字母数字 - _，其余替换为 _）
fn safe_name(name: &str) -> String {
    name.trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
}

/// 每个帐号独立的 WebView 数据目录（Cookie / 缓存隔离）。
/// 目录名 = 用户填的「缓存数据目录名」本身（如填 1 → data/1），
/// 与原版程序语义一致；为空时兜底 slot-{N}。
pub fn profile_dir(slot: u32, name: &str) -> PathBuf {
    profile_dir_with_suffix(slot, name, "")
}

/// 带后缀的数据目录（数据目录被残留进程锁定时的兜底新目录，如 1-r2）
pub fn profile_dir_with_suffix(slot: u32, name: &str, suffix: &str) -> PathBuf {
    let safe = safe_name(name);
    let folder = if safe.is_empty() {
        format!("slot-{slot}{suffix}")
    } else {
        format!("{safe}{suffix}")
    };
    let p = base_dir().join("data").join(folder);
    std::fs::create_dir_all(&p).ok();
    p
}

/// 旧版目录命名（slot-N-名字）→ 新版（data/名字）一次性迁移，保留登录态。
/// 返回 Some(结果描述) 表示尝试过迁移（成功或失败都写日志告知用户）。
pub fn migrate_legacy_profile(slot: u32, name: &str) -> Option<String> {
    let safe = safe_name(name);
    let legacy_name = if safe.is_empty() {
        format!("slot-{slot}")
    } else {
        format!("slot-{slot}-{safe}")
    };
    let legacy = base_dir().join("data").join(&legacy_name);
    let target = profile_dir(slot, name);
    if !legacy.exists() || target.exists() {
        return None; // 没有旧目录 / 新目录已在用，无需迁移
    }
    let target_name = target
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    match std::fs::rename(&legacy, &target) {
        Ok(_) => Some(format!(
            "旧数据目录 {legacy_name} 已改名为 {target_name}（登录态保留）"
        )),
        Err(e) => Some(format!(
            "旧数据目录 {legacy_name} 迁移失败（{e}），本次使用新目录 {target_name}，可能需重新登录一次"
        )),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SlotConfig {
    /// 槽位编号 1~9，同时是老板键 Ctrl+N 的 N
    pub slot: u32,
    /// 帐号名称（建议填手机号），用于缓存目录隔离
    pub name: String,
    /// 平台：mobile 移动云手机 / unicom 联通云手机
    pub platform: String,
    /// 浏览器地址
    pub web_uri: String,
    /// 自定义 Cookie（每行 name=value）
    pub cookies: String,
    /// 窗口宽
    pub width: f64,
    /// 窗口高
    pub height: f64,
    /// 保活引擎开关
    pub keep_alive: bool,
    /// 保活检测间隔（毫秒）
    pub interval_ms: u64,
    /// 空闲时模拟鼠标活动防掉线
    pub simulate_activity: bool,
    /// 注入云手机触点光标（默认关闭：使用系统默认鼠标指针；
    /// 归一化时强制改回 false，清理历史上被「固定启用」写入的 true）
    pub custom_cursor: bool,
    /// 屏蔽页面右键菜单
    pub block_context_menu: bool,
}

impl Default for SlotConfig {
    fn default() -> Self {
        Self {
            slot: 1,
            name: String::new(),
            platform: "mobile".to_string(),
            web_uri: MOBILE_WEB_URI.to_string(),
            cookies: String::new(),
            width: 414.0,
            height: 896.0,
            keep_alive: true,
            interval_ms: 5000,
            simulate_activity: true,
            custom_cursor: false,
            block_context_menu: true,
        }
    }
}

impl SlotConfig {
    /// 归一化非法值
    pub fn normalized(&self) -> SlotConfig {
        let mut c = self.clone();
        if !(1..=9).contains(&c.slot) {
            c.slot = 1;
        }
        if c.platform.trim().is_empty() {
            c.platform = "mobile".into();
        }
        let preset = platform_preset(&c.platform);
        if c.web_uri.trim().is_empty() {
            c.web_uri = preset.web_uri.to_string();
        }
        if c.width < 280.0 || c.height < 400.0 {
            c.width = preset.width;
            c.height = preset.height;
        }
        if c.interval_ms < 1000 {
            c.interval_ms = 5000;
        }
        // 触点光标默认关闭：老配置里被「固定启用」写入的 true 一并清理（界面已无开关）
        c.custom_cursor = false;
        c
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct AppConfig {
    pub slots: Vec<SlotConfig>,
}

impl AppConfig {
    /// 空配置：不再预生成 9 个槽位（此前默认填充 1~9 导致 config.json 里
    /// 一大堆没用的空槽位；真正用到的槽位在首次「进入」时才写入）
    pub fn with_slots() -> Self {
        Self { slots: Vec::new() }
    }
}

pub fn load() -> AppConfig {
    let path = config_path();
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(cfg) = serde_json::from_str::<AppConfig>(&text) {
            return normalize(cfg);
        }
    }
    AppConfig::with_slots()
}

/// 归一化：只保留文件里真实存在的槽位（不再补齐 1~9 空位）；
/// 历史版本写入的空名槽位（从未真正配置过）一并清掉
fn normalize(mut cfg: AppConfig) -> AppConfig {
    let mut seen = std::collections::HashSet::new();
    cfg.slots.retain(|s| {
        seen.insert(s.slot) && !s.name.trim().is_empty()
    });
    cfg.slots.sort_by_key(|s| s.slot);
    cfg.slots = cfg.slots.iter().map(|s| s.normalized()).collect();
    cfg
}

pub fn save(cfg: &AppConfig) -> Result<(), String> {
    let path = config_path();
    let text = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("写入 {} 失败: {e}", path.display()))
}

/// 更新/插入一个槽位并落盘
pub fn upsert_slot(cfg: &mut AppConfig, slot: SlotConfig) {
    let slot = slot.normalized();
    if let Some(pos) = cfg.slots.iter().position(|s| s.slot == slot.slot) {
        cfg.slots[pos] = slot.clone();
    } else {
        cfg.slots.push(slot.clone());
        cfg.slots.sort_by_key(|s| s.slot);
    }
    // 其他空名槽位（从未真正配置过）不再保留，config.json 只留真实帐号
    cfg.slots.retain(|s| !s.name.trim().is_empty() || s.slot == slot.slot);
}
