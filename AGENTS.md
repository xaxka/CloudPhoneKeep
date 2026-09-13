# AGENTS.md — CloudPhoneKeep AI 代理协作指南

## 项目简介

移动云手机 / 联通云手机网页版保活工具：Rust 引擎驱动无头/嵌入式浏览器，自动确认弹窗、
断线重连、掉线自动恢复，7×24 保持在线。双形态同一套保活逻辑：Windows 便携版（Tauri 2）与 Linux CLI/Docker 版。

## 技术栈与结构

- 语言：Rust（edition 2021，rust-version 1.74/1.77），少量 JS（注入脚本与前端页）。
- 两个 Cargo workspace：
  - 根 workspace（`Cargo.toml`）：成员 `cli` + `shared`，release profile（strip/lto/codegen-units=1/opt-level=s）收口在根。
  - `src-tauri/`：**独立 workspace**（保持自身 target/ 与 CI 产物路径），Tauri 2 + WebView2 + tokio，前端在 `src-tauri/ui/`（login.html/css/js）。
- 目录职责：
  - `cli/`：CLI 专属——引擎（`engine.rs` 进程监管+CDP 会话+看门狗+分级恢复）、`cdp.rs`、`ws.rs`、`report_server.rs`（控制页/健康检查）、`config.rs`、`logger.rs`；另有 Dockerfile、install.sh、control_page.html、`tests/`（Python/JS/Shell 端到端脚本）、`docs/`。
  - `shared/`：双端共享 crate（唯一源）——`platform.rs` 平台预设（移动/联通）、`keepalive.rs` 注入脚本构建器（`include_str!` 内嵌 `shared/keepalive.inject.js`）；含共用文档 `shared/docs/`。
  - `src-tauri/src/`：Windows 版窗口/托盘/热键/IPC（commands.rs、browser.rs、keepalive.rs 等）。

## 构建与 CI（仅作参考，本地禁止执行）

- `.github/workflows/ci.yml`（push main / PR / 手动）三个 job：
  - `build`（windows）：PR 跑 `cargo check --manifest-path src-tauri/Cargo.toml`；push 跑 `cargo build --release --manifest-path src-tauri/Cargo.toml`，产物 CloudPhoneKeep.exe 发到 dev Release。
  - `smoke`（ubuntu）：`cargo build --release --manifest-path cli/Cargo.toml` → `sudo bash cli/install.sh --engine <当前构建>` → `bash cli/tests/ci_smoke.sh`、`python3 cli/tests/knob_e2e.py`、`python3 cli/tests/auth_e2e.py`。
  - `linux`（ubuntu）：musl 交叉编译 `RUSTFLAGS="-C linker=rust-lld" cargo build --release --target x86_64|aarch64-unknown-linux-musl --manifest-path cli/Cargo.toml`，并用 `cli/Dockerfile`（context=仓库根）构建多架构镜像推 GHCR。
- `.github/workflows/release.yml`：推 `v*` tag 触发，同构构建发正式 Release。
- 改动是否可用以 GitHub CI 编译通过为准。

## 硬性工作规则（用户长期要求，必须逐条遵守）

1. 禁止本地编译：不要在本地运行任何构建/编译/测试/依赖安装命令（如 cargo、npm 等）；改动是否可用以 GitHub CI 编译通过为准。
2. 每完成一个改动立即 commit 并 push，再进行下一项改动。
3. 所有提交使用 xaxka 身份（本 clone 已配置 user.name=xaxka，user.email=73456104+xaxka@users.noreply.github.com，不要改动）。
4. 任务结束后清理本地 clone。

## 代码约定

- 注释与提交信息以中文为主，Rust 用 `//!` 模块级文档说明线程模型/职责边界；提交信息用中文 conventional commits（如 `chore(tauri): 版本号升至 1.12.0`）。
- 日志走自定义 Logger（`cli/src/logger.rs`）：行格式 `HH:mm:ss.SSS [pid=N] [slot=1|sys] [level] message`，level 约定 nav/beat/click/miss/exit/probe/error/sys，同时镜像到 stdout（docker logs 即诊断台）；不使用 log/ env_logger 等库。
- 注入 JS（`shared/keepalive.inject.js`）是双端唯一源：占位符 `__CPK_CFG__`/`__CPK_CURSOR__` 由 `shared/src/keepalive.rs` 构建器替换，平台差异一律收敛为 CFG 开关。
- CLI 引擎刻意单线程（CDP 串行），改动时勿随意引入多线程/锁模型。

## 关键文档 / 敏感区

- 文档导航见根 `README.md`：`cli/docs/`（部署/配置/排查/构建）、`src-tauri/docs/`（架构/配置/排查）、`shared/docs/`（架构、保活规则、日志排查）。
- 改保活逻辑前先读 `shared/docs/keepalive-rules.md`；改两端共用内容（平台预设、注入脚本、共享 crate）只改 `shared/`，双端自动生效。
- `shared/keepalive.inject.js` 改一行影响双平台：需同时核对 Windows（WebView2）与 Linux（CDP）两条路径的语义。
- 勿把 `src-tauri/` 加入根 workspace；`cli/Dockerfile` 的 context 是仓库根（依赖根 workspace 清单与 shared/）。
- chrome-headless-shell 版本单一源为 `cli/chrome-versions.env`（与 install.sh、CI 同源），改版本只改这里。
- 声明过的安全边界：无自动更新/联网下载执行、无键盘钩子/内存注入，自动点击仅用 DOM `click()`——勿引入违反该边界的能力。
