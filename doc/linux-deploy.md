# Linux / Docker 部署

一个容器 = 一个账号。镜像：`ghcr.io/xaxka/cloudphonekeep`（**多架构**：
`linux/amd64` + `linux/arm64` 单一 manifest，x86 服务器与 ARM 主机/Apple
Silicon 均原生运行，`docker pull` 自动选架构）。

## 快速开始

```bash
# 1. 拉镜像（本地无 Rust 也可 docker compose build）
docker pull ghcr.io/xaxka/cloudphonekeep:latest

# 2. 数据目录（每账号一个）
mkdir -p data/138xxxx1234

# 3. 启动
docker run -d --name cpk-138xxxx1234 \
  -v $PWD/data/138xxxx1234:/data \
  -p 127.0.0.1:8088:8088 \
  -e CPK_PLATFORM=mobile \
  -e CPK_ACCOUNT=138xxxx1234 \
  --shm-size 128m --init --restart unless-stopped \
  ghcr.io/xaxka/cloudphonekeep:latest

# 4. 首次登录（浏览器打开控制页，点截图=触摸）
#    http://127.0.0.1:8088/
#    登录一次后 Cookie/LocalStorage 持久化在 volume，之后自动保活

# 5. 观察健康状态
curl http://127.0.0.1:8088/healthz
docker logs -f cpk-138xxxx1234     # 诊断日志实时镜像
```

多账号用 `linux/docker-compose.yml` 复制服务块即可（端口 8088、8089…
递增，宿主端口可自由改）。

## 镜像构成与多架构

| 层 | 内容 |
| :--- | :--- |
| 基础 | `alpine:3.21`（musl，无 Node / X11 / VNC） |
| 浏览器 | Alpine community 官方 `chromium` 包 + 中文字体 `font-wqy-zenhei` |
| 引擎 | Rust musl **静态二进制**（~3MB，仅依赖 serde_json；手写 RFC6455 WebSocket + CDP 客户端） |

**构建即编译**：多阶段构建（`rust:1-alpine` 交叉编译 → Alpine 运行层），
在容器内完成，本地无需 Rust 工具链。多架构实现：

- Rust 构建阶段固定在 `$BUILDPLATFORM` 原生运行（CI 上 amd64 全速编译），
  按目标架构选 target 三元组，用 rustc 自带 `rust-lld` 交叉链接（纯 Rust +
  musl 自包含 libc，无需目标架构 gcc）
- QEMU 仅用于 arm64 运行层的 `apk add`（Rust 编译不吃模拟开销）
- CI 冒烟在 amd64 原生跑；arm64 层与 amd64 共享 GHA 层缓存

单账号容器典型 RSS ≈ Rust 引擎 ~10MB + Chromium 250-450MB（主要由云手机
页面与 WebRTC 视频流决定）。WebRTC 走软件编解码（容器无 GPU），
`--autoplay-policy=no-user-gesture-required` 确保视频流自动播放。

## 首次登录与控制页

浏览器打开 `http://127.0.0.1:8088/`：页面即云手机画面的实时截图，点击 =
触摸（CDP `Input.dispatchTouchEvent`，内核级注入）。首次登录在控制页完成；
之后 Cookie/LocalStorage 持久化在 `/data` volume，重启免登录。

- 复杂调试可 `docker run` 加 `-e CPK_CDP_PORT=9222 -p 127.0.0.1:9222:9222`，
  桌面 Chrome 打开 `chrome://inspect` 直连（仅本机调试，公网勿开）
- 公网暴露控制页时务必设置 `CPK_CONTROL_TOKEN`

## 自动恢复分级（与 Windows 版同思路）

1. tick 失败 / 状态冻结 / 脚本缺失 → **页面导航回首页**（站点自身重定向兜底）
2. 传输断裂 / 页面级恢复 10 分钟 3 次无效 → **重建 CDP 会话**（Chromium 进程
   保留、页面状态不丢）
3. 重连无效 / Chromium 退出 / 心跳超龄 180s → **重启 Chromium**（指数退避
   5s→300s，防崩溃循环）

Profile 持久化 + 上述分级，容器层面再叠 `restart: unless-stopped`，三层自愈。

## CI（GitHub Actions）

推送 `main` 后 CI 自动（`.github/workflows/ci.yml` 的 `linux` job）：

1. `cargo test`（单元测试：协议编解码/脚本生成/配置/日志/HTTP 服务）
2. `CPK_SELFTEST=1` 无浏览器自检
3. Docker 构建镜像 → **容器内真实 Chromium 冒烟 60 秒**（WS 握手/CDP 注入/
   看门狗/心跳指标验证后按指标退出）
4. 多架构（amd64 + arm64）推送 `ghcr.io/xaxka/cloudphonekeep`
   （`:latest` 与 commit SHA 双标签）

> GHCR 包首次创建后默认私有：GitHub → Packages → cloudphonekeep → Settings
> 改 Public，或 pull 前 `docker login ghcr.io`。

## FAQ

**Q: 首次登录后重启容器还要登录吗？**
不用。Cookie/LocalStorage 都在 `/data` volume 的 Profile 里，跨重启持久。

**Q: WebRTC 视频流能出画面吗？**
能。Chromium headless 完整保留 WebRTC 栈，容器内走软件编解码。保活只要求
流建立不断开，不要求渲染出画面——画面随时可在控制页截图观察。

**Q: 内存还是高？**
主要由云手机页面本身决定。可加 `mem_limit`、
`CPK_EXTRA_CHROME_ARGS="--js-flags=--max-old-space-size=512"` 压制 V8 堆。

**Q: Windows 版会被影响吗？**
保活脚本 `shared/keepalive.inject.js` 两平台共用（改规则一处生效）；除此之外
Linux 版全部文件在 `linux/` 目录，Windows 版其余代码独立，CI 双 job 分别
验证。推送 GHCR / Release 的发布流程互不影响。
