# CloudPhoneKeep 云手机保活

> 云手机网页版多开保活工具（移动云手机 / 联通云手机）。同一套保活逻辑，两种形态：
>
> - **Windows 版**（Tauri 2 + WebView2 + Rust，单文件便携 exe）→ [`src-tauri/`](src-tauri/README.md)
> - **CLI 版**（Linux：Chrome Headless Shell + CDP，Rust 引擎，Docker 容器与裸机直跑两种形态，一个实例一个账号）→ [`cli/`](cli/README.md)
>
> 保活脚本与平台预设双端同源共用（`shared/` crate，改一处双端生效）。

![Tauri](https://img.shields.io/badge/Tauri-2.x-blue) ![License](https://img.shields.io/badge/License-MIT-green) ![Platform](https://img.shields.io/badge/Platform-Windows%20%7C%20Linux%20Docker-lightgrey) ![Portable](https://img.shields.io/badge/便携版-免安装-orange)

## 支持平台

| 平台 | 入口 | 保活能力 |
| :--- | :--- | :--- |
| 移动云手机 | `cloudphoneh5.buy.139.com` | 解锁区自动进入云机、万能确认按钮按文字分流重连/确认、到期弹窗确认、退回首页检测 |
| 联通云手机 | `uphone.wo-adv.cn` | 试用弹窗自动启用、断连自动重试、自动进入云机、到期弹窗确认、退出检测 |

每个帐号可独立选择平台，不同平台的保活选择器互不干扰（规则明细见 [shared/docs/keepalive-rules.md](shared/docs/keepalive-rules.md)）。

## 使用方法

### Windows 版（图形界面）

1. 从 [Releases](https://github.com/xaxka/CloudPhoneKeep/releases) 下载 `CloudPhoneKeep.exe`（dev 预发布为自动构建），放到任意目录双击运行
2. 设置窗口选平台、填手机号（缓存目录名）、选老板键索引 → 「进入」打开云手机窗口
3. 窗口内完成登录；之后点 X 或 `Ctrl+N` 收起窗口（隐藏到托盘，保活继续）
4. 多开：再运行一个 exe（每个实例独立托盘与数据目录）

界面细节、退出语义、老板键/托盘用法见 [src-tauri/README.md](src-tauri/README.md)。

### CLI 版（Docker，一实例一账号）

```bash
docker run -d --name cpk \
  -v $PWD/data/138xxxx1234:/data \
  -p 127.0.0.1:8088:8088 \
  -e CPK_ACCOUNT=138xxxx1234 \
  --shm-size 128m --init --restart unless-stopped \
  ghcr.io/xaxka/cloudphonekeep:latest
# 浏览器打开 http://127.0.0.1:8088/ 完成首次登录，之后自动保活
```

裸机直跑（musl 静态二进制，机器只需 chrome-headless-shell）、控制台交互、
多账号编排、环境变量调优（帧率/画质/分辨率/空闲降频）见 [cli/README.md](cli/README.md)。

## 构建与发布

```bash
# Windows（需要 Rust 1.77+ 与 Windows 环境，WebView2 运行时需系统已装）
cargo build --release --manifest-path src-tauri/Cargo.toml
# 产物：src-tauri/target/release/CloudPhoneKeep.exe（单文件便携版）

# CLI 版（一个实例一个账号；构建上下文在仓库根）
cd cli && docker compose up -d --build

# CLI 裸机直跑（musl 静态二进制，机器只需 chrome-headless-shell；
# CI 同步发布到 dev Release，用法见 cli/README.md「裸机直跑」）
cargo build --release --target x86_64-unknown-linux-musl --manifest-path cli/Cargo.toml
```

前端为纯静态 HTML/JS（`ui/` 目录），无 Node 构建步骤。推送代码后 GitHub Actions 自动构建并发布到 `dev` 预发布版（Windows exe + CLI 静态二进制 amd64/arm64），同时构建 Linux 多架构 Docker 镜像（amd64/arm64）推送 GHCR（`ghcr.io/xaxka/cloudphonekeep`）。

## 文档导航

| 文档 | 位置 |
| :--- | :--- |
| CLI 版：部署 / 配置 / 排查 | [`cli/docs/`](cli/docs/)（[部署](cli/docs/deploy.md) · [配置](cli/docs/configuration.md) · [排查](cli/docs/diagnostics.md)） |
| Windows 版：架构 / 配置 / 排查 | [`src-tauri/docs/`](src-tauri/docs/)（[架构](src-tauri/docs/architecture.md) · [配置](src-tauri/docs/configuration.md) · [排查](src-tauri/docs/diagnostics.md)） |
| 双平台共用：架构总览 / 保活规则 / 日志排查 | [`shared/docs/`](shared/docs/)（[架构](shared/docs/architecture.md) · [保活规则](shared/docs/keepalive-rules.md) · [日志排查](shared/docs/diagnostics.md)） |

## 目录结构

```
├── ui/                      # 前端（Windows 版设置窗口，纯静态）
│   └── login.html / .css / .js
├── src-tauri/               # Windows 版（Tauri，详见 src-tauri/README.md）
│   ├── src/                 #   窗口/托盘/热键/IPC（保活构建器适配层在 shared crate）
│   ├── assets/              #   触点光标等 Tauri 专属资源
│   ├── docs/                #   Windows 版文档（架构/配置/排查）
│   └── tauri.conf.json      #   独立 Cargo workspace
├── shared/                  # CLI + Tauri 共享（crate cloudphonekeep-shared）
│   ├── keepalive.inject.js  #   保活脚本唯一源文件（双端共用，改一处双端生效）
│   ├── src/                 #   platform.rs 平台预设唯一源 / keepalive.rs 构建器唯一源
│   └── docs/                #   双平台文档（架构/保活规则/日志排查）
├── cli/                     # CLI 版（Linux/Docker/裸机，一实例一账号，详见 cli/README.md）
│   ├── Dockerfile           #   rust:1-alpine musl 交叉编译（多架构）→ debian + CfT chrome-headless-shell
│   ├── docker-compose.yml
│   ├── control_page.html    #   控制页模板（CLI 专属）
│   ├── docs/                #   CLI 版文档（部署/配置/排查）
│   ├── src/                 #   Rust 保活引擎（手写 WS/CDP 客户端）
│   └── tests/               #   本地复现/回归脚本集
├── Cargo.toml               # 根 workspace（cli + shared）与锁文件；src-tauri 独立工作区
└── .github/workflows/       # CI：Windows exe + CLI 静态二进制 + 多架构镜像（GHCR）
```

## 安全性说明

- 不含任何自动更新 / 自动下载执行逻辑：无任何联网比对或下载行为
- 触点光标（默认关闭）为**本地内嵌资源**（`src-tauri/assets/cursor.b64`），零外部资源依赖
- 对外网络行为只有一类：云手机页面本身的正常访问
- 无键盘钩子（老板键使用系统全局热键注册，而非 `SetWindowsHookEx`）
- 无内存注入、无自修改代码、无加壳；自动点击全部通过标准 DOM `click()` 完成
- 全部代码开源可审计；原版程序的第三方更新服务器后门已彻底移除

## 免责声明

- 本项目仅供个人学习与研究，请遵守云手机服务商条款
- 自动保活可能违反服务商使用政策，产生的账号风险由使用者自行承担
- 严禁用于任何违法违规用途

## License

MIT © xaxka
