# CloudPhoneKeep 云手机保活

> 联通云手机（uPhone）网页版多开保活工具 — 使用 **Tauri 2 + WebView2** 从零重写的 `摸鱼浏览器`。
> 原版为 aardio/易语言系编译，常被杀毒软件误报且已无法保活；本项目用现代技术栈重实现全部核心功能。

![Tauri](https://img.shields.io/badge/Tauri-2.x-blue) ![License](https://img.shields.io/badge/License-MIT-green) ![Platform](https://img.shields.io/badge/Platform-Windows-lightgrey)

## 功能一览（与原版对照）

| 功能 | 原版 (aardio) | 本项目 (Tauri 2) |
| :--- | :--- | :--- |
| 多帐号多开 | 最多 9 窗口 | 最多 9 窗口（槽位 1~9） |
| 数据隔离 | 每帐号独立 userDataDir | 每槽位独立 WebView2 数据目录 |
| 老板键 | Ctrl+1~9 瞬间隐藏/呼出 | 相同（全局热键） |
| 托盘 | 显示/隐藏/退出 | 显示/隐藏全部、逐帐号切换、退出 |
| 自动保活 | CDP 定时扫描点击弹窗 | 页内脚本 + Rust 看门狗双重驱动 |
| 试用弹窗自动点「立即启用云手机」 | ✅ | ✅ |
| 「无法连接」自动点「再次尝试」 | ✅ | ✅ |
| 自动点「进入云机」 | ✅ | ✅ |
| 到期弹窗自动点「知道了」 | ✅ | ✅ |
| 退出云机 / 到期 系统通知 | 托盘气泡 | Windows 通知 |
| 横竖屏旋转（交换宽高） | ✅ | ✅ |
| 窗口置顶 | ✅ | ✅ |
| 自定义触点光标 (Dotter.cur) | ✅ | ✅ |
| 屏蔽页面右键菜单 | ✅ | ✅ |
| 指定 Cookie | CDP Network.setCookies | 页内注入 + 独立目录持久化 |
| 在线更新检查 | fsys.update | 内置检查（默认指向原更新源，可配置） |
| **空闲模拟鼠标活动防掉线** | ❌ | ✅（新增） |
| **后台隐藏时保活不中断** | 部分 | ✅（Rust 看门狗驱动，不受浏览器定时器节流影响） |

## 保活原理

页面加载后注入的保活脚本会周期执行（窗口可见时由页内 `setInterval` 驱动；窗口被隐藏后由 Rust 侧看门狗周期 `eval` 驱动，绕过 Chromium 对后台页面的定时器节流）：

1. 出现 `.try-content`（试用提示）→ 自动点击 `.try-btn`「立即启用云手机」
2. 出现 `.phone-dialog-wrap`「无法连接」→ 自动点击「再次尝试 / 重试 / 重新连接」
3. 出现 `.detail-info-container` 详情页 → 自动点击 `.enter-intance`「进入云机」
4. 出现 `.van-dialog__confirm` 到期弹窗 → 自动点击「知道了」并发出系统通知
5. 检测到 `.title-bar`（退回首页，即云机退出）→ 状态上报 + 系统通知
6. 空闲周期内向页面派发轻微 `mousemove` 事件，降低会话闲置断开概率
7. 全部状态通过 `http://127.0.0.1:<port>/report` 回环上报（Chromium 允许 HTTPS 页面访问环回地址，不受混合内容限制），管理面板实时显示每个帐号的保活状态与自动点击次数

## 使用方法

1. 从 [Releases](https://github.com/xixka/CloudPhoneKeep/releases) 下载最新的 `CloudPhoneKeep_x.x.x_x64-setup.exe` 安装（依赖 WebView2 运行时，安装器会自动引导下载）
2. 打开主面板，在「帐号 N」卡片中填写：
   - **缓存目录名/帐号标识**：建议填手机号，各窗口登录态完全隔离
   - 其余项一般保持默认即可
3. 点击「启动」，在打开的窗口中完成联通云手机登录
4. 之后即使关闭窗口内容（隐藏），保活引擎仍持续运行
5. **老板键**：任意界面按 `Ctrl+1` ~ `Ctrl+9` 瞬间隐藏/呼出对应帐号窗口；也可用托盘菜单
6. 「保存配置」后下次可一键「全部启动」

## 构建

```bash
# 需要 Rust 1.77+ 与 Windows 环境（WebView2 SDK 由 tauri 自动处理）
cd src-tauri
cargo tauri build        # 产出 NSIS 安装包
# 或开发调试
cargo tauri dev
```

本仓库前端为纯静态 HTML/JS（`ui/` 目录），无 Node 构建步骤。

## 为什么重写后不容易报毒

原版使用 aardio 静态编译，二进制内嵌大量中文运行时与键盘钩子 API 特征，是杀软启发式误报的重灾区。本项目：

- Rust + Tauri 官方工具链，签名与结构规范
- 无键盘钩子（老板键使用系统全局热键注册，而非 `SetWindowsHookEx`）
- 无内存注入、无自修改代码、无加壳
- 自动点击全部通过标准 DOM `click()` 完成，不模拟底层输入

> 注意：任何含"自动操作浏览器"行为的程序仍可能被个别杀软启发式提示，属正常现象，可自行加入白名单。本项目不含任何恶意行为，全部代码开源可审计。

## 目录结构

```
├── ui/                      # 前端（管理面板，纯静态）
│   ├── manager.html / .css / .js
├── src-tauri/
│   ├── src/
│   │   ├── main.rs          # 入口、托盘、热键、自动启动
│   │   ├── browser.rs       # 多窗口管理 / 旋转 / 置顶 / 导航
│   │   ├── keepalive.rs     # 注入页面的保活脚本生成
│   │   ├── report_server.rs # 127.0.0.1 状态回传服务
│   │   ├── config.rs        # 配置持久化、帐号数据目录隔离
│   │   ├── commands.rs      # Tauri 命令
│   │   └── update.rs        # 在线更新检查
│   └── tauri.conf.json
├── docs/ANALYSIS.md         # 原版程序逆向分析笔记
└── .github/workflows/       # CI 自动构建发布
```

## 免责声明

- 本项目仅供个人学习与研究，请遵守联通云手机服务条款
- 自动保活可能违反服务商使用政策，产生的账号风险由使用者自行承担
- 严禁用于任何违法违规用途

## License

MIT © xixka
