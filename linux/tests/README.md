# CloudPhoneKeep Linux 测试/复现脚本

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

## 用法

```bash
cd linux/tests
node e2e_repro.js          # 主回归：末行应输出「控制页发出了 3 次 end」
python3 mock_control_page.py   # 起控制页 mock：http://127.0.0.1:8899/
node test_wall_clock_gate.js   # 墙钟门控（无外部依赖）
bash test_idle_smoke.sh       # 空闲降频引擎冒烟（先 cd ../ && cargo build）
```

脚本从 `../../shared/` 读取控制页与保活脚本（与生产同一份文件），
修改 shared 后重跑即验证。

历史背景：`e2e_repro.js` 曾实证控制页 `fin` 吞 end 的根因
（所有 gap≥60ms 的触摸交互无收尾→点击无效）；`test_ack_*.py`
实证 ack 门控是 Chrome 端节流的可靠阀门。详见仓库提交历史。
