# CLI 版排查（healthz 与实时流判读）

日志级别表与典型排查流程（双端一致）见
[../../shared/docs/diagnostics.md](../../shared/docs/diagnostics.md)。

## healthz（容器健康检查）

```bash
curl http://127.0.0.1:8088/healthz
# {"browser": "running", "page": "ok", "ticks": 42, "clicks": 3, ...,
#   "restarts": 0, "reloads": 1, "dialogs": 2, "lastBeatAge": 1,
#   "pageUrl": "...", "exited": false }
```

完整字段参考（排障速查）：

| 字段 | 含义 | 判读 |
| :--- | :--- | :--- |
| `ok` | 总健康判定 | `browser=running` 且未退出云机且心跳未超龄；false → HTTP 503 |
| `browser` | 浏览器进程状态 | `running` / `idle`（待机未启动）/ 启动中 |
| `chromeVersion` | 浏览器版本 | 排查版本回归用 |
| `page` | 页面健康 | `ok` / `loading` / `nav-error` / `not-installed` 等 |
| `pageUrl` | 当前页面 URL | `chrome-error://` 前缀 = 导航失败（见下） |
| `title` | 页面标题 | 采样周期回读；确认页面身份 |
| `platform` / `platformLabel` | 当前平台 | `mobile` / `unicom`；空 = 待机待选 |
| `homeUri` / `vw` / `vh` | 平台首页与视口 | 平台切换后即刻更新 |
| `account` | 账号标识 | 数据目录 `profile-<account>` 对应 |
| `version` | 引擎版本 | 与 Windows 版对齐版本号 |
| `ticks` / `clicks` | 看门狗 tick 数 / 已执行保活点击数 | `ticks` 持续增长 = 保活在跑；`clicks` 是弹窗确认/重连计数 |
| `lastAction` | 最近一次保活动作 | 如 `confirm(-)` / `enter-instance` |
| `lastStatus` | 最近页面状态上报 | `alive` / `retry` / `entered` / `exited` / `expired` 等 |
| `lastBeatAge` | 心跳年龄（秒） | 持续增长 > `CPK_BEAT_STALE_SEC`（180）→ 硬重启浏览器 |
| `exited` | 已退出云机 | `true` 需人工重新进入（控制页会发通知） |
| `restarts` / `reloads` | 分级恢复次数 | 偶发正常；频繁增长 = 站点改版嫌疑，看日志 |
| `dialogs` | 自动应答的 alert/confirm 数 | 站点弹窗频繁时的参考计数 |
| `lastError` | 最近错误结论 | 含 DNS/TCP 探测结果（导航失败时最有价值） |
| `fps` / `quality` / `scale` | 三旋钮当前值 | 与控制页「设置」一致；运行时可调（POST 即生效） |
| `tickIdle` | 空闲降频是否生效中 | 见下 |
| `conns` / `maxConns` | 当前并发连接数 / 上限 | 长期贴近 `maxConns` = 有客户端反复重连占满名额（画面流重连风暴/爬虫扫描），可调大 `CPK_MAX_CONNS` 或排查客户端 |

- `ticks` 持续增长 = 保活看门狗在跑；`clicks` = 已执行的保活点击数
- `tickIdle` = `true` 表示空闲降频生效中（无人观看且无操作 ≥
  `CPK_IDLE_AFTER_SEC`，tick 降为 `CPK_IDLE_TICK_SEC`，保活语义零变化；
  任一操作/打开画面流 ≤200ms 恢复活跃节奏，详见
  [deploy.md](deploy.md)「空闲 CPU 的治理」）
- `restarts` / `reloads` = 分级恢复次数（偶发正常；频繁增长说明站点改版，看日志）
- `page=nav-error` 且 `pageUrl=chrome-error://chromewebdata/` = **首页导航失败**
  （网络/DNS/站点不可达）：注入脚本在错误页上照常 tick，所以 `ticks` 正常、
  心跳新鲜——看 `lastError` 里的 DNS/TCP 探测结论；引擎在退避自动重试
  （5s→60s），网络恢复后自动回到首页。路由器上最常见根因是容器 DNS 不通，
  `docker run` 加 `--dns 223.5.5.5`。控制页对应现象：画面全白 + 红色
  「导航失败」徽标（与实时画面链路无关，`/stream.mjpg` 本身正常）
- 镜像内置 `HEALTHCHECK`（60s 一次）：浏览器存活 + 心跳不超龄 + 未退出云机，
  任一不满足 → 503 → unhealthy

## 实时画面流判读

实时画面流（`/stream.mjpg`）判读：连接应为亚秒级（订阅即时应答 + 首帧
截图兜底）；**静态页面 0-1 fps 属正常**——合成器无更新即无新帧，
服务端每 2s 心跳重发上一帧保连接，不是卡顿。页面有动画/视频时帧率应显著
上去（帧信箱丢旧保新 + screencast ack 已修复，本地实测动画页 60fps；
弱机/弱网下由 CPU 编码与带宽决定，通常 5-25fps；控制台「设置→帧率」或
`/healthz` 的 `fps` 字段是当前上限，被限到 5 则 5fps 是预期而非故障）。
若动画页仍长期 1-2fps 且未限帧：查引擎是否陷在长采样（日志「慢调用诊断」）。
频繁闪「连接实时画面…」= 流被反复重建：查日志里的 `WS:` 传输错误
（CDP 会话在重建）、`生产侧心跳丢失`（正常重连，随会话重建）或
`首帧 30s 未至`（引擎极端繁忙）。输入点击无响应时：控制页会节流提示
「输入通道异常」——引擎重建期间的快速失败是预期行为（毫秒级反馈代替
旧版 20s 挂起），恢复后自动可用
