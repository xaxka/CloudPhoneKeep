#!/usr/bin/env python3
"""probe v3：复刻引擎的完整环境——全套 Chrome 启动参数 + http 页面 +
保活脚本注入（addScriptToEvaluateOnNewDocument）。若仍 60fps，则差异收窄到
ws.rs/engine 主循环；若掉到 ~1fps，逐项拆除定位。"""
import json, subprocess, sys, time, urllib.request, urllib.parse, threading
import websocket
from http.server import BaseHTTPRequestHandler, HTTPServer

CHROME = "/home/z/.cache/puppeteer/chrome-headless-shell/linux-152.0.7977.54/chrome-headless-shell-linux64/chrome-headless-shell"
PORT = 9335
WEB = 18096

PAGE = """<!doctype html><html><head><style>
body{margin:0;background:#111}
.c{width:80px;height:80px;border-radius:12px;position:absolute;
background:linear-gradient(90deg,#f36,#36f);animation:mv 2s infinite alternate}
@keyframes mv{to{transform:translateX(300px)}}
</style></head><body><div class="c" style="top:100px"></div>
<script>
var d2=document.createElement('div');d2.className='c';d2.style.top='300px';
d2.style.animation='none';document.body.appendChild(d2);
var t=0;
function loop(){t+=4;if(t>700)t=0;d2.style.top=(300+t)+'px';requestAnimationFrame(loop)}
requestAnimationFrame(loop);
window.__CPK_TICK__=function(){return 1};
</script></body></html>"""

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        self.send_response(200)
        self.send_header("content-type", "text/html; charset=utf-8")
        self.end_headers()
        self.wfile.write(PAGE.encode())

srv = HTTPServer(("127.0.0.1", WEB), H)
threading.Thread(target=srv.serve_forever, daemon=True).start()

# 引擎的全套参数（engine.rs launch_chrome，含 --hide-scrollbars 等）
ARGS = [
    f"--user-data-dir=/tmp/probe-v3-profile",
    f"--remote-debugging-port={PORT}", "--remote-debugging-address=127.0.0.1",
    "--remote-allow-origins=*", "--window-size=414,896", "--force-device-scale-factor=1",
    "--hide-scrollbars", "--no-first-run", "--no-default-browser-check", "--disable-gpu",
    "--disable-dev-shm-usage", "--disable-crash-reporter",
    "--disable-background-timer-throttling", "--disable-backgrounding-occluded-windows",
    "--disable-renderer-backgrounding", "--disable-background-networking",
    "--disable-component-update", "--disable-sync",
    "--disable-features=Translate,MediaRouter,OptimizationHints", "--mute-audio",
    "--autoplay-policy=no-user-gesture-required", "--lang=zh-CN",
    "--no-sandbox", "--disable-setuid-sandbox", "about:blank",
]
import os
os.system("rm -rf /tmp/probe-v3-profile")
proc = subprocess.Popen([CHROME] + ARGS, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

def wait_ws():
    for _ in range(60):
        try:
            v = json.load(urllib.request.urlopen(f"http://127.0.0.1:{PORT}/json/version", timeout=2))
            return v["webSocketDebuggerUrl"]
        except Exception:
            time.sleep(0.5)
    raise SystemExit("devtools 端点未起")

ws = websocket.create_connection(wait_ws(), timeout=3)
_id = [0]
def send(method, params=None, session=None):
    _id[0] += 1
    m = {"id": _id[0], "method": method, "params": params or {}}
    if session:
        m["sessionId"] = session
    ws.send(json.dumps(m))
    return _id[0]

# 复刻引擎 attach_all：createTarget + attachToTarget(flatten) + 注入脚本 + 导航
send("Target.createTarget", {"url": "about:blank"})
time.sleep(1)
i = send("Target.getTargets")
while True:
    m = json.loads(ws.recv())
    if m.get("id") == i:
        page = [t for t in m["result"]["targetInfos"] if t["type"] == "page"][0]
        break
i = send("Target.attachToTarget", {"targetId": page["targetId"], "flatten": True})
while True:
    m = json.loads(ws.recv())
    if m.get("id") == i:
        session = m["result"]["sessionId"]; break

# 保活脚本注入（近似 keepalive.inject.js 的注册方式）
send("Page.addScriptToEvaluateOnNewDocument", {"source": "window.__CPK_INJECTED__=true;"}, session)
send("Page.enable", {}, session)
i = send("Page.navigate", {"url": f"http://127.0.0.1:{WEB}/t.html"}, session)
time.sleep(2.5)

send("Page.startScreencast", {"format": "jpeg", "quality": 50, "everyNthFrame": 1, "maxFrameRate": 25}, session)

frames = 0
per_sec = {}
t0 = time.time()
ws.settimeout(0.05)
next_tick = t0 + 1
while time.time() - t0 < 8:
    if time.time() >= next_tick:  # 引擎 tick：每 1s evaluate
        next_tick += 1
        send("Runtime.evaluate", {"expression": "(function(){try{window.__CPK_TICK__&&window.__CPK_TICK__();return 'ok'}catch(e){return 'e'}})()", "returnByValue": True}, session)
    try:
        m = json.loads(ws.recv())
    except Exception:
        continue
    if m.get("sessionId") != session:
        continue
    if m.get("method") == "Page.screencastFrame":
        frames += 1
        per_sec[int(time.time() - t0)] = per_sec.get(int(time.time() - t0), 0) + 1
        fs = m["params"].get("sessionId")
        ws.send(json.dumps({"id": 900000 + frames, "method": "Page.screencastFrameAck",
                            "params": {"sessionId": fs}, "sessionId": session}))

print(f"probe v3（引擎全套参数+http 页+注入+tick）：帧={frames} 每秒={dict(sorted(per_sec.items()))}")
proc.terminate()
srv.shutdown()
