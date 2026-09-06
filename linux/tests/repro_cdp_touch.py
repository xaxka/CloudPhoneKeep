#!/usr/bin/env python3
"""复现 CloudPhoneKeep Linux 的触摸 bug：完全复刻生产参数与 CDP 行为。

生产链路（engine.rs / cdp.rs / report_server.rs 控制页 JS）：
  touchstart: Input.dispatchTouchEvent {type:touchStart, touchPoints:[{x,y,id}]}
  touchmove : Input.dispatchTouchEvent {type:touchMove,  touchPoints:[全部在按触点]}
  touchend  : Input.dispatchTouchEvent {type:touchEnd,   touchPoints:[]}   ← puppeteer 规范形态
  引擎只做 Emulation.setUserAgentOverride（移动 UA + platform=Linux armv8l）
  从未调用 Emulation.setTouchEmulationEnabled

测试矩阵：
  A = 生产现状（仅 UA 覆盖）
  B = A + Emulation.setTouchEmulationEnabled(enabled=true, maxTouchPoints=2)（注入前设置）
  每个变体测：轻点（tap）、拖动（swipe/scroll）
"""
import asyncio, json, subprocess, sys, time, socket, urllib.request
from http.server import HTTPServer, SimpleHTTPRequestHandler
from pathlib import Path
import websockets

import os, shutil
# Chrome 可执行：CPK_SHELL 环境变量优先，否则探测 PATH 常见命令
SHELL = os.environ.get("CPK_SHELL") or next(
    (shutil.which(x) for x in
     ("chromium", "chromium-browser", "google-chrome", "google-chrome-stable", "chrome-headless-shell")
     if shutil.which(x)), "")
if not SHELL:
    sys.exit("未找到 Chrome：请 export CPK_SHELL=/path/to/chrome（或 chromium）")
PORT = 9333   # CDP 调试端口
PAGE_PORT = 8931
UA = "Mozilla/5.0 (Linux; Android 13; Pixel 7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Mobile Safari/537.36"
FLAGS = [
    "--user-data-dir=/tmp/cpk-repro-profile",
    f"--remote-debugging-port={PORT}",
    "--remote-debugging-address=127.0.0.1",
    "--remote-allow-origins=*",
    "--window-size=414,896",
    "--force-device-scale-factor=1",
    "--hide-scrollbars", "--no-first-run", "--no-default-browser-check",
    "--disable-gpu", "--disable-dev-shm-usage", "--disable-crash-reporter",
    "--disable-background-timer-throttling", "--disable-backgrounding-occluded-windows",
    "--disable-renderer-backgrounding", "--disable-background-networking",
    "--disable-component-update", "--disable-sync",
    "--disable-features=Translate,MediaRouter,OptimizationHints",
    "--mute-audio", "--autoplay-policy=no-user-gesture-required",
    "--disable-pinch", "--lang=zh-CN", "--no-sandbox", "--disable-setuid-sandbox",
]

class Cdp:
    def __init__(self, ws):
        self.ws = ws
        self.mid = 0
        self.pending = {}          # id -> future
        self.events = []           # (method, params)
    async def start(self):
        asyncio.get_event_loop().create_task(self._reader())
    async def _reader(self):
        try:
            async for msg in self.ws:
                d = json.loads(msg)
                if "id" in d:
                    fut = self.pending.pop(d["id"], None)
                    if fut and not fut.done():
                        if "error" in d:
                            fut.set_exception(RuntimeError(f"CDP error: {d['error']}"))
                        else:
                            fut.set_result(d.get("result", {}))
                else:
                    self.events.append((d.get("method", ""), d.get("params", {})))
        except Exception as e:
            print(f"[ws reader end] {e}")
    async def call(self, method, params=None, session=None, timeout=10):
        self.mid += 1
        msg = {"id": self.mid, "method": method, "params": params or {}}
        if session:
            msg["sessionId"] = session
        fut = asyncio.get_event_loop().create_future()
        self.pending[self.mid] = fut
        await self.ws.send(json.dumps(msg))
        return await asyncio.wait_for(fut, timeout)

async def eval_js(cdp, session, expr, timeout=10):
    r = await cdp.call("Runtime.evaluate",
        {"expression": expr, "returnByValue": True, "awaitPromise": False}, session, timeout)
    if "exceptionDetails" in r:
        return f"EXC: {r['exceptionDetails'].get('text')}"
    return r.get("result", {}).get("value")

async def wait_load(cdp, session, timeout=15):
    t0 = time.time()
    while time.time() - t0 < timeout:
        st = await eval_js(cdp, session, "document.readyState")
        if st == "complete":
            return True
        await asyncio.sleep(0.2)
    return False

async def stats(cdp, session):
    s = await eval_js(cdp, session,
        "JSON.stringify({ts:__ev.ts,tm:__ev.tm,te:__ev.te,tc:__ev.tc,md:__ev.md,mu:__ev.mu,ck:__ev.ck,sel:__ev.sel,y:Math.round(window.scrollY),"
        "mtp:navigator.maxTouchPoints,ots:('ontouchstart' in window),cls:document.body.firstElementChild&&document.body.firstElementChild.id})")
    return json.loads(s) if s and not str(s).startswith("EXC") else {"raw": str(s)}

async def touch(cdp, session, typ, points):
    await cdp.call("Input.dispatchTouchEvent", {"type": typ, "touchPoints": points}, session, 10)

async def tap_test(cdp, session, x, y, label):
    """生产同款 tap：touchStart(带点) → ~80ms → touchEnd(空点)"""
    b = await stats(cdp, session)
    await touch(cdp, session, "touchStart", [{"x": x, "y": y, "id": 1}])
    await asyncio.sleep(0.08)
    await touch(cdp, session, "touchEnd", [])
    await asyncio.sleep(0.4)   # 给合成 click 留时间
    a = await stats(cdp, session)
    print(f"  [tap {label} @({x},{y})] "
          f"ts {b['ts']}→{a['ts']} tm {b['tm']}→{a['tm']} te {b['te']}→{a['te']} "
          f"md {b['md']}→{a['md']} mu {b['mu']}→{a['mu']} ck {b['ck']}→{a['ck']} sel={a['sel']}")
    return a

async def swipe_test(cdp, session, x, y1, y2, label, steps=12):
    """生产同款拖动：touchStart → N×touchMove(全部在按点) → touchEnd"""
    b = await stats(cdp, session)
    await touch(cdp, session, "touchStart", [{"x": x, "y": y1, "id": 1}])
    for i in range(1, steps + 1):
        yy = y1 + (y2 - y1) * i / steps
        await touch(cdp, session, "touchMove", [{"x": x, "y": yy, "id": 1}])
        await asyncio.sleep(0.033)
    await touch(cdp, session, "touchEnd", [])
    await asyncio.sleep(0.5)
    a = await stats(cdp, session)
    print(f"  [swipe {label} ({x},{y1})→({x},{y2})] "
          f"tm {b['tm']}→{a['tm']} te {b['te']}→{a['te']} y {b.get('y')}→{a.get('y')} "
          f"ts {b['ts']}→{a['ts']}")
    return a

async def run_variant(name, extra_setup=None, reload=True):
    print(f"\n=== 变体 {name} ===")
    # —— 启动浏览器（生产同款 flags）——
    proc = subprocess.Popen([SHELL] + FLAGS + ["about:blank"],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        # 等 CDP 端口就绪
        ws_url = None
        for _ in range(100):
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{PORT}/json/version", timeout=1) as resp:
                    ws_url = json.loads(resp.read())["webSocketDebuggerUrl"]
                break
            except Exception:
                await asyncio.sleep(0.1)
        assert ws_url, "CDP 未就绪"
        async with websockets.connect(ws_url, max_size=64*1024*1024) as ws:
            cdp = Cdp(ws)
            await cdp.start()
            tgt = await cdp.call("Target.createTarget", {"url": "about:blank"})
            r = await cdp.call("Target.attachToTarget", {"targetId": tgt["targetId"], "flatten": True})
            session = r["sessionId"]
            await cdp.call("Page.enable", {}, session)
            await cdp.call("Runtime.enable", {}, session)
            await cdp.call("Emulation.setUserAgentOverride",
                           {"userAgent": UA, "platform": "Linux armv8l"}, session)
            if extra_setup:
                await extra_setup(cdp, session)
            await cdp.call("Page.navigate", {"url": f"http://127.0.0.1:{PAGE_PORT}/repro_page.html"}, session)
            await wait_load(cdp, session)
            await asyncio.sleep(0.5)
            env = await stats(cdp, session)
            print(f"  env: maxTouchPoints={env.get('mtp')} ontouchstart={env.get('ots')}")

            await tap_test(cdp, session, 207, 60, "CARD1-去登陆")
            await tap_test(cdp, session, 207, 190, "CARD2-秒开")
            await swipe_test(cdp, session, 207, 400, 150, "列表上滑")
            await tap_test(cdp, session, 207, 190, "CARD2-再次点击")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except Exception:
            proc.kill()

async def main():
    # 本地静态页服务
    handler = lambda *a, **kw: SimpleHTTPRequestHandler(directory=os.path.dirname(os.path.abspath(__file__)), *a, **kw)
    httpd = HTTPServer(("127.0.0.1", PAGE_PORT), handler)
    import threading
    threading.Thread(target=httpd.serve_forever, daemon=True).start()

    # 变体 A：生产现状（只有 UA 覆盖）
    await run_variant("A 生产现状")
    # 变体 B：+ setTouchEmulationEnabled
    async def b_setup(cdp, session):
        try:
            await cdp.call("Emulation.setTouchEmulationEnabled", {"enabled": True, "maxTouchPoints": 2}, session)
            print("  setTouchEmulationEnabled(true, 2) OK")
        except Exception as e:
            print(f"  setTouchEmulationEnabled 失败: {e}")
    await run_variant("B +touchEmulation", b_setup)
    httpd.shutdown()

if __name__ == "__main__":
    asyncio.run(main())
