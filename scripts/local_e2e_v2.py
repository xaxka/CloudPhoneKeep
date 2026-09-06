#!/usr/bin/env python3
"""端到端验证（单命令内跑完，适配沙坑：引擎后台进程不跨 Bash 命令存活）：
1. 动画页帧率：/stream.mjpg 连续读 10s 数帧（旧版实测 ~1fps，新版目标 ≥8fps）
2. 触摸 fire：/touch start→end 响应时间 + 页面点击回调（经 /log 收集）
3. 拖动流：move 序列正常受理
"""
import os, sys, time, threading, urllib.request, urllib.parse, subprocess, signal

CHROME = "/home/z/.cache/puppeteer/chrome-headless-shell/linux-152.0.7977.54/chrome-headless-shell-linux64/chrome-headless-shell"
BIN = "/home/z/my-project/cloudphonekeep/linux/target/release/cloudphonekeep"
PORT = 18099          # python http.server（测试页 origin，兼收 /log）
CPK_PORT = 18098      # 引擎 report server
DATA = "/tmp/cpk-e2e-v2-data"

TEST_PAGE = """<!doctype html><html><head><style>
body{margin:0;font:16px sans-serif;background:#111;color:#fff}
.box{width:80px;height:80px;border-radius:12px;position:absolute;top:46%;left:0;
background:linear-gradient(90deg,#f36,#36f);animation:mv 2s infinite alternate}
@keyframes mv{to{transform:translateX(300px)}}
#btn{padding:26px 0;background:#333;border-radius:8px;text-align:center;margin-top:130px}
</style></head><body>
<div class="box"></div>
<div id="btn">点我（0）</div>
<script>
var n=0;var btn=document.getElementById('btn');
function log(m){try{fetch('/log',{method:'POST',
headers:{'content-type':'application/x-www-form-urlencoded'},
body:'level=click&msg='+encodeURIComponent(m)})}catch(e){}}
btn.addEventListener('click',function(){n++;btn.textContent='点我（'+n+'）';log('click#'+n)});
</script></body></html>""".encode("utf-8")

clicks = []

class Handler(__import__("http.server", fromlist=["BaseHTTPRequestHandler"]).BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        self.send_response(200)
        self.send_header("content-type", "text/html; charset=utf-8")
        self.end_headers()
        self.wfile.write(TEST_PAGE)
    def do_POST(self):
        n = int(self.headers.get("content-length", 0))
        body = self.rfile.read(n).decode("utf-8", "ignore")
        form = urllib.parse.parse_qs(body)
        msg = form.get("msg", [""])[0]
        if msg.startswith("click#"):
            clicks.append(msg)
        self.send_response(204); self.end_headers()

srv = __import__("http.server", fromlist=["ThreadingHTTPServer"]).ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
threading.Thread(target=srv.serve_forever, daemon=True).start()

os.system(f"rm -rf {DATA}")
env = dict(os.environ,
    CPK_CHROME_BIN=CHROME, CPK_URL=f"http://127.0.0.1:{PORT}/t.html",
    CPK_HOME_URI=f"http://127.0.0.1:{PORT}/t.html",
    CPK_REPORT_PORT=str(CPK_PORT), CPK_BIND="127.0.0.1",
    CPK_DATA_DIR=DATA, CPK_SIMULATE_ACTIVITY="0",
    CPK_ACCOUNT="e2e", CPK_PLATFORM="mobile")
proc = subprocess.Popen([BIN], env=env,
                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

def get(path, timeout=8):
    return urllib.request.urlopen(f"http://127.0.0.1:{CPK_PORT}{path}", timeout=timeout)

def post(path, body, timeout=8):
    req = urllib.request.Request(f"http://127.0.0.1:{CPK_PORT}{path}",
        data=body.encode(), headers={"content-type": "application/x-www-form-urlencoded"})
    return urllib.request.urlopen(req, timeout=timeout)

ok = False
for _ in range(60):  # 等 page=ok（浏览器启动+导航+注入）
    try:
        v = get("/healthz", 4).read().decode()
        if '"page":"ok"' in v:
            ok = True; break
    except Exception:
        pass
    time.sleep(0.5)
if not ok:
    print("FAIL: page 未达 ok"); proc.terminate(); sys.exit(1)
print("page=ok ✓")

# ── 1) 帧率：动画页连读 10s ─────────────────────────────────
resp = get("/stream.mjpg", 20)
t0 = time.time(); frames = 0; buf = b""
last_stat = 0
per_sec = []
while time.time() - t0 < 10:
    chunk = resp.read(65536)
    if not chunk: break
    buf += chunk
    while b"\xff\xd8" in buf:
        buf = buf[buf.index(b"\xff\xd8") + 2:]
        frames += 1
    sec = int(time.time() - t0)
    if sec != last_stat:
        per_sec.append(frames - (per_sec[-1] if per_sec else 0)); last_stat = sec
fps = frames / (time.time() - t0)
print(f"动画页 10s 帧数={frames} fps={fps:.1f} 每秒分布={per_sec}")

# ── 2) 触摸 fire：tap 响应时间 + click 回调 ─────────────────
clicks.clear()
t1 = time.time()
post("/touch", "phase=start&x=207&y=160")
post("/touch", "phase=end&x=207&y=160")
dt = (time.time() - t1) * 1000
print(f"tap(start+end) HTTP 往返={dt:.0f}ms")
time.sleep(1.2)
print(f"页面点击回调={clicks}（期望 click#1）")

# ── 3) 拖动流：start→5×move→end ────────────────────────────
post("/touch", "phase=start&x=100&y=412")
for i in range(5):
    post("/touch", f"phase=move&x={100+i*40}&y={412+i*10}")
post("/touch", "phase=end&x=300&y=462")
print("拖动 7 事件受理 ✓")

proc.send_signal(signal.SIGTERM)
try: proc.wait(10)
except Exception: proc.kill()
srv.shutdown()

# 日志侧检查：无 WS 错误
log_files = []
for root, _, files in os.walk(f"{DATA}/logs"):
    log_files += [os.path.join(root, f) for f in files]
text = "".join(open(f, encoding="utf-8", errors="ignore").read() for f in log_files)
ws_err = text.count("WS:")
print(f"引擎日志 WS 传输错误={ws_err} 次（期望 0）")

verdict = fps >= 8 and clicks and ws_err == 0
print("=" * 50)
print(f"结论：帧率 {'PASS' if fps >= 8 else 'FAIL'}（{fps:.1f}fps，旧版实测 ~1fps）")
print(f"结论：触摸 {'PASS' if clicks else 'FAIL'}（{dt:.0f}ms 往返 + {clicks}）")
sys.exit(0 if verdict else 1)
