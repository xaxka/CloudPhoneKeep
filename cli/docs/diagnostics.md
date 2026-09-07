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
