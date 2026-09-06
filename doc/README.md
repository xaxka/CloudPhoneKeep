# CloudPhoneKeep 技术文档

README 只保留快速上手；原理、规则、配置、排查等长文都在本目录：

| 文档 | 内容 |
| :--- | :--- |
| [architecture.md](architecture.md) | 双平台架构总览、看门狗驱动模型、自动恢复分级、aardio→Tauri 源码复刻对照、WebView2 调优 |
| [keepalive-rules.md](keepalive-rules.md) | **保活注入规则**：双定时器、移动/联通选择器明细、弹窗分级兜底、`shared/keepalive.inject.js` 修改指引 |
| [diagnostics.md](diagnostics.md) | 诊断日志级别表、典型排查流程、healthz 字段、常见现象处理 |
| [configuration.md](configuration.md) | 配置参考：Windows 数据目录 / Linux 环境变量全表 |
| [linux-deploy.md](linux-deploy.md) | Docker 部署：多架构镜像（amd64/arm64）、控制页首次登录、多账号、CI 流程、Linux FAQ |
