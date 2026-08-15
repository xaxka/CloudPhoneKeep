# CloudPhoneKeep 云手机保活

> 联通云手机（uPhone）网页版多开保活工具 — **Tauri 2 + WebView2 + Rust** 实现。

![Tauri](https://img.shields.io/badge/Tauri-2.x-blue) ![License](https://img.shields.io/badge/License-MIT-green) ![Platform](https://img.shields.io/badge/Platform-Windows-lightgrey)

## 功能一览

- **多帐号多开**：最多 9 个窗口（槽位 1~9），每槽位独立 WebView2 数据目录，Cookie / 缓存 / 登录态完全隔离
- **老板键**：任意界面按 `Ctrl+1` ~ `Ctrl+9` 瞬间隐藏 / 呼出对应帐号窗口（系统全局热键注册）
- **系统托盘**：显示 / 隐藏全部、逐帐号切换、打开主面板、退出
- **自动保活引擎**：页内脚本 + Rust 看门狗双重驱动，窗口隐藏后保活不中断（绕过 Chromium 后台定时器节流）
- **弹窗自动处理**：
  - 试用弹窗自动点「立即启用云手机」
  - 「无法连接」自动点「再次尝试」
  - 详情页自动点「进入云机」
  - 到期弹窗自动点「知道了」并发出系统通知
  - 检测到退回首页（云机退出）时状态上报 + 系统通知
- **横竖屏旋转**（交换宽高）、**窗口置顶**
- **自定义触点光标**（本地内嵌资源，零外部依赖）
- **屏蔽页面右键菜单**
- **指定 Cookie** 注入（页内注入 + 独立目录持久化）
- **空闲模拟鼠标活动**防掉线
- **在线更新检查**：手动触发，仅提示新版本，绝不自动下载或执行任何文件

## 安全性说明

- 不含任何自动更新 / 自动下载执行逻辑：更新检查仅在你手动点击时请求一次 GitHub Releases API 读取版本号用于提示，跳转下载页也需你手动点击
- 触点光标为**本地内嵌资源**（`src-tauri/assets/cursor.b64`），零外部资源依赖
- 对外网络行为只有两类：云手机页面本身的正常访问，以及可选的、由你手动触发的更新检查（可自定义为任意你信任的地址，留空即完全禁用）
- 无键盘钩子（老板键使用系统全局热键注册，而非 `SetWindowsHookEx`）
- 无内存注入、无自修改代码、无加壳；自动点击全部通过标准 DOM `click()` 完成
- 全部代码开源可审计

## 保活原理

页面加载后注入的保活脚本会周期执行（窗口可见时由页内 `setInterval` 驱动；窗口被隐藏后由 Rust 侧看门狗周期 `eval` 驱动）：

1. 出现 `.try-content`（试用提示）→ 自动点击 `.try-btn`「立即启用云手机」
2. 出现 `.phone-dialog-wrap`「无法连接」→ 自动点击「再次尝试 / 重试 / 重新连接」
3. 出现 `.detail-info-container` 详情页 → 自动点击 `.enter-intance`「进入云机」
4. 出现 `.van-dialog__confirm` 到期弹窗 → 自动点击「知道了」并发出系统通知
5. 检测到 `.title-bar`（退回首页，即云机退出）→ 状态上报 + 系统通知
6. 空闲周期内向页面派发轻微 `mousemove` 事件，降低会话闲置断开概率
7. 全部状态通过 `http://127.0.0.1:<port>/report` 回环上报（Chromium 允许 HTTPS 页面访问环回地址，不受混合内容限制），管理面板实时显示每个帐号的保活状态与自动点击次数

## 使用方法

1. 从 [Releases](https://github.com/xixka/CloudPhoneKeep/releases) 下载最新的安装包或免安装 exe（依赖 WebView2 运行时，安装器会自动引导下载）
2. 打开主面板，在「帐号 N」卡片中填写：
   - **缓存目录名/帐号标识**：建议填手机号，各窗口登录态完全隔离
   - 其余项一般保持默认即可
3. 点击「启动」，在打开的窗口中完成联通云手机登录
4. 之后即使隐藏窗口，保活引擎仍持续运行
5. **老板键**：任意界面按 `Ctrl+1` ~ `Ctrl+9` 瞬间隐藏/呼出对应帐号窗口；也可用托盘菜单
6. 「保存配置」后下次可一键「全部启动」

## 构建

```bash
# 需要 Rust 1.77+ 与 Windows 环境（WebView2 SDK 由 tauri 自动处理）
npm install -g @tauri-apps/cli@^2
npx tauri build      # 产出 exe 与 NSIS 安装包
# 或开发调试
npx tauri dev
```

本仓库前端为纯静态 HTML/JS（`ui/` 目录），无 Node 构建步骤。推送代码后 GitHub Actions 会自动构建并发布到 `dev` 预发布版。

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
└── .github/workflows/       # CI 自动构建发布
```

## 免责声明

- 本项目仅供个人学习与研究，请遵守联通云手机服务条款
- 自动保活可能违反服务商使用政策，产生的账号风险由使用者自行承担
- 严禁用于任何违法违规用途

## License

MIT © xixka
