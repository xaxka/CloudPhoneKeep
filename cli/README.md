# CloudPhoneKeep CLI 版（Linux / Docker / 裸机直跑）

> 移动云手机 / 联通云手机保活的 **Linux 无头部署**版本 —— **一个实例 = 一个账号**，
> 三种部署形态同一 musl 静态二进制：**Docker 容器 / 裸机直跑 / 源码编译**。
> 用 **chrome-headless-shell（Google Chrome for Testing 官方预编译）+ CDP** 替代 Windows 版的 WebView2，
> 后端为 **纯 Rust 引擎**（musl 静态编译，零 Node / 零 npm 依赖，常驻内存约 10MB），
> 保活逻辑与 Windows 版**同源共用**（`shared/` crate：注入脚本模板 + 构建器 +
> 平台预设，改一处双端生效）。

**镜像多架构**：`ghcr.io/xaxka/cloudphonekeep` 同时提供 `linux/amd64` 与
`linux/arm64`——x86 服务器与 ARM 主机 / Apple Silicon 均原生运行，
`docker pull` 自动选择架构（无需 `--platform`，也不会有平台不匹配告警）。

详细文档：[部署运维](docs/deploy.md) · [保活规则](../shared/docs/keepalive-rules.md) ·
[配置参考](docs/configuration.md) · [排查指南](docs/diagnostics.md) · [架构](../shared/docs/architecture.md) ·
[构建与发布](docs/build.md)。

## 为什么内存占用低

| 组件 | 选择 | 理由 |
| :--- | :--- | :--- |
| 宿主引擎 | Rust musl 静态二进制（~3MB，仅依赖 serde_json） | 替代 Node/脚本运行时（后者常驻 50-80MB）；单引擎线程 + 手写 RFC6455 WebSocket 客户端，无任何重型框架 |
| 浏览器 | chrome-headless-shell（Chrome for Testing 官方预编译：amd64=stable 152，arm64=beta 154——CfT 自 153 起才有 linux-arm64） | 只含渲染内核，无 Chrome UI/标签页/扩展，单页面场景比完整 Chrome 省 100MB+；构建阶段保持 rust:1-alpine musl 静态编译不变 |
| 镜像 | debian:bookworm-slim + 最小运行时库（headless-shell 为 glibc 构建） | 无 Node、无桌面、无 X11、无 VNC、无 Redis、无 SFU；CfT locale 仅留 en/zh、microhei 中文字体、apt/dpkg 元数据零残留 |

单账号容器典型 RSS ≈ **Rust 引擎 ~10MB + headless-shell 250-450MB**（主要由云手机页面与 WebRTC 视频流决定，与 Windows WebView2 同量级）。WebRTC 走软件编解码（容器无 GPU），`--autoplay-policy=no-user-gesture-required` 确保视频流自动播放。

**镜像体积**（amd64，`docker images` DISK USAGE 口径）：**579MB → 约 490MB**。瘦身项：CfT locale 资源仅留 en-US/zh-CN（-43MB；locale .pak 只是 Chrome 自身 UI 文案，页面渲染只依赖字体）、hyphen-data 删除、中文字体 `fonts-wqy-zenhei` → `fonts-wqy-microhei`（-11MB）、dpkg path-exclude 不落盘 doc/man/翻译、apt 清理补上 `/var/cache/apt`（此前 `pkgcache.bin` ~25MB 残留层内）。剩余大头是 chrome-headless-shell 二进制本体（195MB，官方 glibc 预编译，不可再压；UPX 可再省 ~100MB，但启动内存与稳定性代价不适合 7×24 保活，未采用）。

## 与 Windows 版的关系

| | Windows 版 | CLI 版（Linux） |
| :--- | :--- | :--- |
| 内核 | WebView2（Edge） | chrome-headless-shell（CfT 官方预编译，双架构） |
| 宿主引擎 | Rust（Tauri 窗口 + 看门狗） | Rust（CDP 客户端 + 看门狗） |
| 驱动 | 窗口可见时页内定时器；隐藏时 Rust 看门狗 eval 驱动 | Rust 看门狗每秒经 CDP 调 `__CPK_TICK__()`（同一模型的无头恒定态） |
| 多账号 | 多窗口多槽位（单进程） | 多容器（一容器一账号） |
| 首次登录 | 直接在窗口里点 | 浏览器打开控制页：全屏实时画面（手机端点底部 iOS 圆点弹抽屉控制台），输入全覆盖：触摸跟手（多点/长按/双指缩放）、鼠标真实点击/滚轮/悬停、物理键盘全键位、输入法与剪贴板双向复制粘贴、运行时帧率限制（或外部 DevTools）；退出/到期状态转换时控制页发系统通知（对齐 Windows 版托盘通知语义） |
| 平台选择 | 登录窗口「移动云手机/联通云手机」下拉选择器 | **启动后平台留空（无弹窗、不加载页面）**：控制页「设置→平台」选移动/联通后引擎才启动并加载页面，可随时切换（换首页/视口/保活选择器并自动重启实例，Profile 保留双平台登录态） |
| 页内地址栏 | Ctrl+U 全局热键呼出 | **无**（按需求不提供；导航走控制页「回首页」/页面内跳转，外部脚本可 `/nav`） |
| 保活脚本 | `shared/keepalive.inject.js`（`include_str!` 内嵌） | 同一本（shared crate 内嵌，构建器统一） |
| 数据位置 | `AppData\LocalLow\CloudPhoneKeep` | Docker：`/data`（volume）；裸机默认 `~/.local/share/cloudphonekeep`（可 `CPK_DATA_DIR` 覆盖） |

保活脚本唯一源文件、平台差异收敛、分级恢复等说明见 [架构](../shared/docs/architecture.md) 与 [保活规则](../shared/docs/keepalive-rules.md)。

## 快速开始

```bash
# 1. 启动（单账号默认即可：CPK_ACCOUNT 默认 1，可不设；
#    多账号才需要设 CPK_ACCOUNT 区分，建议用手机号）
docker run -d --name cpk \
  -v cpk:/data \
  -p 127.0.0.1:8088:8088 \
  --shm-size 128m --init --restart unless-stopped \
  ghcr.io/xaxka/cloudphonekeep:latest

# 2. 首次登录（浏览器打开控制页：启动后平台留空，「设置→平台」选移动/联通
#    后引擎加载页面（约 10 秒）；全屏实时画面 + 圆点抽屉/侧栏，
#    触摸跟手、鼠标真实点击、键盘/剪贴板、帧率限制、平台切换、
#    退出/到期系统通知；无地址栏）
#    http://127.0.0.1:8088/
#    登录一次后 Cookie/LocalStorage 持久化在 volume，之后自动保活

# 3. 观察健康状态
curl http://127.0.0.1:8088/healthz
docker logs -f cpk     # 诊断日志实时镜像
#    page=nav-error / pageUrl=chrome-error:// = 首页打不开（网络/DNS），
#    引擎退避自动重试并探测根因写 lastError；路由器常见容器 DNS 不通
#    （宿主 resolv.conf 指向本机 dnsmasq），加 --dns 223.5.5.5 即可
```

多账号：`docker run` 换容器名/`-v` 目录/`-p` 宿主端口（8089、8090…）并加
`-e CPK_ACCOUNT=手机号` 区分数据目录，或用本目录 `docker-compose.yml` 复制服务块。
常用环境变量见 [配置参考](docs/configuration.md)：`CPK_PLATFORM` / `CPK_URL`；
安全相关 `CPK_AUTH_USER` + `CPK_AUTH_PASS`（Basic Auth，公网可达时建议启用）与
`CPK_CONTROL_TOKEN`（令牌，可与 Basic Auth 叠加）、`CPK_EXTRA_CHROME_ARGS`
（低内存调优）。平台（移动/联通）不在环境变量里设置——启动后留空，控制页
「设置→平台」选择后才加载页面（显式 `CPK_URL` 视为自动启动，供 CI 冒烟/自定义 H5）。

## 裸机直跑（无 Docker）

**一键安装**（推荐）：自动下载本机架构（amd64/arm64）的引擎二进制与
chrome-headless-shell、检查运行库、装完自检（googleapis 下载失败自动换
npmmirror 国内镜像）：

```bash
curl -fsSL https://raw.githubusercontent.com/xaxka/CloudPhoneKeep/main/cli/install.sh | bash
# root 默认装 /opt/cloudphonekeep，普通用户装 ~/.local/cloudphonekeep
# 常用选项（管道方式加在 bash 后）：
#   ... | bash -s -- --systemd    # root：顺带装 systemd 模板单元 cpk@<账号>
#   ... | bash -s -- --chrome-bin /path/to/chrome-headless-shell   # 复用已有浏览器
#   ... | bash -s -- --mirror     # 下载源反转（npmmirror 国内源优先）
#   ... | bash -s -- --dns        # DNS 解析/连通性排障（只诊断不安装）
#   ... | bash -s -- --uninstall  # 卸载
# 之后直接运行：cloudphonekeep（或 bash install.sh --uninstall 卸载）
```

手动安装（想自己控制每一步）：CI 每次推送发布**与镜像同源同构**的 musl 静态
二进制到 `dev` Release：`cloudphonekeep-linux-amd64` / `cloudphonekeep-linux-arm64`
（glibc 机器也能跑，musl 静态自包含 libc）。机器上只需要装好 chrome-headless-shell：

```bash
# 1. 取二进制（或源码编译：见 docs/build.md）
curl -LO https://github.com/xaxka/CloudPhoneKeep/releases/download/dev/cloudphonekeep-linux-amd64
chmod +x cloudphonekeep-linux-amd64

# 2. 安装 chrome-headless-shell（Chrome for Testing 官方下载）：
#    https://googlechromelabs.github.io/chrome-for-testing/
#    解压后把二进制放进 PATH，或用 CPK_CHROME_BIN 指向绝对路径
#    （默认在 PATH 里找 "chrome-headless-shell"）
#    运行库最小集（Debian/Ubuntu；readelf 直连 + LD_DEBUG dlopen 实测，
#    传递依赖由 apt 自动带入，fontconfig/freetype 全程零加载不需要装）：
#    apt install libasound2 libatk-bridge2.0-0 libatk1.0-0 libatspi2.0-0 \
#        libdbus-1-3 libexpat1 libgbm1 libglib2.0-0 libnss3 libudev1 \
#        libx11-6 libxcomposite1 libxdamage1 libxext6 libxfixes3 \
#        libxkbcommon0 libxrandr2
#    中文字体可选（截图可读性）：fonts-wqy-microhei

# 3. 运行（数据目录默认 ~/.local/share/cloudphonekeep，/data 存在时优先用它——
#    与容器行为对齐；Profile/日志都在里面；单账号可不设 CPK_ACCOUNT，
#    多实例时才需要用它区分）
./cloudphonekeep-linux-amd64

# 4. 浏览器打开控制页（与 Docker 版完全一致）
#    http://127.0.0.1:8088/

curl http://127.0.0.1:8088/healthz
```

环境变量与 Docker 版完全一致（[配置参考](docs/configuration.md)）
——`CPK_PLATFORM`/`CPK_URL`/`CPK_FPS`/`CPK_JPEG_QUALITY`/`CPK_STREAM_SCALE`/
`CPK_IDLE_AFTER_SEC` 等；裸机差异只有两点：

- `CPK_CHROME_BIN`：默认按 PATH 查找 `chrome-headless-shell`（容器里是绝对路径）
- 数据目录：默认 `~/.local/share/cloudphonekeep`（容器里是 `/data`）

多实例多账号：不同 `CPK_ACCOUNT` + `CPK_REPORT_PORT`（8088/8089…）各起一个进程，
数据目录按账号自动隔离（`profile-<账号>`）。systemd 常驻（一键安装已带模板，
`systemctl enable --now cpk@138xxxx1234` 启动，多实例端口用
`systemctl edit cpk@<账号>` 追加 `Environment=CPK_REPORT_PORT=8089`）；
或手写单元：

```ini
# /etc/systemd/system/cpk@.service (cpk@138xxxx1234 启动)
[Unit]
Description=CloudPhoneKeep %i
After=network-online.target

[Service]
Environment=CPK_ACCOUNT=%i CPK_REPORT_PORT=8088
ExecStart=/opt/cloudphonekeep/cloudphonekeep-linux-amd64
Restart=always

[Install]
WantedBy=multi-user.target
```

## 目录结构

```
cli/
├── Dockerfile                # 多阶段多架构：rust:1-alpine 交叉编译（rust-lld）→ 运行层 + 无头浏览器
├── docker-compose.yml        # 一账号一服务（含多账号示例）
├── install.sh                # 裸机一键安装（引擎 + chrome-headless-shell + 可选 systemd）
├── README.md                 # 本文件
├── control_page.html         # 控制页模板（CLI 专属；report_server.rs include_str! 内嵌）
├── Cargo.toml                # 依赖 cloudphonekeep-shared（../shared）+ serde_json；锁文件在仓库根
├── docs/                     # CLI 版文档（部署/配置/排查）
├── src/
│   ├── main.rs               # 入口：装配 + 信号 + selftest/smoke 模式
│   ├── config.rs             # 环境变量配置（平台预设再导出自 shared；数据目录裸机自适应）
│   ├── keepalive.rs          # 注入脚本构建器适配层（模板与替换逻辑唯一源在 shared crate）
│   ├── engine.rs             # Chromium 进程/CDP 会话/注入/看门狗/分级恢复/控制 API
│   ├── cdp.rs                # CDP 客户端（事件内联处理，alert/confirm 自动应答）
│   ├── ws.rs                 # 手写 RFC6455 WebSocket 客户端（含单元测试回环服务器）
│   ├── report_server.rs      # 回环上报 + 健康检查 + 极简控制页（纯 std HTTP）
│   ├── logger.rs             # 按天滚动日志（7 天保留）
│   └── util.rs               # 时间/SHA-1/base64/urldecode/HTTP GET（纯 std）
└── tests/                    # 本地复现/回归脚本集（见 tests/README.md）
```

## 免责声明

与主项目一致：仅供个人学习与研究，请遵守云手机服务商条款；自动保活可能违反
服务商使用政策，账号风险自担；严禁用于任何违法违规用途。
