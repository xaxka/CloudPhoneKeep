# CloudPhoneKeep 云手机保活

> 移动云手机 / 联通云手机网页版保活工具：Rust 引擎驱动无头浏览器，自动确认弹窗、
> 断线重连、掉线自动恢复，7×24 保持登录在线。
>
> *Cloud-phone web keep-alive (China Mobile / China Unicom): a Rust engine drives a
> headless browser to auto-confirm popups and reconnect — keeping sessions alive 24/7.
> Linux CLI/Docker edition + Windows desktop (Tauri) edition.*

![Tauri](https://img.shields.io/badge/Tauri-2.x-blue) ![License](https://img.shields.io/badge/License-MIT-green) ![Platform](https://img.shields.io/badge/Platform-Windows%20%7C%20Linux%20Docker-lightgrey) ![Portable](https://img.shields.io/badge/便携版-免安装-orange)

两种形态，同一套保活逻辑（`shared/` 双端同源共用，改一处双端生效）：

| 形态 | 技术栈 | 适用场景 |
| :--- | :--- | :--- |
| **Windows 版** [`src-tauri/`](src-tauri/README.md) | Tauri 2 + WebView2 + Rust | 单文件便携 exe，图形界面，多开多实例 |
| **CLI 版** [`cli/`](cli/README.md) | chrome-headless-shell + CDP + Rust | Linux 服务器，Docker / 裸机，一实例一账号 |

支持平台：移动云手机（`cloudphoneh5.buy.139.com`）与联通云手机（`uphone.wo-adv.cn`），
每个账号独立选择平台，保活规则明细见 [shared/docs/keepalive-rules.md](shared/docs/keepalive-rules.md)。

## 使用方法

### Windows 版

1. 从 [Releases](https://github.com/xaxka/CloudPhoneKeep/releases) 下载 `CloudPhoneKeep.exe`（dev 预发布为自动构建），双击运行
2. 设置窗口选平台、填手机号（缓存目录名）、选老板键索引 → 「进入」打开云手机窗口
3. 窗口内完成登录；之后点 X 或 `Ctrl+N` 收起窗口（隐藏到托盘，保活继续）
4. 多开：再运行一个 exe（每个实例独立托盘与数据目录）

详见 [src-tauri/README.md](src-tauri/README.md)。

### CLI 版（Docker）

```bash
docker run -d --name cpk -v cpk:/data -p 127.0.0.1:8088:8088 \
  --shm-size 128m --init --restart unless-stopped \
  ghcr.io/xaxka/cloudphonekeep:latest
```

1. 浏览器打开 `http://127.0.0.1:8088/`，「设置 → 平台」选移动/联通 → 引擎加载页面（约 10 秒）
2. 在控制页的全屏实时画面里完成首次登录（触摸/鼠标/键盘/剪贴板输入全覆盖）；
   登录态持久化在 volume，之后自动保活
3. 健康检查：`curl http://127.0.0.1:8088/healthz`

单账号开箱即用（`CPK_ACCOUNT` 默认 `1`，可不设）。不想用 Docker？裸机一键安装：

```bash
curl -fsSL https://raw.githubusercontent.com/xaxka/CloudPhoneKeep/main/cli/install.sh | bash
```

多实例多账号、裸机细节（systemd/多架构）、运行时调优（帧率/画质/分辨率/空闲降频）
见 [cli/README.md](cli/README.md)。

## 构建与发布

CLI 版构建（源码 / Docker / musl 交叉编译）与 CI 自动发布见
[cli/docs/build.md](cli/docs/build.md)；Windows 版构建见
[src-tauri/README.md](src-tauri/README.md#构建)。

## 文档导航

| 文档 | 位置 |
| :--- | :--- |
| CLI 版：部署 / 配置 / 排查 / 构建 | [`cli/docs/`](cli/docs/)（[部署](cli/docs/deploy.md) · [配置](cli/docs/configuration.md) · [排查](cli/docs/diagnostics.md) · [构建](cli/docs/build.md)） |
| Windows 版：架构 / 配置 / 排查 | [`src-tauri/docs/`](src-tauri/docs/)（[架构](src-tauri/docs/architecture.md) · [配置](src-tauri/docs/configuration.md) · [排查](src-tauri/docs/diagnostics.md)） |
| 双平台共用：架构总览 / 保活规则 / 日志排查 | [`shared/docs/`](shared/docs/)（[架构](shared/docs/architecture.md) · [保活规则](shared/docs/keepalive-rules.md) · [日志排查](shared/docs/diagnostics.md)） |

## 目录结构

```
├── src-tauri/   # Windows 版（Tauri，独立 workspace；ui/ 前端在本目录内）
├── shared/      # 双端共享 crate（保活脚本/平台预设/构建器，含共用文档）
├── cli/         # CLI 版（Rust 引擎 + Docker + 测试脚本 + 文档）
└── Cargo.toml   # 根 workspace（cli + shared）
```

## 安全性说明

- 不含任何自动更新 / 联网下载执行逻辑；对外网络行为只有云手机页面的正常访问
- 无键盘钩子（老板键走系统全局热键注册）、无内存注入、无自修改代码、无加壳；
  自动点击全部通过标准 DOM `click()` 完成
- 全部代码开源可审计

## 免责声明

仅供个人学习与研究，请遵守云手机服务商条款；自动保活可能违反服务商使用政策，
账号风险自担；严禁用于任何违法违规用途。

## License

MIT © xaxka
