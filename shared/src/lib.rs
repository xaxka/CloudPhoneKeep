//! CloudPhoneKeep 双端共享库：CLI（`cli/`）与 Tauri（`src-tauri/`）共同使用的
//! 类型、常量与业务逻辑的**唯一源**，避免两端口径漂移：
//! - [`platform`]：平台预设（移动/联通入口、视口、标签）——两端配置层共用
//! - [`keepalive`]：保活注入脚本构建器（模板 `shared/keepalive.inject.js`
//!   经 `include_str!` 内嵌）——占位符替换约定只此一份，两端传各自的策略参数
//!
//! 职责边界（仓库目录约定）：
//! ```text
//! cli/       → CLI 专属（引擎/CDP/WS/报告服务/控制页）
//! src-tauri/ → Tauri 专属（窗口/托盘/热键/IPC；前端 ui/ 在其目录内）
//! shared/    → CLI + Tauri 共享（本 crate）
//! ```

pub mod keepalive;
pub mod platform;
