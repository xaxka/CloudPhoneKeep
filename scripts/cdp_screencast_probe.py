#!/usr/bin/env python3
"""CDP 直连探测：绕过引擎，验证 chrome-headless-shell 的 screencast 行为。
变量：quality / everyNthFrame / maxFrameRate / ack 时机；对照 animation 与 rAF 驱动动画。
结论维度：10s 内 Page.screencastFrame 事件数、停止时间点。"""
import json, subprocess, sys, time, urllib.request
import websocket

CHROME = "/home/z/.cache/puppeteer/chrome-headless-shell/linux-152.0.7977.54/chrome-headless-shell-linux64/chrome-headless-shell"
PORT = 9333

PAGE = """<!doctype html><html><head><style>
body{margin:0;background:#111}
.c{width:80px;height:80px;border-radius:12px;position:absolute;
background:linear-gradient(90deg,#f36,#36f);animation:mv 2s infinite alternate}
@keyframes mv{to{transform:translateX(300px)}}
</style></head><body>
<div class="c" style="top:100px"></div>
<script>
// rAF 驱动的第二动画（每帧改 top）：合成器 vs rAF 帧源对照
var d2=document.createElement('div');d2.className='c';d2.style.top='300px';
d2.style.animation='none';document.body.appendChild(d2);
var t=0;
function loop(){t+=4;if(t>700)t=0;d2.style.top=(300+t)+'px';requestAnimationFrame(loop)}
requestAnimationFrame(loop);
</script></body></html>"""

proc = subprocess.Popen([
    CHROME, f"--remote-debugging-port={PORT}", "--remote-debugging-address=127.0.0.1",
    "--remote-allow-origins=*", "--window-size=414,896", "--force-device-scale-factor=1",
    "--disable-gpu", "--no-sandbox", "--disable-dev-shm-usage", "about:blank",
], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

def wait_ws():
    for _ in range(60):
        try:
            v = json.load(urllib.request.urlopen(f"http://127.0.0.1:{PORT}/json/version", timeout=2))
            return v["webSocketDebuggerUrl"]
        except Exception:
            time.sleep(0.5)
    raise SystemExit("devtools 端点未起")

ws_url = wait_ws()
ws = websocket.create_connection(ws_url, timeout=3)
_id = [0]
def send(method, params=None):
    _id[0] += 1
    ws.send(json.dumps({"id": _id[0], "method": method, "params": params or {}}))
    return _id[0]

send("Target.createTarget", {"url": "data:text/html;charset=utf-8," +
      __import__("urllib.parse", fromlist=["quote"]).quote(PAGE)})
time.sleep(2)

# 找 page target 的 session
targets = None
for _ in range(10):
    i = send("Target.getTargets")
    while True:
        m = json.loads(ws.recv())
        if m.get("id") == i:
            targets = m["result"]["targetInfos"]; break
    pages = [t for t in targets if t["type"] == "page" and t["url"].startswith("data:")]
    if pages: break
    time.sleep(0.5)
sid = pages[0]["targetId"]
i = send("Target.attachToTarget", {"targetId": sid, "flatten": True})
while True:
    m = json.loads(ws.recv())
    if m.get("id") == i:
        session = m["result"]["sessionId"]; break

print("session 建立成功，开始 screencast（quality=50 everyNthFrame=1 maxFrameRate=25）")
send("Page.startScreencast", {"format": "jpeg", "quality": 50, "everyNthFrame": 1, "maxFrameRate": 25}, session=None) if False else send("Page.startScreencast", {"format": "jpeg", "quality": 50, "everyNthFrame": 1, "maxFrameRate": 25})
# 修正：带 sessionId 的发送（flatten 模式下放外层）
ws.send(json.dumps({"id": _id[0] + 1000, "method": "Page.startScreencast",
                    "params": {"format": "jpeg", "quality": 50, "everyNthFrame": 1, "maxFrameRate": 25},
                    "sessionId": session}))
_id[0] += 1000

frames = 0
acks = 0
t0 = time.time()
last_frame_t = t0
per_sec = {}
ws.settimeout(0.2)
while time.time() - t0 < 10:
    try:
        m = json.loads(ws.recv())
    except Exception:
        continue
    if m.get("sessionId") != session:
        continue
    if m.get("method") == "Page.screencastFrame":
        frames += 1
        now = time.time()
        last_frame_t = now
        per_sec[int(now - t0)] = per_sec.get(int(now - t0), 0) + 1
        fs = m["params"].get("sessionId")
        if fs is not None:
            ws.send(json.dumps({"id": 900000 + frames, "method": "Page.screencastFrameAck",
                                "params": {"sessionId": fs}, "sessionId": session}))
            acks += 1

dur = time.time() - t0
print(f"10s screencastFrame 事件={frames} acks={acks}")
print(f"每秒分布={dict(sorted(per_sec.items()))}")
print(f"最后帧到达时刻={last_frame_t - t0:.1f}s")
proc.terminate()
