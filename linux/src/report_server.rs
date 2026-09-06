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
//!  - POST /touch             实时触摸流（phase=start/move/end/cancel；多点 ps=x,y,id;…）
//!  - POST /mouse             真实鼠标事件（action=move/down/up/wheel，全键位/滚轮）
//!  - POST /kbd               键盘事件全字段直通（t=down/up，key/code/vk/text/mods）
//!  - POST /type              文本插入（Input.insertText；输入法/粘贴整段发送）
//!  - GET  /clip              读取云机选中文本（云机 → 本机剪贴板）
//!  - POST /fps               运行时帧率上限（引擎侧软件限帧，实测 Chrome 152
//!                            maxFrameRate 参数无效）
//!  - POST /platform          平台选择/切换（mobile/unicom；打开控制页时首选，
//!                            之后可随时切换：换首页/视口/保活脚本，云机实例自动
//!                            重启，Profile 保留双平台登录态）
//!  - POST /tap /swipe /key /nav /reload  控制端点（token 可选保护；兼容保留）
//!
//!  说明：控制端点经 channel 由引擎线程用 CDP Input 域执行 = 内核级触摸模拟，
//!        无需桌面/VNC；/report 与 /log 即使被 Chromium 专用网络访问(PNA)策略
//!        拦截也不影响保活（诊断另有 CDP __CPK_DRAIN__ 通道兜底，双保险）。

use crate::cdp::FramePoll;
use crate::engine::{health_json, ControlRequest, SharedState, TouchPoint};
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
<meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<title>CloudPhoneKeep 控制台</title>
<style>
:root{--bg:#0f172a;--panel:#1e293b;--line:#334155;--txt:#e2e8f0;--dim:#94a3b8}
*{box-sizing:border-box;-webkit-tap-highlight-color:transparent}
html,body{height:100%}
body{margin:0;background:var(--bg);color:var(--txt);font:14px/1.5 system-ui,sans-serif;
display:flex;overflow:hidden}
main{flex:1;display:flex;min-width:0;min-height:0}
#stage{flex:1;display:flex;align-items:center;justify-content:center;padding:12px;min-width:0}
#wrap{position:relative;height:100%;aspect-ratio:414/896;max-height:100%;max-width:100%;background:#020617;
border:1px solid var(--line);border-radius:14px;overflow:hidden;box-shadow:0 6px 28px #0009}
#shot{width:100%;height:100%;display:block;object-fit:contain;cursor:pointer;
touch-action:none;user-select:none;-webkit-user-select:none;-webkit-touch-callout:none}
/* 画面上的提示层：不挡触摸——重连/截图模式下点击照常穿透到画面 */
#overlay{position:absolute;inset:0;display:flex;flex-direction:column;gap:8px;align-items:center;
justify-content:center;background:#020617e6;color:var(--dim);font-size:13px;text-align:center;
padding:0 24px;pointer-events:none;z-index:5}
#overlay.hidden{display:none}
#ovt{color:var(--txt);font-size:15px}
/* 导航失败徽标：不挡触摸，仅提示「白屏=网络/DNS」 */
#pbadge{position:absolute;top:10px;left:10px;background:#dc2626e6;color:#fff;font-size:12px;
font-weight:600;padding:4px 11px;border-radius:999px;display:none;z-index:7;pointer-events:none;
max-width:92%;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
/* 触摸反馈点：按下显示、拖动跟随，操作即时可见 */
#tdot{position:absolute;width:28px;height:28px;border-radius:50%;border:2px solid #ffffffb3;
box-shadow:0 0 14px #000c;background:#ffffff1f;pointer-events:none;display:none;
transform:translate(-50%,-50%);z-index:6}
/* 桌面：右侧操作栏 */
#panel{flex:none;width:300px;background:var(--panel);border-left:1px solid var(--line);
padding:12px 14px;overflow-y:auto;display:flex;flex-direction:column;gap:9px}
#panel h2{font-size:11px;margin:4px 0 0;color:var(--dim);font-weight:600;
letter-spacing:.08em;text-transform:uppercase}
.row{display:flex;gap:6px;flex-wrap:wrap;align-items:center}
input[type=text]{flex:1;min-width:120px;background:var(--bg);color:var(--txt);font-size:16px;
border:1px solid var(--line);border-radius:6px;padding:7px 9px}
button{background:var(--line);color:var(--txt);border:0;border-radius:6px;padding:7px 12px;
cursor:pointer;font-size:13px}
button:hover{background:#475569}
button.acc{background:#0369a1}button.acc:hover{background:#0284c7}
select{background:var(--bg);color:var(--txt);border:1px solid var(--line);border-radius:6px;
padding:6px 8px;font-size:13px}
.lb{font-size:12px;color:var(--dim)}
/* 状态行（fps 收纳于此，不再悬浮画面上遮挡内容） */
#pstat{display:flex;align-items:center;gap:7px;font-size:13px;color:var(--txt);
background:var(--bg);border:1px solid var(--line);border-radius:8px;padding:8px 10px}
#pstat i{width:8px;height:8px;border-radius:50%;background:#64748b;flex:none}
#pstat i.ok{background:#22c55e}#pstat i.bad{background:#ef4444}#pstat i.warn{background:#f59e0b}
#stats{font-size:12px;color:var(--dim);line-height:1.65;word-break:break-all}
#stats b{color:var(--txt);font-weight:600}
#stats .warn{color:#f87171}
#toast{position:fixed;bottom:70px;left:50%;transform:translateX(-50%);background:var(--line);
padding:6px 14px;border-radius:8px;opacity:0;transition:opacity .3s;pointer-events:none;font-size:13px}
/* —— 移动端：iOS 风格圆点（home indicator）呼出底部抽屉 —— */
#homei{display:none;position:fixed;bottom:calc(2px + env(safe-area-inset-bottom));left:50%;
transform:translateX(-50%);z-index:40;width:60px;height:36px;align-items:center;justify-content:center;
background:none;border:0;cursor:pointer;padding:0}
#homei::after{content:'';width:150px;height:5px;border-radius:3px;background:rgba(255,255,255,.45);
transition:background .15s}
#homei:active::after{background:rgba(255,255,255,.9)}
#mask{display:none;position:fixed;inset:0;background:#000a;z-index:20}
#mask.on{display:block}
/* 平台首选弹窗已按需求移除（启动无弹窗）：平台留空待选，控制页「设置→平台」
 * 选择后引擎才加载页面；画面区用 overlay 提示（见 poll 的 idle-plat 分支） */
@media (max-width:820px){
#stage{padding:6px 6px calc(46px + env(safe-area-inset-bottom))}
#wrap{height:auto;width:100%;max-height:100%}
#panel{position:fixed;left:0;right:0;bottom:0;width:auto;max-height:62%;z-index:30;
border-left:0;border-top:1px solid var(--line);border-radius:16px 16px 0 0;
transform:translateY(105%);transition:transform .26s ease;box-shadow:0 -10px 40px #000a;
padding-bottom:calc(14px + env(safe-area-inset-bottom))}
#panel.open{transform:none}
#homei{display:flex}
}
/* 全屏（整个文档）：画面占满、面板/圆点仍可用（均为 fixed/static 正常层叠） */
:fullscreen #stage{padding:0}
:fullscreen #wrap{border-radius:0;border:0;box-shadow:none}
</style></head><body>
<main>
<section id="stage"><div id="wrap">
<img id="shot" alt="云手机实时画面" draggable="false">
<div id="overlay"><div id="ovt">等待画面…</div><div id="ovs"></div></div>
<div id="pbadge"></div>
<div id="tdot"></div>
</div></section>
</main>
<aside id="panel">
<h2>状态</h2>
<div id="pstat"><i id="pdot"></i><span id="pst">连接中…</span></div>
<div id="stats">—</div>
<h2>输入</h2>
<div class="row" id="kbrow" style="display:none">
<input type="text" id="kbin" placeholder="输入/粘贴后自动发送到云机" autocomplete="off"
autocapitalize="off" autocorrect="off" spellcheck="false">
</div>
<div class="row">
<button id="kbt">键盘 关</button>
<button onclick="doCopy()">复制</button>
<button onclick="doPaste()">粘贴</button>
</div>
<h2>操作</h2>
<div class="row">
<button onclick="doNav()">回首页</button>
<button class="acc" onclick="fs()">全屏</button>
</div>
<h2>设置</h2>
<div class="row">
<span class="lb">平台</span>
<select id="psel">
<option value="">选择平台…</option>
<option value="mobile">移动云手机</option>
<option value="unicom">联通云手机</option>
</select>
</div>
<div class="row">
<span class="lb">帧率</span>
<select id="fpsel">
<option value="25">25 流畅</option>
<option value="15">15</option>
<option value="10">10 低配推荐</option>
<option value="5">5 省流</option>
<option value="2">2</option>
<option value="1">1 最省</option>
</select>
</div>
<div class="row" style="color:#8aa;font-size:11px;line-height:1.5">
<span class="lb" style="color:#8aa">提示</span>
<span>帧率越高 CPU 越高；低配盒子建议 8~12。10fps 时引擎端自动降低编码量（Chrome 端每 6 合成器帧取 1），CPU 约降为 1/6。</span>
</div>
</aside>
<div id="mask"></div>
<button id="homei" aria-label="控制台菜单"></button>
<div id="toast"></div>
<script>
var TK=(new URLSearchParams(location.search)).get('token')||'';
var VW=414,VH=896,HOME='';
var live={mode:'none',abort:null,frames:0,last:0,shotTimer:null,shotBusy:false,shotGuard:null};
var lastFrameAt=0,HLTH={ok:false,page:''};
var IMG=document.getElementById('shot');
var WRAP=document.getElementById('wrap');
var TD=document.getElementById('tdot');
var PANEL=document.getElementById('panel'),MASK=document.getElementById('mask');
var KBIN=document.getElementById('kbin');
function esc(s){return String(s==null?'':s).replace(/[&<>"']/g,function(c){
return {'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]})}
function U(p){var u=new URL(p,location.origin);if(TK)u.searchParams.set('token',TK);return u}
function HDR(){return TK?{'x-cpk-token':TK}:{}}
function post(p,body){return fetch(U(p),{method:'POST',
headers:Object.assign({'content-type':'application/x-www-form-urlencoded'},HDR()),body:body})
.then(function(r){
// 非 2xx 转为 reject：否则 .then 成功分支对 500（引擎忙/待机）也报
// 「已设为/已回首页」——谎报成功掩盖真实故障（fps 设置实测踩坑）
if(!r.ok)return r.text().catch(function(){return ''}).then(function(t){
throw new Error((t&&t.slice(0,120))||('HTTP '+r.status))});
return r})}
function ping(t,dur){var el=document.getElementById('toast');el.textContent=t;el.style.opacity=1;
setTimeout(function(){el.style.opacity=0},dur||1500)}
function ov(t,s){var o=document.getElementById('overlay');
document.getElementById('ovt').textContent=t||'';
document.getElementById('ovs').textContent=s||'';
if(t){o.classList.remove('hidden')}else{o.classList.add('hidden')}}

// —— 移动端控制台：点 iOS 风格圆点弹出/收起底部抽屉（桌面端固定右侧栏）——
function sheet(open){
if(open){PANEL.classList.add('open');MASK.classList.add('on')}
else{PANEL.classList.remove('open');MASK.classList.remove('on')}}
document.getElementById('homei').addEventListener('click',function(){
sheet(!PANEL.classList.contains('open'))});
MASK.addEventListener('click',function(){sheet(false)});
// 面板按钮点击后失焦：键盘输入立刻回到画面（Enter/空格不再误触发按钮）
PANEL.addEventListener('click',function(ev){
var b=ev.target.closest?ev.target.closest('button'):null;
if(b)setTimeout(function(){b.blur()},0)});

// —— 状态轮询（3s）：抽屉状态 + 状态色点（状态行内）+ 视口尺寸（触摸坐标映射基准）——
var PAGEMAP={loading:'加载中',ok:'页面正常',reloading:'重载中','nav-error':'导航失败·重试中'};
function pt(s){return PAGEMAP[s]||s||'—'}
function poll(){
if(document.hidden)return;   // 切后台不轮询（省唤醒）
fetch(U('/healthz')).then(function(r){return r.json()}).then(function(j){
VW=j.vw||414;VH=j.vh||896;
WRAP.style.aspectRatio=VW+'/'+VH;
HLTH={ok:!!j.ok,page:j.page||'',exited:!!j.exited,platform:j.platform||'',fps:j.fps||25};
statNotify(j);
document.getElementById('pdot').className=
j.ok?(j.page==='nav-error'?'warn':'ok'):'bad';
if(j.homeUri)HOME=j.homeUri;
var PB=document.getElementById('pbadge');
if(j.page==='nav-error'){PB.style.display='block';
PB.textContent='首页导航失败·引擎自动重试中'}else{PB.style.display='none'}
syncFpsSel(j.fps);
syncPlatSel(j.platform);
// 平台留空（引擎待机）：不连流，画面区提示选择；选择后（healthz 回显平台）
// 自动开流——「选择好了再加载页面」
if(!HLTH.platform){
if(live.mode!=='idle-plat'){stopLive();live.mode='idle-plat';
ov('请选择云手机平台','菜单/侧栏 → 设置 → 平台 → 移动/联通')}
}else if(live.mode==='idle-plat'){ov(null);startLive()}
document.getElementById('stats').innerHTML=
'<b>'+esc(j.account||'')+' · '+esc(j.platformLabel||'未选择')+' · '+pt(j.page)+
(j.exited?' · 已退出云机!':'')+'</b>'+
'<br><b>浏览器</b> '+esc(j.browser)+
(j.title?'<br><b>页面</b> '+esc(String(j.title).slice(0,40)):'')+
'<br><b>ticks</b> '+j.ticks+' · <b>clicks</b> '+j.clicks+' · <b>弹窗</b> '+j.dialogs+
'<br><b>重启</b> '+j.restarts+' · <b>重载</b> '+j.reloads+
' · <b>心跳</b> '+(j.lastBeatAge==null?'—':j.lastBeatAge+'s')+
'<br><b>URL</b> '+esc((j.pageUrl||'').slice(0,80))+
(j.lastError?'<br><span class="warn"><b>错误</b> '+
esc(String(j.lastError).slice(0,100))+'</span>':'')+
(j.exited?'<br><span class="warn">已退出云机！</span>':'');
// 首次 poll 决定是否开流（引擎待机时不自寻 500）
if(live.mode==='none'){
if(HLTH.platform){startLive()}
else{live.mode='idle-plat';ov('请选择云手机平台','菜单/侧栏 → 设置 → 平台 → 移动/联通')}}
}).catch(function(){document.getElementById('pdot').className='bad'});
}
poll();setInterval(poll,3000);

// —— 帧率设置：引擎侧软件限帧即刻生效；状态行同步显示「实测/上限」——
var FPSEL=document.getElementById('fpsel');
function syncFpsSel(fps){
if(!fps||FPSEL._t)return;
var has=Array.prototype.some.call(FPSEL.options,function(o){return o.value===String(fps)});
if(!has){var o=document.createElement('option');o.value=String(fps);o.textContent=fps;
FPSEL.appendChild(o)}
FPSEL.value=String(fps);
}
FPSEL.addEventListener('change',function(){
var v=parseInt(this.value,10)||25;this._t=1;this.blur();
// 回读验证：post 成功 ≠ 生效——healthz.fps 应等于设置值，不一致即
// 引擎/服务侧未落地，如实报错并解锁下拉（syncFpsSel 恢复显示真值），
// 绝不「已设为 N」的谎报（用户实测「设 10 显示上限 25」的教训）
post('/fps','value='+v).then(function(r){
if(!r.ok)throw new Error('HTTP '+r.status);
return fetch(U('/healthz')).then(function(r2){return r2.json()});
}).then(function(j){
if(j&&j.fps===v){ping('帧率上限已设为 '+v+' fps（画面静止时按页面更新推送）')}
else{FPSEL._t=0;ping('帧率设置未生效（引擎侧仍为 '+(j&&j.fps!=null?j.fps:'?')+' fps），请重试',4000)}
}).catch(function(e){FPSEL._t=0;ping('帧率设置失败：'+e.message)});
});

// —— 平台选择（启动无弹窗：平台留空待选，此处选好后引擎加载页面）——
// healthz 同步 + POST /platform；引擎按新平台（首页/视口/保活脚本）启动或
// 重启实例（冷启动约 10 秒），流自动重连
var PLSEL=document.getElementById('psel');
function syncPlatSel(p){
if(!p||PLSEL._t)return;
PLSEL.value=(p==='unicom')?'unicom':(p==='mobile'?'mobile':'');
}
var PLTAP=false;
function platLabel(v){return v==='unicom'?'联通云手机':'移动云手机'}
function wantPlatform(v,silent){
if(PLTAP)return;PLTAP=true;
post('/platform','value='+v).then(function(){PLTAP=false;
if(!silent)ping('已选择 '+platLabel(v)+'，云机启动中（约 10 秒）',5000)})
.catch(function(){PLTAP=false;ping('平台选择失败（引擎忙/重启中）')});
}
PLSEL.addEventListener('change',function(){
var v=this.value;this._t=1;this.blur();
if(!v){ping('请选择移动云手机或联通云手机');syncPlatSel(HLTH.platform);return}
wantPlatform(v,false);
});

// —— 实时画面：fetch MJPEG 流 → JPEG SOI/EOI 切帧 → Blob 直显 ——
// 断流（引擎重建/浏览器重启）自动重连；流建立失败 → 截图轮询兜底，8s 后重试实时流
// 画面新鲜（8s 内有帧）时重连不闪全屏「连接实时画面…」：画面保留，状态行提示等待
function staleShot(){return !lastFrameAt||Date.now()-lastFrameAt>8000}
function stopLive(){
if(live.abort){try{live.abort.abort()}catch(e){}live.abort=null}
if(live.shotTimer){clearTimeout(live.shotTimer);live.shotTimer=null}
if(live.shotGuard){clearInterval(live.shotGuard);live.shotGuard=null}
live.shotBusy=false;
IMG.onload=null;IMG.onerror=null;
}
function setShot(url){
if(IMG._url)URL.revokeObjectURL(IMG._url);
IMG._url=url;IMG.src=url;lastFrameAt=Date.now();
}
function startLive(){
stopLive();live.mode='live';
var ac=new AbortController();live.abort=ac;
if(staleShot())ov('连接实时画面…');
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
if(staleShot())ov('等待首帧…');
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
live.mode='shot';live.shotBusy=false;
if(staleShot())ov('截图模式','实时流暂不可用（引擎忙或重启中），逐帧轮询截图');
// 链式轮询：上一张完成/失败才发下一张——弱机一张截图可要 1-3s，
// 盲目 0.6s 定时发会把引擎控制通道灌爆，反过来拖垮实时流订阅
function loop(){
if(live.mode!=='shot'||live.shotBusy)return;
live.shotBusy=true;
IMG.onload=function(){live.shotBusy=false;
if(live.mode!=='shot')return;
lastFrameAt=Date.now();ov(null);live.shotTimer=setTimeout(loop,600)};
IMG.onerror=function(){live.shotBusy=false;
if(live.mode!=='shot')return;
ov('画面暂不可用','引擎启动/重启中，自动重试…');live.shotTimer=setTimeout(loop,2500)};
IMG.src=U('/shot.jpg?_='+Date.now()).href;
}
loop();
// 网络层悬挂兜底：图片加载无回调 8s → 强制下一轮
live.shotGuard=setInterval(function(){
if(live.mode==='shot'&&live.shotBusy){live.shotBusy=false;loop()}
},8000);
setTimeout(function(){if(live.mode==='shot')startLive()},8000);
}
document.addEventListener('visibilitychange',function(){
// 切后台/锁屏即断流：连接关闭 → 引擎最后一个订阅者离开 → 自动 stopScreencast
// （无人观看＝零 JPEG 编码开销，CPU 即降）；回前台自动重连（画面新鲜不闪提示）
if(document.hidden){stopLive();live.mode='paused'}
else if(live.mode!=='live'&&live.mode!=='idle-plat'&&HLTH.platform){startLive()}
});
// 首连由首次 poll 决定（引擎待机时不自寻 500）；后台打开的标签页回前台再连
// —— 状态行：每秒刷新实测/上限帧率/连接模式/页面状态 ——
// 上限 = 引擎设置值（healthz.fps，/fps 可调）；实测 = 本秒收到的帧数——
// 静止页面 Chrome 按内容更新出帧（可能远低于上限，属正常省流而非设置失效）
setInterval(function(){
var f=0,stale=false;
if(live.mode==='live'){f=live.frames-live.last;live.last=live.frames;
stale=Date.now()-lastFrameAt>3000}
var mode=live.mode==='live'?'实时':live.mode==='shot'?'截图':live.mode==='paused'?'已暂停':live.mode==='idle-plat'?'待选平台':'连接中';
var mtxt=live.mode==='live'?(stale?'等帧…':'实测 '+f):mode;
document.getElementById('pst').textContent=pt(HLTH.page||'')+' · '+mtxt+' · 上限 '+(HLTH.fps||'—')+' fps';
var dot=document.getElementById('pdot');
if(!HLTH.ok&&HLTH.page){dot.className='bad'}
else if(HLTH.page==='nav-error'||stale){dot.className='warn'}
else if(HLTH.page){dot.className='ok'}
},1000);

// —— 触摸坐标映射：帧原始尺寸等比换算（object-fit:contain 居中修正）——
function xy(cx,cy){
var r=IMG.getBoundingClientRect();
var nw=IMG.naturalWidth||VW,nh=IMG.naturalHeight||VH;
var s=Math.min(r.width/nw,r.height/nh),dw=nw*s,dh=nh*s;
var ox=r.left+(r.width-dw)/2,oy=r.top+(r.height-dh)/2;
return [Math.max(0,Math.min(nw,(cx-ox)/dw*nw)),Math.max(0,Math.min(nh,(cy-oy)/dh*nh))];
}
// 触摸反馈点（视口坐标 → wrap 内定位）
function tdotShow(cx,cy){
var w=WRAP.getBoundingClientRect();
TD.style.left=(cx-w.left)+'px';TD.style.top=(cy-w.top)+'px';TD.style.display='block'}

// ══════════════════════════════════════════════════════════════
// 输入通道：所有远端输入事件（触摸/鼠标/键盘/文本）串行走同一 FIFO，
// 保证「按下→抬起」「keyDown→keyUp」配对顺序；引擎响应慢时丢弃的是
// 中途的 move（位置事件），配对事件绝不丢。失败不再静默——节流 toast。
// ══════════════════════════════════════════════════════════════
var INQ=[],inBusy=false,inpErrAt=0;
function iev(b){  // b:{path,body,move?}  move=true 的可丢（拖动/悬停位置流）
if(b.move){
INQ=INQ.filter(function(x){return !x.move});  // 位置事件只留最新一条
INQ.push(b);
}else{INQ.push(b);
if(INQ.length>96){ // 极端积压：仍优先保配对事件
for(var i=0;i<INQ.length;i++){if(INQ[i].move){INQ.splice(i,1);break}}
}
}
if(!inBusy)ipump();
}
function ipump(){
if(!INQ.length){inBusy=false;return}
inBusy=true;
var e=INQ.shift();
post(e.path,e.body).then(function(r){
if(r&&!r.ok&&Date.now()-inpErrAt>5000){inpErrAt=Date.now();
ping('输入通道异常：HTTP '+r.status+'（引擎忙/重启）')}
ipump();
}).catch(function(){
if(Date.now()-inpErrAt>5000){inpErrAt=Date.now();ping('输入发送失败（网络）')}
ipump();
});
}

// —— 实时触摸流（CDP Input.dispatchTouchEvent，多点直通）——
// PTR=当前按下的指针；按下/抬起带变动的触点（Chromium 逐点语义：
// start 新增 / end 列出释放，空=整组）；移动带全部在按触点（双指缩放协同）
var PTR=new Map(),TID=0,GT=0,tLastMv=0;
function fmtPt(p){return p.x.toFixed(1)+','+p.y.toFixed(1)+','+p.tid}
function allPts(){var a=[];PTR.forEach(function(p){a.push(fmtPt(p))});return a.join(';')}
function tSend(phase,pts){iev({path:'/touch',move:phase==='move',
body:'phase='+phase+(pts?'&ps='+encodeURIComponent(pts):'')})}
function touchDown(ev){
var p=xy(ev.clientX,ev.clientY);
if(flushEnd)flushEnd();
TID++;var tid=((TID-1)%10)+1;   // 触点 id 1..10 循环
PTR.set(ev.pointerId,{x:p[0],y:p[1],tid:tid,at:Date.now()});
GT=Math.max(GT,PTR.size);
tSend('start',fmtPt(PTR.get(ev.pointerId)));
tdotShow(ev.clientX,ev.clientY);
}
function touchMove(ev){
var pt=PTR.get(ev.pointerId);if(!pt)return;
var n=Date.now();if(n-tLastMv<33)return;tLastMv=n;  // ≤30 事件/秒：CDP 从容，弱机不积压
var p=xy(ev.clientX,ev.clientY);
pt.x=p[0];pt.y=p[1];
tSend('move',allPts());   // 全部在按触点一起移动（双指缩放协同，不会互相冲掉）
tdotShow(ev.clientX,ev.clientY);
}
var flushEnd=null;  // 延迟中的轻点 end（新按下前冲刷，防 start 被 end 越过）
function touchUp(ev,phase){
var pt=PTR.get(ev.pointerId);if(!pt)return;
PTR.delete(ev.pointerId);
var isEnd=phase==='end';
if(PTR.size===0){
TD.style.display='none';
var gap=Date.now()-pt.at;
// 整组释放：end/cancel 恒空点（CDP 协议规定 touchEnd/touchCancel
// 不得携带触点——带点形态违反协议点列表约束会被 Chrome 拒绝，页面
// 收不到 tap 收尾，远端 H5 表现为「点击没反应」；puppeteer 同款
// 规范形态）
// fin 幂等防重入：同一 fin 只发一次（touchDown 冲刷 + setTimeout 到期
// 双路径都调它；旧身份检查 if(flushEnd!==fin)return 在立即执行路径
// （gap≥60ms 正常点击/拖动收尾，flushEnd===null）恒真 return —— end
// 永远不发出：远端页面收到 touchstart 无 touchend，Chrome 不合成
// click，所有 gap≥60ms 的点击全部无效（本地双 Chrome 端到端复现实证，
// gap<60ms 超快轻点才走延迟路径侥幸生效）。done 标志三种场景全对：
// 立即路径直发、延迟路径到期直发、冲刷后 setTimeout 二次调用防双发）
function fin(){if(fin.done)return;fin.done=true;flushEnd=null;
tSend(isEnd?'end':'cancel','')}
// 轻点补足 ≥60ms 按下时长：贴近真实触摸节奏，保证 tap 手势识别（合成 click）
if(GT===1&&isEnd&&gap<60){flushEnd=fin;setTimeout(fin,60-gap)}else{fin()}
GT=0;
}else{
// 还有手指按着（双指中途抬一指）：协议限制 end 只能整组释放
// （CDP 无法表达「只抬一指」，与 puppeteer 同款限制），释放整组
tSend(isEnd?'end':'cancel','');
}
}

// —— 真实鼠标事件（CDP Input.dispatchMouseEvent）：点击/双击/右键/滚轮 ——
var MBTN={0:'left',1:'middle',2:'right'};
var MSE={down:false,btn:0,mode:'',x0:0,y0:0,lastMv:0,lastWh:0,cnt:0,ct:0,cx:0,cy:0};
function mSend(b){iev({path:'/mouse',move:b.indexOf('action=move')===0||b.indexOf('action=wheel')===0,body:b})}
function clkCnt(p){
var n=Date.now();
if(n-MSE.ct<500&&Math.abs(p[0]-MSE.cx)<8&&Math.abs(p[1]-MSE.cy)<8){MSE.cnt=Math.min(3,MSE.cnt+1)}
else{MSE.cnt=1}
MSE.ct=n;MSE.cx=p[0];MSE.cy=p[1];return MSE.cnt;
}
// 完整点击序列（guaranteed click：页面一定能收到 mousedown/mouseup/click）
function mClickSeq(p,btn,n,m){
var bn=MBTN[btn]||'left',bit=btn===0?1:btn===2?2:btn===1?4:0;
mSend('action=down&x='+p[0].toFixed(1)+'&y='+p[1].toFixed(1)+'&b='+bn+'&n='+n+'&m='+m+'&bb='+bit);
mSend('action=up&x='+p[0].toFixed(1)+'&y='+p[1].toFixed(1)+'&b='+bn+'&n='+n+'&m='+m+'&bb=0');
}
function mouseDown(ev){
var p=xy(ev.clientX,ev.clientY);
MSE.down=true;MSE.btn=ev.button;MSE.mode='pend';
MSE.x0=p[0];MSE.y0=p[1];
// 抬起时判定——不动=真实点击序列，移动超阈值=转触摸拖动（移动页滚动）
}
function mouseMove(ev){
var n=Date.now();if(n-MSE.lastMv<33)return;MSE.lastMv=n;
var p=xy(ev.clientX,ev.clientY);
if(!MSE.down){   // 纯悬停：真实 mouseMoved（桌面页 hover 菜单可用）
mSend('action=move&x='+p[0].toFixed(1)+'&y='+p[1].toFixed(1)+'&b=none&bb=0&m='+mods(ev));
return;
}
if(MSE.mode==='pend'){
if(Math.abs(p[0]-MSE.x0)<8&&Math.abs(p[1]-MSE.y0)<8)return;  // 抖动内仍算点击
MSE.mode='drag';   // 判定拖动 → 从按下点起转触摸流（移动页拖动/滚动跟手）
TID++;
PTR.set(ev.pointerId,{x:MSE.x0,y:MSE.y0,tid:((TID-1)%10)+1,at:Date.now()});
GT=1;
tSend('start',fmtPt(PTR.get(ev.pointerId)));
tdotShow(ev.clientX,ev.clientY);
}
if(MSE.mode==='drag')touchMove(ev);
}
function mouseUp(ev){
var p=xy(ev.clientX,ev.clientY);
if(MSE.mode==='drag'){MSE.down=false;MSE.mode='';touchUp(ev,'end');return}
if(MSE.mode==='pend'){    // 纯点击（左/右/中键）→ 真实鼠标点击序列（远程合成 click/dblclick/contextmenu）
MSE.down=false;MSE.mode='';
mClickSeq(p,MSE.btn,clkCnt(p),mods(ev));
}
}

// —— 滚轮：真实 mouseWheel（移动页/桌面页均可滚动）——
IMG.addEventListener('wheel',function(ev){
ev.preventDefault();
var n=Date.now();if(n-MSE.lastWh<40)return;MSE.lastWh=n;
var p=xy(ev.clientX,ev.clientY);
var k=ev.deltaMode===1?40:ev.deltaMode===2?800:1;
mSend('action=wheel&x='+p[0].toFixed(1)+'&y='+p[1].toFixed(1)+
'&dx='+(ev.deltaX*k).toFixed(1)+'&dy='+(ev.deltaY*k).toFixed(1)+'&m='+mods(ev));
},{passive:false});

// —— 输入路由（与 Windows 版一致，无模式选择）：鼠标指针=抬起时判定
// （不动=真实鼠标点击，移动超阈值=转触摸拖动）；触摸/笔=触摸流 ——
function useMousePath(ev){return ev.pointerType==='mouse'}
IMG.addEventListener('contextmenu',function(ev){ev.preventDefault()});
IMG.addEventListener('pointerdown',function(ev){
ev.preventDefault();
try{this.setPointerCapture(ev.pointerId)}catch(e){}
if(useMousePath(ev)){mouseDown(ev)}else{touchDown(ev)}
});
IMG.addEventListener('pointermove',function(ev){
if(useMousePath(ev)){mouseMove(ev)}else{touchMove(ev)}
});
function ptrUp(ev,phase){
if(useMousePath(ev)){
if(MSE.mode==='drag'){touchUp(ev,phase)}   // 拖动中取消：按触摸流收尾
else{mouseUp(ev)}   // 纯点击 → 完整鼠标序列（此前误置空 mode，点击零事件发出）
}else{touchUp(ev,phase)}
}
IMG.addEventListener('pointerup',function(ev){ptrUp(ev,'end')});
IMG.addEventListener('pointercancel',function(ev){ptrUp(ev,'cancel')});

// ══════════════════════════════════════════════════════════════
// 键盘：物理键盘全键位直通（CDP Input.dispatchKeyEvent）；
// 移动端「键盘」开关弹出输入框（IME 拼音/粘贴 → insertText 整段发送）
// ══════════════════════════════════════════════════════════════
var KBON=false;
function kbOn(on){
KBON=on;
document.getElementById('kbrow').style.display=on?'flex':'none';
var b=document.getElementById('kbt');
b.textContent=on?'键盘 开':'键盘 关';
if(on){b.classList.add('acc')}else{b.classList.remove('acc')}
if(on){KBIN.focus()}else{KBIN.blur()}
}
document.getElementById('kbt').addEventListener('click',function(){kbOn(!KBON)});
function mods(ev){return (ev.altKey?1:0)|(ev.ctrlKey?2:0)|(ev.metaKey?4:0)|(ev.shiftKey?8:0)}
function vkOf(ev){
var k=ev.key,c=ev.code;
if(c){
if(c.indexOf('Key')===0&&c.length===3)return c.charCodeAt(1);
if(c.indexOf('Digit')===0&&c.length===6)return c.charCodeAt(5);
var np={Numpad0:96,Numpad1:97,Numpad2:98,Numpad3:99,Numpad4:100,Numpad5:101,
Numpad6:102,Numpad7:103,Numpad8:104,Numpad9:105,NumpadMultiply:106,NumpadAdd:107,
NumpadSubtract:109,NumpadDecimal:110,NumpadDivide:111};
if(np[c])return np[c];
}
var m={Enter:13,Backspace:8,Tab:9,Escape:27,Space:32,Delete:46,Insert:45,Home:36,End:35,
PageUp:33,PageDown:34,ArrowLeft:37,ArrowUp:38,ArrowRight:39,ArrowDown:40,Shift:16,Control:17,
Alt:18,Meta:91,CapsLock:20,NumLock:144,ScrollLock:145,ContextMenu:93,Pause:19};
if(m[k]!==undefined)return m[k];
if(k&&k.length===1){var cc=k.toUpperCase().charCodeAt(0);return cc<128?cc:229}
if(k&&/^F\d+$/.test(k))return 111+parseInt(k.slice(1),10);
return 0;
}
var KD={};  // 已转发 keydown 的键（keyup 配对转发）
function kbody(t,ev){
var printable=t==='down'&&ev.key&&ev.key.length===1&&ev.key>=' '&&ev.key!=='\x7f'&&!ev.ctrlKey&&!ev.metaKey;
var b='t='+t+'&key='+encodeURIComponent(ev.key||'')+
'&code='+encodeURIComponent(ev.code||'')+'&vk='+vkOf(ev)+'&m='+mods(ev)+
'&l='+(ev.location||0)+'&r='+(ev.repeat?1:0);
if(printable)b+='&text='+encodeURIComponent(ev.key);
return {path:'/kbd',body:b};
}
function kdown(ev){KD[ev.key]=1;iev(kbody('down',ev))}
function kup(ev){if(KD[ev.key]){delete KD[ev.key];iev(kbody('up',ev))}}
document.addEventListener('keydown',function(ev){
var ae=document.activeElement,k=ev.key;
if(ae&&ae.tagName==='SELECT')return;              // 用户正在操作下拉框
if(ev.isComposing||k==='Process')return;          // IME 合成中间态
var inKB=ae===KBIN;
if(inKB){
if(k&&k.length===1&&!ev.ctrlKey&&!ev.metaKey)return;   // 可打印字符留在输入框 → insertText
if(k==='Backspace'){if(KBIN.value)return;ev.preventDefault();kdown(ev);return}
if(k==='Enter'){ev.preventDefault();kdown(ev);return}
// 方向键/修饰键/Ctrl 组合等继续直通远端
}
if(ev.ctrlKey||ev.metaKey){
var lk=(k||'').toLowerCase();
if(lk==='v'&&!ev.shiftKey&&!ev.altKey){
// 粘贴：本机剪贴板 → 云机（insertText）。键本身不转发（远端剪贴板为空）。
if(navigator.clipboard&&navigator.clipboard.readText){ev.preventDefault();doPaste()}
else{KBIN.focus()}   // 非安全上下文：聚焦输入框让原生粘贴落入 → input 事件转发
return;
}
if((lk==='c'||lk==='x')&&!ev.shiftKey&&!ev.altKey){
ev.preventDefault();kdown(ev);doCopy();return    // 转发按键（页面自身复制逻辑）+ 同步到本机
}
if(lk==='a'&&!ev.shiftKey&&!ev.altKey){ev.preventDefault();kdown(ev);return}
if(lk==='r'){ev.preventDefault();kdown(ev);doReload();return}
}
if(k==='F5'){ev.preventDefault();kdown(ev);return}   // F5 → 云机刷新而非本页刷新
if(k===' '||k==='Enter'||k==='Tab'||k==='Backspace'||k.indexOf('Arrow')===0){
ev.preventDefault()}   // 不触发本页滚动/焦点移动/按钮激活
kdown(ev);
});
document.addEventListener('keyup',function(ev){
if(document.activeElement&&document.activeElement.tagName==='SELECT')return;
kup(ev);
});

// —— 键盘输入框（移动端 IME / 粘贴落点）：内容即发即清 ——
KBIN.addEventListener('input',function(ev){
if(ev.isComposing||ev.inputType==='insertFromPaste')return;
sendKbText();
});
KBIN.addEventListener('compositionend',function(){setTimeout(sendKbText,30)});
function sendKbText(){
var v=KBIN.value;
if(v){iev({path:'/type',body:'text='+encodeURIComponent(v)});KBIN.value=''}
}
// 原生粘贴落入输入框：paste 事件统一接管（阻止本地插入，直接转发远端）
document.addEventListener('paste',function(ev){
ev.preventDefault();
var t=ev.clipboardData?ev.clipboardData.getData('text/plain'):'';
if(t){iev({path:'/type',body:'text='+encodeURIComponent(t)});ping('已粘贴 '+t.length+' 字')}
});

// —— 剪贴板：云机选区 → 本机（复制）；本机剪贴板 → 云机（粘贴）——
function legacyCopy(t){
return new Promise(function(res,rej){
var ta=document.createElement('textarea');ta.value=t;
ta.style.cssText='position:fixed;left:-9999px;top:0;opacity:0';
document.body.appendChild(ta);ta.focus();ta.select();
var ok=false;try{ok=document.execCommand('copy')}catch(e){}
setTimeout(function(){document.body.removeChild(ta);
ok?res():rej(new Error('copy denied'))},0);
});
}
function copyLocal(t){
if(navigator.clipboard&&navigator.clipboard.writeText){
return navigator.clipboard.writeText(t).catch(function(){return legacyCopy(t)});
}
return legacyCopy(t);
}
function doCopy(){
fetch(U('/clip')).then(function(r){return r.ok?r.text():Promise.reject('HTTP '+r.status)})
.then(function(t){
if(!t){ping('云机无选中文本');return}
copyLocal(t).then(function(){ping('已复制 '+t.length+' 字到本机')},
function(){ping('本机复制被浏览器拒绝')});
}).catch(function(){ping('读取云机选区失败')});
}
function doPaste(){
if(navigator.clipboard&&navigator.clipboard.readText){
navigator.clipboard.readText().then(function(t){
if(!t){ping('本机剪贴板为空');return}
iev({path:'/type',body:'text='+encodeURIComponent(t)});
ping('已粘贴 '+t.length+' 字');
}).catch(function(){kbOn(true);
ping('无剪贴板权限：已弹出输入框，Ctrl+V 或长按粘贴')});
}else{kbOn(true);ping('已弹出输入框：Ctrl+V 或长按粘贴')}
}

// —— 面板操作 ——
function doReload(){post('/reload','').then(function(){ping('已刷新页面')})
.catch(function(e){ping('刷新失败：'+e.message)})}
function doNav(){if(!HOME){ping('未知首页地址');return}
post('/nav','url='+encodeURIComponent(HOME)).then(function(){ping('已回首页')})
.catch(function(e){ping('回首页失败：'+e.message)})}
function fs(){var el=document.documentElement;
if(document.fullscreenElement){document.exitFullscreen()}
else if(el.requestFullscreen){el.requestFullscreen()}}

// —— 状态转换通知（对齐 Windows 版系统通知：退出云机/到期）——
// 双通道：① /report 状态迁移（与 Windows 版 on_report 同语义，页面重载后
// 可再次触发）；② /report 被网络策略拦截时，引擎采样标志的上升沿兜底
// （标志单调为真，仅首次触发）。两通道同轮去重。通知权限需用户手势：
// 首次触摸页面时 best-effort 申请，被拒/不支持降级为 toast
var LASTST='',PREVEXIT=false;
document.addEventListener('pointerdown',function askNotif(){
if(!('Notification'in window)||Notification.permission!=='default')return;
try{var p=Notification.requestPermission();if(p&&p.then)p.then(function(){},function(){})}catch(e){}
},{once:true});
function sysNotify(title,body){
ping(title+'：'+body,5000);
if('Notification'in window&&Notification.permission==='granted'){
try{new Notification(title,{body:body,tag:'cpk-status'})}catch(e){}}
}
function statNotify(j){
var st=j.lastStatus||'',noted=false;
if(LASTST&&st&&st!==LASTST){
if(st==='exited'){sysNotify('已退出云手机','帐号已退回云手机首页，请检查会话');noted=true}
else if(st==='expired'){sysNotify('时间已到期','云手机使用时间已到期，到期弹窗已自动确认')}
}
if(st)LASTST=st;
// 兜底通道：exited 标志上升沿（/report 不通时引擎采样仍能触发一次）
if(j.exited&&!PREVEXIT&&!noted){sysNotify('已退出云手机','帐号已退回云手机首页，请检查会话')}
PREVEXIT=!!j.exited;
}
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

/// 实时画面流：向引擎订阅 screencast 帧信箱（只存最新帧），以 multipart/x-mixed-replace
/// 推送（MJPEG）。退出条件：客户端断开（写失败）/ 生产侧心跳丢失（CDP 重建、
/// 浏览器重启：连续两个窗口无引擎泵心跳）/ 首帧 12s 未至（引擎极端繁忙）。
/// 关流后页面侧自动重连。静态页面合成器无更新 → screencast 不发新帧：以 2s
/// 心跳重发上一帧维持连接。
fn stream_mjpeg(stream: &mut TcpStream, ctrl: &Sender<ControlRequest>, logger: &Arc<Logger>) {
    const BOUNDARY: &str = "cpkframe";
    // 1) 订阅引擎实时画面：引擎线程可能正在慢 eval（弱机 tick/采样可达数秒）/
    //    启动浏览器，宽限 8s 再判超时；引擎彻底不可用会立刻 500
    let (tx, rx) = std::sync::mpsc::channel();
    if ctrl.send(ControlRequest::ScreencastAttach { reply: tx }).is_err() {
        respond(stream, 500, "text/plain; charset=utf-8", "引擎不可用".as_bytes());
        return;
    }
    // 平台冷启动全程 ~13s（Chromium 启动 + CDP 装配 + 导航），等待窗放宽到
    // 20s：8s 会在引擎刚到稳态前掐掉订阅，多走一轮「截图模式 → 8s 后重连」
    let (sub_id, frame_box) = match rx.recv_timeout(Duration::from_secs(20)) {
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
    // 2) 流头 + 帧循环（multipart；每次写失败即客户端已断开）。
    //    帧信箱只存最新帧：引擎覆盖写入（丢旧保新），此处 poll 等待/取帧
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary={BOUNDARY}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(head.as_bytes()).is_err() {
        screencast_detach(ctrl, sub_id);
        return;
    }
    let mut last: Option<Vec<u8>> = None;
    let mut last_push = Instant::now();
    let opened = Instant::now();
    loop {
        match frame_box.poll(Duration::from_secs(2)) {
            FramePoll::Frame(frame) => {
                if !write_part(stream, BOUNDARY, &frame) {
                    break;
                }
                last = Some(frame);
                last_push = Instant::now();
            }
            FramePoll::Idle => {
                // 静态页：生产侧活着但无新帧 → 心跳重发上一帧维持连接
                // （路由器/代理不掐空闲连接，页面也不闪重连循环）
                if let Some(f) = &last {
                    if last_push.elapsed() >= Duration::from_secs(2) {
                        if !write_part(stream, BOUNDARY, f) {
                            break;
                        }
                        last_push = Instant::now();
                    }
                } else if opened.elapsed() > Duration::from_secs(12) {
                    // 首帧 12s 未至（引擎极端繁忙/浏览器启动中）→ 关流，页面转截图兜底
                    // （30s→12s：死流期间引擎侧 cast_rescue 已在 6s 级重发拉活，
                    // 12s 仍无帧说明流路彻底不可用——早转截图让用户见到画面，
                    // 不再干等需手动刷新）
                    logger.log(1, "sys", "实时画面流首帧 12s 未至，关流（页面自动转截图轮询并重连）");
                    break;
                }
            }
            // 生产侧心跳丢失：CDP 会话重建/浏览器重启，旧信箱不会再有帧
            // → 立即关流，页面 1.5s 重连新订阅（替代旧 mpsc 的 Disconnected 信号）
            FramePoll::Dead => {
                logger.log(1, "sys", "实时画面流生产侧心跳丢失，关流重连（CDP 会话重建/浏览器重启）");
                break;
            }
        }
    }
    screencast_detach(ctrl, sub_id);
}

/// 写一帧 multipart part；任何写失败 = 客户端已断开。
fn write_part(stream: &mut TcpStream, boundary: &str, frame: &[u8]) -> bool {
    let part = format!(
        "--{boundary}\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
        frame.len()
    );
    stream.write_all(part.as_bytes()).is_ok()
        && stream.write_all(frame).is_ok()
        && stream.write_all(b"\r\n").is_ok()
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
            // 状态记录（对齐 Windows 版 on_report 状态迁移语义）：控制页
            // 据此检测 exited/expired 转换并发通知（Windows 版发系统通知）
            shared.set_status(&status);
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
        "/touch" => {
            // 实时触摸流：phase/ps 在 HTTP 层校验（非法 400，不占引擎）；坐标与 /tap 同坐标系。
            // 单点兼容：phase + x/y；多点：ps="x1,y1,id1;x2,y2,id2"（Chromium 逐点语义）
            let phase = req
                .query
                .get("phase")
                .or_else(|| req.form.get("phase"))
                .cloned()
                .unwrap_or_default();
            if !matches!(phase.as_str(), "start" | "move" | "end" | "cancel") {
                return (
                    400,
                    "text/plain; charset=utf-8".into(),
                    b"phase must be start/move/end/cancel".to_vec(),
                );
            }
            let ps = req
                .query
                .get("ps")
                .or_else(|| req.form.get("ps"))
                .cloned()
                .unwrap_or_default();
            let points: Vec<TouchPoint> = if matches!(phase.as_str(), "end" | "cancel") {
                // 协议规定 touchEnd/touchCancel 不得携带触点（整组释放）：
                // 忽略 ps——带点释放形态违反协议点列表约束会被 Chrome 拒绝，
                // 页面收不到 touchend 的 tap 收尾（轻点「没反应」的直接根源）
                Vec::new()
            } else if !ps.is_empty() {
                match parse_touch_points(&ps) {
                    Some(v) => v,
                    None => {
                        return (
                            400,
                            "text/plain; charset=utf-8".into(),
                            b"ps must be x,y,id triples joined by ; (id 1..=10)".to_vec(),
                        )
                    }
                }
            } else {
                // start/move 单点兼容：phase + x/y
                let x = num(&req.query, &req.form, "x");
                let y = num(&req.query, &req.form, "y");
                vec![TouchPoint { x, y, id: 1 }]
            };
            control_void(ctrl, move |reply| ControlRequest::Touch { phase, points, reply })
        }
        "/mouse" => {
            // 真实鼠标事件：action/action 按钮在 HTTP 层校验；坐标与 /touch 同坐标系。
            // move=悬停/拖动，down/up=按下/抬起（clickCount 支撑双击/三击），
            // wheel=滚轮（dx/dy 像素）。修饰键位图：Alt=1 Ctrl=2 Meta=4 Shift=8
            let action = req
                .query
                .get("action")
                .or_else(|| req.form.get("action"))
                .cloned()
                .unwrap_or_default();
            if !matches!(action.as_str(), "move" | "down" | "up" | "wheel") {
                return (
                    400,
                    "text/plain; charset=utf-8".into(),
                    b"action must be move/down/up/wheel".to_vec(),
                );
            }
            let button = req
                .query
                .get("b")
                .or_else(|| req.form.get("b"))
                .cloned()
                .unwrap_or_else(|| "left".into());
            if !matches!(button.as_str(), "none" | "left" | "right" | "middle") {
                return (
                    400,
                    "text/plain; charset=utf-8".into(),
                    b"b must be none/left/right/middle".to_vec(),
                );
            }
            let x = num(&req.query, &req.form, "x");
            let y = num(&req.query, &req.form, "y");
            let dx = num(&req.query, &req.form, "dx");
            let dy = num(&req.query, &req.form, "dy");
            let buttons = unum(&req.query, &req.form, "bb") as u32;
            let click_count = (unum(&req.query, &req.form, "n") as u32).clamp(1, 3);
            let modifiers = (unum(&req.query, &req.form, "m") as u32).min(31);
            control_void(ctrl, move |reply| ControlRequest::Mouse {
                action, x, y, button, buttons, click_count, dx, dy, modifiers, reply,
            })
        }
        "/kbd" => {
            // 键盘事件全字段直通：t=down/up；key/code 限长防滥用；vk 0..=255；
            // text ≤16 字符（长文本/粘贴走 /type insertText）
            let typ = req
                .query
                .get("t")
                .or_else(|| req.form.get("t"))
                .cloned()
                .unwrap_or_default();
            if !matches!(typ.as_str(), "down" | "up") {
                return (
                    400,
                    "text/plain; charset=utf-8".into(),
                    b"t must be down/up".to_vec(),
                );
            }
            let key = take(&req, "key", 32);
            let code = take(&req, "code", 32);
            let vk = (unum(&req.query, &req.form, "vk") as u32).min(65535);
            let text = take(&req, "text", 16);
            let modifiers = (unum(&req.query, &req.form, "m") as u32).min(31);
            let location = (unum(&req.query, &req.form, "l") as u32).min(3);
            let auto_repeat = matches!(
                req.query.get("r").or_else(|| req.form.get("r")).map(|s| s.as_str()),
                Some("1" | "true")
            );
            control_void(ctrl, move |reply| ControlRequest::KeyEvent {
                typ, key, code, vk, text, modifiers, location, auto_repeat, reply,
            })
        }
        "/clip" => {
            // 云机选区 → 控制页（控制页写入本机剪贴板）：复制按钮/Ctrl+C 数据源
            let (tx, rx) = std::sync::mpsc::channel();
            if ctrl.send(ControlRequest::ClipGet { reply: tx }).is_err() {
                return (500, "text/plain".into(), b"engine unavailable".to_vec());
            }
            match rx.recv_timeout(Duration::from_secs(20)) {
                Ok(Ok(t)) => (200, "text/plain; charset=utf-8".into(), t.into_bytes()),
                Ok(Err(e)) => (500, "text/plain; charset=utf-8".into(), e.into_bytes()),
                Err(_) => (504, "text/plain".into(), b"engine busy / timeout".to_vec()),
            }
        }
        "/platform" => {
            // 平台运行时切换：mobile/unicom（HTTP 层校验）。引擎更新共享状态后
            // 重启云机实例（新视口/新保活脚本 CFG/新首页导航）
            let p = req
                .query
                .get("value")
                .or_else(|| req.form.get("value"))
                .cloned()
                .unwrap_or_default();
            if !matches!(p.as_str(), "mobile" | "unicom") {
                return (
                    400,
                    "text/plain; charset=utf-8".into(),
                    b"value must be mobile/unicom".to_vec(),
                );
            }
            control_void(ctrl, move |reply| ControlRequest::SetPlatform { platform: p, reply })
        }
        "/fps" => {
            // 帧率上限：1..=60；引擎侧软件限帧（Chrome 152 的
            // startScreencast maxFrameRate 参数实测无效，见 cdp.rs push_frame）。
            // 共享状态由 HTTP 层直写：引擎待机/重启窗口期设置同样立即生效
            // （下次 CDP 装配恢复 + 稳态循环每周期同步），不再依赖引擎控制
            // 通道存活——此前待机期设帧率被引擎快速失败，控制页却报
            // 「已设为 N」（post 不检查 r.ok 的谎报），healthz 仍回旧值
            // （用户实测「设置 10 显示上限 25」的根因）。
            // 引擎在线时的即刻生效通知仍尽力转发（失败由稳态同步兜底）。
            let fps = unum(&req.query, &req.form, "value");
            if !(1..=60).contains(&fps) {
                return (
                    400,
                    "text/plain; charset=utf-8".into(),
                    b"value must be 1..=60".to_vec(),
                );
            }
            let fps = fps as u32;
            shared.set_fps(fps);
            let (tx, _rx) = std::sync::mpsc::channel();
            let _ = ctrl.send(ControlRequest::SetFps { fps, reply: tx });
            (200, "text/plain".into(), b"ok".to_vec())
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

/// 无符号整数参数（非法/缺失 → 0）
fn unum(query: &HashMap<String, String>, form: &HashMap<String, String>, key: &str) -> u64 {
    query
        .get(key)
        .or_else(|| form.get(key))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
}

/// 字符串参数截断（限长防滥用）
fn take(req: &Req, key: &str, max_chars: usize) -> String {
    req.query
        .get(key)
        .or_else(|| req.form.get(key))
        .cloned()
        .unwrap_or_default()
        .chars()
        .take(max_chars)
        .collect()
}

/// 多点触控参数解析："x1,y1,id1;x2,y2,id2"（id 1..=10，最多 10 点）
fn parse_touch_points(s: &str) -> Option<Vec<TouchPoint>> {
    let mut v = Vec::new();
    for part in s.split(';') {
        if part.is_empty() {
            continue;
        }
        let f: Vec<&str> = part.split(',').collect();
        if f.len() != 3 {
            return None;
        }
        let x: f64 = f[0].trim().parse().ok()?;
        let y: f64 = f[1].trim().parse().ok()?;
        let id: i64 = f[2].trim().parse().ok()?;
        if !(1..=10).contains(&id) {
            return None;
        }
        v.push(TouchPoint { x, y, id });
        if v.len() > 10 {
            return None;
        }
    }
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
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
        // healthz：未运行浏览器 → 503 + JSON；状态记录（对齐 Windows 版 on_report
        // 状态迁移语义）：/report 后 lastStatus 如实回显，title 字段在位
        let (st, body) = http(port, "GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 503);
        assert!(body.contains("\"platform\""));
        assert!(body.contains("\"lastStatus\":\"exited\""), "exited 未记录：{body}");
        assert!(body.contains("\"title\""), "healthz 缺 title 字段：{body}");
        let (st, body) = http(port, "GET /status HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 503);
        assert!(body.contains("\"homeUri\""));
    }

    #[test]
    fn report_status_field_updates() {
        // lastStatus 随 /report 逐次覆盖：alive → expired → healthz 如实回显
        let (port, _shared, _tx) = start_server("");
        http(port, "GET /report?status=alive HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        let (st, body) = http(port, "GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 503);
        assert!(body.contains("\"lastStatus\":\"alive\""), "{body}");
        http(port, "GET /report?status=expired HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        let (_, body) = http(port, "GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert!(body.contains("\"lastStatus\":\"expired\""), "{body}");
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
    fn stream_mjpeg_frames_keepalive_and_close() {
        // 自建控制通道（start_server 辅助会丢弃接收端）：模拟引擎应答
        // ScreencastAttach 并交出帧信箱（FrameBox）；随后按引擎节奏推帧/泵心跳，
        // 收到 Detach 后收尾退出
        let cfg = Config::from_env();
        let shared = SharedState::new(&cfg);
        let (tx, engine_rx) = std::sync::mpsc::channel();
        let rcfg = ReportCfg { bind: "127.0.0.1".into(), port: 0, control_token: String::new() };
        let port = start(
            rcfg,
            Arc::new(Logger::new(cfg.log_dir.clone())),
            shared,
            tx.clone(),
        )
        .unwrap();
        // 模拟引擎的帧信箱：attach 时交给流线程；推帧=post，泵周期=touch_alive
        let box_ = crate::cdp::FrameSlot::new();
        let box_tx = box_.clone();
        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let running2 = running.clone();
        thread::spawn(move || {
            let mut pending = Some(box_tx);
            while let Ok(req) = engine_rx.recv() {
                match req {
                    ControlRequest::ScreencastAttach { reply } => {
                        if let Some(b) = pending.take() {
                            let _ = reply.send(Ok((1u32, b)));
                        }
                    }
                    ControlRequest::ScreencastDetach { reply, .. } => {
                        let _ = reply.send(Ok(()));
                        break;
                    }
                    _ => {}
                }
            }
            running2.store(false, std::sync::atomic::Ordering::Release);
        });
        // 模拟引擎泵心跳（每 300ms touch，直到 Detach 收尾）
        let heartbeat = box_.clone();
        thread::spawn(move || {
            while running.load(std::sync::atomic::Ordering::Acquire) {
                heartbeat.touch_alive();
                thread::sleep(Duration::from_millis(300));
            }
        });
        // 引擎推一帧（5 字节假帧：SOI+EOI+尾部，客户端只看 multipart 语义）
        box_.post(vec![0xFF, 0xD8, 0xFF, 0xD9, 0x01]);

        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        s.write_all(b"GET /stream.mjpg HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        let t0 = Instant::now();
        // 读到首个 JPEG 帧为止（订阅应答即时；防抖上限 10s）
        while !out.windows(2).any(|w| w == [0xFF, 0xD8]) && t0.elapsed() < Duration::from_secs(10) {
            match s.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(_) => {}
            }
        }
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
        assert!(text.contains("multipart/x-mixed-replace"), "{text}");
        assert!(text.contains("Content-Length: 5"), "{text}");
        // 静态页心跳：无新帧 2s 后重发上一帧 → 字节继续增长（连接保持活性，
        // 生产侧 touch_alive 心跳在 → 不触发 Dead 关流）
        let before = out.len();
        let t1 = Instant::now();
        while out.len() == before && t1.elapsed() < Duration::from_secs(6) {
            match s.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(_) => {}
            }
        }
        assert!(out.len() > before, "2s 心跳未重发上一帧");
        // 客户端断开 → 服务端写失败退出并发 Detach（引擎线程收尾，测试可退出）
        drop(s);
        thread::sleep(Duration::from_millis(300));
    }

    #[test]
    fn touch_endpoint_guards() {
        // phase 非法 → HTTP 层直接 400（不占引擎，无需浏览器）
        let (port, _shared, _tx) = start_server("");
        let (st, body) = http(
            port,
            "GET /touch?phase=poke&x=1&y=2 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 400);
        assert!(body.contains("phase"), "{body}");
        // phase 合法但引擎不可用（控制通道无接收者）→ 500（已过参数校验，进入控制通道）
        let (st2, body2) = http(
            port,
            "GET /touch?phase=move&x=1.5&y=2.5 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st2, 500);
        assert!(body2.contains("engine unavailable"), "{body2}");
        // token 保护与其它控制端点同策略
        let (port2, _shared2, _tx2) = start_server("s3cret");
        let (st3, _) = http(
            port2,
            "GET /touch?phase=start&x=1&y=2 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st3, 403);
        // 控制页已接线实时触摸流（按下/移动/抬起）+ 移动端圆点抽屉 + 触摸反馈
        let (st4, body4) = http(port2, "GET /?token=s3cret HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st4, 200);
        assert!(body4.contains("/touch"), "控制页缺 /touch 接线");
        assert!(body4.contains("pointermove"), "控制页缺实时拖动接线");
        assert!(body4.contains("visibilitychange"), "控制页缺后台暂停接线");
        assert!(body4.contains("homei"), "控制页缺移动端圆点菜单");
        assert!(body4.contains("id=\"pstat\""), "控制页缺状态面板（fps 收纳处）");
        assert!(body4.contains("id=\"fpsel\""), "控制页缺帧率设置");
        // fps 状态行实测+上限双指标（静止页实测远低于上限不再误读为设置失效）
        assert!(body4.contains("实测 "), "状态行缺实测帧率");
        assert!(body4.contains("上限 "), "状态行缺帧率上限");
        // fps 回读验证（绝不谎报：设 N 后 healthz.fps≠N 如实报错并解锁下拉）
        assert!(body4.contains("帧率设置未生效"), "fps 设置缺回读验证接线");
        // 触摸整组释放恒空点（CDP 协议规定 touchEnd/touchCancel 不得携带触点，
        // 带点形态会被 Chrome 拒绝——页面收不到 tap 收尾，「点击没反应」
        // 的直接根源）——控制页与引擎双保险
        assert!(body4.contains("tSend(isEnd?'end':'cancel','')"), "整组释放应恒空点");
        assert!(body4.contains("id=\"kbin\""), "控制页缺键盘输入框");
        assert!(!body4.contains("id=\"imode\""), "触控模式选择器应已移除（与 Windows 版一致）");
        assert!(!body4.contains("cpk_imode"), "触控模式 localStorage 残留应已移除");
        assert!(!body4.contains("id=\"fpsb\""), "fps 悬浮徽标应已移入状态面板");
        assert!(body4.contains("pointer-events:none"), "提示层不应挡触摸");
        assert!(!body4.contains("上滑"), "方向滑动按钮应已删除");
        assert!(!body4.contains("sendKey"), "旧按键按钮应已删除");
        assert!(!body4.contains("<header"), "顶栏应已删除");
    }

    #[test]
    fn status_notify_wiring_and_no_addr_bar() {
        // 地址栏已按需求移除（Linux 版不需要）：端点下线 → 404
        let (port, _shared, _tx) = start_server("");
        let (st, _) = http(port, "POST /addr HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        assert_eq!(st, 404);
        // 控制页不再有地址栏接线（防回归）；状态转换通知/页面标题保留
        let (port2, _shared2, _tx2) = start_server("s3cret");
        let (st3, body3) = http(port2, "GET /?token=s3cret HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st3, 200);
        assert!(!body3.contains("doAddr()"), "地址栏接线应已移除");
        assert!(!body3.contains(">地址</button>"), "地址按钮应已移除");
        assert!(!body3.contains("lk==='u'"), "Ctrl+U 拦截应已移除");
        assert!(body3.contains("statNotify"), "控制页缺状态转换通知");
        assert!(body3.contains("Notification.permission"), "控制页缺系统通知权限申请");
        assert!(body3.contains("lastStatus"), "控制页未消费 lastStatus");
        assert!(body3.contains("j.title"), "控制页未显示页面标题");
    }

    #[test]
    fn platform_endpoint_guards_and_page_wiring() {
        // /platform：非法值 → 400（HTTP 层校验）；合法值但引擎不可用 → 500
        let (port, _shared, _tx) = start_server("");
        let (st, body) = http(
            port,
            "POST /platform HTTP/1.1\r\nHost: x\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 13\r\nConnection: close\r\n\r\nvalue=telecom",
        );
        assert_eq!(st, 400);
        assert!(body.contains("mobile/unicom"), "{body}");
        let (st, _) = http(
            port,
            "POST /platform HTTP/1.1\r\nHost: x\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 12\r\nConnection: close\r\n\r\nvalue=unicom",
        );
        assert_eq!(st, 500);
        // token 保护与其它控制端点同策略
        let (port2, _shared2, _tx2) = start_server("s3cret");
        let (st2, _) = http(
            port2,
            "POST /platform HTTP/1.1\r\nHost: x\r\nContent-Length: 12\r\nConnection: close\r\n\r\nvalue=unicom",
        );
        assert_eq!(st2, 403);
        // 控制页接线：平台选择器（留空占位项）+ healthz 同步 + POST
        let (st3, body3) = http(port2, "GET /?token=s3cret HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        assert_eq!(st3, 200);
        assert!(body3.contains("id=\"psel\""), "控制页缺平台选择器");
        assert!(body3.contains(">联通云手机</option>"), "控制页缺联通选项");
        assert!(body3.contains("选择平台…</option>"), "控制页缺留空占位项");
        assert!(body3.contains("syncPlatSel"), "控制页缺平台同步逻辑");
        assert!(body3.contains("/platform"), "控制页缺平台切换接线");
        // 启动无弹窗（平台留空待选）：无首选层/无 localStorage 记忆/无自动切回
        assert!(!body3.contains("id=\"pick\""), "平台首选弹窗应已移除（启动无弹窗）");
        assert!(!body3.contains("cpk_platform"), "平台 localStorage 记忆应已移除");
        // 待选平台模式：引擎待机时画面区提示选择，选好后自动开流
        assert!(body3.contains("idle-plat"), "控制页缺待选平台分支");
        assert!(body3.contains("请选择云手机平台"), "控制页缺待选提示");
    }

    #[test]
    fn mouse_kbd_fps_clip_endpoint_guards() {
        // /mouse：action/按钮非法 → 400；合法但引擎不可用 → 500（已过参数校验）
        let (port, _shared, _tx) = start_server("");
        let (st, body) = http(
            port,
            "GET /mouse?action=poke&x=1&y=2 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 400);
        assert!(body.contains("action"), "{body}");
        let (st, _) = http(
            port,
            "GET /mouse?action=down&x=1&y=2&b=left&n=1&m=0&bb=1 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 500);
        let (st, _) = http(
            port,
            "GET /mouse?action=wheel&x=1&y=2&dx=0&dy=120 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 500);
        // /kbd：t 非法 → 400；合法 → 500（引擎不可用）
        let (st, body) = http(
            port,
            "GET /kbd?t=press&key=a HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 400);
        assert!(body.contains("down/up"), "{body}");
        let (st, _) = http(
            port,
            "GET /kbd?t=down&key=a&code=KeyA&vk=65&text=a&m=0 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 500);
        // /fps：越界 → 400；合法 → 200 且直写共享状态（引擎不在线也生效——
        // 待机/重启窗口期设帧率不再丢失，healthz 立即回显新值）
        let (port, shared, _tx) = start_server("");
        let (st, _) = http(
            port,
            "GET /fps?value=99 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 400);
        let (st, _) = http(
            port,
            "GET /fps?value=10 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 200, "fps 应由 HTTP 层直写共享状态，引擎不在线也成功");
        assert_eq!(shared.snapshot().fps, 10, "帧率应写入共享状态并回显 healthz");
        // /clip：引擎不可用 → 500
        let (st, body) = http(
            port,
            "GET /clip HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 500);
        assert!(body.contains("engine unavailable"), "{body}");
        // /touch 多点：ps 非法 → 400；合法多点 → 500（已过参数校验）
        let (st, _) = http(
            port,
            "GET /touch?phase=start&ps=1,2,3;bad HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 400);
        let (st, _) = http(
            port,
            "GET /touch?phase=start&ps=100,200,1;400,600,2 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(st, 500);
    }

    #[test]
    fn touch_points_parsing() {
        // 多点解析："x,y,id;…"，id 1..=10，最多 10 点
        let v = parse_touch_points("100.5,200.5,1;400.0,600.0,2").unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].x, 100.5);
        assert_eq!(v[1].y, 600.0);
        assert_eq!(v[1].id, 2);
        assert!(parse_touch_points("").is_none());
        assert!(parse_touch_points("1,2").is_none());
        assert!(parse_touch_points("1,2,3,4").is_none());
        assert!(parse_touch_points("1,2,0").is_none()); // id 超下界
        assert!(parse_touch_points("1,2,11").is_none()); // id 超上界
        assert!(parse_touch_points("a,b,1").is_none());
        let many = "1,1,1;2,2,2;3,3,3;4,4,4;5,5,5;6,6,6;7,7,7;8,8,8;9,9,9;10,10,10;11,11,11";
        assert!(parse_touch_points(many).is_none()); // 超 10 点
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
