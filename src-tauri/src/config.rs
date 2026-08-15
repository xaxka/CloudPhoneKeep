use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::Manager;

pub const DEFAULT_WEB_URI: &str = "https://uphone.wo-adv.cn/cloudphone/#/home";
/// 更新检查默认指向本仓库自己的 GitHub Releases。
/// 仅获取版本信息用于提示，绝不自动下载或执行任何文件。
pub const DEFAULT_UPDATE_URL: &str =
    "https://api.github.com/repos/xixka/CloudPhoneKeep/releases/latest";
pub const DEFAULT_DOWNLOAD_PAGE: &str = "https://github.com/xixka/CloudPhoneKeep/releases";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SlotConfig {
    /// 槽位编号 1~9，同时是老板键 Ctrl+N 的 N
    pub slot: u32,
    /// 是否启用（自动启动时使用）
    pub enabled: bool,
    /// 帐号名称（建议填手机号），用于缓存目录隔离
    pub name: String,
    /// 浏览器地址
    pub web_uri: String,
    /// 自定义 Cookie（每行 name=value; 可选 domain=...）
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
            enabled: false,
            name: String::new(),
            web_uri: DEFAULT_WEB_URI.to_string(),
            cookies: String::new(),
            width: 405.0,
            height: 720.0,
            screen_model: "vertical".to_string(),
            keep_alive: true,
            interval_ms: 5000,
            simulate_activity: true,
            custom_cursor: true,
            block_context_menu: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// 启动时自动开启已启用的帐号
    pub auto_start: bool,
    /// 在线更新检查地址
    pub update_url: String,
    /// 新版本下载页
    pub download_page: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_start: false,
            update_url: DEFAULT_UPDATE_URL.to_string(),
            download_page: DEFAULT_DOWNLOAD_PAGE.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct AppConfig {
    pub settings: Settings,
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
        Self {
            settings: Settings::default(),
            slots,
        }
    }
}

pub fn config_path(app: &tauri::AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_config_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    std::fs::create_dir_all(&dir).ok();
    dir.join("config.json")
}

/// 每个帐号独立的 WebView 数据目录（Cookie / 缓存隔离）
pub fn profile_dir(app: &tauri::AppHandle, slot: u32, name: &str) -> PathBuf {
    let dir = app
        .path()
        .app_local_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
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
    let p = dir.join("profiles").join(folder);
    std::fs::create_dir_all(&p).ok();
    p
}

pub fn load(app: &tauri::AppHandle) -> AppConfig {
    let path = config_path(app);
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(mut cfg) = serde_json::from_str::<AppConfig>(&text) {
            // 补齐缺失槽位，保持 1..=9
            let mut slots: Vec<SlotConfig> = Vec::new();
            for i in 1..=9u32 {
                if let Some(found) = cfg.slots.iter().find(|s| s.slot == i).cloned() {
                    slots.push(found);
                } else {
                    let mut s = SlotConfig::default();
                    s.slot = i;
                    slots.push(s);
                }
            }
            cfg.slots = slots;
            return cfg;
        }
    }
    AppConfig::with_slots()
}

pub fn save(app: &tauri::AppHandle, cfg: &AppConfig) -> Result<(), String> {
    let path = config_path(app);
    let text = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| e.to_string())
}
