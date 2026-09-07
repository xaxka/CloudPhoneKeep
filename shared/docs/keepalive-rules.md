# 保活注入规则（shared/keepalive.inject.js）

## 唯一源文件与修改指引

保活脚本本体是 [`shared/keepalive.inject.js`](../keepalive.inject.js)，
由共享 crate（`shared/src/keepalive.rs`）`include_str!` 内嵌——占位符替换
与 CFG 生成只有这一份实现，两个平台的适配层只传各自策略参数后注入：

| 平台 | 适配层 | 注入通道 |
| :--- | :--- | :--- |
| Windows | `src-tauri/src/keepalive.rs` | WebView2 `AddScriptToExecuteOnDocumentCreated` |
| CLI（Linux） | `cli/src/keepalive.rs` | CDP `Page.addScriptToEvaluateOnNewDocument` |

**修改保活规则、选择器、弹窗处理逻辑，只需要改这一个 JS 文件**——两个平台
重新构建后同时生效。构建器（shared crate）负责生成 CFG JSON 与替换两个占位符：

- `__CPK_CFG__` → 配置 JSON（slot / port / platform / homeUri / keepAlive /
  intervalMs / simulateActivity / customCursor / blockContextMenu / pageTimer）
- `__CPK_CURSOR__` → 触点光标 PNG base64（Windows 注入真实内嵌资源；Linux
  无头恒为空串且 customCursor=false）

平台差异全部收敛为 CFG 开关，脚本内其余逻辑双端 100% 一致：

1. `CFG.pageTimer`：`true` = 页内 `setInterval` 1 秒驱动（Windows 窗口可见态，
   隐藏/最小化时看门狗接管）；`false` = 完全由宿主看门狗驱动（Linux CDP 恒定态，
   避免双驱动把动作周期缩短一半）
2. `window.__CPK_DRAIN__()`：诊断环形缓冲。Linux 宿主每 5 秒经 CDP 取走
   （headless 下回环 fetch 可能受混合内容/PNA 策略影响，缓冲保证日志不丢）；
   Windows 不调用，惰性代码无副作用

## 双定时器结构（忠实还原原版 web.aardio）

| 定时器 | 周期 | 职责 |
| :--- | :--- | :--- |
| `stopCheck`（原 stopTimer） | 1 秒 | 退出检测（`#tabbar` / `.title-bar`）+ 到期「知道了」确认，触发后停用 |
| `actionTick`（原 runTimer） | 5 秒（`CFG.intervalMs`） | 重连 / 进入 / 确认弹窗点击 + 解锁区 / 进入云机 |

驱动方：Windows 窗口可见且未最小化时页内驱动；隐藏**或最小化**后由 Rust
看门狗周期 eval `__CPK_TICK__()` 接管（最小化窗口的页内定时器同样被
Chromium 后台节流）。Linux 无头恒由 CDP 看门狗驱动。

## 移动云手机规则（依据原作者 aardio 源码忠实还原）

1. `.van-dialog__confirm` 是该站万能确认按钮，按按钮文字**包含匹配**分流
   （每 5 秒）：含「重连」断线重连；含「进入」超时重进；含「确认」到期与提示确认
2. 出现 `.unlocked` 解锁区且文字含「进入」→ 直接点击容器本身进入云机
3. 出现 `.enter-intance` 且文字含「进入云机」→ 点击进入
4. 每 1 秒检测 `#tabbar`（退回 H5 首页，即云机退出）→ 状态上报 + 系统通知，
   检测一次后停用（还原原版 topTimerStatus）
5. 每 1 秒检测确认按钮含「知道了」→ 自动点击 + 「时间已到期」通知，同样触发
   一次后停用
6. 遇到未知文字的确认弹窗不盲点，写入 `[miss]` 日志（附弹窗全文与按钮清单）
   供分析；持续 3 分钟未识别自动整页重载兜底（登录态在本地数据目录，重载自动
   回云机页，对齐联通 v1.9.0 同款分级兜底）

## 联通云手机规则

1. 出现 `.try-content`（试用提示）→ 自动点击 `.try-btn`「立即启用云手机」
2. 出现 `.phone-dialog-wrap`「无法连接」→ 按钮按**精确 → 宽松包含 → 确认词**
   三级匹配重试/确定类按钮（v1.9.0）；持续 60 秒无已知按钮则点弹窗内任意非退出
   类按钮兜底，持续 3 分钟未恢复自动整页重载（登录态在本地数据目录，重载自动
   回云机页）
3. 出现 `.detail-info-container` 详情页 → 自动点击 `.enter-intance`「进入云机」
4. 出现 `.van-dialog__confirm` 到期弹窗 → 自动点击「知道了」并发出系统通知
5. 检测到 `.title-bar`（退回首页，即云机退出）→ 状态上报 + 系统通知

## 通用规则

- **空闲鼠标活动模拟**：空闲周期内向页面派发轻微 `mousemove` 事件，降低会话
  闲置断开概率
- **状态回环上报**：全部状态通过 `http://127.0.0.1:<port>/report` 上报
  （Chromium 允许 HTTPS 页面访问环回地址，不受混合内容限制）
- **鼠标→触摸操控模拟**（关键修复）：注入脚本在页面代码运行前补齐
  `ontouchstart`，页面自带的鼠标→触摸模拟器永不加载，鼠标→触摸一律由内置
  同款模拟器接管——任何路由、重载后都能点动云机（桌面 WebView 里页面自带
  模拟器只在部分路由加载，重载后鼠标点不动云机就是它造成的）；两个转换器
  天然互斥，不会双重转换
- **触点光标**：默认关闭（系统默认鼠标指针），Windows 版可开（本地内嵌
  PNG，零外部资源依赖）
- **屏蔽页面右键菜单**（`CFG.blockContextMenu`）
- **路由变化检测**：tick 内纯 `location` 读取对比，SPA 改版定位的第一线索
- **页面加载看门狗**：15 秒后检查 `readyState` / body 子元素数量，空白页
  落盘 `[error]` 并展示重试条（Windows）

## Linux 版专属行为（均在 shared 脚本内以 CFG 开关收敛）

- `__CPK_DRAIN__` 诊断环形缓冲（见上文）
- UA 规范化：`CPK_UA_MODE=windows` 时把 Headless UA 伪装成 Windows Chrome
  （对齐 Windows 版环境）；`auto` 只去掉 Headless 字样；`none` 原样
- CDP `Input.dispatchTouchEvent` 内核级触摸（控制页远程点击/滑动）
- 页内地址栏 `__CPK_ADDR__`（脚本注入 `#cpk-addr-bar`，双端同源代码）：
  Windows 版由 Ctrl+U 全局热键切换（回车跳转、Esc 关闭在页内自理）；
  Linux 版**不提供地址栏**（无触发入口）：`#cpk-addr-bar` 在云机页内
  恒 `display:none` 惰性存在、`__CPK_ADDR__` 从未被调用，无任何副作用；
  导航走控制页「回首页」/`/nav` 端点
