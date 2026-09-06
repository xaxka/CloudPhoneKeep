# CloudPhoneKeep Linux / Docker 版

> 移动云手机 / 联通云手机保活的 **Linux 无头部署**版本 —— **一个 Docker 容器 = 一个账号**，
> 用 **chrome-headless-shell（Google Chrome for Testing 官方预编译）+ CDP** 替代 Windows 版的 WebView2，
> 后端为 **纯 Rust 引擎**（musl 静态编译，零 Node / 零 npm 依赖，常驻内存约 10MB），
> 保活逻辑与 Windows 版**同源共用**（`shared/keepalive.inject.js`，改一处双端生效）。

**镜像多架构**：`ghcr.io/xaxka/cloudphonekeep` 同时提供 `linux/amd64` 与
`linux/arm64`——x86 服务器与 ARM 主机 / Apple Silicon 均原生运行，
`docker pull` 自动选择架构（无需 `--platform`，也不会有平台不匹配告警）。

详细文档见 [`doc/`](../doc/README.md)：[部署运维](../doc/linux-deploy.md) ·
[保活规则](../doc/keepalive-rules.md) · [配置参考](../doc/configuration.md) ·
[排查指南](../doc/diagnostics.md) · [架构](../doc/architecture.md)。

## 为什么内存占用低

| 组件 | 选择 | 理由 |
| :--- | :--- | :--- |
| 宿主引擎 | Rust musl 静态二进制（~3MB，仅依赖 serde_json） | 替代 Node/脚本运行时（后者常驻 50-80MB）；单引擎线程 + 手写 RFC6455 WebSocket 客户端，无任何重型框架 |
| 浏览器 | chrome-headless-shell（Chrome for Testing 官方预编译：amd64=stable 152，arm64=beta 154——CfT 自 153 起才有 linux-arm64） | 只含渲染内核，无 Chrome UI/标签页/扩展，单页面场景比完整 Chrome 省 100MB+；构建阶段保持 rust:1-alpine musl 静态编译不变 |
| 镜像 | debian:bookworm-slim + 最小运行时库（headless-shell 为 glibc 构建） | 无 Node、无桌面、无 X11、无 VNC、无 Redis、无 SFU；CfT locale 仅留 en/zh、microhei 中文字体、apt/dpkg 元数据零残留 |

单账号容器典型 RSS ≈ **Rust 引擎 ~10MB + headless-shell 250-450MB**（主要由云手机页面与 WebRTC 视频流决定，与 Windows WebView2 同量级）。WebRTC 走软件编解码（容器无 GPU），`--autoplay-policy=no-user-gesture-required` 确保视频流自动播放。

**镜像体积**（amd64，`docker images` DISK USAGE 口径）：**579MB → 约 490MB**。瘦身项：CfT locale 资源仅留 en-US/zh-CN（-43MB；locale .pak 只是 Chrome 自身 UI 文案，页面渲染只依赖字体）、hyphen-data 删除、中文字体 `fonts-wqy-zenhei` → `fonts-wqy-microhei`（-11MB）、dpkg path-exclude 不落盘 doc/man/翻译、apt 清理补上 `/var/cache/apt`（此前 `pkgcache.bin` ~25MB 残留层内）。剩余大头是 chrome-headless-shell 二进制本体（195MB，官方 glibc 预编译，不可再压；UPX 可再省 ~100MB，但启动内存与稳定性代价不适合 7×24 保活，未采用）。

## 与 Windows 版的关系

| | Windows 版 | Linux 版 |
| :--- | :--- | :--- |
| 内核 | WebView2（Edge） | chrome-headless-shell（CfT 官方预编译，双架构） |
| 宿主引擎 | Rust（Tauri 窗口 + 看门狗） | Rust（CDP 客户端 + 看门狗） |
| 驱动 | 窗口可见时页内定时器；隐藏时 Rust 看门狗 eval 驱动 | Rust 看门狗每秒经 CDP 调 `__CPK_TICK__()`（同一模型的无头恒定态） |
| 多账号 | 多窗口多槽位（单进程） | 多容器（一容器一账号） |
| 首次登录 | 直接在窗口里点 | 浏览器打开控制页：全屏实时画面（手机端点底部 iOS 圆点弹抽屉控制台），触摸完全跟手：按下/移动/抬起实时注入，轻点自动合成 click、按住＝长按（或外部 DevTools） |
| 保活脚本 | `shared/keepalive.inject.js`（`include_str!` 内嵌） | 同一本 `shared/keepalive.inject.js`（`include_str!` 内嵌） |
| 数据位置 | `AppData\LocalLow\CloudPhoneKeep` | `/data`（volume 持久化 Profile + 日志） |

保活脚本唯一源文件、平台差异收敛、分级恢复等说明见 [doc/architecture.md](../doc/architecture.md) 与 [doc/keepalive-rules.md](../doc/keepalive-rules.md)。

## 快速开始

```bash
# 1. 启动
docker run -d --name cpk \
  -v cpk:/data \
  -p 127.0.0.1:8088:8088 \
  -e CPK_PLATFORM=mobile \
  -e CPK_ACCOUNT=138xxxx1234 \
  --shm-size 128m --init --restart unless-stopped \
  ghcr.io/xaxka/cloudphonekeep:latest

# 2. 首次登录（浏览器打开控制页：全屏实时画面 + 圆点抽屉/侧栏，
#    触摸跟手（按下/移动/抬起实时注入），面板输入文本/按键）
#    http://127.0.0.1:8088/
#    登录一次后 Cookie/LocalStorage 持久化在 volume，之后自动保活

# 3. 观察健康状态
curl http://127.0.0.1:8088/healthz
docker logs -f cpk     # 诊断日志实时镜像
#    page=nav-error / pageUrl=chrome-error:// = 首页打不开（网络/DNS），
#    引擎退避自动重试并探测根因写 lastError；路由器常见容器 DNS 不通
#    （宿主 resolv.conf 指向本机 dnsmasq），加 --dns 223.5.5.5 即可
```

多账号：`docker run` 换容器名/`-v` 目录/`-p` 宿主端口（8089、8090…），或用本目录
`docker-compose.yml` 复制服务块。常用环境变量见 [doc/configuration.md](../doc/configuration.md)：
`CPK_PLATFORM` / `CPK_ACCOUNT` / `CPK_URL`；安全相关 `CPK_CONTROL_TOKEN`（公网可达时务必
设置）、`CPK_EXTRA_CHROME_ARGS`（低内存调优）。

## 目录结构

```
linux/
├── Dockerfile                # 多阶段多架构：rust:1-alpine 交叉编译（rust-lld）→ 运行层 + 无头浏览器
├── docker-compose.yml        # 一账号一服务（含多账号示例）
├── README.md                 # 本文件
├── Cargo.toml / Cargo.lock   # 仅依赖 serde_json
└── src/
    ├── main.rs               # 入口：装配 + 信号 + selftest/smoke 模式
    ├── config.rs             # 环境变量配置（对齐 config.rs 平台预设）
    ├── keepalive.rs          # 注入脚本构建器（include_str! shared/keepalive.inject.js）
    ├── engine.rs             # Chromium 进程/CDP 会话/注入/看门狗/分级恢复/控制 API
    ├── cdp.rs                # CDP 客户端（事件内联处理，alert/confirm 自动应答）
    ├── ws.rs                 # 手写 RFC6455 WebSocket 客户端（含单元测试回环服务器）
    ├── report_server.rs      # 回环上报 + 健康检查 + 极简控制页（纯 std HTTP）
    ├── logger.rs             # 按天滚动日志（对齐 logger.rs，7 天保留）
    └── util.rs               # 时间/SHA-1/base64/urldecode/HTTP GET（纯 std）
```

## 免责声明

与主项目一致：仅供个人学习与研究，请遵守云手机服务商条款；自动保活可能违反
服务商使用政策，账号风险自担；严禁用于任何违法违规用途。
