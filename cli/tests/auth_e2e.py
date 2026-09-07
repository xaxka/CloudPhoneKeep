#!/usr/bin/env python3
"""Basic Auth 端到端实测：真引擎 + 真浏览器 + 动画页。

验证：
  1) 无凭据：控制页/画面流/控制端点 → 401 + WWW-Authenticate
  2) 正确凭据：全通
  3) 免鉴权通道不受影响：/healthz（Docker HEALTHCHECK）
  4) 页内脚本心跳照常（云机页跨域 fetch 无法带凭据，/report 免鉴权 → lastBeatAge 保持新鲜）
  5) Basic + token 可叠加
"""
import base64, http.server, json, os, socket, subprocess, sys, threading, time, urllib.request, urllib.error, tempfile, shutil

# 路径参数化（仓内约定与其它脚本一致）：
#   CPK_ENGINE 引擎二进制（默认 仓库根/target/release/cloudphonekeep）
#   CPK_SHELL  chrome 可执行（默认探测 PATH 常见命令）
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
ENGINE = os.environ.get("CPK_ENGINE") or os.path.join(ROOT, "target", "release", "cloudphonekeep")
CHROME = os.environ.get("CPK_SHELL") or next(
    (shutil.which(x) for x in
     ("chrome-headless-shell", "chromium", "chromium-browser", "google-chrome", "google-chrome-stable")
     if shutil.which(x)), "")
if not os.path.isfile(ENGINE):
    sys.exit(f"引擎不存在：{ENGINE}（先 cargo build --release，或 export CPK_ENGINE=/path/to/cloudphonekeep）")
if not CHROME:
    sys.exit("未找到 Chrome：请 export CPK_SHELL=/path/to/chrome-headless-shell")
PORT, ANIM_PORT = 8089, 8899
H = f"http://127.0.0.1:{PORT}"
USER, PASS = "admin", "p@ss!"
TOKEN = "tk123"

ANIM_HTML = b"""<!doctype html><html><head><meta charset=utf-8><style>body{margin:0;background:#335}</style>
</head><body><canvas id=c></canvas><script>
var c=document.getElementById('c');c.width=414;c.height=896;var x=c.getContext('2d');var t=0;
function draw(){t++;x.fillStyle='#'+((t*7919)%16777216).toString(16).padStart(6,'0');x.fillRect(0,0,414,896);
x.fillStyle='#fff';x.font='40px sans-serif';x.fillText('FRAME '+t,20,100);requestAnimationFrame(draw)}draw();
</script></body></html>"""

class Anim(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200); self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(ANIM_HTML))); self.end_headers()
        self.wfile.write(ANIM_HTML)
    def log_message(self, *a): pass

def http_req(path, method="GET", auth=None, token=None, timeout=25):
    req = urllib.request.Request(H + path, method=method)
    if auth:
        req.add_header("Authorization", "Basic " + base64.b64encode(auth.encode()).decode())
    if token:
        req.add_header("X-CPK-Token", token)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()

def main():
    ok_all = True
    def check(name, cond, detail=""):
        nonlocal ok_all
        print(f"  [{'PASS' if cond else 'FAIL'}] {name} {detail}")
        if not cond: ok_all = False

    srv = http.server.ThreadingHTTPServer(("127.0.0.1", ANIM_PORT), Anim)
    threading.Thread(target=srv.serve_forever, daemon=True).start()

    data_dir = tempfile.mkdtemp(prefix="cpk-auth-")
    env = dict(os.environ,
               CPK_URL=f"http://127.0.0.1:{ANIM_PORT}/", CPK_DATA_DIR=data_dir,
               CPK_REPORT_PORT=str(PORT), CPK_BIND="127.0.0.1",
               CPK_CHROME_BIN=CHROME,
               CPK_AUTH_USER=USER, CPK_AUTH_PASS=PASS, CPK_CONTROL_TOKEN=TOKEN)
    eng = subprocess.Popen([ENGINE], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        # 等页面跑起来（心跳来自注入脚本的 /report 上报）
        t0 = time.time()
        while time.time() - t0 < 40:
            try:
                code, body = http_req("/healthz")
                h = json.loads(body)
                if h.get("browser") == "running" and h.get("page") == "ok": break
            except Exception: pass
            time.sleep(1)
        check("引擎就绪（healthz 免鉴权可达）", code == 200 or code == 503, f"http {code}")

        print("\n== 无凭据：全部 401 ==")
        code, body = http_req("/")
        check("控制页无凭据 → 401", code == 401)
        code, _ = http_req("/stream.mjpg")
        check("画面流无凭据 → 401", code == 401)
        code, _ = http_req("/fps?value=10", "POST")
        check("控制端点无凭据 → 401", code == 401)
        code, body = http_req("/shot.jpg")
        check("截图无凭据 → 401", code == 401)
        # WWW-Authenticate 头（urllib HTTPError headers 可查）
        try:
            urllib.request.urlopen(urllib.request.Request(H + "/"), timeout=10)
        except urllib.error.HTTPError as e:
            check("401 携带 WWW-Authenticate（浏览器弹登录框）",
                  e.headers.get("WWW-Authenticate", "").startswith("Basic realm="),
                  repr(e.headers.get("WWW-Authenticate")))

        print("\n== 正确凭据：全通 ==")
        code, body = http_req("/", auth=f"{USER}:{PASS}", token=TOKEN)
        check("控制页 凭据+token → 200", code == 200 and b"CloudPhoneKeep" in body)
        code, body = http_req("/healthz")
        check("healthz 免鉴权照常（Docker HEALTHCHECK 兼容）", code == 200)
        code, _ = http_req("/fps?value=5", "POST", auth=f"{USER}:{PASS}", token=TOKEN)
        check("POST /fps 凭据+token → 200", code == 200)
        code, body = http_req("/healthz")
        check("fps=5 已生效（healthz 回读）", json.loads(body).get("fps") == 5)
        code, body = http_req("/fps?value=10", "POST", auth=f"{USER}:{PASS}")
        check("仅 auth 无 token → 403（叠加语义）", code == 403)

        print("\n== 页内脚本心跳照常（/report 免鉴权） ==")
        time.sleep(8)
        code, body = http_req("/healthz")
        h = json.loads(body)
        check("lastBeatAge 新鲜（< 10s，注入脚本上报未被鉴权掐断）",
              h.get("lastBeatAge") is not None and h["lastBeatAge"] < 10, f"lastBeatAge={h.get('lastBeatAge')}")
        check("ticks 增长（看门狗在跑）", h.get("ticks", 0) > 0)

        print("\n== 带凭据读画面流（真帧） ==")
        got_frame = False
        try:
            s = socket.create_connection(("127.0.0.1", PORT), timeout=15)
            req = f"GET /stream.mjpg HTTP/1.1\r\nHost: x\r\nAuthorization: Basic {base64.b64encode(f'{USER}:{PASS}'.encode()).decode()}\r\nX-CPK-Token: {TOKEN}\r\n\r\n"
            s.sendall(req.encode())
            s.settimeout(12)
            buf = b""
            t1 = time.time()
            while time.time() - t1 < 12 and not got_frame:
                data = s.recv(65536)
                if not data: break
                buf += data
                if b"--cpkframe\r\nContent-Type: image/jpeg" in buf and b"\r\n\r\n" in buf:
                    got_frame = True
        except Exception as e:
            print("   stream err:", e)
        check("画面流带凭据有真帧", got_frame)
    finally:
        eng.terminate()
        try: eng.wait(timeout=10)
        except Exception: eng.kill()
        srv.shutdown(); shutil.rmtree(data_dir, ignore_errors=True)
    print("\n结果：" + ("全部通过" if ok_all else "存在失败项"))
    return 0 if ok_all else 1

if __name__ == "__main__":
    sys.exit(main())
