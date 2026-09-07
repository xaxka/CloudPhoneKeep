# Windows 版（Tauri）配置

Windows 版为图形程序：配置入口是设置窗口（「新开账号」），不走环境变量。
CLI 版（Linux / Docker / 裸机）的环境变量全表见
[../../cli/docs/configuration.md](../../cli/docs/configuration.md)。

- **配置入口**：设置窗口（「新开账号」）——平台、手机号（缓存目录名）、老板键
  索引、分辨率、触点光标等；配置落盘 `config.json`
- **数据目录**（`AppData\LocalLow\CloudPhoneKeep`，不写注册表）：

```
C:\Users\<用户名>\AppData\LocalLow\CloudPhoneKeep\
├── config.json          # 全部帐号配置
├── cpk-YYYYMMDD.log     # 程序级日志（按天滚动，保留 7 天）
├── panic-pPID.log       # 若程序异常崩溃，原因记录于此
├── 1/                   # 目录名填「1」的帐号：浏览器数据（Cookie/登录态/缓存）
│   └── cpk-YYYYMMDD.log #   该帐号的日志（与数据同目录）
└── 138xxxx1234/         # 目录名填「138xxxx1234」的帐号 …
```

多实例共用数据根目录，按帐号目录名隔离；登录态随目录保留。
