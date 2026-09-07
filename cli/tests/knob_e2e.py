#!/usr/bin/env python3
"""三旋钮（fps/quality/scale）热更新端到端实测。

链路：本地动画页（60fps canvas）→ 引擎（真 chrome-headless-shell）
→ /stream.mjpg 真订阅（形成观众）→ POST 旋钮 → 断言：
  1) healthz 回读值即刻变化（HTTP 层直写）
  2) 引擎日志出现「cast 参数周期同步 ... 重建生效」（周期拉齐触发重建）
  3) /scale 50 后 JPEG 帧实际尺寸降到 ~50%（编码参数真实生效，非谎报）
  4) /fps 3 后实测帧率 ≈3/s，/fps 30 恢复（ack 门控真实生效）
"""
import base64, http.server, json, os, re, socket, subprocess, sys, threading, time, urllib.request, urllib.error, tempfile, shutil

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
PORT = 8089            # 引擎控制端口
ANIM_PORT = 8899       # 动画页端口
H = f"http://127.0.0.1:{PORT}"

ANIM_HTML = b"""<!doctype html><html><head><meta charset=utf-8>
<style>body{margin:0;background:#223}</style></head><body>
<canvas id=c></canvas><script>
var c=document.getElementById('c');c.width=414;c.height=896;
var x=c.getContext('2d');var t=0;
function draw(){t++;x.fillStyle='#'+((t*7919)%16777216).toString(16).padStart(6,'0');
x.fillRect(0,0,414,896);x.fillStyle='#fff';x.font='40px sans-serif';
x.fillText('FRAME '+t,20,100);requestAnimationFrame(draw);}draw();
</script></body></html>"""

class Anim(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200); self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(ANIM_HTML))); self.end_headers()
        self.wfile.write(ANIM_HTML)
    def log_message(self, *a): pass

# ---- MJPEG 流读取器（原始 socket，统计帧数+尺寸） ----
class StreamReader(threading.Thread):
    def __init__(self):
        super().__init__(daemon=True); self.frames = []; self.alive = False; self.lock = threading.Lock()
    def run(self):
        try:
            s = socket.create_connection(("127.0.0.1", PORT), timeout=30)
            s.sendall(f"GET /stream.mjpg HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n".encode())
            buf = b""; self.alive = True
            s.settimeout(20)
            while True:
                data = s.recv(65536)
                if not data: break
                buf += data
                while True:
                    i = buf.find(b"--cpkframe\r\n")
                    if i < 0: break
                    j = buf.find(b"\r\n\r\n", i)
                    if j < 0: break
                    hdr = buf[i:j].decode("latin1")
                    m = re.search(r"Content-Length: (\d+)", hdr)
                    if not m: buf = buf[:i]; break
                    n = int(m.group(1))
                    if len(buf) < j + 4 + n + 2: break
                    frame = buf[j+4 : j+4+n]
                    with self.lock: self.frames.append((time.time(), frame))
                    buf = buf[j+4+n+2:]
        except Exception as e:
            print(f"  [stream] 结束：{e}")
        finally:
            self.alive = False
    def stats(self, since=None):
        with self.lock:
            fs = [(t, f) for t, f in self.frames if since is None or t >= since]
        return fs

def jpeg_size(jpg):
    # SOF0/SOF2 → 高/宽
    i = 2
    while i + 9 < len(jpg):
        if jpg[i] != 0xFF: i += 1; continue
        m = jpg[i+1]
        if m in (0xC0, 0xC1, 0xC2, 0xC3):
            h = (jpg[i+5] << 8) | jpg[i+6]; w = (jpg[i+7] << 8) | jpg[i+8]
            return w, h
        if m in (0xD8, 0xD9, 0x01) or 0xD0 <= m <= 0xD7: i += 2; continue
        ln = (jpg[i+2] << 8) | jpg[i+3]
        i += 2 + ln
    return None

def http_req(path, method="GET", body=None):
    req = urllib.request.Request(H + path, data=body, method=method)
    try:
        with urllib.request.urlopen(req, timeout=25) as r:
            return r.status, r.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")

def wait_healthz(predicate, timeout=40):
    t0 = time.time()
    while time.time() - t0 < timeout:
        try:
            code, body = http_req("/healthz")
            h = json.loads(body)
            if predicate(h): return h
        except Exception: pass
        time.sleep(1)
    return None

def main():
    ok_all = True
    def check(name, cond, detail=""):
        nonlocal ok_all
        print(f"  [{'PASS' if cond else 'FAIL'}] {name} {detail}")
        if not cond: ok_all = False

    srv = http.server.ThreadingHTTPServer(("127.0.0.1", ANIM_PORT), Anim)
    threading.Thread(target=srv.serve_forever, daemon=True).start()

    data_dir = tempfile.mkdtemp(prefix="cpk-knob-")
    log_file = os.path.join(data_dir, "logs", time.strftime("cpk-%Y%m%d.log"))
    env = dict(os.environ,
               CPK_URL=f"http://127.0.0.1:{ANIM_PORT}/", CPK_DATA_DIR=data_dir,
               CPK_REPORT_PORT=str(PORT), CPK_BIND="127.0.0.1",
               CPK_CHROME_BIN=CHROME, CPK_FPS="30")
    # 日志直落文件（logger 按天滚动，目录数据目录/logs）
    eng = subprocess.Popen([ENGINE], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        h = wait_healthz(lambda h: h.get("browser") == "running" and h.get("page") == "ok" and h.get("ticks", 0) > 0)
        check("引擎就绪（页面 ok + tick>0）", h is not None, f"fps={h and h.get('fps')} q={h and h.get('quality')} s={h and h.get('scale')} vw={h and h.get('vw')}x{h and h.get('vh')}" if h else "")
        if h is None: return 1

        print("\n== 基线：订阅流 8s（fps 上限 30, scale 100, q 50） ==")
        rd = StreamReader(); rd.start(); time.sleep(8)
        base = rd.stats()
        check("流有帧产出", len(base) > 20, f"{len(base)} 帧")
        if base:
            w, hgt = jpeg_size(base[-1][1])
            check("基线帧尺寸≈视口 414x896", abs(w - 414) <= 60 and abs(hgt - 896) <= 60, f"{w}x{hgt}")

        print("\n== 旋钮1：/scale 50 → 采集分辨率（应重建 + 帧尺寸减半） ==")
        t_scale = time.time()
        code, _ = http_req("/scale?value=50", "POST")
        check("POST /scale=50 → 200", code == 200)
        time.sleep(1)
        code, body = http_req("/healthz")
        check("healthz scale=50 即刻回读", json.loads(body).get("scale") == 50)
        time.sleep(6)   # 等周期拉齐重建 + 新帧
        frames = rd.stats(since=t_scale)
        if frames:
            w, hgt = jpeg_size(frames[-1][1])
            check("帧尺寸 ~207x448（编码参数真实生效）", abs(w - 207) <= 40 and abs(hgt - 448) <= 40, f"{w}x{hgt}")
        else:
            check("scale 后流有帧", False, "无帧——流断了？")
        log = open(log_file, encoding="utf-8", errors="replace").read() if os.path.exists(log_file) else ""
        check("引擎日志确认重建（直发或周期同步二选一）",
              ("实时画面采集分辨率设为 50" in log) or ("重建生效" in log))

        print("\n== 旋钮2：/quality 80 → JPEG 画质（应重建 + healthz 回读） ==")
        code, _ = http_req("/quality?value=80", "POST")
        check("POST /quality=80 → 200", code == 200)
        time.sleep(0.5)
        code, body = http_req("/healthz")
        check("healthz quality=80 即刻回读", json.loads(body).get("quality") == 80)
        n_before = log.count("设为")
        time.sleep(6)
        log = open(log_file, encoding="utf-8", errors="replace").read()
        check("quality 变化再次触发引擎动作日志", log.count("设为") > n_before, f"共 {log.count('设为')} 次")
        fs = rd.stats(since=time.time() - 5)
        check("quality 80 后流仍有帧", len(fs) > 5, f"{len(fs)} 帧/5s")

        print("\n== 旋钮3：/fps 3 → 帧率上限（应 ≈3fps） ==")
        code, _ = http_req("/fps?value=3", "POST")
        check("POST /fps=3 → 200", code == 200)
        time.sleep(0.5)
        code, body = http_req("/healthz")
        check("healthz fps=3 即刻回读", json.loads(body).get("fps") == 3)
        mark = time.time(); time.sleep(10)
        fs = [f for t, f in rd.stats(since=mark)]
        rate = len(fs) / 10
        check("实测帧率 ≈3/s（ack 门控生效）", 1.5 <= rate <= 5.5, f"{rate:.1f}/s")

        print("\n== 旋钮3回落：/fps 30 → 帧率恢复 ==")
        code, _ = http_req("/fps?value=30", "POST")
        time.sleep(2)
        mark = time.time(); time.sleep(8)
        fs = [f for t, f in rd.stats(since=mark)]
        rate = len(fs) / 8
        check("实测帧率恢复 ≥15/s", rate >= 15, f"{rate:.1f}/s")

        print("\n== 守卫：越界值拒绝 ==")
        for path in ["/scale?value=20", "/quality?value=95", "/fps?value=0"]:
            code, _ = http_req(path, "POST")
            check(f"{path} → 400", code == 400)
    finally:
        eng.terminate()
        try: eng.wait(timeout=10)
        except Exception: eng.kill()
        srv.shutdown(); shutil.rmtree(data_dir, ignore_errors=True)
    print("\n结果：" + ("全部通过" if ok_all else "存在失败项"))
    return 0 if ok_all else 1

if __name__ == "__main__":
    sys.exit(main())
