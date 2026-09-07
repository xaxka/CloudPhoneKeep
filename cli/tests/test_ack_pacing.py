#!/usr/bin/env python3
"""验证 Chrome 152 headless-shell 的 screencast ack 门控：
  A. 立即 ack 基准帧率
  B. 停止 ack 3s → 若门控成立应收 0 帧（或仅周期性重发）
  C. 恢复 ack → 帧是否恢复流动
  D. 延迟 ack 节流（每 300ms ack 一次）→ 实际帧率是否 ≈ 3.3fps
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
    "--no-proxy-server",
    "--user-data-dir=/tmp/cpk-ack-profile",
    f"--remote-debugging-port={PORT}",
    "--remote-allow-origins=*",
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
        frames = []          # (到达时刻, sessionId)
        ack_enabled = True   # 门控开关
        ack_delay = 0.0      # 0 = 立即
        ack_queue = []       # 待发的 ack (sid, 最早发送时刻)

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
                        now = time.monotonic()
                        frames.append((now, sid))
                        if ack_delay > 0:
                            ack_queue.append((sid, now + ack_delay))
                        elif ack_enabled:
                            await send_ack(sid)
            except Exception:
                pass

        async def ack_pumper():
            while True:
                while ack_queue and ack_queue[0][1] <= time.monotonic():
                    sid, _ = ack_queue.pop(0)
                    await send_ack(sid)
                await asyncio.sleep(0.01)

        async def reader_task():
            await reader()

        loop = asyncio.get_event_loop()
        loop.create_task(reader_task())
        loop.create_task(ack_pumper())

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
            n = len(frames) - n0
            print(f"  {label}: {n} 帧 / {dur}s = {n/dur:.1f} fps")
            return n

        print("[A] 立即 ack 基准")
        await call("Page.startScreencast", {"format": "jpeg", "quality": 50, "everyNthFrame": 1}, session)
        await measure("基准", 3)

        print("[B] 停止 ack 3s（门控成立 → ~0 帧；无门控 → 帧继续）")
        ack_enabled = False
        ack_delay = 0
        n_b = await measure("无 ack", 3)
        stale_sid = frames[-1][1] if frames else None

        print("[C] 补发陈旧 ack + 恢复立即 ack 2s（流是否复活）")
        if stale_sid is not None:
            await send_ack(stale_sid)
            print(f"  已补发陈旧 ack sessionId={stale_sid}")
        ack_enabled = True
        await measure("恢复后", 2)

        print("[D] 停 cast，改延迟 ack 300ms 节流（目标 3.3fps）")
        await call("Page.stopScreencast", {}, session)
        await asyncio.sleep(0.5)
        frames.clear()
        ack_queue.clear()
        ack_delay = 0.3
        await call("Page.startScreencast", {"format": "jpeg", "quality": 50, "everyNthFrame": 1}, session)
        await measure("延迟 ack 300ms", 4)
        ack_delay = 0

        print(f"\n结论: B 阶段 {n_b} 帧 —— {'门控成立（ack=流控）' if n_b <= 3 else '本环境不门控（仅靠 everyNthFrame/软限帧）'}")
        await ws.close()
        httpd.shutdown()
    finally:
        proc.terminate()

asyncio.run(main())
