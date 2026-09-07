#!/usr/bin/env python3
"""参数化测试 ack 持有时长上限：每次 stop+start 后立即 ack 若干帧建立流，
然后持有 ack H ms 再补发，测 4s 内流是否存活（本环境基准 1fps → 存活≈4 帧）。
H = 100 / 300 / 600 / 1000 / 3000 ms
"""
import asyncio, json, subprocess, time, urllib.request, websockets, threading
from http.server import HTTPServer, SimpleHTTPRequestHandler
from pathlib import Path
import functools

import os, shutil, sys as _sys
# Chrome 可执行：CPK_SHELL 环境变量优先，否则探测 PATH 常见命令
SHELL = os.environ.get("CPK_SHELL") or next(
    (shutil.which(x) for x in
     ("chromium", "chromium-browser", "google-chrome", "google-chrome-stable", "chrome-headless-shell")
     if shutil.which(x)), "")
if not SHELL:
    _sys.exit("未找到 Chrome：请 export CPK_SHELL=/path/to/chrome（或 chromium）")
PORT = 9444
ANIM_PAGE = "http://127.0.0.1:8932/anim_page.html"
FLAGS = [
    "--no-proxy-server", "--user-data-dir=/tmp/cpk-hold-profile",
    f"--remote-debugging-port={PORT}", "--remote-allow-origins=*",
    "--window-size=414,896", "--force-device-scale-factor=1",
    "--hide-scrollbars", "--no-first-run", "--disable-gpu",
    "--disable-dev-shm-usage", "--disable-background-timer-throttling",
    "--disable-backgrounding-occluded-windows", "--disable-renderer-backgrounding",
    "--disable-pinch", "--no-sandbox", "--mute-audio",
]

async def main():
    handler = functools.partial(SimpleHTTPRequestHandler, directory=str(Path(__file__).parent))
    httpd = HTTPServer(("127.0.0.1", 8932), handler)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()

    proc = subprocess.Popen([SHELL, *FLAGS], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(50):
            try:
                urllib.request.urlopen(f"http://127.0.0.1:{PORT}/json/version", timeout=1).read()
                break
            except Exception:
                await asyncio.sleep(0.2)
        info = json.loads(urllib.request.urlopen(f"http://127.0.0.1:{PORT}/json/version", timeout=5).read())
        ws = await websockets.connect(info["webSocketDebuggerUrl"], max_size=64 * 1024 * 1024)
        mid = 0
        pending = {}
        frames = []
        gate = {"hold": True}   # hold=True 时不 ack

        async def call(method, params=None, session=None, tmo=8):
            nonlocal mid
            mid += 1
            i = mid
            fut = asyncio.get_event_loop().create_future()
            pending[i] = fut
            msg = {"id": i, "method": method, "params": params or {}}
            if session:
                msg["sessionId"] = session
            await ws.send(json.dumps(msg))
            return await asyncio.wait_for(fut, tmo)

        async def send_ack(sid):
            nonlocal mid
            mid += 1
            await ws.send(json.dumps({"id": mid, "method": "Page.screencastFrameAck", "params": {"sessionId": sid}}))

        async def reader():
            try:
                async for msg in ws:
                    d = json.loads(msg)
                    if "id" in d:
                        fut = pending.pop(d["id"], None)
                        if fut and not fut.done():
                            if "error" in d:
                                fut.set_exception(RuntimeError(str(d["error"])))
                            else:
                                fut.set_result(d.get("result", {}))
                    elif d.get("method") == "Page.screencastFrame":
                        sid = d["params"].get("sessionId")
                        frames.append((time.monotonic(), sid))
                        if not gate["hold"]:
                            await send_ack(sid)
            except Exception:
                pass

        asyncio.get_event_loop().create_task(reader())
        tgt = await call("Target.createTarget", {"url": "about:blank"})
        r = await call("Target.attachToTarget", {"targetId": tgt["targetId"], "flatten": True})
        session = r["sessionId"]
        await call("Page.enable", {}, session)
        await call("Page.navigate", {"url": ANIM_PAGE}, session)
        await asyncio.sleep(2)

        async def measure(label, dur):
            n0 = len(frames)
            t0 = time.monotonic()
            while time.monotonic() - t0 < dur:
                await asyncio.sleep(0.05)
            return len(frames) - n0

        for hold_ms in [100, 300, 600, 1000, 3000]:
            # 重建流（已知死流复活手段）
            await call("Page.stopScreencast", {}, session)
            frames.clear()
            gate["hold"] = False
            await call("Page.startScreencast", {"format": "jpeg", "quality": 50, "everyNthFrame": 1}, session)
            await asyncio.sleep(1.5)          # 建立流动
            if not frames:
                print(f"hold={hold_ms}ms: 起始帧未到达（本环境基准低），跳过")
                continue
            # 持有：停止 ack，等一帧到达后持有 H ms
            gate["hold"] = True
            n0 = len(frames)
            t_gate = time.monotonic()
            while len(frames) == n0 and time.monotonic() - t_gate < 2:
                await asyncio.sleep(0.02)     # 等新帧落入持有模式
            held_sid = frames[-1][1] if len(frames) > n0 else None
            if held_sid is None:
                print(f"hold={hold_ms}ms: 持有期内无新帧")
                gate["hold"] = False
                continue
            await asyncio.sleep(hold_ms / 1000)
            await send_ack(held_sid)           # 补发
            n_alive = await measure("revival", 4)
            # 判定：4s 基准 1fps → 存活应有 ≥2 帧
            verdict = "存活" if n_alive >= 2 else "死流"
            print(f"hold={hold_ms:>5}ms: 补发后 4s 收 {n_alive} 帧 → {verdict}")
            gate["hold"] = False

        await ws.close()
        httpd.shutdown()
    finally:
        proc.terminate()

asyncio.run(main())
