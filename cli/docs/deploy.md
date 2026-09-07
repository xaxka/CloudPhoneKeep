# CLI 版部署（Linux / Docker / 裸机）

一个实例 = 一个账号（Docker 容器或裸机进程）。镜像：`ghcr.io/xaxka/cloudphonekeep`（**多架构**：
`linux/amd64` + `linux/arm64` 单一 manifest，x86 服务器与 ARM 主机/Apple
Silicon 均原生运行，`docker pull` 自动选架构）。不想用 Docker 的机器
用一键安装脚本（自动下载引擎二进制与 chrome-headless-shell、检查运行库、
装完自检，googleapis 失败自动换 npmmirror 国内镜像）：

```bash
curl -fsSL https://raw.githubusercontent.com/xaxka/CloudPhoneKeep/main/cli/install.sh | bash
# 选项与裸机细节见 ../README.md「裸机直跑」章节。
```

## 快速开始

```bash
# 1. 拉镜像（本地无 Rust 也可 docker compose build，见 build.md）
docker pull ghcr.io/xaxka/cloudphonekeep:latest

# 2. 启动（单账号默认即可：CPK_ACCOUNT 默认 1 可不设；平台留空，
#    控制页「设置→平台」选移动/联通后引擎加载页面（约 10 秒）。
#    也可 -e CPK_PLATFORM=mobile 启动即加载，免开控制页选择）
docker run -d --name cpk \
  -v cpk:/data \
  -p 127.0.0.1:8088:8088 \
  --shm-size 128m --init --restart unless-stopped \
  ghcr.io/xaxka/cloudphonekeep:latest

# 3. 首次登录：浏览器打开 http://127.0.0.1:8088/（全屏实时画面，
#    触摸/鼠标/键盘/剪贴板输入全覆盖）；
#    登录一次后 Cookie/LocalStorage 持久化在 volume，之后自动保活

# 4. 观察健康状态
curl http://127.0.0.1:8088/healthz
docker logs -f cpk     # 诊断日志实时镜像
```

多账号用 `cli/docker-compose.yml` 复制服务块即可（各账号设 `CPK_ACCOUNT`
区分数据目录，建议手机号；端口 8088、8089… 递增，宿主端口可自由改）。

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

## 浏览器选型说明（chrome-headless-shell）

CLI 版使用 **Google Chrome for Testing 官方预编译的 `chrome-headless-shell`**
（无头渲染内核，无 Chrome UI/标签页/扩展，比完整 Chrome 省内存；WebRTC 栈完整
保留——保活只要求流建立不断开）：

- **架构**：CfT 自 153.0.8001.0 起提供 `linux-arm64` 预编译；镜像双架构
  （amd64/arm64）都用 CfT headless-shell，运行层为 `debian:bookworm-slim`
  （CfT 二进制是 glibc 动态链接，不能跑在 musl/Alpine 上；Rust 引擎是 musl
  静态二进制，与运行层 libc 无耦合）
- **版本**：amd64 用 stable `152.0.7977.82`（开发环境端到端冒烟验证过的
  版本）；arm64 用 beta `154.0.8037.0`（stable 渠道尚无 arm64，取 arm64
  可用的最近渠道），见 `../Dockerfile` 的 ARG
- headless-shell 本身即无头模式，恒不加 `--headless=new`（`CPK_HEADLESS` 开关
  已移除；极少数换用完整 Chromium 的场景经 `CPK_EXTRA_CHROME_ARGS` 自行追加）
- 依赖最小化：运行层 apt 包为 `ldd` 实测结果（nss/glib/X11 基础库/alsa/
  gbm 等，见 Dockerfile 注释），curl/unzip 仅构建期使用后即删除

## 首次登录与控制台

浏览器打开 `http://127.0.0.1:8088/`：**全屏实时画面 + 极简操作**。
桌面端右侧是操作栏；**手机等窄屏**点底部 iOS 风格白色圆点弹出底部抽屉
控制台（再点圆点或点遮罩收起；底部留白已收紧，画面尽量占满）。
状态（页面状态/`N fps`/连接模式）收纳在
控制台顶部状态行，不悬浮在画面上遮挡内容；重连期显示「等帧…」，切后台显示「已暂停」。

画面为 `Page.startScreencast` 合成器帧直推的 MJPEG 流（页面有更新即出帧，
局域网延迟 ≈ 帧间隔；带动画的页面可达 25-60fps，弱机实际由 CPU/网络决定）。
VLC 等标准播放器也可直接打开 `http://<host>:<port>/stream.mjpg` 观看。
**控制台「设置 → 帧率/画质/分辨率」三个旋钮均可运行时调整**（均即时生效，
回读验证不谎报）：
- **帧率** 1-60，默认 10（云机页面内容变化率普遍 5-10fps，10 已足额；
  初始值可用环境变量 `CPK_FPS` 设定）；
- **画质** 10-90，默认 50（`CPK_JPEG_QUALITY`）；
- **分辨率** 30-100，默认 100 原画（`CPK_STREAM_SCALE`）。
帧率越高 CPU 越高：引擎按目标帧率对 Chrome 端采集/编码做 ack 门控节流
（Chrome 确认一帧后才采下一帧），传输 CPU 大致正比帧率。
**传输 CPU 的治理（screencast 高 CPU 是 Chromium 官方已知缺陷
[issue 40934921](https://issues.chromium.org/issues/40934921)：页面变化越多
帧事件越多）**，引擎做了三层：
1. **ack 门控节流**（主力，本地实测 Chrome 收到 `screencastFrameAck` 才
   采集/编码下一帧）：被限帧丢弃的帧延迟补 ack → Chrome 端采集+编码
   频率被压到目标帧率附近，而非合成器帧率。传输 CPU 大致正比帧率。
   （曾用 everyNthFrame=floor(60/fps) 做第二保险，已除名：它会把「内容驱动的
   合成器出帧率」整除下来——弱机合成器本身只有 ~5fps 时设 10fps 反被
   ÷6 到 0.9/s，CPU 有余而帧数上不去；ack 门控单独承担节流即无此问题。）
2. **丢帧先于解码 + base64 O(1) 查表**：Rust 侧只为真正推送的帧解码。
3. **可调旋钮（控制台面板/环境变量均可）**：`CPK_JPEG_QUALITY`（默认 50，
   降质量省编码 CPU/带宽）、`CPK_STREAM_SCALE`（默认 100；如 75 = 分辨率
   缩 75%，编码量按像素近线性下降，触摸坐标是 CSS 坐标系不受影响）。
观看中每 30s 日志输出一行帧流统计（Chrome 出帧 N/s → 解码推送 M/s）：
出帧 ≈ min(页面内容变化率, 目标帧率)——远低于目标＝页面本身变化慢
（静态页省流省 CPU，属正常），非设置失效。

**空闲 CPU 的治理（tick 自适应降频）**：不传输画面时引擎仍要常驻唤醒
Chrome（每 1s tick eval 驱动页面保活双定时器 + 每 5s 采样 eval 读状态，
每次都唤醒渲染主线程跑 JS + JSON 往返——这是空闲 CPU 的主要来源）。
现在引擎按「有没有人在用」自适应：

- **空闲判定**：无画面订阅 && 距最近一次用户操作（触摸/键鼠/导航/设置调整/
  流订阅）≥ `CPK_IDLE_AFTER_SEC`（默认 60s）&& 无恢复动作在途；
- **空闲态**：tick 降为 `CPK_IDLE_TICK_SEC`（默认 5s），采样降为 3 倍动作
  周期（默认 15s）——eval 唤醒次数降约 5 倍；**保活语义零变化**：保活动作
  周期恒 ≈ 5s（注入脚本按墙钟门控，不随 tick 周期拉长）、心跳照发、
  弹窗自动确认与自动恢复全部保留；
- **即时恢复**：任一操作或打开画面流，≤200ms 内回到 1s/5s 活跃节奏；
  空闲中页面出状况（冻结/脚本缺失）也会自动暂退活跃节奏密集救治；
- **安全钳**：若配置会让「页面级恢复」慢于「心跳硬重启」（如
  `CPK_IDLE_TICK_SEC` 过大而 `CPK_BEAT_STALE_SEC` 过短），引擎自动放弃
  降频维持活跃节奏，绝不为省 CPU 打破恢复分级；
- **验证**：`curl /healthz` 看 `tickIdle` 字段（true = 空闲降频生效中），
  日志有「tick 降频 1s→5s」「检测到观看/操作，tick 恢复 1s」两条迁移记录。

**输入全覆盖**（桌面鼠标与手机触摸自动分流，无模式选择）：

| 输入 | 通道 | 说明 |
| --- | --- | --- |
| 触摸拖动/长按/双指缩放 | CDP `Input.dispatchTouchEvent` | 按下/移动/抬起逐点直通（fire 即答）；移动带全部在按触点，双指缩放可用；抬一指手势延续 |
| 鼠标左键点击 | CDP `Input.dispatchMouseEvent` | 真实 mousedown/mouseup/click（非触摸合成，兼容所有页面）；双击/三击由 clickCount 合成 dblclick |
| 鼠标拖动 | 触摸流 | 自动模式：鼠标按住移动超阈值即转触摸拖动（移动页滚动跟手）；「鼠标」模式下为真实 mouseMoved 拖动 |
| 右键/中键 | CDP 鼠标事件 | 右键点击→远程 contextmenu 菜单 |
| 悬停 | CDP 鼠标事件 | 纯移动（无按键）转发为真实 mouseMoved，桌面页 hover 菜单可用 |
| 滚轮 | CDP `mouseWheel` | 画面上滚动即转发（移动页/桌面页均可滚动） |
| 物理键盘全键位 | CDP `Input.dispatchKeyEvent` | 常开直通（含修饰键/功能键/方向键）；Ctrl+C/X 同步云机选区到本机剪贴板，Ctrl+V 把本机剪贴板粘贴到云机，Ctrl+A 远程全选，F5 远程刷新 |
| 手机文本输入 | 云机 H5 自带软键盘 | 点击画面内输入框弹出（触摸链路）；输入框 UI 已按需求移除，文本可用「粘贴」按钮送入 |
| 剪贴板复制/粘贴 | `/clip` + `insertText` | 「复制」读云机选中文本（含输入框选区）写入本机剪贴板；「粘贴」读本机剪贴板插入云机焦点处 |

> Linux 版**无页内地址栏**（按需求不提供）：导航走控制页「回首页」/页面内跳转；外部脚本可 `POST /nav`（token 同控制页）。shared 脚本注入的 `#cpk-addr-bar` 在云机页内保持 `display:none` 惰性存在，无任何触发入口，无副作用（Windows 版仍由 Ctrl+U 使用）。

外部脚本直调端点（token 保护同控制页）：`POST /platform`（`value=mobile|unicom`
运行时切换，实例自动重启）、`POST /touch`（`phase` +
`ps=x,y,id;x,y,id` 多点或 `x/y` 单点）、`POST /mouse`（`action=move/down/up/wheel`
+ `b/n/bb/m/dx/dy`）、`POST /kbd`（`t=down/up` + `key/code/vk/text/m/l/r`）、
`POST /type`（整段文本）、`GET /clip`（选区文本）、
`POST /fps`（1-60）、`POST /quality`（10-90）、`POST /scale`（30-100）；
`/tap /swipe /key /nav /reload` 兼容保留。所有输入事件 fire 即发，引擎线程
占用 <0.1ms——导航/重连期间输入不再卡死；引擎重建期间请求毫秒级快速失败
（控制页提示「输入通道异常」而非无响应）。

**切后台/锁屏自动省 CPU**：控制页不可见即断流，引擎最后一个订阅者离开后
自动 `Page.stopScreencast`——无人观看＝零 JPEG 编码开销，CPU 即降（云机页面
本身的运行开销仍在，那是保活语义）；回前台自动重连。关闭标签页同理。

**状态转换通知（对齐 Windows 版系统通知）**：云机「退出/到期」是一次性
事件，控制页轮询 healthz 的 `lastStatus`/`exited` 检测状态迁移，先弹 5 秒
toast，浏览器通知权限已授予时同时发系统通知（权限需用户手势——首次
触摸页面时自动申请一次，被拒/不支持则只用 toast，不影响使用）。控制台
状态面板同时显示当前页面标题（`title`，对齐 Windows 版窗口标题可见性）。

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

推送 `main` 后 CI 自动（`.github/workflows/ci.yml` 的 `linux` job，
源码在 `cli/` + `shared/`，根 workspace）：

1. runner 交叉编译 musl 静态二进制（amd64 + arm64，rust-lld）→
   发布 `cloudphonekeep-linux-amd64` / `cloudphonekeep-linux-arm64` 到
   `dev` Release（与镜像内引擎同源同构，裸机直跑用）
2. Docker 多阶段构建（rust:1-alpine 交叉编译引擎 → debian 运行层）→
   多架构（amd64 + arm64）推送 `ghcr.io/xaxka/cloudphonekeep`
   （`:latest` 与 commit SHA 双标签）

> 单测/selftest/冒烟步骤已按用户要求移除（无浏览器环境的快速回归对真实
> 问题无覆盖，验证靠生产日志留痕与 `cli/tests/` 本地复现脚本集）。

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
  -e CPK_ACCOUNT=138xxxx1234 \
  -e CPK_PLATFORM=mobile \
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
保活脚本与平台预设在 `shared/`（双端唯一源，改规则一处生效）；CLI 版全部
文件在 `cli/` 目录，Tauri 版在 `src-tauri/`，CI 双 job 分别验证。推送
GHCR / Release 的发布流程互不影响。
