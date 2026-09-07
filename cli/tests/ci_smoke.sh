#!/bin/bash
# CI 冒烟档：真实引擎 + 真实 chrome-headless-shell 端到端
# =====================================================
# 在无登录态的环境验证完整链路（CI 用，本地亦可）：
#   ① 引擎以 CPK_URL 指向本地动画页启动（真浏览器 + 真 CDP + 真注入脚本）
#   ② 控制页 GET / 返回 200 且含 CloudPhoneKeep
#   ③ healthz：browser=running、ticks>0、account 默认 1、conns<=maxConns
#   ④ /stream.mjpg 真出帧（multipart 边界 + JPEG 帧头）
#   ⑤ 控制链路活着：POST /fps → healthz 回读 fps=15
#   ⑥ CPK_SMOKE 结束 PASS（browser=running && ticks>=5）退出码 0
# 用法：bash ci_smoke.sh [引擎二进制]
#   引擎默认 仓库根/target/release/cloudphonekeep（cargo build --release 产物）
#   浏览器用 CPK_SHELL 指定（CI 里由 workflow 下载后传入）
set -u
SELF_DIR=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$SELF_DIR/../.." && pwd)
BIN="${1:-${CPK_ENGINE:-$ROOT/target/release/cloudphonekeep}}"
[ -x "$BIN" ] || { echo "FAIL: 引擎不存在：$BIN（先 cargo build --release）"; exit 1; }
SHELL_BIN="${CPK_SHELL:-}"
if [ -z "$SHELL_BIN" ]; then
  for p in /usr/bin/chrome-headless-shell /usr/bin/chromium /usr/bin/chromium-browser \
           /usr/bin/google-chrome /usr/bin/google-chrome-stable; do
    [ -x "$p" ] && SHELL_BIN="$p" && break
  done
fi
[ -n "$SHELL_BIN" ] || { echo "FAIL: 未找到 Chrome（export CPK_SHELL=/path/to/chrome-headless-shell）"; exit 1; }

PORT_HTTP=8899    # 静态页（anim_page.html）
PORT_ENG=8977     # 引擎 HTTP
DATA=$(mktemp -d /tmp/cpk-ci-smoke-XXXX)
PASS=1
note() { printf '  [%s] %s\n' "$1" "$2"; }

cd "$SELF_DIR"
python3 -m http.server $PORT_HTTP --bind 127.0.0.1 >/dev/null 2>&1 &
HTTP_PID=$!
CPK_URL=http://127.0.0.1:$PORT_HTTP/anim_page.html \
CPK_CHROME_BIN="$SHELL_BIN" \
CPK_DATA_DIR="$DATA" \
CPK_REPORT_PORT=$PORT_ENG \
CPK_SMOKE=1 CPK_SMOKE_SECONDS=45 \
"$BIN" > "$DATA/engine.log" 2>&1 &
ENG_PID=$!
trap 'kill $ENG_PID $HTTP_PID 2>/dev/null; wait 2>/dev/null' EXIT

# —— 等引擎就绪（healthz browser=running）——
READY=0
for i in $(seq 1 40); do
  H=$(curl -s "http://127.0.0.1:$PORT_ENG/healthz" 2>/dev/null) || true
  if echo "$H" | grep -q '"browser":"running"'; then READY=1; break; fi
  sleep 1
done
if [ "$READY" = 1 ]; then note PASS "引擎就绪（browser=running，页面 ok）"; else
  note FAIL "引擎 40s 未就绪"; PASS=0; cat "$DATA/engine.log" | tail -20; fi

# —— ① 控制页 ——
BODY=$(curl -s "http://127.0.0.1:$PORT_ENG/" 2>/dev/null)
echo "$BODY" | grep -q "CloudPhoneKeep" && note PASS "控制页 200 且模板在位" \
  || { note FAIL "控制页异常"; PASS=0; }

# —— ② healthz 字段（重新拉取并等 ticks>0，避免用就绪时刻的陈旧快照）——
if [ "$READY" = 1 ]; then
  H=$(curl -s "http://127.0.0.1:$PORT_ENG/healthz" 2>/dev/null)
  for i in $(seq 1 20); do
    TICKS=$(echo "$H" | python3 -c "import json,sys;print(json.load(sys.stdin).get('ticks',0))" 2>/dev/null)
    [ "${TICKS:-0}" -gt 0 ] 2>/dev/null && break
    sleep 1
    H=$(curl -s "http://127.0.0.1:$PORT_ENG/healthz" 2>/dev/null)
  done
  echo "$H" | python3 -c "
import json,sys
d=json.load(sys.stdin)
checks=[
 (d.get('browser')=='running','healthz browser=running'),
 (d.get('ticks',0)>0,'healthz ticks>0（看门狗在跑）'),
 (d.get('account')=='1','healthz account=1（默认单账号）'),
 (0<=d.get('conns',-1)<=d.get('maxConns',0),'healthz conns<=maxConns（连接闸门字段）'),
 (d.get('maxConns')==16,'healthz maxConns=16（CPK_MAX_CONNS 默认）'),
]
for c,m in checks: print(f'  [{\"PASS\" if c else \"FAIL\"}] {m}')
sys.exit(1 if [m for c,m in checks if not c] else 0)" || PASS=0
fi

# —— ③ 画面流真出帧（原始 socket 收首帧）——
if [ "$READY" = 1 ]; then
  python3 - "$PORT_ENG" <<'PYEOF' || { note FAIL "画面流无帧"; PASS=0; }
import socket, sys, time
port=int(sys.argv[1]); got=False
try:
    s=socket.create_connection(("127.0.0.1",port),timeout=20)
    s.sendall(b"GET /stream.mjpg HTTP/1.1\r\nHost: x\r\n\r\n")
    s.settimeout(20); buf=b""; t0=time.time()
    while time.time()-t0<20 and not got:
        d=s.recv(65536)
        if not d: break
        buf+=d
        i=buf.find(b"--cpkframe")
        if i>=0:
            # 帧分部自身的头结束（boundary 行之后的空行）——不是 HTTP 响应头的结束
            j=buf.find(b"\r\n\r\n", i)
            if j>=0 and len(buf)>j+4+100 and buf[j+4:j+6]==b"\xff\xd8":
                got=True
finally:
    try: s.close()
    except Exception: pass
print(f'  [{"PASS" if got else "FAIL"}] 画面流出首帧（multipart + JPEG 魔数）')
sys.exit(0 if got else 1)
PYEOF
fi

# —— ④ 控制链路：POST /fps 即时生效 ——
if [ "$READY" = 1 ]; then
  CODE=$(curl -s -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$PORT_ENG/fps?value=15")
  FPS=$(curl -s "http://127.0.0.1:$PORT_ENG/healthz" | python3 -c "import json,sys;print(json.load(sys.stdin).get('fps'))" 2>/dev/null)
  if [ "$CODE" = "200" ] && [ "$FPS" = "15" ]; then note PASS "POST /fps=15 → healthz 回读 15（控制链路活）"
  else note FAIL "fps 调整失败（http=$CODE fps=$FPS）"; PASS=0; fi
fi

# —— ⑤ 引擎 smoke 退出码（等自然结束）——
wait $ENG_PID; ENG_RC=$?
if [ "$ENG_RC" = "0" ] && grep -q "smoke 结束" "$DATA/engine.log" && grep "smoke 结束" "$DATA/engine.log" | grep -q "PASS$"; then
  note PASS "引擎 smoke PASS（退出码 0）"
else
  note FAIL "引擎 smoke 未 PASS（rc=$ENG_RC）"; grep "smoke 结束" "$DATA/engine.log" | tail -1; PASS=0
fi

if [ "$PASS" = 1 ]; then echo "结果：CI 冒烟全部通过"; exit 0
else echo "结果：CI 冒烟存在失败项"; exit 1; fi
