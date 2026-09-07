#!/usr/bin/env python3
"""控制页视觉/交互验证服务器：mock healthz + 模拟云机画面 JPEG 流（MJPEG）。
配合 agent-browser 移动视口截图，验证 dock/抽屉/把手/按钮布局。"""
import http.server, socketserver, threading, time, io, sys, os

try:
    from PIL import Image, ImageDraw
except ImportError:
    sys.exit("需要 PIL：pip install pillow")

PORT = 8899
HTML = open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "control_page.html"), encoding="utf-8").read()

# 生成一张 414×896 模拟云机画面（移动 H5 首页观感：状态栏/卡片/底部导航）
img = Image.new("RGB", (414, 896), (243, 246, 251))
d = ImageDraw.Draw(img)
d.rectangle([0, 0, 414, 40], fill=(28, 100, 242))          # 状态栏
d.text((20, 14), "9:41  移动云手机", fill=(255, 255, 255))
for i, c in enumerate([(255, 255, 255), (235, 240, 250), (235, 240, 250), (235, 240, 250)]):
    y = 70 + i * 120
    d.rounded_rectangle([16, y, 398, y + 104], 14, fill=c, outline=(220, 226, 238))
    d.rectangle([32, y + 18, 32 + 56, y + 62], fill=(210, 222, 240))       # 缩略图位
    d.rectangle([104, y + 24, 330, y + 40], fill=(210, 216, 228))          # 标题条
    d.rectangle([104, y + 50, 260, y + 64], fill=(222, 228, 238))          # 副标题条
d.rectangle([0, 800, 414, 896], fill=(255, 255, 255))
for i, x in enumerate((45, 135, 225, 315)):
    d.ellipse([x - 18, 820, x + 18, 856], fill=(28, 100, 242) if i == 0 else (198, 206, 220))
buf = io.BytesIO(); img.save(buf, "JPEG", quality=60)
FRAME = buf.getvalue()

HEALTHZ = (b'{"ok":true,"page":"ok","vw":414,"vh":896,"platform":"mobile",'
           b'"platformLabel":"\\u79fb\\u52a8\\u4e91\\u624b\\u673a","fps":10,"quality":50,"scale":75,'
           b'"account":"test",'
           b'"browser":"chrome-headless-shell 152","ticks":42,"clicks":7,"dialogs":1,'
           b'"restarts":0,"reloads":1,"lastBeatAge":2,"title":"\\u4e91\\u624b\\u673a\\u9996\\u9875",'
           b'"pageUrl":"https://example.com/home","homeUri":"https://example.com/home",'
           b'"lastStatus":"","lastError":""}')

class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def _send(self, code, ctype, body):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def do_GET(self):
        p = self.path.split("?")[0]
        if p == "/": self._send(200, "text/html; charset=utf-8", HTML.encode())
        elif p == "/healthz": self._send(200, "application/json", HEALTHZ)
        elif p == "/stream.mjpg":
            self.send_response(200)
            self.send_header("Content-Type", "multipart/x-mixed-replace; boundary=cpkframe")
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            t0 = time.time()
            while time.time() - t0 < 300:                    # 5 分钟后自动断（页面会重连）
                try:
                    self.wfile.write(b"--cpkframe\r\nContent-Type: image/jpeg\r\nContent-Length: "
                                     + str(len(FRAME)).encode() + b"\r\n\r\n" + FRAME + b"\r\n")
                    self.wfile.flush()
                except Exception: break
                time.sleep(0.2)
        else: self._send(404, "text/plain", b"")
    def do_POST(self):
        # fps/quality/scale 的 POST 解析 body 并改写 HEALTHZ（控制页回读验证可测成功路径）
        global HEALTHZ
        p = self.path.split("?")[0]
        ln = int(self.headers.get("content-length") or 0)
        body = self.rfile.read(ln).decode() if ln else ""
        if p in ("/fps", "/quality", "/scale") and "value=" in body:
            v = body.split("value=")[1].split("&")[0]
            key = p.lstrip("/")
            import re as _re
            HEALTHZ = _re.sub(('"%s":\\d+' % key).encode(), ('"%s":%s' % (key, v)).encode(), HEALTHZ)
        self._send(200, "text/plain", b"ok")

class Th(http.server.ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

with Th(("127.0.0.1", PORT), H) as srv:
    print(f"mock 服务: http://127.0.0.1:{PORT}/ （Ctrl+C 停止）")
    srv.serve_forever()
