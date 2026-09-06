#!/usr/bin/env python3
"""跨域 iframe 精探：鼠标事件是否路由进 OOPIF + 触摸坐标映射。
phone.html 带坐标日志的探针网格。"""
import os, sys, time, threading, json, subprocess, urllib.request, urllib.parse
import http.server

CHROME = "/home/z/.cache/puppeteer/chrome-headless-shell/linux-152.0.7977.54/chrome-headless-shell-linux64/chrome-headless-shell"
BIN = "/home/z/my-project/cloudphonekeep/linux/target/release/cloudphonekeep"
CPK_PORT = 18096
PORT_TOP = 18097
PORT_PHONE = 18098
DATA = "/tmp/cpk-iframe-data"

# 探针页：整页网格分区，每个区域记录 touch/mouse/click + 坐标
PHONE = """<!doctype html><html><head><meta charset=utf-8></head>
<body style="margin:0">
<div id="z1" style="height:120px;background:#4a6;color:#fff;padding:10px">A区 0-120px（按钮带touch/click）</div>
<div id="z2" style="height:240px;background:#888;padding:10px">B区 120-360px</div>
<div id="z3" style="height:240px;background:#a86;padding:10px">C区 360-600px</div>
<script>
var log=[];
function rec(tag,e){
  var cx='-',cy='-';
  try{
    if(e.touches&&e.touches[0]){cx=Math.round(e.touches[0].clientX);cy=Math.round(e.touches[0].clientY)}
    else if(e.changedTouches&&e.changedTouches[0]){cx=Math.round(e.changedTouches[0].clientX);cy=Math.round(e.changedTouches[0].clientY)}
    else if('clientX'in e){cx=Math.round(e.clientX);cy=Math.round(e.clientY)}
  }catch(err){}
  var el=(e.target&&e.target.id)||e.target;
  log.push(tag+'@('+cx+','+cy+')->'+el);
  try{fetch('/log?ev='+encodeURIComponent(log[log.length-1]),{mode:'no-cors'})}catch(e2){}
}
['touchstart','touchend','mousedown','mouseup','click'].forEach(function(t){
  document.addEventListener(t,function(e){rec(t,e)},true);
});
window.__EV=function(){return log.join('|')};
</script></body></html>"""

TOP = """<!doctype html><html><head><meta charset=utf-8></head><body style="margin:0">
<iframe id="f" src="http://localhost:%d/phone.html" style="border:0;width:414px;height:600px"></iframe>
</body></html>""" % PORT_PHONE

events = []
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def _page(self, html):
        self.send_response(200)
        self.send_header("content-type", "text/html; charset=utf-8")
        self.end_headers()
        self.wfile.write(html.encode())
    def do_GET(self):
        if self.path.startswith("/log"):
            events.append(urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query).get("ev", [""])[0])
            self.send_response(204); self.end_headers()
        elif "phone" in self.path: self._page(PHONE)
        else: self._page(TOP)

for port in (PORT_TOP, PORT_PHONE):
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", port), H)
    threading.Thread(target=srv.serve_forever, daemon=True).start()

def get(path, timeout=15):
    return urllib.request.urlopen(f"http://127.0.0.1:{CPK_PORT}{path}", timeout=timeout)
def post(path, body, timeout=15):
    req = urllib.request.Request(f"http://127.0.0.1:{CPK_PORT}{path}",
        data=body.encode(), headers={"content-type": "application/x-www-form-urlencoded"})
    return urllib.request.urlopen(req, timeout=timeout)

os.system(f"rm -rf {DATA}")
env = dict(os.environ, CPK_CHROME_BIN=CHROME,
    CPK_URL=f"http://127.0.0.1:{PORT_TOP}/top.html",
    CPK_REPORT_PORT=str(CPK_PORT), CPK_BIND="127.0.0.1", CPK_DATA_DIR=DATA,
    CPK_SIMULATE_ACTIVITY="0", CPK_ACCOUNT="probe", CPK_PLATFORM="mobile")
subprocess.Popen(["setsid", BIN], env=env,
                 stdout=open("/tmp/cpk-probe.log", "w"), stderr=subprocess.STDOUT,
                 start_new_session=True)

t0 = time.time(); page = ""
while time.time() - t0 < 60:
    try:
        v = json.loads(get("/healthz", 5).read().decode())
        if v.get("page") == "ok": break
    except Exception: pass
    time.sleep(1.2)
assert v.get("page") == "ok", "未就绪"
print("[就绪]")
time.sleep(2)

# 事件靠 phone.html fetch /log 上报（同 handler 收集）
def read_console_events():
    out = list(events)
    return out

def mark(): return len(read_console_events())

# —— 测试矩阵 ——
tests = [
    # (名, 请求序列, 期望)
    ("鼠标点击A区(60)", [("mouse","action=down&x=207&y=60&b=left&n=1&m=0&bb=1"),
                        ("mouse","action=up&x=207&y=60&b=left&n=1&m=0&bb=0")]),
    ("触摸轻点A区(60)", [("touch","phase=start&ps=207,60,1"),("touch","phase=end&")]),
    ("触摸轻点B区(240)", [("touch","phase=start&ps=207,240,1"),("touch","phase=end&")]),
    ("触摸轻点C区(480)", [("touch","phase=start&ps=207,480,1"),("touch","phase=end&")]),
    ("纯悬停B区", [("mouse","action=move&x=207&y=240&b=none&bb=0&m=0")]),
    ("鼠标拖动A→B", [("mouse","action=down&x=207&y=60&b=left&n=1&m=0&bb=1"),
                     ("mouse","action=move&x=207&y=240&b=left&bb=1&m=0"),
                     ("mouse","action=up&x=207&y=240&b=left&n=1&m=0&bb=0")]),
]
for name, seq in tests:
    m0 = mark()
    for path, body in seq:
        post("/"+path, body); time.sleep(0.05)
    time.sleep(0.8)
    got = read_console_events()[m0:]
    print(f"[{name}] → {got if got else '无事件'}")

os.system("pkill -f 'cloudphonekeep' 2>/dev/null")
print("完成")
