# 架构与引擎模型

## 双平台总览

同一套保活逻辑，两种宿主形态：

| | Windows 版 | CLI 版（Linux / Docker / 裸机） |
| :--- | :--- | :--- |
| 内核 | WebView2（Edge） | chrome-headless-shell（CfT 官方预编译，双架构） |
| 宿主引擎 | Rust（Tauri 窗口 + 看门狗） | Rust musl 静态二进制（手写 RFC6455 WebSocket + CDP 客户端 + 看门狗） |
| 驱动 | 窗口可见时页内定时器；隐藏/最小化时 Rust 看门狗 eval 驱动 | Rust 看门狗每秒经 CDP 调 `__CPK_TICK__()`（同一模型的无头恒定态） |
| 多账号 | 多窗口多槽位（单进程） | 多容器/多进程（一实例一账号） |
| 首次登录 | 直接在窗口里点 | 浏览器打开控制页：平台留空待选（「设置→平台」选择后引擎加载页面），MJPEG 实时画面 + 全量输入（触摸/鼠标/键盘/剪贴板）、运行时平台切换、退出/到期通知（对齐 Windows 版交互，或外部 DevTools） |
| 保活脚本 | `shared/keepalive.inject.js`（shared crate 内嵌） | 同一本 `shared/keepalive.inject.js`（shared crate 内嵌） |
| 数据位置 | `AppData\LocalLow\CloudPhoneKeep` | Docker：`/data`（volume）；裸机默认 `~/.local/share/cloudphonekeep` |
| 内存 | 单账号 WebView2 300-500MB | Rust 引擎 ~10MB + headless-shell 250-450MB |

**保活脚本唯一源文件**：`shared/keepalive.inject.js` 由共享 crate
（`shared/src/keepalive.rs`）`include_str!` 内嵌，双端适配层
（`src-tauri/src/keepalive.rs` / `cli/src/keepalive.rs`）传各自策略参数后注入——
修改保活规则只需改这一份文件，双端重新构建后同时生效。平台预设（移动/联通
入口、视口）同样收敛为 `shared/src/platform.rs` 唯一源。平台差异全部收敛为
CFG 开关（见 [keepalive-rules.md](keepalive-rules.md)）。

## 看门狗驱动模型

保活脚本内的双定时器（`stopCheck` 1s / `actionTick` 5s）需要有人周期驱动
`__CPK_TICK__()`：

- **Windows**：窗口可见时由页内 `setInterval` 驱动（`CFG.pageTimer=true`）；
  窗口被隐藏**或最小化**后，页内定时器被 Chromium 后台节流，Rust 看门狗
  每 1 秒 eval `__CPK_TICK__()` 接管（v1.11.0 起最小化与隐藏同等接管）。
- **CLI**：无头页面永不可见，恒由宿主看门狗经 CDP 驱动
  （`CFG.pageTimer=false`），避免页内 + 外部双驱动把 5 秒动作周期缩短一半；
  空闲（无人观看且无操作）时 tick 自适应降频（保活动作墙钟门控恒 ≈ 5s，
  见 [../../cli/docs/deploy.md](../../cli/docs/deploy.md)「空闲 CPU 的治理」）。

自动恢复分级（页面级恢复 → CDP 会话重建 → Chromium 重启，双端同思路）：
CLI 版明细（含 0 级导航失败退避）见
[../../cli/docs/deploy.md](../../cli/docs/deploy.md)。

## 版本专属深档

- **Windows（Tauri）**：源码级复刻对照（aardio → Tauri）、WebView2 性能调优 →
  [../../src-tauri/docs/architecture.md](../../src-tauri/docs/architecture.md)
- **CLI（Linux / Docker / 裸机）**：浏览器选型（chrome-headless-shell）、镜像构成与
  多架构、控制台交互、自动恢复分级 → [../../cli/docs/deploy.md](../../cli/docs/deploy.md)
