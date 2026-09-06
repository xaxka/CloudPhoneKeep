# 配置参考

## Windows 版

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

## Linux 版环境变量

| 变量 | 默认 | 说明 |
| :--- | :--- | :--- |
| `CPK_PLATFORM` | `mobile` | 平台：`mobile` 移动云手机 / `unicom` 联通云手机 |
| `CPK_ACCOUNT` | `account1` | 账号名（数据目录名、日志标识） |
| `CPK_URL` | 平台默认 | 覆盖云手机入口 URL（调试/私有部署用） |
| `CPK_WIDTH` / `CPK_HEIGHT` | 414×896（mobile） | 窗口分辨率 |
| `CPK_KEEP_ALIVE` | `1` | 保活总开关 |
| `CPK_INTERVAL_MS` | `5000` | actionTick 动作周期（1000-600000ms） |
| `CPK_SIMULATE_ACTIVITY` | `1` | 空闲鼠标活动模拟 |
| `CPK_BLOCK_CONTEXT_MENU` | `1` | 屏蔽页面右键菜单 |
| `CPK_PAGE_TIMER` | `0` | 页内 setInterval 驱动（默认关：宿主 CDP 看门狗驱动） |
| `CPK_REPORT_PORT` | `8088` | 回环上报/控制页端口（0=自动） |
| `CPK_BIND` | `0.0.0.0` | 上述端口绑定地址（`127.0.0.1` 最保守；默认配合端口映射/防火墙） |
| `CPK_CONTROL_TOKEN` | 空 | 控制页/截图/触摸端点的访问令牌（强烈建议公网可达时设置） |
| `CPK_CDP_PORT` | `0` | Chromium DevTools 固定端口（0=自动分配；固定端口可用于外部 DevTools） |
| `CPK_CHROME_BIN` | `/opt/chrome-headless-shell/chrome-headless-shell` | 浏览器二进制路径（镜像内为 CfT chrome-headless-shell；调试时可指向其他 Chrome） |
| `CPK_HEADLESS` | `0` | 仅换用完整 Chromium 时置 1（加 `--headless=new`）；镜像内 headless-shell 本身即无头 |
| `CPK_NO_SANDBOX` | `1` | 容器内通常需关闭 Chromium 沙箱 |
| `CPK_UA_MODE` | `windows` | UA 策略：`windows` 伪装 Windows Chrome / `auto` 去 Headless 字样 / `none` 原样 |
| `CPK_LANG` | `zh-CN` | Chromium UI 语言 |
| `TZ` | `Asia/Shanghai` | 时区（日志时间戳 + Chromium） |
| `CPK_DATA_DIR` | `/data` | 数据根目录（Profile + 日志） |
| `CPK_EXTRA_CHROME_ARGS` | 空 | 透传给 Chromium 的额外参数（如 `--js-flags=--max-old-space-size=512` 压 V8 堆） |
| `CPK_TICK_FAIL_RELOAD` | `10` | tick 连续失败 N 次后导航回首页 |
| `CPK_FROZEN_RELOAD` | `3` | 状态冻结 N 个采样周期后导航回首页 |
| `CPK_BEAT_STALE_SEC` | `180` | 心跳超龄 N 秒硬重启浏览器 |
| `CPK_FPS` | `25` | 实时画面帧率上限（1-60；控制台「设置→帧率」可运行时调整，此为初始值） |
| `CPK_SELFTEST` | `0` | 自检模式（不启动 Chromium，CI 用） |
| `CPK_SMOKE` / `CPK_SMOKE_SECONDS` | `0` / `60` | 冒烟模式（跑 N 秒按指标退出，CI 用） |

> 数值类变量超范围会被自动钳回边界并落 `[error]` 日志。
