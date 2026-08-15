# 逆向分析记录

本项目是两款 aardio 云手机保活工具的 Tauri 2 重写版。本文档记录逆向得到的原始逻辑与重写时的修正，便于后续网站改版时对照排查。

## 一、联通云手机（摸鱼浏览器.exe）

32 位 aardio 编译程序，主窗体 `forms/web.aardio`，入口 `uphone.wo-adv.cn/cloudphone/#/home`。

逆向得到的核心保活选择器：

| 场景 | 选择器 | 动作 |
| :--- | :--- | :--- |
| 试用弹窗 | `.try-content` / `.try-btn` | 点击「立即启用云手机」 |
| 无法连接 | `.phone-dialog-wrap` | 按文字点「再次尝试/重试/重新连接/重新载入」 |
| 详情页 | `.detail-info-container` / `.enter-intance` | 点击「进入云机」 |
| 到期弹窗 | `.van-dialog__confirm` | 点击「知道了」 |
| 退出检测 | `.title-bar` | 退回首页即云机退出，托盘气泡通知 |

其他特征：Cookie 域 `.wo-adv.cn`；老板键 Ctrl+1~9；标题格式「联通云手机 - {name} - 老板键 Ctrl+{n}」；默认窗口 405×720。

## 二、移动云手机（移动云手机.exe + mobile_cloud 源码）

后获得原作者 aardio 源码（mobile_cloud 项目），比 exe 字符串逆向更准确。`forms/web.aardio` 中为双定时器结构：

- `runTimer`（5 秒）：保活动作
  1. `.van-dialog__confirm` 按钮文字含「重连」→ 点击（断线重连）
  2. `.van-dialog__confirm` 按钮文字含「进入」→ 点击（超时重连）
  3. `.van-dialog__confirm` 按钮文字含「确认」→ 点击
  4. `.unlocked` 区域含「进入」→ **直接点击 `.unlocked` 容器本身**
  5. `.enter-intance` 含「进入云机」→ 点击
- `stopTimer`（1 秒）：退出/到期检测
  1. `#tabbar` 存在 → 已退出云手机（托盘气泡 + 停止检测）
  2. `.van-dialog__confirm` 含「知道了」→ 点击 + 到期通知（停止检测）

判定方式：`cdpWaitQuery(selector)` 取容器 `nodeId`，再 `DOM.getOuterHTML` 在 HTML 文本中匹配关键词（相当于容器可见且文字匹配的双重验证）。

关键认知：**`.van-dialog__confirm` 是该站万能确认按钮**，重连/超时进入/确认/到期全部走同一个按钮，靠按钮文字区分语义。

其他特征：Cookie 域 `.139.com` path `/`，经 CDP `Network.setCookies` 注入；默认窗口 414×896；入口 `cloudphoneh5.buy.139.com`。

### 重写时的修正（相对 exe 字符串逆向的初版）

| 初版实现（猜测） | 源码证实后 |
| :--- | :--- |
| confirm 弹窗一律当到期处理，重连靠全页文字找按钮 | confirm 按按钮文字分流：重连/进入/确认/知道了 |
| 解锁区找 `.enter-intance` 按钮点击 | 直接点击 `.unlocked` 容器本身 |
| 任意 confirm 弹窗点「知道了」 | 仅按钮文字为「确认/知道了」时点击；未知文字记录日志不盲点 |

## 三、两版共有的风险点（本项目已移除）

1. **更新后门**：`fsys.update.simpleMain` 启动即连 `download.617kan.cn`（联通版与移动版各自路径），下载并替换可执行文件 → 本项目改为仅提示的 GitHub Releases 检查，绝不自动下载执行
2. **第三方光标资源**：触点光标 PNG/CUR 来自 `fs-im-kefu.7moor-fs1.com` → 本项目改为本地程序化绘制的安卓官方风格触点图标（内嵌 base64），零外部依赖
3. 衍生版含大漠插件（dm.dll）自动接码功能 → 与保活无关，未引入

## 四、改版排查方法

配合诊断日志（见 README「诊断日志」一节）：

1. `[beat]` 的 `hits=[...]` 观察选择器命中是否全 0
2. `[miss]` 出现「容器可见但未找到按钮/未知文字」即改版证据
3. 「DOM 采样」获取当前页面全部 class 清单，对照本文档选择器表修正 `src-tauri/src/keepalive.rs`
