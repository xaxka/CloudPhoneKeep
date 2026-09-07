# CloudPhoneKeep CLI 测试/复现脚本

本地复现与回归验证脚本集（不依赖 Rust 引擎：Node/Python 直接驱动 Chrome CDP，
复刻生产链路：控制页 → 引擎 HTTP 端点 → CDP 输入 → 云机页面）。

## 依赖

- Node.js ≥ 18（`node`、`fetch`、WebSocket 由脚本内实现，无需 npm install）
- Python 3（`websockets`、`pillow` 可选，仅个别脚本需要）
- 任意 Chrome/Chromium。找不到时显式指定：

```bash
export CPK_SHELL=/path/to/chrome        # chromium / google-chrome / chrome-headless-shell 均可
```

默认自动探测 `/usr/bin/chromium` 等常见路径。

## 脚本清单

| 脚本 | 用途 |
|------|------|
| `e2e_repro.js` | **主回归**：双 Chrome 端到端——控制页轻点/拖动 → mini 引擎 → 云机测试页（`repro_page.html` + 保活注入同生产），验证 touch start/end 与合成 click |
| `e2e_repro_fast.js` | 同上的快速版（更短等待） |
| `e2e_139.js` | 真实 139 H5（cloudphoneh5.buy.139.com）全链路验证，需外网 |
| `mouse_path_check.js` | 控制页鼠标路径回归（纯点击 → mClickSeq 完整序列） |
| `probe_control.js` | 控制页事件流探针（pointer/touch 各相位发出诊断） |
| `mock_control_page.py` | mock 引擎 HTTP + MJPEG 画面流，配合浏览器/截图工具做控制页 UI 视觉验证（需 pillow） |
| `repro_cdp_touch.py` | CDP 触摸协议复现（touchEnd 空点语义、setTouchEmulationEnabled 对照） |
| `test_everynth.py` | Chrome startScreencast 参数实验（everyNthFrame/maxFrameRate 实际效果） |
| `test_ack_pacing.py` | screencastFrameAck 门控实验（停止 ack 是否压制 Chrome 出帧） |
| `test_ack_hold.py` | ack 持有语义实验（持有超时死流、stop+start 复活） |
| `test_wall_clock_gate.js` | 保活脚本墙钟门控验证（空闲降频下 actionTick 周期恒 ≈ intervalMs，不被 tick 周期拉长） |
| `test_idle_smoke.sh` | 空闲降频真实引擎冒烟（需先 `cargo build`；降频迁移日志 + healthz tickIdle + 空闲态 ticks 每 5s 前进 + smoke PASS） |
| `ci_smoke.sh` | **CI 冒烟档**：真实引擎 + chrome 端到端（本地动画页/注入/画面流出帧/控制链路/healthz 字段/smoke PASS）。CI 用 install.sh 装好的 /opt 产物跑 |
| `knob_e2e.py` | 三旋钮（fps/quality/scale）热更新端到端：healthz 即刻回读 + 帧实际尺寸/实测帧率证真实生效 + 越界 400 |
| `auth_e2e.py` | Basic Auth 端到端：无凭据 401×4 + WWW-Authenticate + 免鉴权通道照常 + 心跳不断 + 凭据/token 叠加 |
| `check_md_links.py` | 文档冒烟：全仓 md 相对链接有效性检查（改文档后随手跑，退出码可直接进脚本/CI） |

双端组织约定：CLI（Linux/Docker）侧测试统一在本目录；Tauri（Windows）侧
将来如需 e2e，对应放 `src-tauri/tests/`，命名与端口约定与本地保持一致。

## 用法

```bash
cd cli/tests
node e2e_repro.js          # 主回归：末行应输出「控制页发出了 3 次 end」
python3 mock_control_page.py   # 起控制页 mock：http://127.0.0.1:8899/
node test_wall_clock_gate.js   # 墙钟门控（无外部依赖）
bash test_idle_smoke.sh       # 空闲降频引擎冒烟（先在仓库根 cargo build，产物在根 target/）
bash ci_smoke.sh              # CI 冒烟档（需先 cargo build --release）
python3 knob_e2e.py           # 三旋钮热更新（需 chrome-headless-shell）
python3 auth_e2e.py           # Basic Auth 端到端（需 chrome-headless-shell）
python3 check_md_links.py     # 全仓 md 链接检查（无外部依赖）
```

真实引擎类脚本（`ci_smoke.sh` / `knob_e2e.py` / `auth_e2e.py` / `test_idle_smoke.sh`）
的路径约定：
- 引擎二进制：`CPK_ENGINE` 环境变量，默认 仓库根 `target/release/cloudphonekeep`
  （`test_idle_smoke.sh` 默认 debug 产物、可用首参覆盖；先 `cargo build [--release]`）；
- Chrome：`CPK_SHELL` 环境变量，否则探测 PATH 常见命令；
- 三者端口互不冲突但共用 8089/8899（`knob_e2e.py` 与 `auth_e2e.py` 顺序跑，勿并行）。

脚本读取的模板与生产同源：保活脚本取 `../../shared/keepalive.inject.js`，
控制页取 `../control_page.html`（cli/ 根，CLI 专属）；修改后重跑即验证。

历史背景：`e2e_repro.js` 曾实证控制页 `fin` 吞 end 的根因
（所有 gap≥60ms 的触摸交互无收尾→点击无效）；`test_ack_*.py`
实证 ack 门控是 Chrome 端节流的可靠阀门。详见仓库提交历史。
