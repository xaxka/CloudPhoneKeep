# CloudPhoneKeep Windows 版（Tauri）

> 云手机网页版多开保活的 **Windows 桌面版** — **Tauri 2 + WebView2 + Rust** 实现，
> 界面还原原版 aardio 程序，单文件免安装便携版。多开 = 多 exe 多实例。
> CLI 版（Linux / Docker / 裸机）见 [`../cli/`](../cli/README.md)。

![Tauri](https://img.shields.io/badge/Tauri-2.x-blue) ![Platform](https://img.shields.io/badge/Platform-Windows-lightgrey) ![便携版](https://img.shields.io/badge/便携版-免安装-orange)

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

隐藏与最小化窗口均继续保活（看门狗接管，见
[docs/architecture.md](docs/architecture.md)）。

## 使用方法

1. 从 [Releases](https://github.com/xaxka/CloudPhoneKeep/releases) 下载 `CloudPhoneKeep.exe`（dev 预发布为自动构建），放到任意目录
2. 双击运行 → 设置窗口中选平台、填手机号（缓存目录名）、选老板键索引 → 「进入」
3. 在打开的窗口中完成云手机登录；之后点窗口 X 或 `Ctrl+N` 收起窗口（隐藏到托盘），保活继续
4. 需要多开：再运行一个 exe，每个实例都有自己的云手机窗口与独立托盘
5. 数据位置：`AppData\LocalLow\CloudPhoneKeep`（配置/日志/各帐号数据隔离，详见 [docs/configuration.md](docs/configuration.md)）

## 构建

```bash
# 需要 Rust 1.77+ 与 Windows 环境，WebView2 运行时需系统已装
cargo build --release --manifest-path src-tauri/Cargo.toml
# 产物：src-tauri/target/release/CloudPhoneKeep.exe（单文件便携版）
```

src-tauri 为独立 Cargo workspace（根 workspace `exclude`），自身 target/ 与
CI 产物路径不变。前端为纯静态 HTML/JS（`src-tauri/ui/` 目录，设置窗口，
随 Tauri 子项目就地维护），无 Node 构建步骤。推送代码后 GitHub Actions
自动构建并发布到 `dev` 预发布版。

## 文档

- [架构（aardio 复刻对照 / WebView2 调优）](docs/architecture.md)
- [配置与数据目录](docs/configuration.md)
- [常见现象排查](docs/diagnostics.md)
- 保活规则（双平台共用）：[`../shared/docs/keepalive-rules.md`](../shared/docs/keepalive-rules.md)

## 免责声明

与主项目一致：仅供个人学习与研究，请遵守云手机服务商条款；自动保活可能违反
服务商使用政策，账号风险自担；严禁用于任何违法违规用途。
