#!/usr/bin/env python3
"""验证 Chrome 152 headless-shell 的 Page.startScreencast 参数实际效果：
  1) rAF 动画页面下 compositor 原始出帧率是多少（基准）
  2) everyNthFrame=N 是否真实生效（Chrome 端节流）
  3) maxFrameRate 是否无效（复证旧结论）
  4) stop→start 重建是否把新 everyNthFrame 生效（生产 SetFps 路径要靠它）

方法：订阅 screencast 数帧（只数不解码），每秒 ack，统计窗口 3s。
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

# 每帧重绘的动画页（红绿蓝循环 + 位置移动，强制 compositor 出帧）
# 每帧重绘的动画页（红绿蓝循环 + 位置移动，强制 compositor 出帧；页面自带 fps 自计）
ANIM_PAGE = "http://127.0.0.1:8932/anim_page.html"

FLAGS = [
    "--no-proxy-server",
    "--user-data-dir=/tmp/cpk-everynth-profile",
    f"--remote-debugging-port={PORT}",
    "--remote-allow-origins=*",
    "--window-size=414,896", "--force-device-scale-factor=1",
    "--hide-scrollbars", "--no-first-run", "--disable-gpu",
    "--disable-dev-shm-usage", "--disable-background-timer-throttling",
    "--disable-backgrounding-occluded-windows", "--disable-renderer-backgrounding",
    "--disable-pinch", "--no-sandbox", "--mute-audio",
]

async def main():
    # 脚本内置静态页服务器（进程存活期间有效，不依赖外部终端）
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
        ws_url = json.loads(urllib.request.urlopen(f"http://127.0.0.1:{PORT}/json/new?{ANIM_PAGE.replace('data:text/html,','') if False else ''}", timeout=5).read())["webSocketDebuggerUrl"] if False else None
        # 用 /json/list 找 page target
        info = json.loads(urllib.request.urlopen(f"http://127.0.0.1:{PORT}/json/version", timeout=5).read())
        ws = await websockets.connect(info["webSocketDebuggerUrl"], max_size=64 * 1024 * 1024)
        mid = 0

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

        pending = {}
        frames = []

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
                        frames.append(time.monotonic())
                        sid = d["params"].get("sessionId")
                        try:
                            await ws.send(json.dumps({"id": 1000000, "method": "Page.screencastFrameAck", "params": {"sessionId": sid}}))
                        except Exception:
                            pass
            except Exception:
                pass

        asyncio.get_event_loop().create_task(reader())
        tgt = await call("Target.createTarget", {"url": "about:blank"})
        r = await call("Target.attachToTarget", {"targetId": tgt["targetId"], "flatten": True})
        session = r["sessionId"]
        await call("Page.enable", {}, session)
        await call("Runtime.enable", {}, session)
        await call("Page.navigate", {"url": ANIM_PAGE}, session)
        await asyncio.sleep(2)
        raf = await call("Runtime.evaluate", {"expression": "document.readyState+'|'+(typeof st)+'|'+(typeof a!=='undefined'&&a?a.style.left:'-')+'|'+location.href", "returnByValue": True}, session)
        print(f"[0] 页面状态：{json.dumps(raf, ensure_ascii=False)[:200]}")

        async def count_frames(duration_s, label):
            frames.clear()
            t0 = time.monotonic()
            while time.monotonic() - t0 < duration_s:
                await asyncio.sleep(0.05)
            n = len(frames)
            print(f"  {label}: {n} 帧 / {duration_s}s = {n/duration_s:.1f} fps")
            return n

        async def start(params):
            return await call("Page.startScreencast", params, session)

        async def stop():
            try:
                await call("Page.stopScreencast", {}, session)
            except Exception as e:
                print(f"  stopScreencast: {e}")

        print("[1] everyNthFrame=1 + maxFrameRate=10（预期：若 maxFrameRate 有效≈10，无效≈原始）")
        await start({"format": "jpeg", "quality": 50, "everyNthFrame": 1, "maxFrameRate": 10})
        await count_frames(3, "结果")
        await stop()

        print("[2] everyNthFrame=6（目标 10fps：60/6=10。预期≈原始/6）")
        await start({"format": "jpeg", "quality": 50, "everyNthFrame": 6})
        await count_frames(3, "结果")
        await stop()

        print("[3] everyNthFrame=12（60/12=5）")
        await start({"format": "jpeg", "quality": 50, "everyNthFrame": 12})
        await count_frames(3, "结果")
        await stop()

        print("[4] 直接重发 startScreencast 不 stop（验证 already active 拒绝）")
        await start({"format": "jpeg", "quality": 50, "everyNthFrame": 6})
        await asyncio.sleep(0.3)
        try:
            await start({"format": "jpeg", "quality": 50, "everyNthFrame": 3})
            print("  第二次 start：被接受（无 already active 保护）")
        except Exception as e:
            print(f"  第二次 start 被拒：{e}")
        await stop()

        print("[5] stop→start 换 everyNthFrame（SetFps 生产路径）")
        await start({"format": "jpeg", "quality": 50, "everyNthFrame": 6})
        await asyncio.sleep(0.5)
        frames.clear()
        await stop()
        await start({"format": "jpeg", "quality": 50, "everyNthFrame": 1})
        await count_frames(2, "everyNthFrame 6→1 后")
        await stop()

        await ws.close()
        httpd.shutdown()
    finally:
        proc.terminate()

asyncio.run(main())
