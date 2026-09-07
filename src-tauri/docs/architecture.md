# Windows 版架构（Tauri）

双平台总览与看门狗驱动模型见
[../../shared/docs/architecture.md](../../shared/docs/architecture.md)。
本文为 Windows（Tauri）版专属内容。

## 源码级复刻对照（aardio → Tauri）

原版行为逐项对照（Windows 版实现依据）：

| 原版行为（aardio 源码） | 本项目实现 |
| :--- | :--- |
| `login.aardio` 目录名为空 → msgbox「缓存数据目录名不能为空」 | 前端红横幅同文案阻止，后端同样拦截 |
| `showWebForm` 成功 → `loginForm.show(false)` | 点「进入」**立即**隐藏设置窗口（前端直接隐藏，不等窗口创建）；启动失败自动唤回并显示错误 |
| `login/web.aardio` 关闭窗口 → `win.quitMessage()` | 设置窗口点 X → 销毁全部窗口并退出进程；**云手机窗口点 X → 隐藏到托盘继续保活**（退出走托盘菜单） |
| `web.aardio` 菜单五项：云手机首页/旋转/窗口置顶/设置/检查更新 | 窗口菜单栏已移除：「首页/窗口置顶」并入托盘右键菜单；「旋转/检查更新」删除 |
| 「设置」settingWin：仅分辨率 + 保存，内存生效不落盘 | `winset` 小窗：改窗口与会话内覆盖，重启恢复配置值 |
| 「旋转」交换 userInfo 宽高 + `go(location)` 刷新，不落盘 | **已移除**（随菜单栏一起删除；横竖屏需求可在设置窗口直接改窗口分辨率） |
| `win.util.tray(webForm)` 每窗口一托盘，菜单 显示(●)/隐藏/退出 | 每槽位独立托盘，左键单击/双击呼出窗口（不再设显示/隐藏菜单项），退出=退出程序 |
| `reghotkey Ctrl+N` 显隐 + 置前 | 同；注册失败仅告警不阻塞（原版亦不检查返回值） |
| `reghotkey Ctrl+U` 地址栏，回车 `go(url)` | **CLI 版不提供地址栏**（按需求：无头远程控制无需浏览器地址栏；导航走控制页「回首页」/`/nav`。shared 脚本注入的 `#cpk-addr-bar` 在云机页内惰性存在，无副作用） |
| `runTimer 5000ms`：重连/进入/确认弹窗 + 解锁区 + 进入云机 | `actionTick` 每 5 秒，文字**包含匹配**（还原 `string.keywords`） |
| `stopTimer 1000ms`：#tabbar 退出检测 + 「知道了」到期确认，触发后停用 | `stopCheck` 每 1 秒，`stopDone` 标志触发一次后停用 |
| CDP `Network.setCookies` domain=`.139.com` 导航前生效 | **已移除**（登录态由各帐号独立数据目录保持，无需手动指定 Cookie） |
| `enableDefaultContextMenus(false)`、触点光标注入、`onDocumentInit` 重装 | 右键屏蔽 + 触点光标**默认关闭**（使用系统默认鼠标指针，资源本地内嵌备用）+ 每次导航自动重装 |
| 桌面浏览器打开 H5 即可用鼠标操控（页面自带鼠标→触摸模拟器） | **鼠标→触摸操控模拟**：注入脚本在页面代码运行前补齐 `ontouchstart`，页面自带的模拟器永不加载，鼠标→触摸一律由内置同款模拟器接管——任何路由、**重载后**都能点动云机，鼠标拖动=滑动、不再选择文本 |
| 启动时 `fsys.update` 联网检查并自动更新 | **已移除**（本项目不含任何自动更新/联网下载逻辑；原「检查更新」菜单项也随菜单栏删除） |

原版已知的坑未复刻（属 bug 而非行为）：`appComponents` 全局命名空间导致多窗口
互相覆盖定时器、地址栏 `myTimer` 空引用、同索引重复加载窗口等——本项目按槽位
隔离修复，否则多开保活无法工作。

## WebView2 性能调优（Windows 版 v1.8.1 起）

程序启动早期自动设置 `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`（WebView2
加载器与内置参数**合并**生效，全部窗口共享）：

- **关闭 `CalculateNativeWinOcclusion`（遮挡限流）**：Chromium 默认检测窗口
  遮挡，云手机窗口被其他窗口盖住时可能被当作后台而限流渲染器——对每秒传输
  画面的保活场景是直接威胁。关闭后盖住照常跑（代价：被盖住时照常渲染，略多 CPU）
- **关闭 Edge 后台服务**：`Translate`（翻译）、`AutofillServerCommunication`
  （表单自动填充云端上报）、`OptimizationHints`（优化指南预取）、
  `InterestFeedContentSuggestions`（资讯流）、`HardwareMediaKeyHandling`/
  `MediaSessionService`（系统媒体键/正在播放集成）、`msEdgeBackgroundProcessing`
  （后台维护）。均与页面脚本、视频流无关，只省待命开销
- `--disable-features` 多处出现时 WebView2 按**并集合并**（官方文档明确的例外，
  普通开关才只认最后一个），与 wry 内置的关闭项自动合并互不覆盖；清单仍重复
  带上 wry 默认三项（`msWebOOUI,msPdfOOUI,msSmartScreenProtection`）作防御

注意：若外部已设置该环境变量，程序**不覆盖**（启动日志有记录），需要内置调优时
请清除该变量后重启。启动参数完整内容见程序级日志（数据根目录 `cpk-*.log`）首条的
「WebView2 调优参数已应用」。
