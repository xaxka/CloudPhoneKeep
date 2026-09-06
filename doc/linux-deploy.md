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

# 4. 首次登录（浏览器打开控制页，画面即触屏）
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
| 基础 | `debian:bookworm-slim`（glibc，无 Node / X11 / VNC） |
| 浏览器 | CfT 官方预编译 `chrome-headless-shell`（amd64=stable 152 / arm64=beta 154） + 中文字体 `fonts-wqy-microhei` |
| 引擎 | Rust musl **静态二进制**（~3MB，仅依赖 serde_json；手写 RFC6455 WebSocket + CDP 客户端） |

**构建即编译**：多阶段构建（`rust:1-alpine` musl 静态交叉编译 → debian 运行层下载 CfT headless-shell），
在容器内完成，本地无需 Rust 工具链。多架构实现：

- Rust 构建阶段固定在 `$BUILDPLATFORM` 原生运行（CI 上 amd64 全速编译），
  按目标架构选 target 三元组，用 rustc 自带 `rust-lld` 交叉链接（纯 Rust +
  musl 自包含 libc，无需目标架构 gcc）
- QEMU 仅用于 arm64 运行层的 `apt` 安装与 CfT 下载解压（Rust 编译不吃模拟开销）
- CI 冒烟在 amd64 原生跑；arm64 层与 amd64 共享 GHA 层缓存

单账号容器典型 RSS ≈ Rust 引擎 ~10MB + Chromium 250-450MB（主要由云手机
页面与 WebRTC 视频流决定）。WebRTC 走软件编解码（容器无 GPU），
`--autoplay-policy=no-user-gesture-required` 确保视频流自动播放。

## 首次登录与控制台

浏览器打开 `http://127.0.0.1:8088/`：**全屏实时画面 + 极简操作**。
桌面端右侧是操作栏；**手机等窄屏**点击底部 iOS 风格白色圆点弹出底部抽屉
控制台（再点圆点或点遮罩收起）。状态（页面状态/`N fps`/连接模式）收纳在
控制台顶部状态行，不悬浮在画面上遮挡内容；重连期显示「等帧…」，切后台显示「已暂停」。

画面为 `Page.startScreencast` 合成器帧直推的 MJPEG 流（页面有更新即出帧，
局域网延迟 ≈ 帧间隔；带动画的页面可达 25-60fps，弱机实际由 CPU/网络决定）。
VLC 等标准播放器也可直接打开 `http://<host>:<port>/stream.mjpg` 观看。
**控制台「设置 → 帧率」可运行时限制帧率**（1-60，默认 25；引擎侧重建
screencast 生效，弱机/省流量场景建议 5-10；初始值可用环境变量 `CPK_FPS` 设定）。

**输入全覆盖**（自动模式下桌面鼠标与手机触摸自动分流，也可在「设置 → 触控」
里强制触摸/鼠标）：

| 输入 | 通道 | 说明 |
| --- | --- | --- |
| 触摸拖动/长按/双指缩放 | CDP `Input.dispatchTouchEvent` | 按下/移动/抬起逐点直通（fire 即答）；移动带全部在按触点，双指缩放可用；抬一指手势延续 |
| 鼠标左键点击 | CDP `Input.dispatchMouseEvent` | 真实 mousedown/mouseup/click（非触摸合成，兼容所有页面）；双击/三击由 clickCount 合成 dblclick |
| 鼠标拖动 | 触摸流 | 自动模式：鼠标按住移动超阈值即转触摸拖动（移动页滚动跟手）；「鼠标」模式下为真实 mouseMoved 拖动 |
| 右键/中键 | CDP 鼠标事件 | 右键点击→远程 contextmenu 菜单 |
| 悬停 | CDP 鼠标事件 | 纯移动（无按键）转发为真实 mouseMoved，桌面页 hover 菜单可用 |
| 滚轮 | CDP `mouseWheel` | 画面上滚动即转发（移动页/桌面页均可滚动） |
| 物理键盘全键位 | CDP `Input.dispatchKeyEvent` | keyDown/keyUp 逐键直通（含修饰键/功能键/方向键）；Ctrl+C/X 同步云机选区到本机剪贴板，Ctrl+V 把本机剪贴板粘贴到云机，Ctrl+A 远程全选，F5 远程刷新 |
| 手机输入法（拼音） | 键盘开关 → `Input.insertText` | 控制台「键盘」按钮弹出输入框，IME 组合/输入/粘贴内容即发即转发（清空重打）；桌面端也可直接用 |
| 剪贴板复制/粘贴 | `/clip` + `insertText` | 「复制」读云机选中文本（含输入框选区）写入本机剪贴板；「粘贴」读本机剪贴板插入云机焦点处 |

外部脚本直调端点（token 保护同控制页）：`POST /touch`（`phase` +
`ps=x,y,id;x,y,id` 多点或 `x/y` 单点）、`POST /mouse`（`action=move/down/up/wheel`
+ `b/n/bb/m/dx/dy`）、`POST /kbd`（`t=down/up` + `key/code/vk/text/m/l/r`）、
`POST /type`（整段文本）、`GET /clip`（选区文本）、`POST /fps`（1-60）；
`/tap /swipe /key /nav /reload` 兼容保留。所有输入事件 fire 即发，引擎线程
占用 <0.1ms——导航/重连期间输入不再卡死；引擎重建期间请求毫秒级快速失败
（控制页提示「输入通道异常」而非无响应）。

**切后台/锁屏自动省 CPU**：控制页不可见即断流，引擎最后一个订阅者离开后
自动 `Page.stopScreencast`——无人观看＝零 JPEG 编码开销，CPU 即降（云机页面
本身的运行开销仍在，那是保活语义）；回前台自动重连。关闭标签页同理。

流的连接语义（弱机/慢网络均按此设计，控制台状态行显示 `N fps`）：

- **首帧**：订阅即由引擎补发一张当前截图（静态页/错误页合成器无更新时
  screencast 不发帧，兜底保证打开就有画面），连接建立为亚秒级
- **丢旧保新**：引擎帧信箱只存最新帧，消费慢时跳过中间帧——画面永远最新，
  不排队不积压（宁可跳帧，不延迟）
- **静态页面**：无新帧时每 2s 重发上一帧作心跳——连接保持活性，
  徽标显示 0-1 fps 属正常（画面没变就没有新帧，不是卡顿）
- **断流自愈**：引擎重建/浏览器重启（生产侧心跳丢失即判死）会立即关流，
  页面 1.5s 内自动重连；期间画面保留（不闪全屏「连接实时画面…」），
  徽标提示 `等帧…`
- **极端繁忙**（引擎 8s 未应答订阅）：页面退化为逐帧截图轮询
  （上一张完成才发下一张，弱机不会把引擎通道灌爆），8s 后自动重试实时流

## 自动恢复分级（与 Windows 版同思路）

0. **首页导航失败**（页面停在 `chrome-error://` 网络错误页）→ **退避重导航**
   5s→10s→…→60s 封顶。注入脚本在错误页上照常 tick、`readyState=complete`，
   常规监督项看不见这种故障——引擎靠采样 URL 显式识别（`status` 里
   `page=nav-error`），并异步 DNS/TCP 探测把结论写进 `lastError`
   （区分「容器 DNS 不通 / TCP 不通 / 站点层拒绝」）。此类故障**不升级**重启
   浏览器：重启修不了网络。网络恢复后首个重试即回到首页，状态自动转回正常。
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

**Q: 控制页画面全白，`/status` 里 `pageUrl` 是 `chrome-error://chromewebdata/`？**
首页导航失败（DNS/网络/站点不可达）。旧版本引擎会把错误页误报为
`page: "ok"` 且不恢复；新版本会显示红色「导航失败」徽标、`page=nav-error`，
退避自动重试，并把 DNS/TCP 探测结论写进 `lastError`。路由器上最常见的根因是
**容器 DNS 不通**（宿主 resolv.conf 指向本机 dnsmasq，桥接网络里不可达），加
`--dns 223.5.5.5`（compose 里 `dns: [223.5.5.5]`）即可：

```bash
docker run -d --name cpk-138xxxx1234 --dns 223.5.5.5 \
  -v $PWD/data/138xxxx1234:/data -p 127.0.0.1:8088:8088 \
  -e CPK_PLATFORM=mobile -e CPK_ACCOUNT=138xxxx1234 \
  --shm-size 128m --init --restart unless-stopped \
  ghcr.io/xaxka/cloudphonekeep:latest
```

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
