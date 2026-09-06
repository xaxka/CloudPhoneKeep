#!/usr/bin/env python3
"""probe v2：逐项复刻引擎行为，二分定位帧率瓶颈。
模式：
  A. 纯阻塞 recv + ack            （基线，v1 实测 60fps）
  B. A + 每 1s Runtime.evaluate    （tick 挤帧假设）
  C. A + ack 改为先存帧再 ack       （顺序无关性）
  D. A + rbuf 小缓冲逐帧读          （模拟引擎 fill 64KB 分块）
"""
import json, subprocess, sys, time, urllib.request, urllib.parse
import websocket

CHROME = "/home/z/.cache/puppeteer/chrome-headless-shell/linux-152.0.7977.54/chrome-headless-shell-linux64/chrome-headless-shell"
PORT = 9334

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

TICK = ("(function(){try{if(!window.__CPK_TICK__)return 'noscript';"
        "window.__CPK_TICK__();return 'ok'}catch(e){return 'err:'}})()")

MODE = sys.argv[1] if len(sys.argv) > 1 else "A"
DUR = 8

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

ws = websocket.create_connection(wait_ws(), timeout=3)
_id = [0]
def send(method, params=None, session=None):
    _id[0] += 1
    m = {"id": _id[0], "method": method, "params": params or {}}
    if session:
        m["sessionId"] = session
    ws.send(json.dumps(m))
    return _id[0]

send("Target.createTarget", {"url": "data:text/html;charset=utf-8," + urllib.parse.quote(PAGE)})
time.sleep(2)
i = send("Target.getTargets")
while True:
    m = json.loads(ws.recv())
    if m.get("id") == i:
        pages = [t for t in m["result"]["targetInfos"] if t["type"] == "page" and t["url"].startswith("data:")]
        break
i = send("Target.attachToTarget", {"targetId": pages[0]["targetId"], "flatten": True})
while True:
    m = json.loads(ws.recv())
    if m.get("id") == i:
        session = m["result"]["sessionId"]; break

send("Page.startScreencast", {"format": "jpeg", "quality": 50, "everyNthFrame": 1, "maxFrameRate": 25}, session)

frames = 0
per_sec = {}
next_tick = time.time() + 1
t0 = time.time()
ws.settimeout(0.05)  # 模拟引擎 50ms poll 节奏
pending_eval = None
evals = 0
while time.time() - t0 < DUR:
    # 模拟引擎：每 1s 发 tick evaluate（不等应答，响应稍后以「无关 id」丢弃）
    if MODE in ("B",) and time.time() >= next_tick:
        next_tick += 1
        pending_eval = send("Runtime.evaluate", {"expression": TICK, "returnByValue": True}, session)
        evals += 1
    try:
        raw = ws.recv()
        m = json.loads(raw)
    except Exception:
        continue
    if m.get("sessionId") != session:
        continue
    if m.get("method") == "Page.screencastFrame":
        frames += 1
        per_sec[int(time.time() - t0)] = per_sec.get(int(time.time() - t0), 0) + 1
        fs = m["params"].get("sessionId")
        if MODE == "C":
            # 先「分发帧」（no-op 模拟 push_frame），再 ack
            pass
        ws.send(json.dumps({"id": 900000 + frames, "method": "Page.screencastFrameAck",
                            "params": {"sessionId": fs}, "sessionId": session}))
    # evaluate 响应（无关 id）：丢弃 = 引擎 call() 的 continue 行为

print(f"模式 {MODE}（{DUR}s, evals={evals}）：帧={frames} 每秒={dict(sorted(per_sec.items()))}")
proc.terminate()
