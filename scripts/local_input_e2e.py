#!/usr/bin/env python3
"""输入全链路 e2e（单命令内跑完，适配沙坑：引擎后台进程不跨 Bash 命令存活）。
覆盖本轮 7 项反馈的核心路径（真 chrome-headless-shell 152）：
1. /mouse 真实鼠标点击 → 页面 click 回调（左键点击必须发 click 事件）
2. /mouse clickCount=2 → dblclick
3. /mouse wheel → 滚动
4. /kbd 全键位键盘：点击聚焦输入框 → 逐键打字 → Ctrl+A → /clip 复制选区
5. /type insertText + Backspace 退格
6. /touch 多点（双指）：第二指按下 touches=2；抬一指 touches=1
7. /fps 运行时改帧率：healthz 回显 + 流继续出帧
8. /nav fire 化：导航请求毫秒级返回，导航后触摸立即可用（回首页回归）
9. 引擎日志 WS 传输错误 = 0
"""
import os, sys, time, threading, urllib.request, urllib.parse, subprocess, signal, json

CHROME = "/home/z/.cache/puppeteer/chrome-headless-shell/linux-152.0.7977.54/chrome-headless-shell-linux64/chrome-headless-shell"
BIN = "/home/z/my-project/cloudphonekeep/linux/target/release/cloudphonekeep"
PORT = 18097          # python http.server（测试页 origin，兼收 /log）
CPK_PORT = 18096      # 引擎 report server
DATA = "/tmp/cpk-e2e-input-data"

TEST_PAGE = """<!doctype html><html><head><meta charset=utf-8><style>
body{margin:0;font:16px sans-serif;background:#111;color:#fff}
#btn{padding:26px 0;background:#333;border-radius:8px;text-align:center;margin-top:60px}
#ti{font-size:20px;padding:14px;width:80%;margin:20px 0 0 5%}
#scr{height:220px;overflow-y:scroll;background:#222;margin:20px 5%;padding:6px}
#scr p{height:80px;border-bottom:1px solid #444}
#ani{position:fixed;top:0;left:0;width:100%;height:3px;background:#fff}
</style></head><body>
<canvas id="ani" width="414" height="896"></canvas>
<div id="btn">点我（0）</div>
<input id="ti" placeholder="键盘输入测试">
<div id="scr"><p>1</p><p>2</p><p>3</p><p>4</p><p>5</p><p>6</p><p>7</p><p>8</p></div>
<script>
// 动画 canvas：持续重绘（合成器恒有新帧 → 软件限帧可测）
(function(){var c=document.getElementById('ani'),x=c.getContext('2d');
function draw(){x.fillStyle='#111';x.fillRect(0,0,414,896);
x.fillStyle='#'+((Date.now()/16|0)%4096).toString(16).padStart(3,'0');
x.fillRect(0,(Date.now()/8)%800,414,10);
requestAnimationFrame(draw)}draw()})();
// busy 模式（?busy=1）：主线程周期性阻塞 3.5s/5s——模拟真实云机页
// （WebRTC 视频/重 JS）让引擎 eval 常态秒级，检验输入泵让点击仍即时可达
if(location.search.indexOf('busy=1')>=0){
setInterval(function(){var t=Date.now();while(Date.now()-t<3500);},5000);
}
var n=0;
function log(m){try{fetch('/log',{method:'POST',
headers:{'content-type':'application/x-www-form-urlencoded'},
body:'level=click&msg='+encodeURIComponent(m)})}catch(e){}}
var btn=document.getElementById('btn');
btn.addEventListener('click',function(){n++;btn.textContent='点我（'+n+'）';log('click#'+n)});
btn.addEventListener('dblclick',function(){log('dblclick')});
var ti=document.getElementById('ti');
ti.addEventListener('input',function(){log('input:'+ti.value)});
ti.addEventListener('keydown',function(e){log('kd:'+e.key)});
document.addEventListener('touchstart',function(e){log('ts:'+e.touches.length)});
document.addEventListener('touchmove',function(e){log('tm:'+e.touches.length)});
document.addEventListener('touchend',function(e){log('te:'+(e.touches.length)+'/'+(e.changedTouches?e.changedTouches.length:0))});
var scr=document.getElementById('scr');
scr.addEventListener('scroll',function(){log('scroll:'+Math.round(scr.scrollTop))});
</script></body></html>""".encode("utf-8")

events = []

class Handler(__import__("http.server", fromlist=["BaseHTTPRequestHandler"]).BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        self.send_response(200)
        self.send_header("content-type", "text/html; charset=utf-8")
        self.end_headers()
        self.wfile.write(TEST_PAGE)
    def do_POST(self):
        n = int(self.headers.get("content-length", 0))
        body = self.rfile.read(n).decode("utf-8", "ignore")
        form = urllib.parse.parse_qs(body)
        msg = form.get("msg", [""])[0]
        if msg:
            events.append(msg)
        self.send_response(204); self.end_headers()

srv = __import__("http.server", fromlist=["ThreadingHTTPServer"]).ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
threading.Thread(target=srv.serve_forever, daemon=True).start()

os.system(f"rm -rf {DATA}")
env = dict(os.environ,
    CPK_CHROME_BIN=CHROME, CPK_URL=f"http://127.0.0.1:{PORT}/t.html",
    CPK_REPORT_PORT=str(CPK_PORT), CPK_BIND="127.0.0.1",
    CPK_DATA_DIR=DATA, CPK_SIMULATE_ACTIVITY="0",
    CPK_ACCOUNT="e2e", CPK_PLATFORM="mobile")
proc = subprocess.Popen([BIN], env=env,
                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

def get(path, timeout=8):
    return urllib.request.urlopen(f"http://127.0.0.1:{CPK_PORT}{path}", timeout=timeout)

def post(path, body, timeout=8):
    req = urllib.request.Request(f"http://127.0.0.1:{CPK_PORT}{path}",
        data=body.encode(), headers={"content-type": "application/x-www-form-urlencoded"})
    return urllib.request.urlopen(req, timeout=timeout)

def wait_events(pred, wait=2.0):
    t0 = time.time()
    while time.time() - t0 < wait:
        got = [e for e in events if pred(e)]
        if got:
            return got
        time.sleep(0.15)
    return []

ok = False
for _ in range(60):  # 等 page=ok（浏览器启动+导航+注入）
    try:
        v = get("/healthz", 4).read().decode()
        if '"page":"ok"' in v:
            ok = True; break
    except Exception:
        pass
    time.sleep(0.5)
if not ok:
    print("FAIL: page 未达 ok"); proc.terminate(); sys.exit(1)
print("page=ok ✓")

results = []
def check(name, cond, detail=""):
    results.append((name, bool(cond)))
    print(f"{'PASS' if cond else 'FAIL'}  {name}  {detail}")

# 视口：414x896（移动 emulation）；按钮在 ~60px 高度顶部区域
# ── 1) 真实鼠标左键点击 → click ─────────────────────────────
events.clear()
post("/mouse", "action=down&x=207&y=98&b=left&n=1&m=0&bb=1")
post("/mouse", "action=up&x=207&y=98&b=left&n=1&m=0&bb=0")
t0 = time.time()
clicks = wait_events(lambda e: e.startswith("click#"))
check("鼠标左键真实点击→click", clicks, f"HTTP≈{(time.time()-t0)*1000:.0f}ms {clicks}")

# ── 2) 双击（clickCount=2 → dblclick）──────────────────────
events.clear()
post("/mouse", "action=down&x=207&y=98&b=left&n=1&m=0&bb=1")
post("/mouse", "action=up&x=207&y=98&b=left&n=1&m=0&bb=0")
post("/mouse", "action=down&x=207&y=98&b=left&n=2&m=0&bb=1")
post("/mouse", "action=up&x=207&y=98&b=left&n=2&m=0&bb=0")
dbls = wait_events(lambda e: e == "dblclick")
check("clickCount=2→dblclick", dbls, f"{dbls}")

# ── 3) 键盘：真实鼠标点输入框聚焦 → /kbd 逐键打字 ──────────
events.clear()
# 输入框 y ≈ 60(按钮) + 54(按钮高) + 20(margin) + 25(半高) ≈ 160~190
post("/mouse", "action=down&x=207&y=178&b=left&n=1&m=0&bb=1")
post("/mouse", "action=up&x=207&y=178&b=left&n=1&m=0&bb=0")
time.sleep(0.4)
def kbd(t, key, code, vk, m=0, text=""):
    body = f"t={t}&key={urllib.parse.quote(key)}&code={code}&vk={vk}&m={m}&l=0&r=0"
    if text:
        body += "&text=" + urllib.parse.quote(text)
    post("/kbd", body)
for ch in "Hi":
    kbd("down", ch, "Key"+ch.upper(), ord(ch.upper()), 0, ch)
    kbd("up", ch, "Key"+ch.upper(), ord(ch.upper()), 0)
time.sleep(0.5)
kds = wait_events(lambda e: e.startswith("kd:"), 1.5)
ins = wait_events(lambda e: e.startswith("input:"), 1.5)
check("/kbd 键盘事件→keydown", [k for k in kds if k in ("kd:H", "kd:i")], f"{kds}")
check("/kbd 带文本→input", [i for i in ins if i.endswith("Hi")], f"{ins}")

# ── 4) /type 整段插入 + Backspace 退格 ─────────────────────
events.clear()
post("/type", "text=" + urllib.parse.quote("World"))
time.sleep(0.5)
ins = wait_events(lambda e: e.startswith("input:"), 1.5)
check("/type insertText", [i for i in ins if i.endswith("HiWorld")], f"{ins}")
kbd("down", "Backspace", "Backspace", 8)
kbd("up", "Backspace", "Backspace", 8)
time.sleep(0.5)
ins = wait_events(lambda e: e.startswith("input:"), 1.5)
check("Backspace 退格", [i for i in ins if i.endswith("HiWorl")], f"{ins}")

# ── 5) Ctrl+A 全选 → /clip 复制选区 ────────────────────────
kbd("down", "a", "KeyA", 65, m=2)
kbd("up", "a", "KeyA", 65, m=2)
time.sleep(0.4)
clip = get("/clip", 6).read().decode("utf-8", "ignore")
check("/clip 读取输入框选区（Ctrl+A 后）", clip == "HiWorl", f"clip={clip!r}")

# ── 6) 滚轮 → 滚动 ─────────────────────────────────────────
events.clear()
# #scr 大致位于 y≈207~427（按钮60+54 输入框~53+两段margin20）
post("/mouse", "action=wheel&x=207&y=300&dx=0&dy=600")
time.sleep(0.6)
scr = wait_events(lambda e: e.startswith("scroll:"), 1.5)
check("滚轮 wheel→scroll", scr, f"{scr}")

# ── 7) 多点触控（双指）────────────────────────────────────
events.clear()
post("/touch", "phase=start&ps=120,400,1")           # 第一指
post("/touch", "phase=start&ps=300,500,2")           # 第二指 → ts:2
post("/touch", "phase=move&ps=125,405,1;305,505,2")  # 双指移动 → tm:2
post("/touch", "phase=end&ps=300,500,2")             # 抬第二指 → te:1/1
post("/touch", "phase=end&ps=")                      # 全部抬起
time.sleep(0.8)
ts2 = wait_events(lambda e: e == "ts:2", 1.5)
tm2 = wait_events(lambda e: e == "tm:2", 1.5)
te1 = wait_events(lambda e: e == "te:1/1", 1.5)
check("双指按下 touches=2", ts2, f"{[e for e in events if e.startswith('ts')]}/{[e for e in events if e.startswith('te')]}")
check("双指移动 touches=2", tm2, f"{[e for e in events if e.startswith('tm')]}")
check("抬一指后 touches=1（手势延续）", te1, f"{[e for e in events if e.startswith('te')]}")

# ── 8) 单点轻点回归（触摸→合成 click）──────────────────────
events.clear()
post("/touch", "phase=start&x=207&y=98")
post("/touch", "phase=end&x=207&y=98")
clicks = wait_events(lambda e: e.startswith("click#"))
check("触摸轻点→click（回归）", clicks, f"{clicks}")

# ── 9) /fps 运行时改帧率 + 流继续出帧 ──────────────────────
post("/fps", "value=5")
hz = get("/healthz", 4).read().decode()
check("/fps→healthz 回显", '"fps":5' in hz, "")
resp = get("/stream.mjpg", 12)
t0 = time.time(); frames = 0; buf = b""
while time.time() - t0 < 4:
    chunk = resp.read(65536)
    if not chunk: break
    buf += chunk
    while b"\xff\xd8" in buf:
        buf = buf[buf.index(b"\xff\xd8") + 2:]
        frames += 1
check("改帧率后流继续出帧", frames >= 2, f"4s 内 {frames} 帧（fps=5）")
post("/fps", "value=25")

# ── 10) /nav fire 化：导航毫秒级返回 + 触摸立即可用 ─────────
events.clear()
t0 = time.time()
post("/nav", "url=" + urllib.parse.quote(f"http://127.0.0.1:{PORT}/t.html"), timeout=10)
nav_ms = (time.time() - t0) * 1000
time.sleep(1.0)   # 等新文档加载+脚本注入
events.clear()
post("/touch", "phase=start&x=207&y=98")
post("/touch", "phase=end&x=207&y=98")
clicks = wait_events(lambda e: e.startswith("click#"), 3.0)
check("/nav 返回毫秒级（fire 化）", nav_ms < 400, f"{nav_ms:.0f}ms")
check("导航后触摸立即可用（回首页回归）", clicks, f"{clicks}")

# ── 11) 重页面输入延迟（输入泵：eval 阻塞期点击仍即时可达）──
events.clear()
post("/nav", "url=" + urllib.parse.quote(f"http://127.0.0.1:{PORT}/t.html?busy=1"), timeout=10)
time.sleep(2.5)   # 等新文档加载 + busy 定时器起转
t_busy = time.time()
post("/mouse", "action=down&x=207&y=98&b=left&n=1&m=0&bb=1")
post("/mouse", "action=up&x=207&y=98&b=left&n=1&m=0&bb=0")
got = []
t0 = time.time()
while time.time() - t0 < 9:
    got = [e for e in events if e.startswith("click#")]
    if got: break
    time.sleep(0.2)
busy_ms = (time.time() - t_busy) * 1000
# busy 页主线程阻塞最长 3.5s（渲染侧固有）+ 泵修复后引擎侧 ≤0.2s ≈ 3.7s；
# 无输入泵时引擎 tick(5s超时)+sample(8s超时) eval 串行占线程，点击排队 7s+
check("重页面点击即时可达（输入泵）", bool(got) and busy_ms < 6000,
      f"点击→页面收到 {busy_ms:.0f}ms {'有事件' if got else '无事件'}")

# ── 12) 软件限帧：fps 目标值精确生效（maxFrameRate 无效的替代）──
def stream_count(seconds):
    resp = get("/stream.mjpg", seconds + 8)
    t0 = time.time(); n = 0; buf = b""
    while time.time() - t0 < seconds:
        chunk = resp.read(65536)
        if not chunk: break
        buf += chunk
    return buf.count(b"\xff\xd8")
post("/nav", "url=" + urllib.parse.quote(f"http://127.0.0.1:{PORT}/t.html"), timeout=10)
time.sleep(2.0)
post("/fps", "value=5")
time.sleep(1.0)
f5 = stream_count(6)
post("/fps", "value=30")
time.sleep(1.0)
f30 = stream_count(6)
check("软件限帧 fps=5 精确生效", 15 <= f5 <= 45, f"6s {f5} 帧（≈{f5/6:.1f} fps）")
check("软件限帧 fps=30 放开", f30 > f5, f"6s {f30} 帧（≈{f30/6:.1f} fps）")
post("/fps", "value=25")

# ── 13) 平台运行时切换：healthz 即刻回显 + 实例重启生效 ─────
def hz_read(t=8):
    # 重启期 healthz 合法返回 503（浏览器 stopped）——按 JSON 读而非异常
    import urllib.error
    try:
        return json.loads(get("/healthz", t).read().decode())
    except urllib.error.HTTPError as e:
        try:
            return json.loads(e.read().decode())
        except Exception:
            return {}
    except Exception:
        return {}
hz0 = hz_read()
r0 = hz0.get("restarts", 0)
post("/platform", "value=unicom", timeout=25)
time.sleep(1.0)
hz1 = hz_read()
check("/platform→healthz 即刻回显", hz1.get("platform") == "unicom"
      and hz1.get("platformLabel") == "联通云手机"
      and hz1.get("vw") == 405 and hz1.get("vh") == 720,
      f"{hz1.get('platform')}/{hz1.get('vw')}x{hz1.get('vh')}")
# 重启异步完成（kill+relaunch+attach 约 10s）：等计数上升而非 1s 即查
r_new = r0
t0 = time.time()
while time.time() - t0 < 40:
    if hz_read(5).get("restarts", 0) > r0:
        r_new = hz_read(5).get("restarts", 0); break
    time.sleep(1.5)
check("平台切换触发实例重启", r_new > r0, f"restarts {r0}→{r_new}")
# 等联通首页加载（真实站点，网络可达；CI 无网时容忍 nav-error）
ok_uni = False
t0 = time.time()
while time.time() - t0 < 60:
    hz = hz_read(5)
    if hz.get("page") == "ok" or "uphone" in (hz.get("pageUrl") or ""):
        ok_uni = True; break
    if hz.get("page") == "nav-error":
        break
    time.sleep(2)
check("联通平台首页加载", ok_uni, f"page={hz.get('page')} url={ (hz.get('pageUrl') or '')[:50]}")
# 切回移动（回到真实移动站）
post("/platform", "value=mobile", timeout=25)
time.sleep(1.0)
hz2 = hz_read()
check("切回移动平台", hz2.get("platform") == "mobile" and hz2.get("vw") == 414,
      f"{hz2.get('platform')}/{hz2.get('vw')}x{hz2.get('vh')}")

proc.send_signal(signal.SIGTERM)
try: proc.wait(10)
except Exception: proc.kill()
srv.shutdown()

# ── 日志侧检查：无 WS 错误 ─────────────────────────────────
log_files = []
for root, _, files in os.walk(f"{DATA}/logs"):
    log_files += [os.path.join(root, f) for f in files]
text = "".join(open(f, encoding="utf-8", errors="ignore").read() for f in log_files)
ws_err = text.count("WS:")
check("引擎日志 WS 传输错误=0", ws_err == 0, f"{ws_err} 次")

print("=" * 56)
fails = [r for r in results if not r[1]]
print(f"结论：{len(results)-len(fails)}/{len(results)} 通过")
sys.exit(1 if fails else 0)
