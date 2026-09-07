#!/bin/bash
# 空闲降频真实引擎冒烟（tick 自适应降频，方案 A）
# =====================================================
# 本地静态页 + 真实 Rust 引擎（cargo build 后的 debug 二进制）跑 CPK_SMOKE，
# CPK_IDLE_AFTER_SEC=5 快速触发空闲，验证：
#   ①日志出现「无观看/无操作 5s，tick 降频 1s→5s、采样 5s→15s」
#   ②运行中 healthz 的 tickIdle=true
#   ③空闲态 ticks 仍每 ~5s 前进（保活动作周期不变——墙钟门控）
#   ④smoke 结束 PASS（browser=running && ticks≥5）
# 用法：bash test_idle_smoke.sh [二进制路径]（默认 仓库根/target/debug/cloudphonekeep，根 workspace 产物）
set -u
# 二进制路径解析为绝对路径（后面会 cd，相对路径会失效）
SELF_DIR=$(cd "$(dirname "$0")" && pwd)
BIN="${1:-$SELF_DIR/../../target/debug/cloudphonekeep}"
if [ ! -x "$BIN" ]; then echo "二进制不存在：$BIN（先 cargo build）"; exit 1; fi

# Chrome 可执行：CPK_SHELL 显式指定优先，否则探测常见路径
SHELL_BIN="${CPK_SHELL:-}"
if [ -z "$SHELL_BIN" ]; then
  for p in /usr/bin/chromium /usr/bin/chromium-browser /usr/bin/google-chrome \
           /usr/bin/google-chrome-stable /usr/bin/chrome-headless-shell; do
    [ -x "$p" ] && SHELL_BIN="$p" && break
  done
fi
if [ -z "$SHELL_BIN" ]; then echo "未找到 Chrome：请 export CPK_SHELL=/path/to/chrome"; exit 1; fi

TESTS=$SELF_DIR
DATA=$(mktemp -d /tmp/cpk-idle-XXXX)
PORT_HTTP=8932   # 静态页（repro_page.html）
PORT_ENG=8988    # 引擎 HTTP

cd "$TESTS"
python3 -m http.server $PORT_HTTP --bind 127.0.0.1 >/dev/null 2>&1 &
HTTP_PID=$!
CPK_URL=http://127.0.0.1:$PORT_HTTP/repro_page.html \
CPK_CHROME_BIN="$SHELL_BIN" \
CPK_DATA_DIR="$DATA" \
CPK_REPORT_PORT=$PORT_ENG \
CPK_FPS=5 \
CPK_IDLE_AFTER_SEC=5 \
CPK_SMOKE=1 CPK_SMOKE_SECONDS=60 \
"$BIN" > "$DATA/stdout.log" 2>&1 &
ENG_PID=$!
trap 'kill $ENG_PID $HTTP_PID 2>/dev/null; wait 2>/dev/null' EXIT

probe() {
  curl -s "http://127.0.0.1:$PORT_ENG/healthz" | python3 -c \
    "import json,sys; d=json.load(sys.stdin); print('tickIdle=', d['tickIdle'], ' page=', d['page'], ' ticks=', d['ticks'])" 2>/dev/null \
    || echo "(引擎尚在启动)"
}

sleep 30; echo "—— 30s（应已空闲降频）healthz:"; probe
sleep 15; echo "—— 45s（ticks 应比 30s 时 +3 ≈ 每 5s 前进）healthz:"; probe
sleep 18; echo "—— 结束日志:"
grep -E "tick 降频|tick 恢复|smoke 结束" "$DATA/stdout.log" | tail -3

PASS=1
grep -q "tick 降频 1s→5s" "$DATA/stdout.log" || { echo "FAIL: 未见降频迁移日志"; PASS=0; }
grep -q "PASS$" <(grep "smoke 结束" "$DATA/stdout.log") || { echo "FAIL: smoke 未 PASS"; PASS=0; }
[ "$PASS" = 1 ] && echo "PASS: 空闲降频真实引擎冒烟全部通过"
exit $((1 - PASS))
