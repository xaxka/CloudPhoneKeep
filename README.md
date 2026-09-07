# CloudPhoneKeep 云手机保活

> 云手机网页版多开保活工具（移动云手机 / 联通云手机）— **Tauri 2 + WebView2 + Rust** 实现，界面还原原版 aardio 程序，单文件便携版。Windows 版之外另有 **CLI 版**（Linux：Chrome Headless Shell + CDP，Rust 引擎，Docker 容器与裸机直跑两种形态，一个实例一个账号，见 [`cli/`](cli/README.md)）。

![Tauri](https://img.shields.io/badge/Tauri-2.x-blue) ![License](https://img.shields.io/badge/License-MIT-green) ![Platform](https://img.shields.io/badge/Platform-Windows%20%7C%20Linux%20Docker-lightgrey) ![Portable](https://img.shields.io/badge/便携版-免安装-orange)

**技术文档**（架构 / 保活规则 / 排查 / 配置 / Linux 部署）已集中到 [`doc/`](doc/README.md)，本文件只保留上手所需。

## 支持平台

| 平台 | 入口 | 保活能力 |
| :--- | :--- | :--- |
| 移动云手机 | `cloudphoneh5.buy.139.com` | 解锁区自动进入云机、万能确认按钮按文字分流重连/确认、到期弹窗确认、退回首页检测 |
| 联通云手机 | `uphone.wo-adv.cn` | 试用弹窗自动启用、断连自动重试、自动进入云机、到期弹窗确认、退出检测 |

每个帐号可独立选择平台，不同平台的保活选择器互不干扰（规则明细见 [doc/keepalive-rules.md](doc/keepalive-rules.md)）。

## 界面（还原原版 exe）

启动后是一个小「新开账号」窗口（窗口标题即「新开账号」，与原版 login 窗体一致）：选平台、填手机号（缓存目录名）、选老板键索引 → 点「进入」打开云手机窗口。

- 窗口不挂菜单栏（原五项已移除），标题栏只保留关闭按钮，可拖拽自由调整大小
- **不在任务栏显示**：交互通过各云手机窗口的独立托盘完成
- `Ctrl+N`（N=老板键索引）瞬间隐藏/呼出，`Ctrl+U` 呼出地址栏（回车跳转、Esc 关闭，与原版一致）
- 每个云手机窗口**自己的托盘图标**：左键打开窗口，右键菜单「首页 / 窗口置顶 / 打开数据目录 / 退出」；悬停提示「平台 - 帐号名」
- 多开 = 再运行一个 exe（不同实例用不同缓存目录名与老板键索引）

## 退出语义

- **云手机窗口右上角 X = 隐藏到托盘，保活继续**（要退出请用托盘菜单「退出」）
- **设置窗口右上角 X = 退出整个程序**（原版 `loginForm.onClose → win.quitMessage()`）
- **托盘右键 → 退出 = 退出整个程序**

隐藏与最小化窗口均继续保活（看门狗接管，见 [doc/architecture.md](doc/architecture.md)）。

## 使用方法

1. 从 [Releases](https://github.com/xaxka/CloudPhoneKeep/releases) 下载 `CloudPhoneKeep.exe`（dev 预发布为自动构建），放到任意目录
2. 双击运行 → 设置窗口中选平台、填手机号（缓存目录名）、选老板键索引 → 「进入」
3. 在打开的窗口中完成云手机登录；之后点窗口 X 或 `Ctrl+N` 收起窗口（隐藏到托盘），保活继续
4. 需要多开：再运行一个 exe，每个实例都有自己的云手机窗口与独立托盘
5. 数据位置：`AppData\LocalLow\CloudPhoneKeep`（配置/日志/各帐号数据隔离，详见 [doc/configuration.md](doc/configuration.md)）

## 安全性说明

- 不含任何自动更新 / 自动下载执行逻辑：无任何联网比对或下载行为
- 触点光标（默认关闭）为**本地内嵌资源**（`src-tauri/assets/cursor.b64`），零外部资源依赖
- 对外网络行为只有一类：云手机页面本身的正常访问
- 无键盘钩子（老板键使用系统全局热键注册，而非 `SetWindowsHookEx`）
- 无内存注入、无自修改代码、无加壳；自动点击全部通过标准 DOM `click()` 完成
- 全部代码开源可审计；原版程序的第三方更新服务器后门已彻底移除

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

## 目录结构

```
├── ui/                      # 前端（设置窗口，纯静态）
│   ├── login.html / .css / .js
├── src-tauri/               # Tauri 专属（Windows GUI，独立 Cargo workspace）
│   ├── src/
│   │   ├── main.rs          # 入口、panic 记录、托盘、菜单事件
│   │   ├── browser.rs       # 窗口生命周期 / 原生菜单 / 老板键 / 托盘
│   │   ├── keepalive.rs     # 注入脚本构建器适配层（核心在 shared crate）
│   │   ├── report_server.rs # 127.0.0.1 状态回传服务
│   │   ├── config.rs        # 便携配置、帐号数据目录隔离
│   │   ├── commands.rs      # Tauri 命令
│   │   ├── logger.rs        # 按天滚动诊断日志
│   │   └── state.rs         # 槽位运行状态
│   ├── assets/              # 触点光标等 Tauri 专属资源
│   └── tauri.conf.json
├── shared/                  # CLI + Tauri 共享（crate cloudphonekeep-shared）
│   ├── keepalive.inject.js  # 保活脚本唯一源文件（双端共用，改一处双端生效）
│   └── src/
│       ├── platform.rs      # 平台预设（移动/联通入口、视口）唯一源
│       └── keepalive.rs     # 注入脚本构建器（占位符替换约定）唯一源
├── cli/                     # CLI 专属（Linux/Docker/裸机，一实例一账号，详见 cli/README.md）
│   ├── Dockerfile           # rust:1-alpine musl 交叉编译（多架构）→ debian + CfT chrome-headless-shell
│   ├── docker-compose.yml
│   ├── control_page.html    # 控制页模板（CLI 专属）
│   ├── src/                 # Rust 保活引擎（手写 WS/CDP 客户端）
│   └── tests/               # 本地复现/回归脚本集
├── Cargo.toml               # 根 workspace（cli + shared）与锁文件；src-tauri 独立工作区
├── doc/                     # 技术文档（架构/保活规则/排查/配置/部署）
└── .github/workflows/       # CI：Windows exe + CLI 静态二进制 + 多架构镜像（GHCR）
```

## 免责声明

- 本项目仅供个人学习与研究，请遵守云手机服务商条款
- 自动保活可能违反服务商使用政策，产生的账号风险由使用者自行承担
- 严禁用于任何违法违规用途

## License

MIT © xaxka
