use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};

/// 上报状态中视为"自动点击动作"的集合（用于统计点击次数）
pub const ACTION_STATUSES: [&str; 4] = ["try-enable", "retry", "enter", "expired-confirm"];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotState {
    pub slot: u32,
    pub running: bool,
    pub visible: bool,
    pub topmost: bool,
    pub last_status: String,
    pub last_at: i64,
    pub clicks: u64,
    /// 本次会话是否已提示过「隐藏到托盘」通知（避免每次隐藏都打扰）
    pub hide_notified: bool,
}

impl SlotState {
    pub fn new(slot: u32) -> Self {
        Self {
            slot,
            running: false,
            visible: false,
            topmost: false,
            last_status: "未启动".to_string(),
            last_at: 0,
            clicks: 0,
            hide_notified: false,
        }
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
