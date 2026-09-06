#!/usr/bin/env node
/** 深挖：控制页里 pointer/touch 事件流诊断（为什么 end 不发出）*/
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
const SHARED = path.join(__dirname, '..', '..', 'shared');   // 仓库根 shared/
const ENG_PORT = 8934, B_PORT = 9556;
const FLAGS = ['--remote-debugging-address=127.0.0.1', '--remote-allow-origins=*',
  '--window-size=414,896', '--force-device-scale-factor=1', '--hide-scrollbars',
  '--no-first-run', '--no-default-browser-check', '--disable-gpu', '--disable-dev-shm-usage',
  '--disable-crash-reporter', '--disable-pinch', '--lang=zh-CN', '--no-sandbox',
  '--disable-setuid-sandbox', '--disable-features=Translate,MediaRouter,OptimizationHints'];

class Cdp {
  constructor(ws) { this.ws = ws; this.mid = 0; this.pend = new Map(); this.handlers = []; }
  static async connect(port) {
    const ver = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
    return new Cdp(await wsDial(ver.webSocketDebuggerUrl));
  }
  start() {
    this.ws.on('message', (data) => {
      const d = JSON.parse(data.toString());
      if (d.id && this.pend.has(d.id)) {
        const { resolve, reject } = this.pend.get(d.id);
        this.pend.delete(d.id);
        d.error ? reject(new Error(JSON.stringify(d.error))) : resolve(d.result || {});
      }
    });
  }
  send(method, params, sessionId) {
    this.mid++;
    const msg = { id: this.mid, method, params: params || {} };
    if (sessionId) msg.sessionId = sessionId;
    this.ws.send(JSON.stringify(msg));
  }
  call(method, params, sessionId, timeoutMs = 10000) {
    this.mid++;
    const id = this.mid;
    const msg = { id, method, params: params || {} };
    if (sessionId) msg.sessionId = sessionId;
    const p = new Promise((resolve, reject) => {
      this.pend.set(id, { resolve, reject });
      setTimeout(() => { if (this.pend.has(id)) { this.pend.delete(id); reject(new Error('timeout ' + method)); } }, timeoutMs);
    });
    this.ws.send(JSON.stringify(msg));
    return p;
  }
  async eval(expr, sessionId) {
    const r = await this.call('Runtime.evaluate', { expression: expr, returnByValue: true }, sessionId);
    if (r.exceptionDetails) return { exc: r.exceptionDetails.text + ' ' + JSON.stringify(r.exceptionDetails.exception || {}) };
    return { value: r.result && r.result.value };
  }
}
function wsDial(url) {
  return new Promise((resolve, reject) => {
    const u = new URL(url);
    const key = crypto.randomBytes(16).toString('base64');
    const req = http.request({ hostname: u.hostname, port: u.port, path: u.pathname + u.search,
      headers: { Connection: 'Upgrade', Upgrade: 'websocket', 'Sec-WebSocket-Key': key, 'Sec-WebSocket-Version': 13 } });
    req.on('upgrade', (res, socket) => {
      let buf = Buffer.alloc(0);
      const listeners = {};
      socket.on('data', (chunk) => {
        buf = Buffer.concat([buf, chunk]);
        for (;;) { const m = wsRead(buf); if (!m) break; buf = m.rest;
          const s = m.payload.toString();
          if (listeners.message) for (const fn of listeners.message) fn(s); }
      });
      resolve({ on: (ev, fn) => { (listeners[ev] = listeners[ev] || []).push(fn); },
        send: (s) => wsWrite(socket, s), close: () => socket.destroy() });
    });
    req.on('error', reject); req.end();
  });
}
function wsWrite(socket, text) {
  const p = Buffer.from(text, 'utf8');
  const mask = crypto.randomBytes(4);
  let header;
  if (p.length < 126) { header = Buffer.alloc(2); header[1] = 0x80 | p.length; }
  else { header = Buffer.alloc(4); header[1] = 0x80 | 126; header.writeUInt16BE(p.length, 2); }
  header[0] = 0x81;
  const masked = Buffer.alloc(p.length);
  for (let i = 0; i < p.length; i++) masked[i] = p[i] ^ mask[i % 4];
  socket.write(Buffer.concat([header, mask, masked]));
}
function wsRead(buf) {
  if (buf.length < 2) return null;
  const len0 = buf[1] & 0x7f;
  let off = 2, len = len0;
  if (len0 === 126) { if (buf.length < 4) return null; len = buf.readUInt16BE(2); off = 4; }
  else if (len0 === 127) { if (buf.length < 10) return null; len = Number(buf.readBigUInt64BE(2)); off = 10; }
  if (buf.length < off + len) return null;
  return { payload: buf.slice(off, off + len), rest: buf.slice(off + len) };
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

(async () => {
  // mini 引擎（只记录 /touch）
  const srv = http.createServer((req, res) => {
    if (req.method === 'POST') {
      let body = '';
      req.on('data', (c) => body += c);
      req.on('end', () => {
        const form = {};
        body.split('&').forEach((kv) => { const [k, v] = kv.split('='); form[k] = decodeURIComponent(v || ''); });
        if (req.url === '/touch') console.log(`    [engine] TOUCH ${form.phase} ${form.ps || ''}`);
        res.writeHead(200); res.end('ok');
      });
      return;
    }
    if (req.url === '/' ) { res.writeHead(200, {'content-type':'text/html; charset=utf-8'}); return res.end(fs.readFileSync(path.join(SHARED, 'control_page.html'))); }
    if (req.url === '/healthz') { res.writeHead(200, {'content-type':'application/json'});
      return res.end(JSON.stringify({ok:true,platform:'mobile',fps:10,page:'cloudAppList',url:'http://x/',alive:true})); }
    if (req.url === '/stream.mjpg') { res.writeHead(200, {'content-type':'multipart/x-mixed-replace; boundary=cpk'});
      const f = Buffer.from('/9j/4AAQSkZJRgABAQEAYABgAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDwsLDBkPEw8UFx8SFBcWGxQfGx8cJCwhJSUnMTM1MzIkKys1NDM1MzY7QTc5QTc5RTU8Pz//AAD//9sAhAAQEBAQEBAAAAAAAAAAAAAAAAAQIDCAkKCAoLCgAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD/wAARCAABAAEDASIAAhEBAxEB/8QAFQABAQAAAAAAAAAAAAAAAAAAAAv/xAAUEAEAAAAAAAAAAAAAAAAAAAAA/8QAFQEBAQAAAAAAAAAAAAAAAAAAAAX/xAAUEQEAAAAAAAAAAAAAAAAAAAAA/9oADAMBAAIRAxEAPwCdABmX/9k=','base64');
      const part = Buffer.concat([Buffer.from('--cpk\r\nContent-Type: image/jpeg\r\nContent-Length: '+f.length+'\r\n\r\n'), f, Buffer.from('\r\n')]);
      res.write(part);
      const iv = setInterval(()=>{try{res.write(part)}catch(e){clearInterval(iv)}},1000);
      req.on('close',()=>clearInterval(iv)); return; }
    res.writeHead(404); res.end();
  });
  await new Promise((r) => srv.listen(ENG_PORT, '127.0.0.1', r));

  const proc = spawn(SHELL, [...FLAGS, `--remote-debugging-port=${B_PORT}`, '--user-data-dir=/tmp/cpk-rB2', `http://127.0.0.1:${ENG_PORT}/`], { stdio: 'ignore' });
  for (let i = 0; i < 100; i++) { try { await fetch(`http://127.0.0.1:${B_PORT}/json/version`); break; } catch (e) { await sleep(100); } }
  const cdp = await Cdp.connect(B_PORT); cdp.start();
  const list = await (await fetch(`http://127.0.0.1:${B_PORT}/json/list`)).json();
  const page = list.find((t) => t.type === 'page');
  const at = await cdp.call('Target.attachToTarget', { targetId: page.id, flatten: true });
  const S = at.sessionId;
  await cdp.call('Page.enable', {}, S);
  await cdp.call('Runtime.enable', {}, S);
  for (let i = 0; i < 50; i++) { const r = await cdp.eval('document.readyState', S); if (r.value === 'complete') break; await sleep(200); }
  await sleep(2500);

  // ===== 注入事件流探针（window 捕获层：先于页面自身监听器）=====
  const probe = `(function(){
    window.__E = [];
    function rec(tag, ev){
      try {
        window.__E.push(tag + ' pid=' + ev.pointerId + ' pt=' + (ev.pointerType||'-') +
          ' xy=' + Math.round(ev.clientX||0) + ',' + Math.round(ev.clientY||0) +
          ' t=' + ev.target.tagName + (ev.target.id ? '#'+ev.target.id : '') +
          (ev.isTrusted===false ? ' SYNTH' : ''));
      } catch(e){}
    }
    ['pointerdown','pointerup','pointercancel','pointermove'].forEach(function(t){
      window.addEventListener(t, function(ev){ rec(t, ev); }, true);
    });
    ['touchstart','touchend','touchcancel'].forEach(function(t){
      window.addEventListener(t, function(ev){ rec('*'+t, ev); }, true);
    });
    ['mousedown','mouseup','click'].forEach(function(t){
      window.addEventListener(t, function(ev){ rec('*'+t, ev); }, true);
    });
    return 'probe ok';
  })()`;
  console.log((await cdp.eval(probe, S)).value);

  // ===== 派发一次轻点（touchStart → 120ms → touchEnd）=====
  console.log('\n=== CDP 轻点 (207,200) start→120ms→end ===');
  await cdp.call('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: 207, y: 200, id: 1 }] }, S);
  await sleep(120);
  await cdp.call('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] }, S);
  await sleep(800);
  console.log((await cdp.eval('JSON.stringify(window.__E, null, 1)', S)).value);

  // ===== 对照：完整 mouse 事件点击（应产出 mousedown/up + click）=====
  console.log('\n=== CDP 鼠标点击对照 (207,200) ===');
  await cdp.eval('window.__E = []', S);
  await cdp.call('Input.dispatchMouseEvent', { type: 'mousePressed', x: 207, y: 200, button: 'left', buttons: 1, clickCount: 1 }, S);
  await sleep(60);
  await cdp.call('Input.dispatchMouseEvent', { type: 'mouseReleased', x: 207, y: 200, button: 'left', buttons: 0, clickCount: 1 }, S);
  await sleep(500);
  console.log((await cdp.eval('JSON.stringify(window.__E, null, 1)', S)).value);

  proc.kill(); srv.close(); process.exit(0);
})().catch((e) => { console.error('FATAL', e); process.exit(1); });
