/// 每个帐号槽位的运行状态（进程内使用；看板/点击统计等纯展示字段
/// 已随初版管理面板一并移除，只留有真实消费者的字段）
#[derive(Debug, Default)]
pub struct SlotState {
    pub slot: u32,
    pub running: bool,
    pub visible: bool,
    pub topmost: bool,
    /// 最近一次上报状态（"未启动"/"installed"/"alive"/"exited"/"expired"...），
    /// exited/expired 状态迁移时触发系统通知（见 report_server.rs）
    pub last_status: String,
    /// 会话内分辨率覆盖（旋转/窗口设置只改内存，不落盘——还原原版语义）
    pub size_override: Option<(f64, f64)>,
}

impl SlotState {
    pub fn new(slot: u32) -> Self {
        Self {
            slot,
            last_status: "未启动".into(),
            ..Default::default()
        }
    }
}
