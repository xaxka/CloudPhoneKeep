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

/// 每个帐号独立的 WebView 数据目录（Cookie / 缓存隔离）
pub fn profile_dir(slot: u32, name: &str) -> PathBuf {
    let safe = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    let folder = if safe.is_empty() {
        format!("slot-{slot}")
    } else {
        format!("slot-{slot}-{safe}")
    };
    let p = base_dir().join("data").join(folder);
    std::fs::create_dir_all(&p).ok();
    p
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
    /// 横屏 / 竖屏
    pub screen_model: String,
    /// 保活引擎开关
    pub keep_alive: bool,
    /// 保活检测间隔（毫秒）
    pub interval_ms: u64,
    /// 空闲时模拟鼠标活动防掉线
    pub simulate_activity: bool,
    /// 注入云手机触点光标
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
            screen_model: "vertical".to_string(),
            keep_alive: true,
            interval_ms: 5000,
            simulate_activity: true,
            custom_cursor: true,
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
        c
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct AppConfig {
    pub slots: Vec<SlotConfig>,
}

impl AppConfig {
    /// 生成 9 个槽位的默认配置
    pub fn with_slots() -> Self {
        let mut slots = Vec::new();
        for i in 1..=9u32 {
            let mut s = SlotConfig::default();
            s.slot = i;
            slots.push(s);
        }
        Self { slots }
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

fn normalize(mut cfg: AppConfig) -> AppConfig {
    let mut slots: Vec<SlotConfig> = Vec::new();
    for i in 1..=9u32 {
        let found = cfg.slots.iter().find(|s| s.slot == i).cloned();
        let s = match found {
            Some(s) => s.normalized(),
            None => {
                let mut d = SlotConfig::default();
                d.slot = i;
                d
            }
        };
        slots.push(s);
    }
    cfg.slots = slots;
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
        cfg.slots[pos] = slot;
    } else {
        cfg.slots.push(slot);
        cfg.slots.sort_by_key(|s| s.slot);
    }
}
