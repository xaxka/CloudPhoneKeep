# CLI 版配置（环境变量，Docker 与裸机通用）

CLI 版全部配置走环境变量；Windows 版配置见
[../../src-tauri/docs/configuration.md](../../src-tauri/docs/configuration.md)。

| 变量 | 默认 | 说明 |
| :--- | :--- | :--- |
| `CPK_ACCOUNT` | `1` | 账号名（数据目录名、日志标识）；单账号可不设，多账号建议用手机号区分 |
| `CPK_PLATFORM` | 空（待机待选） | 启动平台：`mobile`（移动云手机）/ `unicom`（联通云手机）；留空则控制页「设置→平台」选择后加载；与 `CPK_URL` 同设时 URL 优先 |
| `CPK_URL` | 平台默认 | 覆盖云手机入口 URL（调试/私有部署用） |
| `CPK_WIDTH` / `CPK_HEIGHT` | 414×896（mobile） | 窗口分辨率 |
| `CPK_KEEP_ALIVE` | `1` | 保活总开关 |
| `CPK_INTERVAL_MS` | `5000` | actionTick 动作周期（1000-600000ms） |
| `CPK_SIMULATE_ACTIVITY` | `1` | 空闲鼠标活动模拟 |
| `CPK_BLOCK_CONTEXT_MENU` | `1` | 屏蔽页面右键菜单 |
| `CPK_PAGE_TIMER` | `0` | 页内 setInterval 驱动（默认关：宿主 CDP 看门狗驱动） |
| `CPK_REPORT_PORT` | `8088` | 回环上报/控制页端口（0=自动） |
| `CPK_BIND` | `127.0.0.1`（镜像内 ENV 固定 `0.0.0.0`） | 上述端口绑定地址；要从其他机器访问控制页设 `0.0.0.0`（务必同时设鉴权：`CPK_AUTH_USER`/`CPK_AUTH_PASS` 或 `CPK_CONTROL_TOKEN`） |
| `CPK_CONTROL_TOKEN` | 空 | 控制页/截图/触摸端点的访问令牌（强烈建议公网可达时设置） |
| `CPK_AUTH_USER` / `CPK_AUTH_PASS` | 空 | HTTP Basic Auth（两者同时非空才启用）：控制页/画面流/控制端点弹账号密码登录框；`/healthz` `/status` `/report` `/log` 保持开放（探活与页内脚本上报通道）。与 `CPK_CONTROL_TOKEN` 可叠加（先过 Basic 再过 token） |
| `CPK_CDP_PORT` | `0` | Chromium DevTools 固定端口（0=自动分配；固定端口可用于外部 DevTools） |
| `CPK_CHROME_BIN` | `chrome-headless-shell`（按 PATH 查找；镜像内 ENV 设为绝对路径 `/opt/chrome-headless-shell/chrome-headless-shell`） | 浏览器二进制路径（调试时可指向其他 Chrome） |
| `CPK_NO_SANDBOX` | `1` | 容器内通常需关闭 Chromium 沙箱 |
| `CPK_UA_MODE` | `mobile` | UA 策略：`mobile`（默认，Android Chrome + 手机布局）/ `windows` 伪装 Windows Chrome（旧部署兼容）/ `auto` 去 Headless 字样 / `none` 原样 |
| `CPK_LANG` | `zh-CN` | Chromium UI 语言 |
| `TZ` | `Asia/Shanghai` | 时区（日志时间戳 + Chromium） |
| `CPK_DATA_DIR` | 未设时：`/data` 存在则用 `/data`（Docker），否则 `~/.local/share/cloudphonekeep`（裸机）；镜像内 ENV 固定为 `/data` | 数据根目录（Profile + 日志） |
| `CPK_EXTRA_CHROME_ARGS` | 空 | 透传给 Chromium 的额外参数（如 `--js-flags=--max-old-space-size=512` 压 V8 堆） |
| `CPK_TICK_FAIL_RELOAD` | `10` | tick 连续失败 N 次后导航回首页 |
| `CPK_FROZEN_RELOAD` | `3` | 状态冻结 N 个采样周期后导航回首页 |
| `CPK_BEAT_STALE_SEC` | `180` | 心跳超龄 N 秒硬重启浏览器 |
| `CPK_IDLE_AFTER_SEC` | `60` | 空闲判定：无画面订阅且距最近操作 N 秒后进入空闲降频（0-3600；`0` = 关闭空闲降频）。空闲态 tick 由 1s 降至 `CPK_IDLE_TICK_SEC`、采样降为 3 倍动作周期，保活语义零变化；任一操作/打开画面流 ≤200ms 恢复活跃节奏 |
| `CPK_IDLE_TICK_SEC` | `5` | 空闲态 tick 周期（1-60）。配置会让「页面级恢复」慢于「心跳硬重启」时引擎自动否决降频维持活跃节奏 |
| `CPK_FPS` | `10` | 实时画面帧率上限（1-60；控制台「设置→帧率」可运行时调整，此为初始值。默认 10：云机页面内容变化率普遍 5-10fps，已足额；引擎按目标帧率对 Chrome 端采集/编码做门控节流（ack 确认一帧才采下一帧），传输 CPU 大致正比帧率） |
| `CPK_JPEG_QUALITY` | `50` | 实时画面 JPEG 质量（10-90；控制台「设置→画质」可运行时调整）：质量越高编码 CPU 与带宽越大，弱机优先降 |
| `CPK_STREAM_SCALE` | `100` | 采集分辨率百分比（30-100；控制台「设置→分辨率」可运行时调整）：<100 时 Chrome 编码前先缩小，编码 CPU 与带宽按像素数近线性下降（如 75 ≈ 省 44%），触摸坐标不受影响。弱机推荐 75-90 |
| `CPK_SELFTEST` | `0` | 自检模式（不启动 Chromium，CI 用） |
| `CPK_SMOKE` / `CPK_SMOKE_SECONDS` | `0` / `60` | 冒烟模式（跑 N 秒按指标退出，CI 用） |

> 数值类变量超范围会被自动钳回边界并落 `[error]` 日志。
>
> 平台（移动/联通）不在环境变量设置：启动后平台留空（引擎待机、不加载页面、无弹窗），
> 控制页「设置→平台」选择后才启动并加载；引擎恒无头（镜像固定
> chrome-headless-shell，本身就无头；换用完整 Chromium 的极少数场景经
> `CPK_EXTRA_CHROME_ARGS` 自行追加 `--headless=new`）。

空闲降频的判定与恢复机制详见 [deploy.md](deploy.md)「空闲 CPU 的治理」；
部署示例与容器编排见 [deploy.md](deploy.md)。
