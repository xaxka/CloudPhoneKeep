#!/usr/bin/env node
/** 真实 139 H5 端到端验证：修复后的控制页 → 引擎 → 139 页面全链路 */
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
const UA = 'Mozilla/5.0 (Linux; Android 13; Pixel 7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Mobile Safari/537.36';
const PAGE_URL = 'https://cloudphoneh5.buy.139.com';
const ENG_PORT = 8938, A_PORT = 9448, B_PORT = 9558;
const FLAGS = ['--remote-debugging-address=127.0.0.1', '--remote-allow-origins=*',
  '--window-size=414,896', '--force-device-scale-factor=1', '--hide-scrollbars',
  '--no-first-run', '--no-default-browser-check', '--disable-gpu', '--disable-dev-shm-usage',
  '--disable-crash-reporter', '--disable-background-timer-throttling', '--disable-backgrounding-occluded-windows',
  '--disable-renderer-backgrounding', '--disable-background-networking', '--disable-component-update',
  '--disable-sync', '--disable-features=Translate,MediaRouter,OptimizationHints',
  '--mute-audio', '--autoplay-policy=no-user-gesture-required', '--disable-pinch',
  '--lang=zh-CN', '--no-sandbox', '--disable-setuid-sandbox'];

class Cdp {
  constructor(ws) { this.ws = ws; this.mid = 0; this.pend = new Map(); }
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
  call(method, params, sessionId, timeoutMs = 15000) {
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
    const r = await this.call('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: false }, sessionId, 20000);
    if (r.exceptionDetails) return { exc: r.exceptionDetails.text };
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
const log = (tag, msg) => console.log(`${new Date().toISOString().slice(11, 23)} [${tag}] ${msg}`);
const touchStats = { start: 0, move: 0, end: 0, cancel: 0 };

let chromeA, procA, sessionA, chromeB, procB, sessionB;

async function startCloudChrome() {
  procA = spawn(SHELL, [...FLAGS, `--remote-debugging-port=${A_PORT}`, '--user-data-dir=/tmp/cpk-139A', 'about:blank'], { stdio: 'ignore' });
  for (let i = 0; i < 100; i++) { try { await fetch(`http://127.0.0.1:${A_PORT}/json/version`); break; } catch (e) { await sleep(100); } }
  chromeA = await Cdp.connect(A_PORT); chromeA.start();
  const t = await chromeA.call('Target.createTarget', { url: 'about:blank' });
  const at = await chromeA.call('Target.attachToTarget', { targetId: t.targetId, flatten: true });
  sessionA = at.sessionId;
  await chromeA.call('Page.enable', {}, sessionA);
  await chromeA.call('Runtime.enable', {}, sessionA);
  await chromeA.call('Emulation.setUserAgentOverride', { userAgent: UA, platform: 'Linux armv8l' }, sessionA);
  // 事件计数探针（早于页面脚本：addScriptToEvaluateOnNewDocument 最先）
  await chromeA.call('Page.addScriptToEvaluateOnNewDocument', { source: `(function(){
    window.__ev={ts:0,tm:0,te:0,tc:0,md:0,mu:0,ck:0};
    ['touchstart','touchmove','touchend','touchcancel'].forEach(function(t){
      document.addEventListener(t,function(e){ if(e.isTrusted)window.__ev[t.slice(5)]++; },true);});
    ['mousedown','mouseup','click'].forEach(function(t){
      document.addEventListener(t,function(e){ if(e.isTrusted)window.__ev[(t==='mousedown'?'md':t==='mouseup'?'mu':'ck')]++; },true);});
  })()`, runImmediately: true }, sessionA);
  // keepalive.inject.js 同生产
  const cfg = { slot: 1, port: ENG_PORT, platform: 'mobile', homeUri: PAGE_URL,
    keepAlive: true, intervalMs: 5000, simulateActivity: true, customCursor: false,
    blockContextMenu: true, pageTimer: false };
  let js = fs.readFileSync(path.join(SHARED, 'keepalive.inject.js'), 'utf8');
  js = js.replace('__CPK_CFG__', JSON.stringify(cfg)).replace(/__CPK_CURSOR__/g, '');
  await chromeA.call('Page.addScriptToEvaluateOnNewDocument', { source: js, runImmediately: true }, sessionA);
  await chromeA.call('Page.navigate', { url: PAGE_URL }, sessionA);
  for (let i = 0; i < 90; i++) { const r = await chromeA.eval('document.readyState', sessionA); if (r.value === 'complete') break; await sleep(300); }
  await sleep(4000); // SPA 路由+渲染
  log('sys', '139 页面加载完成');
}

async function cloudState() {
  const r = await chromeA.eval(`JSON.stringify({h:location.hash,ev:window.__ev,inst:!!window.__CPK_INSTALLED__,ots:('ontouchstart' in window)})`, sessionA);
  return r.value;
}

async function ctrlTap(x, y, label) {
  log('user', `>>> 轻点控制页 (${x},${y}) ${label}`);
  const b = JSON.parse(JSON.stringify(touchStats));
  await chromeB.call('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y, id: 1 }] }, sessionB);
  await sleep(100);
  await chromeB.call('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] }, sessionB);
  await sleep(1200);
  log('user', `    引擎收到: start+${touchStats.start - b.start} move+${touchStats.move - b.move} end+${touchStats.end - b.end} | 139页面: ${await cloudState()}`);
}
async function ctrlSwipe(x, y1, y2, label, steps = 16) {
  log('user', `>>> 控制页拖动 (${x},${y1})→(${x},${y2}) ${label}`);
  const b = JSON.parse(JSON.stringify(touchStats));
  await chromeB.call('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y: y1, id: 1 }] }, sessionB);
  for (let i = 1; i <= steps; i++) {
    const yy = y1 + (y2 - y1) * i / steps;
    await chromeB.call('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x, y: yy, id: 1 }] }, sessionB);
    await sleep(33);
  }
  await chromeB.call('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] }, sessionB);
  await sleep(1200);
  log('user', `    引擎收到: start+${touchStats.start - b.start} move+${touchStats.move - b.move} end+${touchStats.end - b.end} | 139页面: ${await cloudState()}`);
}

(async () => {
  // mini 引擎
  const srv = http.createServer((req, res) => {
    if (req.method === 'POST') {
      let body = '';
      req.on('data', (c) => body += c);
      req.on('end', () => {
        const form = {};
        body.split('&').forEach((kv) => { const [k, v] = kv.split('='); form[k] = decodeURIComponent(v || ''); });
        if (req.url === '/touch') {
          touchStats[form.phase]++;
          if (form.phase !== 'move') log('click', `TOUCH ${form.phase} ${form.ps || ''}`);
          if (chromeA && form.phase) {
            const typ = { start: 'touchStart', move: 'touchMove', end: 'touchEnd', cancel: 'touchCancel' }[form.phase];
            let pts = [];
            if (form.phase === 'start' || form.phase === 'move') {
              if (form.ps) pts = form.ps.split(';').map((s) => { const [x, y, id] = s.split(','); return { x: +x, y: +y, id: +id }; });
            }
            chromeA.send('Input.dispatchTouchEvent', { type: typ, touchPoints: pts }, sessionA);
          }
        }
        res.writeHead(200); res.end('ok');
      });
      return;
    }
    if (req.url === '/') { res.writeHead(200, {'content-type':'text/html; charset=utf-8'}); return res.end(fs.readFileSync(path.join(SHARED, 'control_page.html'))); }
    if (req.url === '/healthz') { res.writeHead(200, {'content-type':'application/json'});
      return res.end(JSON.stringify({ok:true,platform:'mobile',fps:10,page:'cloudAppList',url:PAGE_URL,alive:true})); }
    if (req.url === '/stream.mjpg') { res.writeHead(200, {'content-type':'multipart/x-mixed-replace; boundary=cpk'});
      const f = Buffer.from('/9j/4AAQSkZJRgABAQEAYABgAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDwsLDBkPEw8UFx8SFBcWGxQfGx8cJCwhJSUnMTM1MzIkKys1NDM1MzY7QTc5QTc5RTU8Pz//AAD//9sAhAAQEBAQEBAAAAAAAAAAAAAAAAAQIDCAkKCAoLCgAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD/wAARCAABAAEDASIAAhEBAxEB/8QAFQABAQAAAAAAAAAAAAAAAAAAAAv/xAAUEAEAAAAAAAAAAAAAAAAAAAAA/8QAFQEBAQAAAAAAAAAAAAAAAAAAAAX/xAAUEQEAAAAAAAAAAAAAAAAAAAAA/9oADAMBAAIRAxEAPwCdABmX/9k=','base64');
      const part = Buffer.concat([Buffer.from('--cpk\r\nContent-Type: image/jpeg\r\nContent-Length: '+f.length+'\r\n\r\n'), f, Buffer.from('\r\n')]);
      res.write(part);
      const iv = setInterval(()=>{try{res.write(part)}catch(e){clearInterval(iv)}},1000);
      req.on('close',()=>clearInterval(iv)); return; }
    res.writeHead(404); res.end();
  });
  await new Promise((r) => srv.listen(ENG_PORT, '127.0.0.1', r));
  await startCloudChrome();

  procB = spawn(SHELL, [...FLAGS, `--remote-debugging-port=${B_PORT}`, '--user-data-dir=/tmp/cpk-139B', `http://127.0.0.1:${ENG_PORT}/`], { stdio: 'ignore' });
  for (let i = 0; i < 100; i++) { try { await fetch(`http://127.0.0.1:${B_PORT}/json/version`); break; } catch (e) { await sleep(100); } }
  chromeB = await Cdp.connect(B_PORT); chromeB.start();
  const list = await (await fetch(`http://127.0.0.1:${B_PORT}/json/list`)).json();
  const page = list.find((t) => t.type === 'page');
  const at = await chromeB.call('Target.attachToTarget', { targetId: page.id, flatten: true });
  sessionB = at.sessionId;
  await chromeB.call('Page.enable', {}, sessionB);
  await chromeB.call('Runtime.enable', {}, sessionB);
  for (let i = 0; i < 50; i++) { const r = await chromeB.eval('document.readyState', sessionB); if (r.value === 'complete') break; await sleep(200); }
  await sleep(2500);
  log('sys', '控制页就绪');

  log('user', `139 初始: ${await cloudState()}`);
  // 用户日志里的真实点击坐标（22:38:12 起 10 连点的第 1、4 个）
  await ctrlTap(278, 224, 'app-icon（用户原坐标1）');
  await ctrlTap(237, 158, 'app-btn（用户原坐标4）');
  await ctrlSwipe(207, 500, 200, '列表上滑');
  await ctrlTap(278, 224, '再次轻点 app-icon');

  log('verdict', `引擎累计: start=${touchStats.start} move=${touchStats.move} end=${touchStats.end} cancel=${touchStats.cancel}`);
  log('verdict', touchStats.end >= 3 ? '✓ end 全部到达（修复生效）' : `✗ end 仅 ${touchStats.end}（仍丢失）`);

  procA.kill(); procB.kill(); srv.close(); process.exit(0);
})().catch((e) => { console.error('FATAL', e); try { procA && procA.kill(); procB && procB.kill(); } catch (_) {} process.exit(1); });
