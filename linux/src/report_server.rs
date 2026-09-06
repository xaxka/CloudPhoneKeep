//! 127.0.0.1/0.0.0.0 回环 HTTP 服务（对齐 Windows 版 report_server.rs 语义）：
//!  页面把 127.0.0.1 视为可信来源，HTTPS 页面 fetch http://127.0.0.1
//!  不受混合内容策略拦截 —— 同一机制在 Chromium Headless 下继续生效。
//!
//!  - GET  /report?status=…   页面状态上报（alive/retry/enter/confirm/try-enable/
//!                            expired/exited/paused/error/installed）
//!  - POST /log               页面诊断日志落盘（level+msg 表单）
//!  - GET  /healthz /status   健康检查（JSON；不健康 503）
//!  - GET  /                  控制页：左实时画面 + 右操作栏（自适应布局）
//!  - GET  /stream.mjpg       实时画面流（MJPEG multipart，Page.startScreencast 帧直推，
//!                            断流自动重连；页面侧不可用时退化 /shot.jpg 轮询）
//!  - GET  /shot.jpg          单帧页面截图（JPEG，兼容/兜底用）
//!  - POST /tap /swipe /type /key /nav /reload  控制端点（token 可选保护）
//!
//!  说明：控制端点经 channel 由引擎线程用 CDP Input 域执行 = 内核级触摸模拟，
//!        无需桌面/VNC；/report 与 /log 即使被 Chromium 专用网络访问(PNA)策略
//!        拦截也不影响保活（诊断另有 CDP __CPK_DRAIN__ 通道兜底，双保险）。

use crate::engine::{health_json, ControlRequest, SharedState};
use crate::logger::Logger;
use crate::util::urldecode;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// 状态转换提示（对齐 Windows report_server.rs 的通知语义，容器内落日志）
fn notify_text(status: &str) -> Option<&'static str> {
    match status {
        "exited" => Some("已退出云机"),
        "expired" => Some("云手机到期弹窗已确认"),
        "entered" => Some("已自动进入云机"),
        _ => None,
    }
}

#[derive(Clone)]
pub struct ReportCfg {
    pub bind: String,
    pub port: u16,
    pub control_token: String,
}

impl ReportCfg {
    pub fn from(cfg: &crate::config::Config) -> ReportCfg {
        ReportCfg {
            bind: cfg.bind.clone(),
            port: cfg.report_port,
            control_token: cfg.control_token.clone(),
        }
    }
}

const CONTROL_PAGE_HTML: &str = r#"<!doctype html>
<html lang="zh"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>CloudPhoneKeep 控制台</title>
<style>
:root{--bg:#0f172a;--panel:#1e293b;--line:#334155;--txt:#e2e8f0;--dim:#94a3b8}
*{box-sizing:border-box}
html,body{height:100%}
body{margin:0;background:var(--bg);color:var(--txt);font:14px/1.5 system-ui,sans-serif;
display:flex;flex-direction:column}
header{display:flex;align-items:center;gap:10px;padding:8px 14px;background:var(--panel);
border-bottom:1px solid var(--line);flex:none}
header h1{font-size:15px;margin:0;font-weight:600}
#dot{width:9px;height:9px;border-radius:50%;background:#64748b;flex:none}
#dot.ok{background:#22c55e}#dot.bad{background:#ef4444}
#fps{color:var(--dim);font-size:12px}
#meta{color:var(--dim);font-size:12px;margin-left:auto;text-align:right;line-height:1.35;
max-width:46vw;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
main{flex:1;display:flex;min-height:0}
#stage{flex:1;display:flex;align-items:center;justify-content:center;padding:12px;min-width:0}
#wrap{position:relative;height:100%;aspect-ratio:414/896;max-height:100%;max-width:100%;background:#020617;
border:1px solid var(--line);border-radius:14px;overflow:hidden;box-shadow:0 6px 28px #0009}
#wrap:fullscreen{border-radius:0;border:0}
#shot{width:100%;height:100%;display:block;object-fit:contain;cursor:pointer;
touch-action:none;user-select:none;-webkit-user-select:none}
#overlay{position:absolute;inset:0;display:flex;flex-direction:column;gap:8px;align-items:center;
justify-content:center;background:#020617e6;color:var(--dim);font-size:13px;text-align:center;padding:0 24px}
#overlay.hidden{display:none}
#ovt{color:var(--txt);font-size:15px}
#panel{flex:none;width:300px;background:var(--panel);border-left:1px solid var(--line);
padding:12px 14px;overflow-y:auto;display:flex;flex-direction:column;gap:9px}
#panel h2{font-size:11px;margin:4px 0 0;color:var(--dim);font-weight:600;
letter-spacing:.08em;text-transform:uppercase}
.row{display:flex;gap:6px;flex-wrap:wrap}
input[type=text]{flex:1;min-width:120px;background:var(--bg);color:var(--txt);
border:1px solid var(--line);border-radius:6px;padding:7px 9px}
button{background:var(--line);color:var(--txt);border:0;border-radius:6px;padding:7px 12px;
cursor:pointer;font-size:13px}
button:hover{background:#475569}
button.acc{background:#0369a1}button.acc:hover{background:#0284c7}
#stats{font-size:12px;color:var(--dim);line-height:1.65;word-break:break-all}
#stats b{color:var(--txt);font-weight:600}
#stats .warn{color:#f87171}
.note{color:var(--dim);font-size:12px;line-height:1.55}
#toast{position:fixed;bottom:14px;left:50%;transform:translateX(-50%);background:var(--line);
padding:6px 14px;border-radius:8px;opacity:0;transition:opacity .3s;pointer-events:none;font-size:13px}
@media (max-width:820px){
main{flex-direction:column}
#stage{flex:1;min-height:0;padding:8px}
#wrap{height:auto;width:100%;aspect-ratio:414/896;max-height:100%}
#panel{width:auto;border-left:0;border-top:1px solid var(--line);max-height:46%}
}
</style></head><body>
<header><span id="dot"></span><h1>CloudPhoneKeep</h1><span id="fps"></span>
<div id="meta">连接中…</div></header>
<main>
<section id="stage"><div id="wrap">
<img id="shot" alt="云手机实时画面" draggable="false">
<div id="overlay"><div id="ovt">等待画面…</div><div id="ovs"></div></div>
</div></section>
<aside id="panel">
<h2>状态</h2>
<div id="stats">—</div>
<h2>操作</h2>
<div class="row"><input type="text" id="text" placeholder="输入文本，回车发送"></div>
<div class="row">
<button onclick="sendKey('Enter')">Enter</button>
<button onclick="sendKey('Backspace')">⌫ 删除</button>
<button onclick="doReload()">刷新</button>
</div>
<div class="row">
<button onclick="doNav()">回首页</button>
<button class="acc" onclick="fs()">全屏</button>
</div>
<div class="row">
<button onclick="swipe(0,-1)">↑ 上滑</button>
<button onclick="swipe(0,1)">↓ 下滑</button>
<button onclick="swipe(-1,0)">← 左滑</button>
<button onclick="swipe(1,0)">→ 右滑</button>
</div>
<h2>说明</h2>
<div class="note">左侧为云手机实时画面：单击＝触摸，按住拖动＝滑动（返回/切后台等手势）。
首次登录：画面中点「登录」→ 点手机号输入框 → 右侧输入手机号回车 → 收到验证码后输入 →
登录态自动持久化，之后免登录。画面不更新或空白时，看上方「状态」诊断（页面/lastError）。</div>
</aside>
</main>
<div id="toast"></div>
<script>
var TK=(new URLSearchParams(location.search)).get('token')||'';
var VW=414,VH=896,HOME='';
var live={mode:'none',abort:null,frames:0,last:0,shotTimer:null};
var IMG=document.getElementById('shot');
function esc(s){return String(s==null?'':s).replace(/[&<>"']/g,function(c){
return {'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]})}
function U(p){var u=new URL(p,location.origin);if(TK)u.searchParams.set('token',TK);return u}
function HDR(){return TK?{'x-cpk-token':TK}:{}}
function post(p,body){return fetch(U(p),{method:'POST',
headers:Object.assign({'content-type':'application/x-www-form-urlencoded'},HDR()),body:body})}
function ping(t){var el=document.getElementById('toast');el.textContent=t;el.style.opacity=1;
setTimeout(function(){el.style.opacity=0},1500)}
function ov(t,s){var o=document.getElementById('overlay');
document.getElementById('ovt').textContent=t||'';
document.getElementById('ovs').textContent=s||'';
if(t){o.classList.remove('hidden')}else{o.classList.add('hidden')}}

// —— 状态轮询（3s）：状态灯 + 面板 + 视口尺寸（触摸坐标映射基准）——
function poll(){
fetch(U('/healthz')).then(function(r){return r.json()}).then(function(j){
VW=j.vw||414;VH=j.vh||896;
document.getElementById('wrap').style.aspectRatio=VW+'/'+VH;
document.getElementById('dot').className=j.ok?'ok':'bad';
document.getElementById('meta').textContent=(j.account||'')+' · '+(j.platformLabel||'')+
' · '+(j.page||'')+(j.exited?' · 已退出云机!':'');
if(j.homeUri)HOME=j.homeUri;
document.getElementById('stats').innerHTML=
'<b>浏览器</b> '+esc(j.browser)+' · <b>页面</b> '+esc(j.page)+
'<br><b>ticks</b> '+j.ticks+' · <b>clicks</b> '+j.clicks+' · <b>弹窗</b> '+j.dialogs+
'<br><b>重启</b> '+j.restarts+' · <b>重载</b> '+j.reloads+
' · <b>心跳</b> '+(j.lastBeatAge==null?'—':j.lastBeatAge+'s')+
'<br><b>URL</b> '+esc((j.pageUrl||'').slice(0,80))+
(j.lastError?'<br><span class="warn"><b>错误</b> '+
esc(String(j.lastError).slice(0,100))+'</span>':'')+
(j.exited?'<br><span class="warn">已退出云机！</span>':'');
}).catch(function(){document.getElementById('dot').className='bad'});
}
poll();setInterval(poll,3000);

// —— 实时画面：fetch MJPEG 流 → JPEG SOI/EOI 切帧 → Blob 直显 ——
// 断流（引擎重建/浏览器重启）自动重连；流建立失败 → 截图轮询兜底，8s 后重试实时流
function stopLive(){
if(live.abort){try{live.abort.abort()}catch(e){}live.abort=null}
if(live.shotTimer){clearTimeout(live.shotTimer);live.shotTimer=null}
}
function setShot(url){
if(IMG._url)URL.revokeObjectURL(IMG._url);
IMG._url=url;IMG.src=url;
}
function startLive(){
stopLive();live.mode='live';
var ac=new AbortController();live.abort=ac;
ov('连接实时画面…');
fetch(U('/stream.mjpg'),{headers:HDR(),signal:ac.signal}).then(function(r){
if(!r.ok||!r.body)throw new Error('HTTP '+r.status);
var reader=r.body.getReader(),buf=new Uint8Array(0);
function push(a){var b=new Uint8Array(buf.length+a.length);b.set(buf);b.set(a,buf.length);buf=b}
function cut(){for(var i=0;i<buf.length-1;i++){
if(buf[i]===255&&buf[i+1]===216){
for(var j=i+2;j<buf.length-1;j++){
if(buf[j]===255&&buf[j+1]===217){
var f=buf.slice(i,j+2);buf=buf.slice(j+2);return f}}}}
return null}
ov('等待首帧…');
function step(){
reader.read().then(function(x){
if(x.done)throw new Error('end');
push(x.value);
var f;
while((f=cut())!==null){
setShot(URL.createObjectURL(new Blob([f],{type:'image/jpeg'})));
live.frames++;ov(null);
}
step();
}).catch(function(){
if(live.abort!==ac)return;
setTimeout(startLive,1500);
});
}
step();
}).catch(function(){
if(live.abort!==ac)return;
shotMode();
});
}
function shotMode(){
live.mode='shot';
ov('截图模式','实时流暂不可用（引擎忙或重启中），0.6s/帧轮询');
(function loop(){
if(live.mode!=='shot')return;
IMG.onload=function(){if(live.mode==='shot')ov(null)};
IMG.onerror=function(){if(live.mode==='shot')ov('画面暂不可用','引擎启动/重启中，自动重试…')};
IMG.src=U('/shot.jpg?_='+Date.now()).href;
live.shotTimer=setTimeout(loop,600);
})();
setTimeout(function(){if(live.mode==='shot')startLive()},8000);
}
document.addEventListener('visibilitychange',function(){
if(!document.hidden&&live.mode!=='shot')startLive();
});
startLive();
setInterval(function(){
var fps=live.frames-live.last;live.last=live.frames;
document.getElementById('fps').textContent=live.mode==='live'?(fps+' fps'):'';
},1000);

// —— 触摸坐标映射：帧原始尺寸等比换算（object-fit:contain 居中修正）——
function xy(cx,cy){
var r=IMG.getBoundingClientRect();
var nw=IMG.naturalWidth||VW,nh=IMG.naturalHeight||VH;
var s=Math.min(r.width/nw,r.height/nh),dw=nw*s,dh=nh*s;
var ox=r.left+(r.width-dw)/2,oy=r.top+(r.height-dh)/2;
return [Math.max(0,Math.min(nw,(cx-ox)/dw*nw)),Math.max(0,Math.min(nh,(cy-oy)/dh*nh))];
}
var lastSwipe=0;
IMG.addEventListener('click',function(ev){
if(Date.now()-lastSwipe<450)return;
var p=xy(ev.clientX,ev.clientY);
post('/tap','x='+p[0].toFixed(1)+'&y='+p[1].toFixed(1))
.then(function(){ping('触摸 '+Math.round(p[0])+','+Math.round(p[1]))});
});
var drag=null;
IMG.addEventListener('pointerdown',function(ev){
drag={x:ev.clientX,y:ev.clientY};
try{this.setPointerCapture(ev.pointerId)}catch(e){}
});
IMG.addEventListener('pointerup',function(ev){
if(!drag)return;
var dx=ev.clientX-drag.x,dy=ev.clientY-drag.y,a=xy(drag.x,drag.y),b=xy(ev.clientX,ev.clientY);
drag=null;
if(Math.abs(dx)<10&&Math.abs(dy)<10)return;
lastSwipe=Date.now();
post('/swipe','x1='+a[0].toFixed(1)+'&y1='+a[1].toFixed(1)+
'&x2='+b[0].toFixed(1)+'&y2='+b[1].toFixed(1))
.then(function(){ping('滑动 '+Math.round(dx)+','+Math.round(dy))});
});
// —— 面板操作 ——
function swipe(hx,hy){
var cx=VW/2,cy=VH/2,d=Math.min(VW,VH)*0.35;
post('/swipe','x1='+cx+'&y1='+cy+'&x2='+(cx+hx*d)+'&y2='+(cy+hy*d));
ping(hx?(hx>0?'右滑':'左滑'):(hy>0?'下滑':'上滑'));
}
function sendKey(k){post('/key','key='+encodeURIComponent(k)).then(function(){ping('按键 '+k)})}
function doReload(){post('/reload','').then(function(){ping('已刷新页面')})}
function doNav(){if(!HOME){ping('未知首页地址');return}
post('/nav','url='+encodeURIComponent(HOME)).then(function(){ping('已回首页')})}
function fs(){var el=document.getElementById('wrap');
if(document.fullscreenElement){document.exitFullscreen()}
else if(el.requestFullscreen){el.requestFullscreen()}}
document.getElementById('text').addEventListener('keydown',function(ev){
if(ev.key!=='Enter')return;
var v=this.value;this.value='';
post('/type','text='+encodeURIComponent(v)).then(function(){
return post('/key','key=Enter');
}).then(function(){ping('已输入 '+v.slice(0,24))});
});
</script></body></html>"#;

/// 启动服务（端口绑定必须在保活脚本注入前完成——脚本里写死了端口号）
pub fn start(
    cfg: ReportCfg,
    logger: Arc<Logger>,
    shared: Arc<SharedState>,
    ctrl: Sender<ControlRequest>,
) -> Result<u16, String> {
    let listener = TcpListener::bind((cfg.bind.as_str(), cfg.port))
        .map_err(|e| format!("绑定 {}:{} 失败：{e}", cfg.bind, cfg.port))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    thread::spawn(move || {
        for conn in listener.incoming() {
            match conn {
                Ok(stream) => {
                    let cfg = cfg.clone();
                    let logger = logger.clone();
                    let shared = shared.clone();
                    let ctrl = ctrl.clone();
                    thread::spawn(move || {
                        let _ = handle_conn(stream, &cfg, &logger, &shared, &ctrl);
                    });
                }
                Err(_) => thread::sleep(Duration::from_millis(200)),
            }
        }
    });
    Ok(port)
}

struct Req {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    form: HashMap<String, String>,
}

fn handle_conn(
    mut stream: TcpStream,
    cfg: &ReportCfg,
    logger: &Arc<Logger>,
    shared: &Arc<SharedState>,
    ctrl: &Sender<ControlRequest>,
) -> Result<(), String> {
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(30))).ok();
    let req = parse_request(&mut stream)?;
    // 实时画面流：响应体无界（multipart），绕过 route/respond 一次性模型直写 socket
    if req.method == "GET" && req.path == "/stream.mjpg" {
        if !token_ok(&req, cfg) {
            respond(&mut stream, 403, "text/plain", "forbidden: token mismatch".as_bytes());
        } else {
            stream_mjpeg(&mut stream, ctrl, logger);
        }
        let _ = stream.shutdown(std::net::Shutdown::Both);
        return Ok(());
    }
    let (status, ctype, body) = route(&req, cfg, logger, shared, ctrl);
    respond(&mut stream, status, &ctype, &body);
    let _ = stream.shutdown(std::net::Shutdown::Both);
    Ok(())
}

/// 实时画面流：向引擎订阅 screencast 帧，以 multipart/x-mixed-replace 推送（MJPEG）。
/// 退出条件：客户端断开（写失败）/ 引擎 10s 无帧（CDP 重建或浏览器重启）——
/// 关流后页面侧自动重连，无需服务端维持状态。
fn stream_mjpeg(stream: &mut TcpStream, ctrl: &Sender<ControlRequest>, logger: &Arc<Logger>) {
    const BOUNDARY: &str = "cpkframe";
    // 1) 订阅引擎实时画面（3s 内未应答 = 引擎忙/浏览器启动中）
    let (tx, rx) = std::sync::mpsc::channel();
    if ctrl.send(ControlRequest::ScreencastAttach { reply: tx }).is_err() {
        respond(stream, 500, "text/plain; charset=utf-8", "引擎不可用".as_bytes());
        return;
    }
    let (sub_id, frame_rx) = match rx.recv_timeout(Duration::from_secs(3)) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            respond(
                stream,
                500,
                "text/plain; charset=utf-8",
                format!("画面流建立失败：{e}").as_bytes(),
            );
            return;
        }
        Err(_) => {
            respond(
                stream,
                504,
                "text/plain; charset=utf-8",
                "画面流建立超时（引擎忙或浏览器启动中）".as_bytes(),
            );
            return;
        }
    };
    // 2) 流头 + 帧循环（multipart；每次写失败即客户端已断开）
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary={BOUNDARY}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(head.as_bytes()).is_err() {
        screencast_detach(ctrl, sub_id);
        return;
    }
    let mut last_frame = Instant::now();
    loop {
        match frame_rx.recv_timeout(Duration::from_millis(500)) {
            Ok(frame) => {
                let part = format!(
                    "--{BOUNDARY}\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
                    frame.len()
                );
                if stream.write_all(part.as_bytes()).is_err()
                    || stream.write_all(&frame).is_err()
                    || stream.write_all(b"\r\n").is_err()
                {
                    break;
                }
                last_frame = Instant::now();
            }
            Err(_) => {
                // 500ms 无帧：静态页面属正常（合成器无更新，不推帧）；
                // >10s 无帧 = 引擎重建/浏览器重启 → 关流，页面侧自动重连
                if last_frame.elapsed() > Duration::from_secs(10) {
                    logger.log(1, "sys", "实时画面流 10s 无帧，关流等页面重连（引擎重建/浏览器重启）");
                    break;
                }
            }
        }
    }
    screencast_detach(ctrl, sub_id);
}

/// 发后即忘的取消订阅：应答接收端立即丢弃（引擎应答时发送失败被静默忽略），
/// 引擎在下一监督周期执行；最后一个订阅者离开时自动 Page.stopScreencast。
fn screencast_detach(ctrl: &Sender<ControlRequest>, id: u32) {
    let (tx, _rx) = std::sync::mpsc::channel();
    let _ = ctrl.send(ControlRequest::ScreencastDetach { id, reply: tx });
}

fn parse_request(stream: &mut TcpStream) -> Result<Req, String> {
    let mut raw: Vec<u8> = Vec::new();
    let mut buf = [0u8; 2048];
    // 读到请求头结束
    let head_end = loop {
        if let Some(p) = find(&raw, b"\r\n\r\n") {
            break p + 4;
        }
        if raw.len() > 32768 {
            return Err("请求头过大".into());
        }
        match stream.read(&mut buf) {
            Ok(0) => return Err("连接过早关闭".into()),
            Ok(n) => raw.extend_from_slice(&buf[..n]),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Err("读取请求超时".into())
            }
            Err(e) => return Err(format!("读取失败：{e}")),
        }
    };
    let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_uppercase();
    let target = parts.next().unwrap_or("/");
    let (path, query_str) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.to_string(), String::new()),
    };
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    // body（POST 表单，上限 64KB）
    let content_len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut body: Vec<u8> = raw[head_end..].to_vec();
    while body.len() < content_len && body.len() < 65536 {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }
    }
    let query = parse_form(&query_str);
    let form = if method == "POST" {
        parse_form(&String::from_utf8_lossy(&body))
    } else {
        HashMap::new()
    };
    Ok(Req { method, path, query, headers, form })
}

fn parse_form(s: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in s.split('&') {
        if pair.is_empty() {
            continue;
        }
        match pair.split_once('=') {
            Some((k, v)) => {
                map.insert(urldecode(k), urldecode(v));
            }
            None => {
                map.insert(urldecode(pair), String::new());
            }
        }
    }
    map
}

fn route(
    req: &Req,
    cfg: &ReportCfg,
    logger: &Arc<Logger>,
    shared: &Arc<SharedState>,
    ctrl: &Sender<ControlRequest>,
) -> (u16, String, Vec<u8>) {
    let p = req.path.as_str();
    // —— 页面上报通道（无鉴权：内容仅为状态/诊断，绑定地址由 CPK_BIND 控制）——
    if p == "/report" && req.method == "GET" {
        let status: String = req.query.get("status").cloned().unwrap_or_default().chars().take(40).collect();
        if !status.is_empty() {
            shared.touch_beat();
            if status == "exited" {
                shared.mark_exited();
            }
            if let Some(text) = notify_text(&status) {
                logger.log(1, "sys", &format!("页面状态：{status}（{text}）"));
            }
        }
        return (204, "text/plain".into(), Vec::new());
    }
    if p == "/log" && req.method == "POST" {
        let level = req.form.get("level").cloned().unwrap_or_else(|| "sys".into());
        let msg = req.form.get("msg").cloned().unwrap_or_default();
        if !msg.is_empty() {
            logger.log(1, &level, &msg);
        }
        return (204, "text/plain".into(), Vec::new());
    }
    if p == "/healthz" || p == "/status" {
        let h = shared.snapshot();
        let ok = h.ok;
        let body = health_json(&h).to_string();
        return (if ok { 200 } else { 503 }, "application/json".into(), body.into_bytes());
    }
    // —— 控制通道（token 可选保护）——
    if !token_ok(req, cfg) {
        return (403, "text/plain".into(), b"forbidden: token mismatch".to_vec());
    }
    match p {
        "/" | "/index.html" => (
            200,
            "text/html; charset=utf-8".into(),
            CONTROL_PAGE_HTML.as_bytes().to_vec(),
        ),
        "/shot.jpg" => match screenshot(ctrl) {
            Ok(jpg) => (200, "image/jpeg".into(), jpg),
            Err(e) => (500, "text/plain; charset=utf-8".into(), e.into_bytes()),
        },
        "/tap" => {
            let x = num(&req.query, &req.form, "x");
            let y = num(&req.query, &req.form, "y");
            control_void(ctrl, move |reply| ControlRequest::Tap { x, y, reply })
        }
        "/swipe" => {
            let x1 = num(&req.query, &req.form, "x1");
            let y1 = num(&req.query, &req.form, "y1");
            let x2 = num(&req.query, &req.form, "x2");
            let y2 = num(&req.query, &req.form, "y2");
            control_void(ctrl, move |reply| ControlRequest::Swipe { x1, y1, x2, y2, reply })
        }
        "/type" => {
            let text = req
                .query
                .get("text")
                .or_else(|| req.form.get("text"))
                .cloned()
                .unwrap_or_default();
            control_void(ctrl, move |reply| ControlRequest::TypeText { text, reply })
        }
        "/key" => {
            let key = req
                .query
                .get("key")
                .or_else(|| req.form.get("key"))
                .cloned()
                .unwrap_or_else(|| "Enter".into());
            control_void(ctrl, move |reply| ControlRequest::Key { key, reply })
        }
        "/nav" => {
            let url = req
                .query
                .get("url")
                .or_else(|| req.form.get("url"))
                .cloned()
                .unwrap_or_default();
            control_void(ctrl, move |reply| ControlRequest::Navigate { url, reply })
        }
        "/reload" => control_void(ctrl, |reply| ControlRequest::Reload { reply }),
        _ => (404, "text/plain".into(), b"not found".to_vec()),
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

fn token_ok(req: &Req, cfg: &ReportCfg) -> bool {
    if cfg.control_token.is_empty() {
        return true;
    }
    let q = req.query.get("token").map(|s| s.as_str()).unwrap_or("");
    let h = req.headers.get("x-cpk-token").map(|s| s.as_str()).unwrap_or("");
    q == cfg.control_token || h == cfg.control_token
}

fn num(query: &HashMap<String, String>, form: &HashMap<String, String>, key: &str) -> f64 {
    query
        .get(key)
        .or_else(|| form.get(key))
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0)
}

fn screenshot(ctrl: &Sender<ControlRequest>) -> Result<Vec<u8>, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    ctrl.send(ControlRequest::Screenshot { reply: tx })
        .map_err(|_| "引擎不可用".to_string())?;
    match rx.recv_timeout(Duration::from_secs(20)) {
        Ok(r) => r,
        Err(_) => Err("截图超时（引擎忙或浏览器未就绪）".into()),
    }
}

fn control_void(
    ctrl: &Sender<ControlRequest>,
    build: impl FnOnce(Sender<Result<(), String>>) -> ControlRequest,
) -> (u16, String, Vec<u8>) {
    let (tx, rx) = std::sync::mpsc::channel();
    if ctrl.send(build(tx)).is_err() {
        return (500, "text/plain".into(), b"engine unavailable".to_vec());
    }
    match rx.recv_timeout(Duration::from_secs(20)) {
        Ok(Ok(())) => (200, "text/plain".into(), b"ok".to_vec()),
        Ok(Err(e)) => (500, "text/plain; charset=utf-8".into(), e.into_bytes()),
        Err(_) => (504, "text/plain".into(), b"engine busy / timeout".to_vec()),
    }
}

fn respond(stream: &mut TcpStream, status: u16, ctype: &str, body: &[u8]) {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "OK",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn start_server(token: &str) -> (u16, Arc<SharedState>, Sender<ControlRequest>) {
        let cfg = Config::from_env();
        let shared = SharedState::new(&cfg);
        let (tx, _rx) = std::sync::mpsc::channel();
        let rcfg = ReportCfg { bind: "127.0.0.1".into(), port: 0, control_token: token.into() };
        let port = start(rcfg, Arc::new(Logger::new(cfg.log_dir.clone())), shared.clone(), tx.clone()).unwrap();
        (port, shared, tx)
    }

    fn http(port: u16, req: &str) -> (u16, String) {
        use std::io::{Read, Write};
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        let mut buf = [0u8; 4096];
        loop {
            match s.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => out.push_str(&String::from_utf8_lossy(&buf[..n])),
            }
        }
        let status: u16 = out.split_whitespace().nth(1).and_then(|v| v.parse().ok()).unwrap_or(0);
        let body = out.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
        (status, body)
    }

    #[test]
    fn report_updates_beat_and_status_endpoint() {
        let (port, shared, _tx) = start_server("");
        let (st, _) = http(port, "GET /report?status=alive&slot=1 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 204);
        assert!(shared.snapshot().last_beat_ms > 0);
        let (st, _) = http(port, "GET /report?status=exited HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 204);
        assert!(shared.snapshot().exited);
        // healthz：未运行浏览器 → 503 + JSON
        let (st, body) = http(port, "GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 503);
        assert!(body.contains("\"platform\""));
        let (st, body) = http(port, "GET /status HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 503);
        assert!(body.contains("\"homeUri\""));
    }

    #[test]
    fn token_protection() {
        let (port, _shared, _tx) = start_server("s3cret");
        // 无 token → 403
        let (st, _) = http(port, "GET /shot.jpg HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 403);
        // 带 token → 引擎侧无接收者时 500（证明已过鉴权）
        let (st, body) = http(port, "GET /shot.jpg?token=s3cret HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 500);
        assert!(body.contains("引擎不可用"));
        // 控制页带 token 可访问
        let (st, body) = http(port, "GET /?token=s3cret HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 200);
        assert!(body.contains("CloudPhoneKeep"));
        // /report 与 /healthz 不要求 token（页面脚本无法携带）
        let (st, _) = http(port, "GET /report?status=alive HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 204);
        let (st, _) = http(port, "GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 503);
    }

    #[test]
    fn stream_endpoint_guards() {
        // token 保护与 /shot.jpg 同策略：无 token → 403
        let (port, _shared, _tx) = start_server("s3cret");
        let (st, _) = http(port, "GET /stream.mjpg HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 403);
        // 引擎不可用（控制通道接收端已随测试辅助函数返回被丢弃）→ 500
        let (port2, _shared2, _tx2) = start_server("");
        let (st2, body2) = http(port2, "GET /stream.mjpg HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st2, 500);
        assert!(body2.contains("引擎不可用"), "{body2}");
        // 带 token：同样引擎不可用，但已过鉴权
        let (port3, _shared3, _tx3) = start_server("s3cret");
        let (st3, body3) = http(
            port3,
            "GET /stream.mjpg?token=s3cret HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st3, 500);
        assert!(body3.contains("引擎不可用"), "{body3}");
        // 控制页：新布局（左画面右操作栏 + 实时流）
        let (st4, body4) = http(port3, "GET /?token=s3cret HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st4, 200);
        assert!(body4.contains("stream.mjpg"));
        assert!(body4.contains("CloudPhoneKeep"));
    }

    #[test]
    fn post_log_and_form_parsing() {
        let dir = std::env::temp_dir().join(format!("cpk-srv-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = Config::from_env();
        let shared = SharedState::new(&cfg);
        let (tx, _rx) = std::sync::mpsc::channel();
        let rcfg = ReportCfg { bind: "127.0.0.1".into(), port: 0, control_token: String::new() };
        let logger = Arc::new(Logger::new(dir.clone()));
        let port = start(rcfg, logger, shared, tx).unwrap();
        let body = "level=beat&msg=tick%3D3%20url%3D%2Fhome";
        let req = format!(
            "POST /log HTTP/1.1\r\nHost: x\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let (st, _) = http(port, &req);
        assert_eq!(st, 204);
        // 落盘验证
        let files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect();
        assert_eq!(files.len(), 1);
        let text = std::fs::read_to_string(&files[0]).unwrap();
        assert!(text.contains("[slot=1] [beat] tick=3 url=/home"), "{text}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
