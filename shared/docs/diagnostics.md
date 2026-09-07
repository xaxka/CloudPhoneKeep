# 诊断日志与排查

云手机网站改版导致保活失效时，通过日志快速定位。日志按天滚动、自动保留 7 天。
级别表与行格式双端一致（Windows / CLI 同一套）。

## 日志位置

| 平台 | 位置 |
| :--- | :--- |
| Windows 程序级 | 数据根目录 `AppData\LocalLow\CloudPhoneKeep\cpk-YYYYMMDD.log` |
| Windows 帐号级 | 各帐号数据目录内 `cpk-YYYYMMDD.log`（托盘菜单「打开数据目录」直达） |
| CLI | `data/<账号>/logs/cpk-YYYYMMDD.log` + `docker logs -f <容器>`（stdout 镜像） |

行格式：`HH:mm:ss.SSS [pid] [slot=N|sys] [level] msg`（双端一致；`[pid]`
区分多实例）。

## 级别表

| 级别 | 含义 | 排查价值 |
| :--- | :--- | :--- |
| `click` | 保活动作 | 点击的目标元素描述 + 命中理由（如 `confirm(重连) -> button.van-dialog__confirm("重连")`） |
| `nav` | 导航 | 自动进入云机 / 恢复性导航的目标 URL |
| `beat` | 心跳采样 | 每 20 个动作周期记录一次选择器命中全貌（`hits=[...]`），**全 0 = 疑似改版** |
| `miss` | 选择器未命中 | 附弹窗全文与按钮清单——改版分析的第一手材料 |
| `exit` | 云机退出 | 退回首页的时机与原因（`#tabbar` / `.title-bar` 命中） |
| `probe` | 手动 DOM 采样 | 页内 `__CPK_PROBE__` 调试钩子输出当前页面全部 class（`exit` 日志也会自动附带 DOM 采样） |
| `error` | 脚本异常 | 含异常 message 与调用栈头部 |
| `sys` | 窗口/看门狗生命周期 | 启动、隐藏（切换看门狗驱动）、显示、停止、eval 失败 |

## 典型排查流程

1. 搜 `[beat]` 看选择器是否全 0（疑似改版）
2. 搜 `[miss]` 看哪个选择器失效（miss 日志附弹窗按钮清单）
3. 结合 `exit` 日志自动附带的 DOM class 清单，修正
   [`shared/keepalive.inject.js`](../keepalive.inject.js) 中的选择器
   （双平台同时生效，见 [keepalive-rules.md](keepalive-rules.md)）

日志不记录任何帐号凭证，只含页面结构与保活动作。

## 版本专属排查

- **CLI 版**：`healthz` 字段判读、实时画面流（`/stream.mjpg`）判读 →
  [../../cli/docs/diagnostics.md](../../cli/docs/diagnostics.md)
- **Windows 版**：常见现象（红横幅/WebView2 缺失/页面空白等） →
  [../../src-tauri/docs/diagnostics.md](../../src-tauri/docs/diagnostics.md)
