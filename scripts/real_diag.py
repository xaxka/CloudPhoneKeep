#!/usr/bin/env python3
"""真实云手机页三链路诊断：①鼠标点击 ②触摸 ③fps 限制。
在真实页面里注入事件记录器 → 经控制端点发输入 → 读回记录 → 判定。
"""
import os, sys, time, json, threading, urllib.request, urllib.parse, subprocess

CHROME = "/home/z/.cache/puppeteer/chrome-headless-shell/linux-152.0.7977.54/chrome-headless-shell-linux64/chrome-headless-shell"
BIN = "/home/z/my-project/cloudphonekeep/linux/target/release/cloudphonekeep"
CPK_PORT = 18096
DATA = "/tmp/cpk-real-data"
URL = "https://cloudphoneh5.buy.139.com"

def get(path, timeout=15):
    return urllib.request.urlopen(f"http://127.0.0.1:{CPK_PORT}{path}", timeout=timeout)

def post(path, body, timeout=15):
    req = urllib.request.Request(f"http://127.0.0.1:{CPK_PORT}{path}",
        data=body.encode(), headers={"content-type": "application/x-www-form-urlencoded"})
    return urllib.request.urlopen(req, timeout=timeout)

# —— 1. 后台启动引擎（setsid 脱离会话，跨 bash 命令存活）——
os.system(f"rm -rf {DATA}")
env = dict(os.environ, CPK_CHROME_BIN=CHROME, CPK_URL=URL,
    CPK_REPORT_PORT=str(CPK_PORT), CPK_BIND="127.0.0.1", CPK_DATA_DIR=DATA,
    CPK_SIMULATE_ACTIVITY="0", CPK_ACCOUNT="repro", CPK_PLATFORM="mobile")
subprocess.Popen(["setsid", BIN], env=env,
                 stdout=open("/tmp/cpk-real.log", "w"), stderr=subprocess.STDOUT,
                 start_new_session=True)

# —— 2. 等 page=ok ——
t0 = time.time(); page = ""
while time.time() - t0 < 90:
    try:
        v = json.loads(get("/healthz", 5).read().decode())
        page = v.get("page", "")
        if page == "ok": break
    except Exception: pass
    time.sleep(1.5)
assert page == "ok", f"未就绪 page={page}"
h = v
print(f"[就绪] page={page} url={h.get('pageUrl','')[:60]} 视口={h.get('vw')}x{h.get('vh')} fps={h.get('fps')}")

# —— 3. 注入事件记录器（经 /nav 不可行；用页面内 eval？—— 控制端无 eval 端点。
#      改用 /type 与截图对比太糙。直接用截图对比方案：
#      3a. 截图 A（点击前）
#      3b. 在页面中部（app 列表区域）做鼠标点击（auto 模式的真实序列）
#      3c. 截图 B（点击后）→ 对比字节差异
# —— 同时用 /healthz 的 ticks/clicks/dialogs 观察引擎侧是否受理。
def shot():
    return get("/shot.jpg", 20).read()

def frames_of(mjpg_bytes):  # 数 MJPEG 帧数
    return mjpg_bytes.count(b"\xff\xd8")

# 3. 记录器方案B：用引擎已受理日志判断 —— tail 引擎日志里的 [click] 行
def engine_clicks():
    n = 0
    try:
        with open("/tmp/cpk-real.log", encoding="utf-8", errors="ignore") as f:
            for line in f:
                if "[click]" in line and "触摸按下" in line: n += 1
                elif "[click]" in line and "鼠标按下" in line: n += 1
    except FileNotFoundError: pass
    return n

# —— 鼠标点击（真实序列：down+up，clickCount=1）——
before = engine_clicks()
post("/mouse", "action=down&x=207&y=400&b=left&n=1&m=0&bb=1")
post("/mouse", "action=up&x=207&y=400&b=left&n=1&m=0&bb=0")
time.sleep(0.6)
after = engine_clicks()
print(f"[链路1-引擎侧] 鼠标点击 → 引擎已受理日志行：{before}→{after}（>0 即链路通）")

# —— 触摸轻点 ——
before = engine_clicks()
post("/touch", "phase=start&ps=207,400,1")
time.sleep(0.08)
post("/touch", "phase=end&")
time.sleep(0.6)
after = engine_clicks()
print(f"[链路2-引擎侧] 触摸轻点 → 引擎已受理：{before}→{after}")

# —— 页面侧效果判定：截图对比（点击可能触发弹窗/路由跳转 → 画面变化）——
sa = shot()
post("/mouse", "action=down&x=207&y=500&b=left&n=1&m=0&bb=1")
post("/mouse", "action=up&x=207&y=500&b=left&n=1&m=0&bb=0")
time.sleep(1.2)
sb = shot()
print(f"[链路1-页面侧] 点击前后截图 {len(sa)}B/{len(sb)}B {'有变化' if sa != sb else '无变化（点击可能无效）'}")

# —— 链路3：fps 限制 ——
# 页面注入 CSS 动画强制持续重绘（经 /type 无法注入 style；用 /kbd 打开控制台也不行——
# 直接读 /stream.mjpg 数帧：真实页面有动画/视频即出帧）
def stream_frames(seconds):
    frames = [0]
    def run():
        try:
            r = urllib.request.urlopen(f"http://127.0.0.1:{CPK_PORT}/stream.mjpg", timeout=seconds + 5)
            t_end = time.time() + seconds
            buf = b""
            while time.time() < t_end:
                chunk = r.read(65536)
                if not chunk: break
                buf += chunk
                frames[0] = buf.count(b"\xff\xd8")
        except Exception as e:
            print(f"  流读取异常: {e}")
    th = threading.Thread(target=run); th.start(); th.join(seconds + 6)
    return frames[0]

post("/fps", "value=30")
time.sleep(1.0)
f30 = stream_frames(6)
print(f"[链路3] fps=30 → 6s 收到 {f30} 帧（≈{f30/6:.1f} fps）")
post("/fps", "value=5")
time.sleep(1.0)
f5 = stream_frames(6)
print(f"[链路3] fps=5  → 6s 收到 {f5} 帧（≈{f5/6:.1f} fps）")
hz = json.loads(get("/healthz", 5).read().decode())
print(f"[链路3] healthz 回显 fps={hz.get('fps')}")
verdict = "有效" if f30 > 0 and f5 * 2 < f30 else ("无效或页面静态" if f30 == 0 else "可疑")
print(f"[链路3-判定] {verdict}")

os.system("pkill -f 'cloudphonekeep' 2>/dev/null")
print("完成")
