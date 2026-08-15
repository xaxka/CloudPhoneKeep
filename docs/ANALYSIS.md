# 原版「摸鱼浏览器」逆向分析笔记

> 分析对象：`摸鱼浏览器.exe`（PE32 GUI, 2.68 MB, FileVersion 0.0.0.148, Copyright 2024）
> 分析方式：静态解析 PE 结构 / 资源目录 / aardio 窗体数据。未做任何动态行为检测。

## 1. 程序本质

该程序并非易语言编译，而是 **aardio** 静态编译产物（资源中含 112 个内嵌支持库 `LIB/...`，窗体为 `RES/FORMS/*.AARDIO`）。报毒根因：aardio 静态编译将全部中文运行时 + 键盘钩子支持库（`KEY.HOOK`）打包进单个 EXE，特征与大量灰产工具高度重合。

## 2. 窗体清单

| 窗体 | 作用 |
| :--- | :--- |
| `login.aardio` | 帐号配置：缓存目录名(建议手机号)、老板键索引 Ctrl+1~9、浏览器地址、指定 Cookie、窗口分辨率；`fsys.table` 按窗口索引持久化 `config_N` |
| `web.aardio` | 主浏览器窗口：内嵌 `web.view`(WebView2)，标题 `联通云手机 - {帐号} - 老板键 Ctrl+N`，托盘、菜单(首页/旋转/置顶/设置/检查更新)、保活引擎 |
| `address.aardio` | 地址栏快速导航 |
| `web_config.aardio` | 窗口分辨率设置 |
| `update.aardio` | 在线更新（fsys.update.simpleMain） |

## 3. 关键常量

```
默认地址:   https://uphone.wo-adv.cn/cloudphone/#/home   (联通云手机 uPhone)
更新源:     http://download.617kan.cn/moyu-webview-update-files/version.txt
触点光标:   https://fs-im-kefu.7moor-fs1.com/ly/.../Dotter.cur
Cookie域:   .139.com  path=/
定时器:     1000ms / 5000ms 双定时器（timerStatus / topTimerStatus）
```

## 4. 保活引擎逻辑（web.aardio 还原）

通过 CDP（`cdpWaitQuery` + `DOM.getOuterHTML`）周期扫描页面并自动点击：

| 页面特征 | 动作 |
| :--- | :--- |
| `.try-content` 出现 | 点击 `.nut-popup--center .try-btn`（立即启用云手机） |
| `.phone-dialog-wrap` 含「无法连接」 | `go(webUrl)` 后点击「再次尝试」 |
| `.detail-info-container` 含「进入/确认/重连」 | 点击 `.enter-intance`（进入云机） |
| `.van-dialog__confirm`（时间已到期弹窗） | 自动点击「知道了」 |
| `.title-bar` 出现 | 判定已退出云手机 → 停表 + 托盘气泡「帐号 X 已退出云手机，窗口索引：N」 |

关键词匹配使用 `string.keywords` 对 DOM 文本按行搜索。

## 5. 其它行为

- 老板键：`key.hotkey` 注册 Ctrl+1~9，切换窗口显隐并 `setForeground`
- 托盘：`win.util.tray`，菜单 ● 显示 / ● 隐藏 / 退出；最小化隐藏到托盘
- Cookie：`Network.setCookies` 按 `;` 分行解析 name/value，统一 domain=`.139.com`
- 每帐号独立 WebView2 `userDataDir`，实现多帐号登录隔离
- 注入 CSS 全局替换光标为触点（模拟手机触屏观感）、禁用右键菜单与状态栏
- 屏幕方向 vertical/horizontal 切换时交换窗口宽高并 `go(location)` 刷新

## 6. 原版失效原因推测

1. 保活完全依赖 CDP 定时查询 + 固定 CSS 选择器/文案，云手机前端改版后选择器失配
2. 窗口隐藏后 aardio 侧定时器虽在跑，但 WebView2 页面侧被节流，CDP 查询目标节点长期不存在即静默失效
3. 更新源 `download.617kan.cn` 失效后无法推送新选择器

本项目 (CloudPhoneKeep) 针对性改进：选择器与文案常量集中在 `keepalive.rs` 一处便于维护；Rust 看门狗直接 `eval` 驱动页面内函数，不受页面定时器节流影响；新增空闲鼠标活动模拟。
