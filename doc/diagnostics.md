# 诊断日志与排查

云手机网站改版导致保活失效时，通过日志快速定位。日志按天滚动、自动保留 7 天。

## 日志位置

| 平台 | 位置 |
| :--- | :--- |
| Windows 程序级 | 数据根目录 `AppData\LocalLow\CloudPhoneKeep\cpk-YYYYMMDD.log` |
| Windows 帐号级 | 各帐号数据目录内 `cpk-YYYYMMDD.log`（托盘菜单「打开数据目录」直达） |
| Linux | `data/<账号>/logs/cpk-YYYYMMDD.log` + `docker logs -f <容器>`（stdout 镜像） |

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
   [`shared/keepalive.inject.js`](../shared/keepalive.inject.js) 中的选择器
   （双平台同时生效，见 [keepalive-rules.md](keepalive-rules.md)）

日志不记录任何帐号凭证，只含页面结构与保活动作。

## Linux healthz（容器健康检查）

```bash
curl http://127.0.0.1:8088/healthz
# {"browser": "running", "page": "ok", "ticks": 42, "clicks": 3, ...,
#   "restarts": 0, "reloads": 1, "dialogs": 2, "lastBeatAge": 1,
#   "pageUrl": "...", "exited": false }
```

- `ticks` 持续增长 = 保活看门狗在跑；`clicks` = 已执行的保活点击数
- `restarts` / `reloads` = 分级恢复次数（偶发正常；频繁增长说明站点改版，看日志）
- `page=nav-error` 且 `pageUrl=chrome-error://chromewebdata/` = **首页导航失败**
  （网络/DNS/站点不可达）：注入脚本在错误页上照常 tick，所以 `ticks` 正常、
  心跳新鲜——看 `lastError` 里的 DNS/TCP 探测结论；引擎在退避自动重试
  （5s→60s），网络恢复后自动回到首页。路由器上最常见根因是容器 DNS 不通，
  `docker run` 加 `--dns 223.5.5.5`。控制页对应现象：画面全白 + 红色
  「导航失败」徽标（与实时画面链路无关，`/stream.mjpg` 本身正常）
- 实时画面流（`/stream.mjpg`）判读：连接应为亚秒级（订阅即时应答 + 首帧
  截图兜底）；**静态页面顶栏 0-1 fps 属正常**——合成器无更新即无新帧，
  服务端每 2s 心跳重发上一帧保连接，不是卡顿。页面有动画/视频时帧率
  才会上去（受弱机 JPEG 编码能力限制）。频繁闪「连接实时画面…」=
  流被反复重建：查日志里的 `WS:` 传输错误（CDP 会话在重建）或
  `首帧 30s 未至`（引擎极端繁忙）
- 镜像内置 `HEALTHCHECK`（60s 一次）：浏览器存活 + 心跳不超龄 + 未退出云机，
  任一不满足 → 503 → unhealthy

## Windows 常见现象

| 现象 | 原因与处理 |
| :--- | :--- |
| 点「进入」弹红色横幅 | 横幅里就是具体原因。常见：老板键 Ctrl+N 被占用（窗口仍会打开，仅无老板键并弹系统通知）；数据目录被占用（自动换新目录重试，日志有记录） |
| 提示缺少 WebView2 | 程序自动打开微软官方下载页，装「Evergreen 独立安装包」后重开 |
| 页面空白/加载不出来 | 15 秒后页面底部出现黄色重试条，帐号日志有对应 `[error]` 记录 |
| 窗口最小化后保好像停了 | v1.11.0 起最小化与隐藏同等由看门狗驱动（`[sys]` 日志有「看门狗已接管」记录）；旧版本请升级 |
