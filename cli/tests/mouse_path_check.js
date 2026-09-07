/** 验证控制页鼠标点击路径（此前 ptrUp 置空 mode 零事件发出，已改 mouseUp） */
const { spawn } = require('child_process');
const http = require('http');
const crypto = require('crypto');
const fs = require('fs');
const path = require('path');
// Chrome 可执行：CPK_SHELL 显式指定优先，否则探测常见安装路径
const SHELL = process.env.CPK_SHELL || ['/usr/bin/chromium', '/usr/bin/chromium-browser',
  '/usr/bin/google-chrome', '/usr/bin/google-chrome-stable', '/usr/bin/chrome-headless-shell']
  .find(p => fs.existsSync(p)) || (() => {
  console.error('未找到 Chrome：请 export CPK_SHELL=/path/to/chrome（或 chromium）'); process.exit(1); })();
const CLI = path.join(__dirname, '..');      // cli/ 根（控制页模板 control_page.html，CLI 专属）
const ENG_PORT = 8940, B_PORT = 9560;
const FLAGS = ['--remote-debugging-address=127.0.0.1','--remote-allow-origins=*','--window-size=414,896',
  '--force-device-scale-factor=1','--no-first-run','--no-default-browser-check','--disable-gpu',
  '--disable-dev-shm-usage','--disable-pinch','--lang=zh-CN','--no-sandbox','--disable-setuid-sandbox'];
const mouseStats = { down: 0, up: 0, move: 0, wheel: 0, touch: 0 };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
// 裸 ws 客户端（同前）
function wsDial(url){return new Promise((resolve,reject)=>{const u=new URL(url);const key=crypto.randomBytes(16).toString('base64');
 const req=http.request({hostname:u.hostname,port:u.port,path:u.pathname,headers:{Connection:'Upgrade',Upgrade:'websocket','Sec-WebSocket-Key':key,'Sec-WebSocket-Version':13}});
 req.on('upgrade',(res,socket)=>{let buf=Buffer.alloc(0);const listeners={};
  socket.on('data',(chunk)=>{buf=Buffer.concat([buf,chunk]);for(;;){const m=wsRead(buf);if(!m)break;buf=m.rest;const s=m.payload.toString();if(listeners.message)for(const fn of listeners.message)fn(s);}});
  resolve({on:(ev,fn)=>{(listeners[ev]=listeners[ev]||[]).push(fn);},send:(s)=>wsWrite(socket,s),close:()=>socket.destroy()});});
 req.on('error',reject);req.end();});}
function wsWrite(socket,text){const p=Buffer.from(text,'utf8');const mask=crypto.randomBytes(4);let header;
 if(p.length<126){header=Buffer.alloc(2);header[1]=0x80|p.length;}else{header=Buffer.alloc(4);header[1]=0x80|126;header.writeUInt16BE(p.length,2);}
 header[0]=0x81;const masked=Buffer.alloc(p.length);for(let i=0;i<p.length;i++)masked[i]=p[i]^mask[i%4];socket.write(Buffer.concat([header,mask,masked]));}
function wsRead(buf){if(buf.length<2)return null;const len0=buf[1]&0x7f;let off=2,len=len0;
 if(len0===126){if(buf.length<4)return null;len=buf.readUInt16BE(2);off=4;}else if(len0===127){if(buf.length<10)return null;len=Number(buf.readBigUInt64BE(2));off=10;}
 if(buf.length<off+len)return null;return{payload:buf.slice(off,off+len),rest:buf.slice(off+len)};}
async function main(){
 const srv = http.createServer((req,res)=>{
   if(req.method==='POST'){let body='';req.on('data',c=>body+=c);req.on('end',()=>{
     const form={};body.split('&').forEach(kv=>{const[k,v]=kv.split('=');form[k]=decodeURIComponent(v||'');});
     if(req.url==='/mouse'){mouseStats[form.action]++;console.log(`    [engine] MOUSE ${form.action} ${form.x||''},${form.y||''}`);}
     if(req.url==='/touch'){mouseStats.touch++;console.log(`    [engine] TOUCH ${form.phase}`);}
     res.writeHead(200);res.end('ok');});return;}
   if(req.url==='/'){res.writeHead(200,{'content-type':'text/html; charset=utf-8'});return res.end(fs.readFileSync(path.join(CLI,'control_page.html')));}
   if(req.url==='/healthz'){res.writeHead(200,{'content-type':'application/json'});return res.end(JSON.stringify({ok:true,platform:'mobile',fps:10,page:'cloudAppList',url:'http://x/',alive:true}));}
   if(req.url==='/stream.mjpg'){res.writeHead(200,{'content-type':'multipart/x-mixed-replace; boundary=cpk'});
     const f=Buffer.from('/9j/4AAQSkZJRgABAQEAYABgAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDwsLDBkPEw8UFx8SFBcWGxQfGx8cJCwhJSUnMTM1MzIkKys1NDM1MzY7QTc5QTc5RTU8Pz//AAD//9sAhAAQEBAQEBAAAAAAAAAAAAAAAAAQIDCAkKCAoLCgAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD/wAARCAABAAEDASIAAhEBAxEB/8QAFQABAQAAAAAAAAAAAAAAAAAAAAv/xAAUEAEAAAAAAAAAAAAAAAAAAAAA/8QAFQEBAQAAAAAAAAAAAAAAAAAAAAX/xAAUEQEAAAAAAAAAAAAAAAAAAAAA/9oADAMBAAIRAxEAPwCdABmX/9k=','base64');
     const part=Buffer.concat([Buffer.from('--cpk\r\nContent-Type: image/jpeg\r\nContent-Length: '+f.length+'\r\n\r\n'),f,Buffer.from('\r\n')]);res.write(part);
     const iv=setInterval(()=>{try{res.write(part)}catch(e){clearInterval(iv)}},1000);req.on('close',()=>clearInterval(iv));return;}
   res.writeHead(404);res.end();});
 await new Promise((r)=>srv.listen(ENG_PORT,'127.0.0.1',r));
 const proc = spawn(SHELL,
   [...FLAGS,`--remote-debugging-port=${B_PORT}`,'--user-data-dir=/tmp/cpk-mB',`http://127.0.0.1:${ENG_PORT}/`],{stdio:'ignore'});
 for(let i=0;i<100;i++){try{await fetch(`http://127.0.0.1:${B_PORT}/json/version`);break;}catch(e){await sleep(100);}}
 const ver = await (await fetch(`http://127.0.0.1:${B_PORT}/json/version`)).json();
 const ws = await wsDial(ver.webSocketDebuggerUrl);
 let mid=0;const pend=new Map();
 ws.on('message',(s)=>{const d=JSON.parse(s);if(d.id&&pend.has(d.id)){const{resolve,reject}=pend.get(d.id);pend.delete(d.id);d.error?reject(new Error(JSON.stringify(d.error))):resolve(d.result||{});}});
 const call=(method,params,sid)=>{mid++;const id=mid;const m={id,method,params:params||{}};if(sid)m.sessionId=sid;
   return new Promise((resolve,reject)=>{pend.set(id,{resolve,reject});setTimeout(()=>{if(pend.has(id)){pend.delete(id);reject(new Error('timeout'));}},15000);ws.send(JSON.stringify(m));});};
 const list=await(await fetch(`http://127.0.0.1:${B_PORT}/json/list`)).json();
 const page=list.find(t=>t.type==='page');
 const at=await call('Target.attachToTarget',{targetId:page.id,flatten:true});
 const S=at.sessionId;
 await call('Page.enable',{},S);
 for(let i=0;i<50;i++){const r=await call('Runtime.evaluate',{expression:'document.readyState',returnByValue:true},S);if(r.result&&r.result.value==='complete')break;await sleep(200);}
 await sleep(2000);
 console.log('=== 模拟电脑用户鼠标点击控制页 (207,200)（不动=纯点击）===');
 await call('Input.dispatchMouseEvent',{type:'mousePressed',x:207,y:200,button:'left',buttons:1,clickCount:1},S);
 await sleep(100);
 await call('Input.dispatchMouseEvent',{type:'mouseReleased',x:207,y:200,button:'left',buttons:0,clickCount:1},S);
 await sleep(600);
 console.log(`引擎收到鼠标事件: down=${mouseStats.down} up=${mouseStats.up}（修复前为 0/0）`);
 console.log(mouseStats.down>=1&&mouseStats.up>=1?'✓ 鼠标点击路径已通（mClickSeq 发出）':'✗ 鼠标点击仍零事件');
 console.log('=== 模拟电脑用户鼠标拖动 (207,500)→(207,200)（超阈值转触摸流）===');
 const t0=mouseStats.touch;
 await call('Input.dispatchMouseEvent',{type:'mousePressed',x:207,y:500,button:'left',buttons:1,clickCount:1},S);
 for(let i=1;i<=10;i++){await call('Input.dispatchMouseEvent',{type:'mouseMoved',x:207,y:500+(200-500)*i/10,button:'left',buttons:1},S);await sleep(40);}
 await call('Input.dispatchMouseEvent',{type:'mouseReleased',x:207,y:200,button:'left',buttons:0,clickCount:1},S);
 await sleep(600);
 console.log(`拖动转触摸: touch事件=${mouseStats.touch-t0}（含 start/end）`);
 proc.kill();srv.close();process.exit(0);
}
main().catch(e=>{console.error('FATAL',e);process.exit(1);});
