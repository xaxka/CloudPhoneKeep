use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};

/// 上报状态中视为"自动点击动作"的集合（用于统计点击次数）
/// 视为「主动点击」的上报状态（计入 clicks 统计）
pub const ACTION_STATUSES: [&str; 5] = ["try-enable", "retry", "enter", "confirm", "expired"];

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
    /// 会话内分辨率覆盖（旋转/窗口设置只改内存，不落盘——还原原版语义）
    pub size_override: Option<(f64, f64)>,
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
            size_override: None,
        }
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
