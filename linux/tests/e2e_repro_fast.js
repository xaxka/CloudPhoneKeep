#!/usr/bin/env node
/**
 * CloudPhoneKeep Linux 全链路 bug 复现器
 * =====================================
 * 完全复刻生产链路（不依赖 Rust 引擎，node 模拟引擎 HTTP 端点）：
 *
 *   [Chrome B 控制页] ←CDP 触摸（模拟用户在手机上点/拖控制页）
 *        │ POST /touch /mouse ...
 *        ▼
 *   [node mini 引擎]（记日志：start/move/end 是否到达）
 *        │ CDP Input.dispatchTouchEvent（同 engine.rs dispatch_touch：end 空点）
 *        ▼
 *   [Chrome A 云机页]（139 真实 H5 + keepalive.inject.js 同生产注入）
 *
 * 验证目标：
 *  1. 控制页轻点 → node 是否收到 phase=end（用户日志：从未收到 ← 核心 bug）
 *  2. 控制页拖动 → 139 页面是否滚动
 *  3. 139 页面点击是否有效（页面状态变化）
 */
const { spawn } = require('child_process');
const http = require('http');
const fs = require('fs');
const path = require('path');
const urllib = require('url');

// ---------- 配置 ----------
// Chrome 可执行：CPK_SHELL 显式指定优先，否则探测常见安装路径
const SHELL = process.env.CPK_SHELL || ['/usr/bin/chromium', '/usr/bin/chromium-browser',
  '/usr/bin/google-chrome', '/usr/bin/google-chrome-stable', '/usr/bin/chrome-headless-shell']
  .find(p => fs.existsSync(p)) || (() => {
  console.error('未找到 Chrome：请 export CPK_SHELL=/path/to/chrome（或 chromium）'); process.exit(1); })();
const SHARED = path.join(__dirname, '..', '..', 'shared');   // 仓库根 shared/
const UA = 'Mozilla/5.0 (Linux; Android 13; Pixel 7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Mobile Safari/537.36';
const PAGE_URL = process.env.CPK_TARGET || 'http://127.0.0.1:8932/repro_page.html';
// 控制页所在端口（mini 引擎端口）
const ENG_PORT = 8936;
const A_PORT = 9446; // Chrome A CDP
const B_PORT = 9557; // Chrome B CDP
const BASE_FLAGS = [
  '--remote-debugging-address=127.0.0.1', '--remote-allow-origins=*',
  '--window-size=414,896', '--force-device-scale-factor=1', '--hide-scrollbars',
  '--no-first-run', '--no-default-browser-check', '--disable-gpu',
  '--disable-dev-shm-usage', '--disable-crash-reporter',
  '--disable-background-timer-throttling', '--disable-backgrounding-occluded-windows',
  '--disable-renderer-backgrounding', '--disable-background-networking',
  '--disable-component-update', '--disable-sync',
  '--disable-features=Translate,MediaRouter,OptimizationHints',
  '--mute-audio', '--autoplay-policy=no-user-gesture-required',
  '--disable-pinch', '--lang=zh-CN', '--no-sandbox', '--disable-setuid-sandbox',
];

// ---------- 极简 CDP 客户端（裸 WebSocket，同 Rust 实现语义：fire/await）----------
class Cdp {
  constructor(ws) { this.ws = ws; this.mid = 0; this.pend = new Map(); this.handlers = []; this.closed = false; }
  static async connect(port) {
    const ver = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
    const ws = await wsDial(ver.webSocketDebuggerUrl);
    return new Cdp(ws);
  }
  start() {
    this.ws.on('message', (data) => {
      const d = JSON.parse(data.toString());
      if (d.id && this.pend.has(d.id)) {
        const { resolve, reject } = this.pend.get(d.id);
        this.pend.delete(d.id);
        d.error ? reject(new Error(JSON.stringify(d.error))) : resolve(d.result || {});
      } else if (d.method) {
        for (const h of this.handlers) h(d.method, d.params);
      }
    });
    this.ws.on('close', () => { this.closed = true; });
  }
  on(fn) { this.handlers.push(fn); }
  send(method, params, sessionId) {  // fire（不等应答）
    this.mid++;
    const msg = { id: this.mid, method, params: params || {} };
    if (sessionId) msg.sessionId = sessionId;
    this.ws.send(JSON.stringify(msg));
    return this.mid;
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
    const r = await this.call('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: false }, sessionId);
    if (r.exceptionDetails) return { exc: r.exceptionDetails.text };
    return { value: r.result && r.result.value };
  }
}
// 裸 ws 拨号（node 无 ws 库：用 http upgrade 手写 RFC6455 客户端）
const crypto = require('crypto');
function wsDial(url) {
  return new Promise((resolve, reject) => {
    const u = new URL(url);
    const key = crypto.randomBytes(16).toString('base64');
    const req = http.request({
      hostname: u.hostname, port: u.port, path: u.pathname + u.search,
      headers: { Connection: 'Upgrade', Upgrade: 'websocket', 'Sec-WebSocket-Key': key, 'Sec-WebSocket-Version': 13 },
    });
    req.on('upgrade', (res, socket, head) => {
      const ws = {
        socket, on: (ev, fn) => socket.on('data', () => {}) ,
        send: (s) => wsWrite(socket, s), close: () => socket.destroy(),
      };
      // 简单帧解析
      let buf = Buffer.alloc(0);
      const listeners = {};
      socket.on('data', (chunk) => {
        buf = Buffer.concat([buf, chunk]);
        for (;;) {
          const m = wsRead(buf);
          if (!m) break;
          buf = m.rest;
          const s = m.payload.toString();
          if (listeners.message) for (const fn of listeners.message) fn(s);
        }
      });
      ws.on = (ev, fn) => { (listeners[ev] = listeners[ev] || []).push(fn); };
      resolve(ws);
    });
    req.on('error', reject);
    req.end();
  });
}
function wsWrite(socket, text) {
  const p = Buffer.from(text, 'utf8');
  const mask = crypto.randomBytes(4);
  let header;
  if (p.length < 126) { header = Buffer.alloc(2); header[1] = 0x80 | p.length; }
  else if (p.length < 65536) { header = Buffer.alloc(4); header[1] = 0x80 | 126; header.writeUInt16BE(p.length, 2); }
  else { header = Buffer.alloc(10); header[1] = 0x80 | 127; header.writeBigUInt64BE(BigInt(p.length), 2); }
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

// ---------- 引擎日志（复刻 logger [click] 留痕）----------
const engLog = [];
function log(tag, msg) {
  const line = `${new Date().toISOString().slice(11, 23)} [engine] [${tag}] ${msg}`;
  console.log(line); engLog.push(line);
}
const touchStats = { start: 0, move: 0, end: 0, cancel: 0, mouse: 0, firstEndAt: null };

// ---------- Chrome A（云机 139 页）----------
let chromeA, procA, sessionA;
async function startCloudChrome() {
  procA = spawn(SHELL, [...BASE_FLAGS, `--remote-debugging-port=${A_PORT}`, '--user-data-dir=/tmp/cpk-rA2', 'about:blank'], { stdio: 'ignore' });
  await waitPort(A_PORT);
  chromeA = await Cdp.connect(A_PORT);
  chromeA.start();
  const t = await chromeA.call('Target.createTarget', { url: 'about:blank' });
  const at = await chromeA.call('Target.attachToTarget', { targetId: t.targetId, flatten: true });
  sessionA = at.sessionId;
  await chromeA.call('Page.enable', {}, sessionA);
  await chromeA.call('Runtime.enable', {}, sessionA);
  await chromeA.call('Emulation.setUserAgentOverride', { userAgent: UA, platform: 'Linux armv8l' }, sessionA);
  // 同生产：注入 keepalive.inject.js（构造 CFG JSON + 占位符替换，与 keepalive.rs 相同）
  const cfg = { slot: 1, port: ENG_PORT, platform: 'mobile', homeUri: PAGE_URL,
    keepAlive: true, intervalMs: 5000, simulateActivity: true, customCursor: false,
    blockContextMenu: true, pageTimer: false };
  let js = fs.readFileSync(path.join(SHARED, 'keepalive.inject.js'), 'utf8');
  js = js.replace('__CPK_CFG__', JSON.stringify(cfg)).replace(/__CPK_CURSOR__/g, '');
  await chromeA.call('Page.addScriptToEvaluateOnNewDocument', { source: js, runImmediately: true }, sessionA);
  await chromeA.call('Page.navigate', { url: PAGE_URL }, sessionA);
  await waitLoad(chromeA, sessionA);
  log('sys', `Chrome A 云机页就绪: ${PAGE_URL}`);
}

// ---------- Chrome B（控制页：模拟用户手机浏览器）----------
let chromeB, procB, sessionB;
async function startControlChrome() {
  procB = spawn(SHELL, [...BASE_FLAGS, `--remote-debugging-port=${B_PORT}`, '--user-data-dir=/tmp/cpk-rB2', `http://127.0.0.1:${ENG_PORT}/`], { stdio: 'ignore' });
  await waitPort(B_PORT);
  chromeB = await Cdp.connect(B_PORT);
  chromeB.start();
  // 找已打开的页面 target（启动 URL 即控制页）
  const list = await (await fetch(`http://127.0.0.1:${B_PORT}/json/list`)).json();
  const page = list.find(t => t.type === 'page' && t.url.includes(String(ENG_PORT)));
  const at = await chromeB.call('Target.attachToTarget', { targetId: page.id, flatten: true });
  sessionB = at.sessionId;
  await chromeB.call('Page.enable', {}, sessionB);
  await chromeB.call('Runtime.enable', {}, sessionB);
  await waitLoad(chromeB, sessionB);
  log('sys', `Chrome B 控制页就绪 url=${(await chromeB.eval('location.href', sessionB)).value}`);
}

// ---------- mini 引擎 HTTP 服务（模拟 report_server 端点）----------
function startEngineServer() {
  const html = fs.readFileSync(path.join(SHARED, 'control_page.html'));
  const srv = http.createServer((req, res) => {
    const u = urllib.parse(req.url, true);
    const send = (code, body, type) => { res.writeHead(code, { 'content-type': type || 'text/plain; charset=utf-8' }); res.end(body); };
    if (req.method === 'POST') {
      let body = '';
      req.on('data', (c) => body += c);
      req.on('end', () => {
        const form = {};
        body.split('&').forEach((kv) => { const [k, v] = kv.split('='); form[k] = decodeURIComponent(v || ''); });
        if (u.pathname === '/touch') {
          touchStats[form.phase]++;
          if (form.phase !== 'move') log('click', `TOUCH ${form.phase} ${form.ps || ''}`); // move 不刷屏
          if (form.phase === 'end' && !touchStats.firstEndAt) touchStats.firstEndAt = Date.now();
          forwardTouch(form);
          return send(200, 'ok');
        }
        if (u.pathname === '/mouse') { touchStats.mouse++; log('click', `MOUSE ${form.action} ${form.x || ''},${form.y || ''}`); return send(200, 'ok'); }
        if (u.pathname === '/log') return send(200, 'ok');      // inject 脚本诊断上报（忽略）
        if (u.pathname === '/report') return send(200, 'ok');
        send(404, 'nf');
      });
      return;
    }
    if (u.pathname === '/' || u.pathname === '/index.html') {
      res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' }); return res.end(html);
    }
    if (u.pathname === '/healthz') {
      return send(200, JSON.stringify({ ok: true, platform: 'mobile', fps: 10, page: 'cloudAppList', url: PAGE_URL, alive: true }), 'application/json');
    }
    if (u.pathname === '/stream.mjpg') {  // 恒定 1 帧小图（足够让控制页 live 模式运转）
      res.writeHead(200, { 'content-type': 'multipart/x-mixed-replace; boundary=cpk' });
      const frame = Buffer.from('/9j/4AAQSkZJRgABAQEAYABgAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNCwsLDBkPEw8UFw8SFBcWGxQeGx8cJCwhJSUnMTM1MzIkKys1NDM1MzY7QTc5QTc5RTU8Pz//AAD//9sAhAAQEBAQEBAAAAAAAAAAAAAAAAAQIDCAkKCAoLCgAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD/wAARCAABAAEDASIAAhEBAxEB/8QAFQABAQAAAAAAAAAAAAAAAAAAAAv/xAAUEAEAAAAAAAAAAAAAAAAAAAAA/8QAFQEBAQAAAAAAAAAAAAAAAAAAAAX/xAAUEQEAAAAAAAAAAAAAAAAAAAAA/9oADAMBAAIRAxEAPwCdABmX/9k=', 'base64');
      const part = Buffer.concat([Buffer.from('--cpk\r\nContent-Type: image/jpeg\r\nContent-Length: ' + frame.length + '\r\n\r\n'), frame, Buffer.from('\r\n')]);
      res.write(part);
      const iv = setInterval(() => { try { res.write(part); } catch (e) { clearInterval(iv); } }, 1000);
      req.on('close', () => clearInterval(iv));
      return;
    }
    send(404, 'nf');
  });
  return new Promise((r) => srv.listen(ENG_PORT, '127.0.0.1', () => r(srv)));
}

// ---------- 转发触摸到 Chrome A（同 engine.rs dispatch_touch）----------
function forwardTouch(form) {
  if (!chromeA || chromeA.closed) return;
  const typ = { start: 'touchStart', move: 'touchMove', end: 'touchEnd', cancel: 'touchCancel' }[form.phase];
  let pts = [];
  if (form.phase === 'start' || form.phase === 'move') {
    // ps="x,y,id;..."；单点兼容 x/y（引擎 HTTP 层同款）
    if (form.ps) {
      pts = form.ps.split(';').map((s) => { const [x, y, id] = s.split(','); return { x: +x, y: +y, id: +id }; });
    } else { pts = [{ x: +form.x, y: +form.y, id: 1 }]; }
  } // end/cancel 恒空点（CDP 协议）
  chromeA.send('Input.dispatchTouchEvent', { type: typ, touchPoints: pts }, sessionA);
}

// ---------- 模拟用户在控制页上操作（CDP 触摸 → 控制页产生 pointer 事件）----------
async function ctrlTap(x, y, label) {
  log('user', `>>> 轻点控制页 (${x},${y}) ${label}`);
  const b = JSON.parse(JSON.stringify(touchStats));
  await chromeB.call('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y, id: 1 }] }, sessionB);
  await sleep(30);
  await chromeB.call('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] }, sessionB);
  await sleep(500);
  log('user', `    引擎收到: start+${touchStats.start - b.start} move+${touchStats.move - b.move} end+${touchStats.end - b.end} cancel+${touchStats.cancel - b.cancel} | 累计 end=${touchStats.end}`);
}
async function ctrlSwipe(x, y1, y2, label, steps = 14) {
  log('user', `>>> 控制页拖动 (${x},${y1})→(${x},${y2}) ${label}`);
  const b = JSON.parse(JSON.stringify(touchStats));
  await chromeB.call('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y: y1, id: 1 }] }, sessionB);
  for (let i = 1; i <= steps; i++) {
    const yy = y1 + (y2 - y1) * i / steps;
    await chromeB.call('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x, y: yy, id: 1 }] }, sessionB);
    await sleep(33);
  }
  await chromeB.call('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] }, sessionB);
  await sleep(500);
  log('user', `    引擎收到: start+${touchStats.start - b.start} move+${touchStats.move - b.move} end+${touchStats.end - b.end} | 累计 end=${touchStats.end}`);
}

// ---------- 云机页面状态探测 ----------
async function cloudState() {
  if (PAGE_URL.includes('repro_page')) {
    const r = await chromeA.eval('JSON.stringify(window.__ev)', sessionA);
    return r.value;
  }
  // 139 真实页：hash + 标题 + 首屏关键 class
  const r = await chromeA.eval(`JSON.stringify({h:location.hash,t:document.title,y:Math.round(window.scrollY)})`, sessionA);
  return r.value;
}

// ---------- util ----------
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function waitPort(port) {
  for (let i = 0; i < 100; i++) {
    try { await fetch(`http://127.0.0.1:${port}/json/version`); return; } catch (e) { await sleep(100); }
  }
  throw new Error('CDP port timeout ' + port);
}
async function waitLoad(cdp, session) {
  for (let i = 0; i < 80; i++) {
    const r = await cdp.eval('document.readyState', session);
    if (r.value === 'complete') return true;
    await sleep(200);
  }
  return false;
}

// ---------- 主流程 ----------
(async () => {
  // 本地测试页服务（139 不可达时的兜底目标）
  const psrv = http.createServer((req, res) => {
    if (req.url.includes('repro_page')) {
      res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
      res.end(fs.readFileSync(path.join(__dirname, 'repro_page.html')));
    } else { res.writeHead(404); res.end(); }
  });
  await new Promise((r) => psrv.listen(8932, '127.0.0.1', r));

  await startEngineServer();
  log('sys', `mini 引擎 http://127.0.0.1:${ENG_PORT}/ 就绪`);
  await startCloudChrome();
  await startControlChrome();
  await sleep(2000); // 控制页初始化（poll healthz、连流）

  // 控制页状态
  const pst = await chromeB.eval(`JSON.stringify({live:window.live&&live.mode, hlth:window.HLTH&&HLTH.platform})`, sessionB);
  log('ctrl', `控制页状态: ${pst.value}`);

  log('user', `云机页初始状态: ${await cloudState()}`);

  // ===== 场景 1：轻点（用户主诉「无法点击」）=====
  await ctrlTap(207, 200, 'CARD1');
  await sleep(600);
  log('cloud', `云机页状态: ${await cloudState()}`);
  await ctrlTap(207, 330, 'CARD2');
  await sleep(600);
  log('cloud', `云机页状态: ${await cloudState()}`);

  // ===== 场景 2：拖动（用户主诉「拖动无效」）=====
  await ctrlSwipe(207, 500, 200, '上滑列表');
  await sleep(400);
  log('cloud', `云机页状态: ${await cloudState()}`);

  // ===== 结论 =====
  log('verdict', `引擎累计收到 touch: start=${touchStats.start} move=${touchStats.move} end=${touchStats.end} cancel=${touchStats.cancel} mouse=${touchStats.mouse}`);
  if (touchStats.end === 0) log('verdict', '!!! 复现成功：控制页轻点从未发送 phase=end（与用户日志一致：无「触摸抬起」）');
  else log('verdict', `控制页发出了 ${touchStats.end} 次 end`);

  procA.kill(); procB.kill();
  process.exit(0);
})().catch((e) => { console.error('FATAL', e); try { procA && procA.kill(); procB && procB.kill(); } catch (_) {} process.exit(1); });
