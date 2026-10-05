#!/usr/bin/env node
/**
 * 保活弹窗决策回归测试（真浏览器端到端，CI smoke 档 4/4）
 * =====================================================
 * 背景：CI 冒烟档用动画页（无 139 弹窗 DOM），弹窗决策规则
 * （重连/进入/确认/知道了/云机更新/未知弹窗/退出自动重进）此前零 CI 保障，
 * 2026-10-05 的「云机更新中」miss 就靠翻生产日志才发现（见 keepalive-rules.md）。
 *
 * 链路完全对齐生产：chrome-headless-shell + Page.addScriptToEvaluateOnNewDocument
 * 注入 shared/keepalive.inject.js（唯一源，与 keepalive.rs 相同的 CFG 占位符替换），
 * fixture 页（popup_page.html）按 hash 复刻 139 弹窗 DOM；intervalMs=500ms
 * 加速节拍（更新弹窗 60 秒兜底 → 12 拍 = 6 秒，可在 CI 里等出来）。
 * 断言走页内 __CPK_DRAIN__ 诊断缓冲 + __CLICKED__ 点击标记 +
 * performance navigation type，不依赖引擎日志。
 *
 * 用例：
 *   reconnect / enterbtn / know  —— 该点的按钮必须点，click 留痕
 *   update                       —— 「云机更新中」不点「返回首页」，miss 留痕，12 拍整页重载
 *   unknown                      —— 未知弹窗不盲点，miss 留痕，不满 36 拍不重载
 *   redact                       —— 注入留痕 URL 的 token= 打码为 ***
 *   hometab                      —— #tabbar 退出检测 → 自动重进 3 次后达上限停止
 *
 * 用法：node cli/tests/popup_decide.js   （CPK_SHELL 指定 chrome-headless-shell）
 */
const { spawn } = require('child_process');
const http = require('http');
const fs = require('fs');
const path = require('path');
const crypto = require('crypto');

const SHELL = process.env.CPK_SHELL || ['/opt/cloudphonekeep/chrome-headless-shell/chrome-headless-shell',
  '/usr/bin/chrome-headless-shell', '/usr/bin/chromium',
  '/usr/bin/chromium-browser', '/usr/bin/google-chrome', '/usr/bin/google-chrome-stable']
  .find(p => fs.existsSync(p)) || (() => {
    console.error('未找到 Chrome：请 export CPK_SHELL=/path/to/chrome-headless-shell'); process.exit(1); })();
const SHARED = path.join(__dirname, '..', '..', 'shared');
const FIXTURE = fs.readFileSync(path.join(__dirname, 'popup_page.html'), 'utf8');
const HTTP_PORT = 8941, CDP_PORT = 9623;
const UA = 'Mozilla/5.0 (Linux; Android 13; Pixel 7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0.0.0 Mobile Safari/537.36';
const FLAGS = [
  '--remote-debugging-address=127.0.0.1', '--remote-allow-origins=*',
  '--window-size=414,896', '--force-device-scale-factor=1', '--hide-scrollbars',
  '--no-first-run', '--no-default-browser-check', '--disable-gpu', '--disable-dev-shm-usage',
  '--disable-crash-reporter', '--disable-background-timer-throttling',
  '--disable-backgrounding-occluded-windows', '--disable-renderer-backgrounding',
  '--no-sandbox', '--disable-setuid-sandbox', '--mute-audio',
];

const sleep = (ms) => new Promise(r => setTimeout(r, ms));
function waitPort(port, timeoutMs = 15000) {
  const t0 = Date.now();
  return new Promise((resolve, reject) => {
    (function probe() {
      fetch(`http://127.0.0.1:${port}/json/version`).then(r => r.ok ? resolve() : retry()).catch(retry);
      function retry() { Date.now() - t0 > timeoutMs ? reject(new Error('chrome 启动超时')) : setTimeout(probe, 200); }
    })();
  });
}

// ---------- 极简 CDP 客户端（裸 WebSocket，同 e2e_repro_fast.js 语义）----------
class Cdp {
  constructor(ws) { this.ws = ws; this.mid = 0; this.pend = new Map(); this.handlers = []; this.closed = false; }
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
      } else if (d.method) for (const h of this.handlers) h(d.method, d.params);
    });
    this.ws.on('close', () => { this.closed = true; });
  }
  on(fn) { this.handlers.push(fn); }
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
    if (r.exceptionDetails) return { exc: r.exceptionDetails.text };
    return { value: r.result && r.result.value };
  }
}
function wsDial(url) {
  return new Promise((resolve, reject) => {
    const u = new URL(url);
    const key = crypto.randomBytes(16).toString('base64');
    const req = http.request({
      hostname: u.hostname, port: u.port, path: u.pathname + u.search,
      headers: { Connection: 'Upgrade', Upgrade: 'websocket', 'Sec-WebSocket-Key': key, 'Sec-WebSocket-Version': 13 },
    });
    req.on('upgrade', (res, socket) => {
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
      resolve({ on: (ev, fn) => { (listeners[ev] = listeners[ev] || []).push(fn); },
                send: (s) => wsWrite(socket, s), close: () => socket.destroy() });
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

// ---------- 测试主体 ----------
let cdp, proc, sessionId;
const BASE = `http://127.0.0.1:${HTTP_PORT}/popup_page.html`;
let failed = 0;

function pass(name) { console.log(`  [PASS] ${name}`); }
function fail(name, why, r) {
  console.error(`  [FAIL] ${name}：${why}`);
  if (r && r.drain && r.drain.length) {
    console.error('    —— drain 摘要（前 8 条）：');
    for (const e of r.drain.slice(0, 8)) console.error(`    [${e.l}] ${String(e.m).slice(0, 110)}`);
  }
  failed = 1;
}

let navSeq = 0;
async function navigate(frag) {
  // 加递增 query 强制文档级导航：Page.navigate 仅 hash 不同时是 fragment 导航，
  // 页面不重新加载、fixture 弹窗不会换（曾致第 2 个用例起全部误测首个弹窗）。
  // frag 可带自有 query（如 ?token=..&x=1#redact），与 r= 用 & 拼接避免双 ?
  navSeq += 1;
  const extra = (frag || '').replace(/^\?/, '&');
  await cdp.call('Page.navigate', { url: `${BASE}?r=${navSeq}${extra}` }, sessionId);
  await sleep(500);
  // 清上一 case 的重进计数与点击标记（新文档后脚本全新初始化）
  await cdp.eval('try{localStorage.clear()}catch(e){}', sessionId);
  await cdp.eval('window.__CLICKED__=[]', sessionId);
}

// 轮询页内状态：累计取走 __CPK_DRAIN__（每 400ms），期间检查谓词
// （clicked 累计跨轮 union：reload 后页面重置不会掩盖中途的误点击）
async function watch(hash, timeoutMs, check) {
  await navigate(hash);
  const drain = [];
  const clickedAll = [];
  const t0 = Date.now();
  let last = { nav: '' };
  while (Date.now() - t0 < timeoutMs) {
    const r = await cdp.eval(
      `(function(){
         var d = (window.__CPK_DRAIN__ && window.__CPK_DRAIN__()) || [];
         var nav = (performance.getEntriesByType && performance.getEntriesByType('navigation')[0]) || {};
         return JSON.stringify({ d: d, clicked: window.__CLICKED__ || [], navType: nav.type || '' });
       })()`, sessionId);
    const s = JSON.parse(r.value || '{}');
    for (const e of (s.d || [])) drain.push(e);
    for (const c of (s.clicked || [])) if (clickedAll.indexOf(c) < 0) clickedAll.push(c);
    last = { nav: s.navType };
    if (check && check(drain, { clicked: clickedAll, nav: s.navType })) return { drain, clicked: clickedAll, ...last, ok: true };
    await sleep(400);
  }
  return { drain, clicked: clickedAll, ...last, ok: false };
}

const D = (drain, lvl, sub) => drain.filter(e => e.l === lvl && String(e.m).indexOf(sub) >= 0);

async function main() {
  // HTTP：fixture 页
  const srv = http.createServer((req, res) => {
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
    res.end(FIXTURE);
  }).listen(HTTP_PORT, '127.0.0.1');

  // Chrome
  const udd = '/tmp/cpk-popup-test';
  fs.rmSync(udd, { recursive: true, force: true });
  proc = spawn(SHELL, [...FLAGS, `--remote-debugging-port=${CDP_PORT}`, `--user-data-dir=${udd}`, 'about:blank'], { stdio: 'ignore' });
  await waitPort(CDP_PORT);
  cdp = await Cdp.connect(CDP_PORT);
  cdp.start();
  const t = await cdp.call('Target.createTarget', { url: 'about:blank' });
  sessionId = (await cdp.call('Target.attachToTarget', { targetId: t.targetId, flatten: true })).sessionId;
  await cdp.call('Page.enable', {}, sessionId);
  await cdp.call('Runtime.enable', {}, sessionId);
  await cdp.call('Emulation.setUserAgentOverride', { userAgent: UA, platform: 'Linux armv8l' }, sessionId);

  // 同生产注入：CFG 占位符替换（intervalMs=500 加速节拍；port=9 不可达，上报走 catch）
  const cfg = { slot: 1, port: 9, platform: 'mobile', homeUri: BASE,
    keepAlive: true, intervalMs: 500, simulateActivity: false, customCursor: false,
    blockContextMenu: false, pageTimer: true };
  let js = fs.readFileSync(path.join(SHARED, 'keepalive.inject.js'), 'utf8');
  // 注意只替换代码占位符（= __CPK_CFG__;）：文件头注释里也有 __CPK_CFG__ 字样，
  // String.replace(字符串) 只换第一次出现——换成注释处会把代码行漏掉（曾致本测试全红）
  js = js.replace(/__CPK_CFG__;/, JSON.stringify(cfg) + ';').replace(/__CPK_CURSOR__/g, '');
  await cdp.call('Page.addScriptToEvaluateOnNewDocument', { source: js, runImmediately: true }, sessionId);
  console.log('chrome 就绪，注入完成（intervalMs=500ms 加速）');

  // —— 1. 重连弹窗：必须点 ——
  {
    const r = await watch('#reconnect', 4000, (d, s) => s.clicked.indexOf('点击重连') >= 0);
    (r.clicked.indexOf('点击重连') >= 0 && D(r.drain, 'click', 'retry(confirm)').length >= 1)
      ? pass('重连弹窗 → 点击「点击重连」+ click 留痕')
      : fail('重连弹窗', `clicked=${JSON.stringify(r.clicked)} drain点击=${D(r.drain, 'click', 'retry').length}`, r);
  }
  // —— 2. 进入弹窗：必须点 ——
  {
    const r = await watch('#enterbtn', 4000, (d, s) => s.clicked.indexOf('进入云机') >= 0);
    (r.clicked.indexOf('进入云机') >= 0 && D(r.drain, 'click', 're-enter(confirm)').length >= 1)
      ? pass('超时重进弹窗 → 点击「进入云机」+ click 留痕')
      : fail('进入弹窗', `clicked=${JSON.stringify(r.clicked)}`);
  }
  // —— 3. 到期弹窗：点「知道了」+ expired ——
  {
    const r = await watch('#know', 4000, (d, s) => s.clicked.indexOf('知道了') >= 0);
    (r.clicked.indexOf('知道了') >= 0 && D(r.drain, 'click', 'expired(知道了)').length >= 1)
      ? pass('到期弹窗 → 点击「知道了」+ expired 留痕')
      : fail('到期弹窗', `clicked=${JSON.stringify(r.clicked)}`);
  }
  // —— 4. 云机更新弹窗：不点按钮，miss 留痕，12 拍整页重载 ——
  {
    const r = await watch('#update', 14000, (d, s) => s.nav === 'reload');
    const missOk = D(r.drain, 'miss', '云机更新/维护弹窗').length >= 1;
    const notClicked = r.clicked.length === 0;
    (r.nav === 'reload' && missOk && notClicked)
      ? pass('云机更新弹窗 → 不点「返回首页」+ miss 留痕 + 60 秒档整页重载（加速为 12×500ms）')
      : fail('云机更新弹窗', `nav=${r.nav} miss=${missOk} clicked=${JSON.stringify(r.clicked)}`);
  }
  // —— 5. 未知弹窗：不盲点，miss 留痕，不满 36 拍不重载 ——
  {
    const r = await watch('#unknown', 4000, null);
    (r.clicked.length === 0 && D(r.drain, 'miss', '未知文字').length >= 1 && r.nav !== 'reload')
      ? pass('未知弹窗 → 不盲点 + miss 留痕 + 未到 3 分钟不重载')
      : fail('未知弹窗', `clicked=${JSON.stringify(r.clicked)} nav=${r.nav}`);
  }
  // —— 6. 注入留痕 URL 脱敏 ——
  {
    const r = await watch('?token=SECRETVALUExyz&x=1#redact', 3000, (d) =>
      d.some(e => String(e.m).indexOf('保活脚本已注入') >= 0));
    const inj = D(r.drain, 'sys', '保活脚本已注入');
    const masked = inj.some(e => String(e.m).indexOf('token=***') >= 0);
    const leaked = inj.some(e => String(e.m).indexOf('SECRETVALUExyz') >= 0);
    (inj.length >= 1 && masked && !leaked)
      ? pass('URL 凭证脱敏 → 注入留痕 token=***，原文不落盘')
      : fail('URL 脱敏', `inj=${inj.length} masked=${masked} leaked=${leaked}`);
  }
  // —— 7. 退出检测 → 自动重进 3 次达上限 ——
  {
    const r = await watch('#hometab', 18000, (d) =>
      d.some(e => e.l === 'sys' && String(e.m).indexOf('自动重进已达上限') >= 0));
    const exited = D(r.drain, 'exit', '已退出云机').length >= 1;
    const reentered = D(r.drain, 'sys', '自动重进 3/3').length >= 1;
    const capped = D(r.drain, 'sys', '自动重进已达上限').length >= 1;
    const ls = await cdp.eval('(function(){try{return JSON.parse(localStorage.getItem("cpk_reentry_v1")||"{}").n}catch(e){return -1}})()', sessionId);
    (r.nav === 'reload' && exited && reentered && capped && ls.value === 3)
      ? pass('退出检测 → 自动重进 3 轮整页重载后达上限停止（localStorage 计数=3）')
      : fail('退出自动重进', `nav=${r.nav} exited=${exited} reentered=${reentered} capped=${capped} n=${ls.value}`);
  }

  // —— 8. custom 平台：通用保活——弹窗不点、不检测退出、心跳正常 ——
  // CFG.platform 注入时定死，custom 用例需独立 Chrome 实例重新注入
  {
    const CDP2 = CDP_PORT + 1;
    const udd2 = '/tmp/cpk-popup-test-custom';
    fs.rmSync(udd2, { recursive: true, force: true });
    const proc2 = spawn(SHELL, [...FLAGS, `--remote-debugging-port=${CDP2}`, `--user-data-dir=${udd2}`, 'about:blank'], { stdio: 'ignore' });
    await waitPort(CDP2);
    const cdp2 = await Cdp.connect(CDP2);
    cdp2.start();
    const t2 = await cdp2.call('Target.createTarget', { url: 'about:blank' });
    const sid2 = (await cdp2.call('Target.attachToTarget', { targetId: t2.targetId, flatten: true })).sessionId;
    await cdp2.call('Page.enable', {}, sid2);
    await cdp2.call('Runtime.enable', {}, sid2);
    await cdp2.call('Emulation.setUserAgentOverride', { userAgent: UA, platform: 'Linux armv8l' }, sid2);
    const cfg2 = { ...cfg, platform: 'custom' };
    // 从原始模板重新读：上面主用例的 js 已完成占位符替换，replace 会落空
    const js2 = fs.readFileSync(path.join(SHARED, 'keepalive.inject.js'), 'utf8')
      .replace(/__CPK_CFG__;/, JSON.stringify(cfg2) + ';').replace(/__CPK_CURSOR__/g, '');
    await cdp2.call('Page.addScriptToEvaluateOnNewDocument', { source: js2, runImmediately: true }, sid2);
    // 页面带 unknown 弹窗 + 首页特征（hometab）：custom 一律不点不检测
    await cdp2.call('Page.navigate', { url: `${BASE}?r=99#hometab&unknown` }, sid2);
    await sleep(3500);
    const ev2 = await cdp2.eval(
      `(function(){
         var d = (window.__CPK_DRAIN__ && window.__CPK_DRAIN__()) || [];
         return JSON.stringify({ d: d, clicked: window.__CLICKED__ || [],
           hasDialog: !!document.querySelector('.van-dialog__confirm'),
           hasTabbar: document.getElementById('tabbar').style.display === 'block' });
       })()`, sid2);
    const s2 = JSON.parse(ev2.value || '{}');
    const noClick = (s2.clicked || []).length === 0;
    const noDetect = !s2.d.some(e => e.l === 'exit' || e.l === 'miss' || String(e.m).indexOf('自动重进') >= 0);
    const alive = s2.d.some(e => e.l === 'sys' && String(e.m).indexOf('保活脚本已注入') >= 0);
    (s2.hasDialog && s2.hasTabbar && noClick && noDetect && alive)
      ? pass('custom 平台 → 通用保活：弹窗/首页特征一律不点不检测，心跳留痕正常')
      : fail('custom 平台', `dialog=${s2.hasDialog} tabbar=${s2.hasTabbar} clicked=${JSON.stringify(s2.clicked)} noDetect=${noDetect} alive=${alive}`,
             { drain: s2.d });
    try { proc2.kill(); } catch (e) {}
  }

  srv.close();
  try { proc.kill(); } catch (e) {}
  console.log(failed ? '\nFAIL: 存在未通过的用例' : '\nPASS: 弹窗决策回归全部通过');
  process.exit(failed);
}

main().catch(e => { console.error('测试脚本异常：', e); try { proc && proc.kill(); } catch (x) {} process.exit(1); });
