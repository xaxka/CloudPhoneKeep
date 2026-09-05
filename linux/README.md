# CloudPhoneKeep Linux / Docker 版

> 移动云手机 / 联通云手机保活的 **Linux 无头部署**版本 —— **一个 Docker 容器 = 一个账号**，
> 用 **Alpine Chromium（headless）+ CDP** 替代 Windows 版的 WebView2，
> 后端为 **纯 Rust 引擎**（musl 静态编译，零 Node / 零 npm 依赖，常驻内存约 10MB），
> 保活逻辑与 Windows 版**逐行同源**（自动进入云机、断线重连、弹窗处理、
> actionTick / stopCheck 双定时器、JS 注入、触摸模拟）。

Windows 版（Tauri + WebView2 便携 exe）见仓库根目录，功能与发布流程**不受本目录影响**。

## 为什么内存占用低

| 组件 | 选择 | 理由 |
| :--- | :--- | :--- |
| 宿主引擎 | Rust musl 静态二进制（~3MB，仅依赖 serde_json） | 替代 Node/脚本运行时（后者常驻 50-80MB）；单引擎线程 + 手写 RFC6455 WebSocket 客户端，无任何重型框架 |
| 浏览器 | Alpine 发行版 Chromium（community 官方包，`--headless=new`） | 原生 musl 构建与 APK 依赖一体管理，无 glibc 兼容层；单页面无头场景不启动 UI 相关进程 |
| 镜像 | alpine:3.21 + 最小运行时库 | 无 Node、无桌面、无 X11、无 VNC、无 Redis、无 SFU；musl 本身也比 glibc 更省内存 |

单账号容器典型 RSS ≈ **Rust 引擎 ~10MB + Chromium 250-450MB**（主要由云手机页面与 WebRTC 视频流决定，与 Windows WebView2 同量级）。WebRTC 走软件编解码（容器无 GPU），`--autoplay-policy=no-user-gesture-required` 确保视频流自动播放。

## 与 Windows 版的关系

| | Windows 版 | Linux 版 |
| :--- | :--- | :--- |
| 内核 | WebView2（Edge） | Alpine Chromium headless |
| 宿主引擎 | Rust（Tauri 窗口 + 看门狗） | Rust（CDP 客户端 + 看门狗） |
| 驱动 | 窗口可见时页内定时器；隐藏时 Rust 看门狗 eval 驱动 | Rust 看门狗每秒经 CDP 调 `__CPK_TICK__()`（同一模型的无头恒定态） |
| 多账号 | 多窗口多槽位（单进程） | 多容器（一容器一账号） |
| 首次登录 | 直接在窗口里点 | 浏览器打开控制页：截图 + 触摸/输入（或外部 DevTools） |
| 保活脚本 | `src-tauri/src/keepalive.rs` 生成 | `src/keepalive.inject.js`（同逻辑移植，`include_str!` 内嵌进二进制） |
| 数据位置 | `AppData\LocalLow\CloudPhoneKeep` | `/data`（volume 持久化 Profile + 日志） |

移植差异只有两处（都标注在 `keepalive.inject.js` 文件头）：

1. `CFG.pageTimer` 默认 `false`：页内 setInterval 关闭，由 Rust 看门狗每 1 秒经 CDP
   驱动 `__CPK_TICK__()` —— 与 Windows「窗口隐藏时由 Rust 看门狗驱动」完全同一
   模型（无头页面永远不可见，恒由外部驱动；避免双驱动把 5 秒周期缩短一半）。
2. `window.__CPK_DRAIN__()` 诊断环形缓冲：headless 下页面 `fetch http://127.0.0.1`
   的 /log 上报可能被混合内容/专用网络访问策略拦截，宿主每 5 秒经 CDP 直接取走
   缓冲，保证诊断日志任何网络策略下不丢（原 /log 上报保留，双保险）。

## 快速开始

```bash
# 1. 拉镜像（首次推送后 GHCR 可用；本地无 Rust 也可 docker compose build）
docker pull ghcr.io/xaxka/cloudphonekeep:latest

# 2. 数据目录（每账号一个）
mkdir -p data/138xxxx1234

# 3. 启动
docker run -d --name cpk-138xxxx1234 \
  -v $PWD/data/138xxxx1234:/data \
  -p 127.0.0.1:18080:8080 \
  -e CPK_PLATFORM=mobile \
  -e CPK_ACCOUNT=138xxxx1234 \
  --shm-size 128m --init --restart unless-stopped \
  ghcr.io/xaxka/cloudphonekeep:latest

# 4. 首次登录（浏览器打开控制页，点截图=触摸）
#    http://127.0.0.1:18080/
#    登录一次后 Cookie/LocalStorage 持久化在 volume，之后自动保活

# 5. 观察健康状态
curl http://127.0.0.1:18080/healthz
docker logs -f cpk-138xxxx1234     # 诊断日志实时镜像
```

多账号用 `docker-compose.yml`（本目录）复制服务块即可。

## 环境变量

| 变量 | 默认 | 说明 |
| :--- | :--- | :--- |
| `CPK_PLATFORM` | `mobile` | `mobile` 移动云手机 / `unicom` 联通云手机 |
| `CPK_ACCOUNT` | `account1` | 账号名（仅用于 Profile 目录名与日志标识，建议填手机号） |
| `CPK_URL` | 平台默认入口 | 覆盖云手机 H5 入口 URL |
| `CPK_WIDTH` / `CPK_HEIGHT` | 414×896（移动）/ 405×720（联通） | 视口分辨率（对齐 Windows 版预设） |
| `CPK_DATA_DIR` | `/data` | 数据根目录（Profile + 日志） |
| `CPK_PROFILE_DIR` | `{data}/profile-{account}` | Chromium Profile 目录（Cookie/LocalStorage 持久化点） |
| `CPK_INTERVAL_MS` | `5000` | actionTick 动作周期（对齐 Windows 版 runTimer） |
| `CPK_KEEP_ALIVE` | `1` | 保活总开关 |
| `CPK_SIMULATE_ACTIVITY` | `1` | 空闲鼠标活动模拟 |
| `CPK_PAGE_TIMER` | `0` | 页内 setInterval 驱动（默认关：由宿主看门狗经 CDP 驱动） |
| `CPK_REPORT_PORT` | `8080` | 回环上报/控制页端口（0=自动） |
| `CPK_BIND` | `0.0.0.0` | 上述端口绑定地址（`127.0.0.1` 最保守；默认配合端口映射/防火墙） |
| `CPK_CONTROL_TOKEN` | 空 | 控制页/截图/触摸端点的访问令牌（强烈建议公网可达时设置） |
| `CPK_CDP_PORT` | `0` | Chromium DevTools 固定端口（0=自动分配；固定端口可用于外部 DevTools） |
| `CPK_CHROME_BIN` | `chromium-browser` | Chromium 二进制路径（镜像内为 Alpine chromium；调试时可指向其他 Chrome） |
| `CPK_HEADLESS` | `1` | 完整 Chromium 需 `--headless=new`（镜像默认开；chrome-headless-shell 类二进制无需） |
| `CPK_NO_SANDBOX` | `1` | 容器内通常需关闭 Chromium 沙箱 |
| `CPK_UA_MODE` | `windows` | UA 策略：`windows` 伪装 Windows Chrome（对齐 Windows 版环境） / `auto` 去 Headless 字样 / `none` 原样 |
| `CPK_LANG` | `zh-CN` | Chromium UI 语言 |
| `TZ` | `Asia/Shanghai` | 时区（日志时间戳 + Chromium） |
| `CPK_EXTRA_CHROME_ARGS` | 空 | 追加 Chromium 参数（空格分隔，如 `--js-flags=--max-old-space-size=512`） |
| `CPK_TICK_FAIL_RELOAD` | `10` | tick 连续失败 N 次触发页面导航恢复 |
| `CPK_FROZEN_RELOAD` | `3` | 状态冻结 N 个采样周期（×5s）触发页面导航恢复 |
| `CPK_BEAT_STALE_SEC` | `180` | 心跳丢失 N 秒触发浏览器硬重启 |
| `CPK_SELFTEST` | `0` | 自检模式（CI 用）：验证配置/脚本生成/回环服务后退出 |
| `CPK_SMOKE` / `CPK_SMOKE_SECONDS` | `0` / `60` | 冒烟模式：真实跑 N 秒后按指标退出（CI 用） |

## 首次登录（控制页）

无桌面环境，首次登录用浏览器打开控制页 `http://<host>:<映射端口>/`：

- **截图即触摸**：点截图上的任意位置 = 一次真实 `TouchEvent`（CDP `Input.dispatchTouchEvent`，与真机同源）
- **文本输入**：输入框回车 = `Input.insertText` + Enter
- 快捷按钮：Enter / ⌫ / 刷新 / 回首页
- 移动端验证码页面：点输入框 → 输入手机号 → 收码后输入 → 登录

登录态（Cookie / LocalStorage）持久化在 `/data` volume；容器重启后免登录继续保活。

## 运行状态与诊断

```bash
curl http://127.0.0.1:18080/healthz | jq
# { "ok": true, "browser": "running", "page": "ok", "ticks": 12345, "clicks": 87,
#   "platform": "mobile", "account": "138xxxx1234", "restarts": 0, "reloads": 1,
#   "dialogs": 2, "lastBeatAge": 1, "pageUrl": "...", "exited": false, ... }
```

- `ticks` 持续增长 = 保活看门狗在跑；`clicks` = 已执行的保活点击数
- `restarts` / `reloads` = 分级恢复次数（偶发正常；频繁增长说明站点改版，看日志）
- 日志：`docker logs -f <容器>`（stdout 镜像）或 `data/<账号>/logs/cpk-YYYYMMDD.log`（按天滚动保留 7 天，格式与 Windows 版一致：`HH:mm:ss.SSS [pid] [slot=N|sys] [level] msg`）

### 自动恢复分级（与 Windows 版同思路）

1. tick 失败 / 状态冻结 / 脚本缺失 → **页面导航回首页**（站点自身重定向兜底）
2. 传输断裂 / 页面级恢复 10 分钟 3 次无效 → **重建 CDP 会话**（Chromium 进程保留、页面状态不丢）
3. 重连无效 / Chromium 退出 / 心跳超龄 180s → **重启 Chromium**（指数退避 5s→300s，防崩溃循环）

Profile 持久化 + 上述分级，容器层面再叠 `restart: unless-stopped`，形成三层自愈。

## CI（GitHub Actions）

推送 `main` 后 CI 自动（见 `.github/workflows/ci.yml` 的 `linux` job）：

1. `cargo test`（单元测试：协议编解码/脚本生成/配置/日志/HTTP 服务）
2. `CPK_SELFTEST=1` 无浏览器自检
3. 多阶段 Docker 构建（Rust 编译在容器内完成）
4. **冒烟**：`docker run` 起真实 Chromium 跑 60 秒，验证 CDP 握手/脚本注入/看门狗/心跳指标后退出
5. 推送镜像到 `ghcr.io/xaxka/cloudphonekeep`（`:latest` 与 commit SHA 双标签）

> GHCR 包首次创建后默认私有：在 GitHub → Packages → cloudphonekeep → Settings 里改为 Public，或 pull 前 `docker login ghcr.io`。

## 常见问题

**Q: 首次登录后重启容器还要登录吗？**
不用。Cookie/LocalStorage 都在 `/data` volume 的 Profile 里，跨重启持久。

**Q: WebRTC 视频流能出画面吗？**
能。Chromium headless 完整保留 WebRTC 栈，容器内走软件编解码。保活只要求流建立不断开，不要求渲染出画面——画面随时可在控制页截图观察。

**Q: 内存还是高？**
主要由云手机页面本身决定。可加 `mem_limit`、`CPK_EXTRA_CHROME_ARGS="--js-flags=--max-old-space-size=512"` 压制 V8 堆。

**Q: 怎么像 Windows 版那样直接看/操作页面？**
控制页截图+触摸已覆盖绝大多数操作；复杂调试可 `docker run` 时加 `-e CPK_CDP_PORT=9222 -p 127.0.0.1:9222:9222`，用桌面 Chrome 打开 `chrome://inspect` 连上去（仅建议本机调试，公网勿开）。

**Q: Windows 版会被影响吗？**
不会。Linux 版全部文件在 `linux/` 目录，CI 中 Windows job 原样保留，`src-tauri/` 一行未动。

## 目录结构

```
linux/
├── Dockerfile                # 多阶段：rust:1-alpine musl 静态编译 → alpine:3.21 + chromium 运行
├── docker-compose.yml        # 一账号一服务（含多账号示例）
├── .dockerignore
├── README.md                 # 本文件
├── Cargo.toml                # 仅依赖 serde_json
├── src/
│   ├── main.rs               # 入口：装配 + 信号 + selftest/smoke 模式
│   ├── config.rs             # 环境变量配置（对齐 config.rs 平台预设）
│   ├── keepalive.rs          # 注入脚本构建器（占位符替换，include_str! 内嵌）
│   ├── keepalive.inject.js   # 保活脚本本体（keepalive.rs 逐行移植，含文件头移植差异说明）
│   ├── engine.rs             # Chromium 进程/CDP 会话/注入/看门狗/分级恢复/控制 API
│   ├── cdp.rs                # CDP 客户端（事件内联处理，alert/confirm 自动应答）
│   ├── ws.rs                 # 手写 RFC6455 WebSocket 客户端（含单元测试回环服务器）
│   ├── report_server.rs      # 回环上报 + 健康检查 + 极简控制页（纯 std HTTP）
│   ├── logger.rs             # 按天滚动日志（对齐 logger.rs，7 天保留）
│   └── util.rs               # 时间/SHA-1/base64/urldecode/HTTP GET（纯 std）
```

## 免责声明

与主项目一致：仅供个人学习与研究，请遵守云手机服务商条款；自动保活可能违反
服务商使用政策，账号风险自担；严禁用于任何违法违规用途。
