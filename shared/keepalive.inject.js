(function(){
  // =====================================================================
  // CloudPhoneKeep 保活脚本 —— Windows / CLI 双平台唯一源文件
  // ---------------------------------------------------------------------
  // 注入方（构建器唯一实现在 shared crate：shared/src/keepalive.rs
  // include_str! 本文件；两端适配层传各自策略参数，只改此处即双端生效）：
  //   Windows: src-tauri/src/keepalive.rs   （WebView2，Tauri 窗口）
  //   CLI:     cli/src/keepalive.rs         （Chromium headless，CDP 驱动）
  // 构建器只做两件事：生成 CFG JSON + 替换下方两个占位符。
  //   __CPK_CFG__     → 配置 JSON：slot/port/platform/homeUri/keepAlive/
  //                     intervalMs/simulateActivity/customCursor/
  //                     blockContextMenu/pageTimer
  //   __CPK_CURSOR__  → 触点光标 PNG base64（Windows 注入真实内嵌资源；
  //                     Linux 无可见光标，恒替换为空串且 customCursor=false）
  //
  // 平台差异全部收敛为 CFG 开关（其余逻辑双端 100% 一致）：
  //  0) CFG.platform 分支：mobile 移动 139 / unicom 联通 / custom 自定义
  //     URL 通用保活（不点弹窗不检测退出，规则明细见 shared/docs/
  //     keepalive-rules.md）；改动平台分支必须配 cli/tests/popup_decide.js 用例
  //  1) CFG.pageTimer：true = 页内 setInterval 1 秒驱动（Windows 窗口可见态；
  //     隐藏/最小化时仍由 Rust 看门狗 eval __CPK_TICK__ 接管）；false = 完全
  //     由宿主看门狗驱动（Linux CDP 恒定态——无头页面永不可见，避免
  //     页内+外部双驱动把 5 秒动作周期缩短一半）。
  //  2) window.__CPK_DRAIN__()：诊断环形缓冲（取走即清空，最多 200 条）。
  //     Linux 宿主每 5 秒经 CDP 取走——headless 内 fetch 回环 /log 上报可能
  //     受混合内容/专用网络访问策略影响，缓冲保证诊断日志任何网络策略下
  //     不丢（/log 上报保留，双保险）；Windows 不调用，纯惰性代码无副作用。
  // =====================================================================
  if (window.__CPK_INSTALLED__) return;
  window.__CPK_INSTALLED__ = true;
  var CFG = __CPK_CFG__;
  var PORT = CFG.port, SLOT = CFG.slot;

  // ===== 统一触摸环境（关键修复：重载后鼠标点不动云机）=====
  // 页面 app.js 的逻辑：'ontouchstart' in window 为 false 时才懒加载自带的
  // 鼠标→触摸 polyfill，且该 chunk 只在部分路由加载。桌面 WebView2 里
  // ontouchstart 通常不存在 → 首页路由由页面 polyfill 负责转换；一旦重新
  // 加载/直达云机路由，polyfill 不在 → 鼠标事件没有任何转换 → 点不动。
  // 这里在页面脚本运行前补齐 ontouchstart，页面从此不再加载自家 polyfill，
  // 鼠标→触摸一律由下方内置模拟器接管：任何路由、任何重载后都生效，
  // 且两个转换器天然互斥，绝不会双重转换。
  try { if (!('ontouchstart' in window)) window.ontouchstart = null; } catch(e){}

  var state = { ticks: 0, clicks: 0, last: '', diagAt: {}, lastUrl: '', wasExited: false, stopDone: false, entered: false, nextActionAt: 0, pdwMiss: 0, cfMiss: 0, updMiss: 0, diagBuf: [] };
  window.__CPK_STATE__ = state;

  // document_start 阶段 body/head 可能尚未解析（初始化脚本在文档创建时执行）：
  // 所有 DOM 挂载必须等就绪后进行，否则 appendChild 抛错会让整个脚本静默死亡
  function whenDom(cb){
    if (document.body || document.documentElement) { cb(); return; }
    var t = setInterval(function(){
      try { if (document.body || document.documentElement) { clearInterval(t); cb(); } } catch(e){}
    }, 10);
    setTimeout(function(){ try{clearInterval(t);}catch(e){} }, 30000);
  }

  function send(status){
    try{
      var qs = 'slot=' + SLOT + '&status=' + encodeURIComponent(status) + '&t=' + Date.now();
      fetch('http://127.0.0.1:' + PORT + '/report?' + qs, { mode: 'no-cors', cache: 'no-store' }).catch(function(){});
    }catch(e){}
  }

  // ===== 诊断日志：POST 到本地 /log，由宿主侧落盘（同内容 5 秒内去重）=====
  // 同时写入页内环形缓冲，Linux 宿主经 CDP 每周期 __CPK_DRAIN__ 取走（Windows 不取，无副作用），
  // 网络策略拦截 /log 时日志依然完整（见文件头注释第 2 条）
  function diag(level, msg){
    try {
      var now = Date.now();
      var key = level + '|' + String(msg).slice(0, 80);
      if (state.diagAt[key] && now - state.diagAt[key] < 5000) return;
      state.diagAt[key] = now;
      try {
        state.diagBuf.push({ t: now, l: level, m: String(msg).slice(0, 2000) });
        if (state.diagBuf.length > 200) state.diagBuf.shift();
      } catch(e2) {}
      var body = 'slot=' + SLOT + '&level=' + encodeURIComponent(level) + '&msg=' + encodeURIComponent(String(msg).slice(0, 3800));
      fetch('http://127.0.0.1:' + PORT + '/log', {
        method: 'POST',
        headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
        body: body, mode: 'no-cors', cache: 'no-store'
      }).catch(function(e){});
    } catch(e){}
  }

  // ===== URL 日志脱敏（与 shared/src/redact.rs 双端同款规则）=====
  // 页面 URL 常带会话凭证（ai-helper-phone 的 token 等），日志会被拿来分享排障，
  // 敏感参数值统一打码 ***；参数名集合与 Rust 侧 redact_url 保持同步。
  var CPK_SENS_KEYS = { token:1, session:1, sess:1, key:1, secret:1, auth:1, ticket:1,
                        password:1, pwd:1, signature:1, sign:1, code:1 };
  function safeUrl(u){
    try {
      var s = String(u), out = '', inQ = false, i = 0;
      while (i < s.length) {
        var c = s.charAt(i);
        if (c === '?' || c === '#') { inQ = true; out += c; i++; continue; }
        if (!inQ) { out += c; i++; continue; }
        var j = i;
        while (j < s.length && s.charAt(j) !== '&' && s.charAt(j) !== '#') j++;
        var p = s.slice(i, j), eq = p.indexOf('=');
        if (eq > 0 && CPK_SENS_KEYS[p.slice(0, eq).toLowerCase()]) out += p.slice(0, eq) + '=***';
        else out += p;
        if (j < s.length) { var sp = s.charAt(j); if (sp === '#') inQ = false; out += sp; i = j + 1; }
        else i = j;
      }
      return out;
    } catch(e) { return ''; }
  }

  // 诊断缓冲（取走即清空，最多 200 条防泄漏；由 Linux 宿主经 CDP 调用）
  window.__CPK_DRAIN__ = function(){
    var b = state.diagBuf;
    state.diagBuf = [];
    return b;
  };

  // 元素描述：tag.class("text")，用于日志中还原点击目标
  function desc(el){
    if (!el) return 'null';
    try {
      var t = el.tagName ? el.tagName.toLowerCase() : '?';
      var c = el.className;
      c = (c && c.baseVal !== undefined) ? c.baseVal : String(c || '');
      c = c.trim().split(/\s+/).slice(0, 4).join('.');
      var txt = (el.innerText || '').trim().slice(0, 20);
      return t + (c ? '.' + c : '') + (txt ? '("' + txt + '")' : '');
    } catch(e) { return 'desc-err'; }
  }

  // DOM 采样：当前页面全部 class 去重清单（改版分析的核心数据）
  function domSample(){
    try {
      var seen = {}, n = 0;
      var els = document.querySelectorAll('*');
      for (var i = 0; i < els.length && i < 4000; i++){
        var c = els[i].className;
        c = (c && c.baseVal !== undefined) ? c.baseVal : String(c || '');
        var parts = c.trim().split(/\s+/);
        for (var j = 0; j < parts.length; j++) if (parts[j]) { seen[parts[j]] = 1; n++; }
      }
      var arr = Object.keys(seen).sort();
      return 'url=' + location.pathname + location.hash +
             ' title=' + (document.title || '').slice(0, 30) +
             ' els=' + Math.min(els.length, 4000) +
             ' classes(' + arr.length + ')= ' + arr.join(' ').slice(0, 3400);
    } catch(e) { return 'sample-err ' + (e && e.message); }
  }

  if (CFG.blockContextMenu) {
    document.addEventListener('contextmenu', function(e){ e.preventDefault(); }, true);
  }

  if (CFG.customCursor) {
    try {
      // 本地内嵌触点光标（无任何第三方网络依赖）
      var st = document.createElement('style');
      st.type = 'text/css';
      st.innerHTML = '*{cursor:url("data:image/png;base64,__CPK_CURSOR__") 13 13, default;}';
      whenDom(function(){ (document.head || document.documentElement).appendChild(st); });
    } catch(e){}
  }

  // ===== 操控模拟（还原 mobile_cloud 可用鼠标操控云手机的体验）=====
  // 云手机 H5 只监听 touch 事件。页面自带的「鼠标→触摸」模拟器（TouchEmulator）
  // 仅在 "ontouchstart" in window 为 false 时才加载（其 app.js 源码：
  // "ontouchstart"in window||加载polyfill chunk）。脚本开头已把 ontouchstart
  // 补齐 → 页面模拟器永不加载，鼠标→触摸一律由这里内置的同款模拟器负责，
  // 两者天然互斥，任何路由、任何重载后都生效。
  // 【Linux】无头模式没有真实鼠标，但宿主经 CDP Input 派发的触摸/按键事件
  // 在内核输入层直达页面（不等价于 DOM mouse 事件），本模拟器保留以维持
  // 与 Windows 版完全相同的页面环境（页面不会加载自带 polyfill）。
  var tsOn = false;
  if ('ontouchstart' in window) {
    tsOn = true;
    var tsEl = null, tsDown = false;
    // —— 防双发（Linux 无头版实测根因）——
    // 宿主经 CDP 派发的真实触摸轻点（touchstart→touchend）后，Chrome 按
    // 规范自动合成鼠标仿真事件（mousedown/mouseup/click）。若不识别这批
    // 「由触摸合成的鼠标事件」，下方 mousedown→touchstart 转换会把它们
    // 再转一轮触摸 → 页面每次轻点收到两套 touchstart/touchend，开/关型
    // 操作被二次触发相互抵消，表现为「点击没反应」（Windows 版输入源是
    // 鼠标、单次转换无此问题；Linux 版输入源是真实触摸，必须防双发）。
    // 识别：真实（isTrusted）touchend/touchcancel 在 window 捕获层记时；
    // 其后 80ms 内的 mousedown/mouseup 即该次轻点的鼠标仿真事件，跳过转换。
    // isTrusted 同时把本模拟器自派的合成 touchend 排除在外（否则真实鼠标
    // 快速连击会被误判跳过）。
    var tsLastRealEnd = 0;
    window.addEventListener('touchend', function(ev){
      if (ev.isTrusted) tsLastRealEnd = Date.now();
    }, true);
    window.addEventListener('touchcancel', function(ev){
      if (ev.isTrusted) tsLastRealEnd = Date.now();
    }, true);
    function tsFromTouchSynth(){
      return tsLastRealEnd && (Date.now() - tsLastRealEnd) < 80;
    }
    // —— 触摸轻点 → 合成 click 兜底（对齐 Windows 完整 Chrome 行为）——
    // Windows 完整 Chrome：CDP 触摸轻点后 Chrome 自动合成 mousedown/mouseup/
    // click（监听 click 的 H5 按钮由此响应；「去登陆/秒开」即此类）。chrome-
    // headless-shell 没有这条 touch→mouse 合成链：页面只收到 touchstart/
    // touchend，监听 click 的按钮全部无反应（轻点无反馈的成因）。这里补齐：
    // 真实轻点的 touchend 后 90ms 内没有 mousedown 到达（= 无合成链）→ 对
    // 落点元素派发合成 mousedown/mouseup/click；有合成链的环境自动跳过
    // （天然防双发）。移动浏览器每个 tap 本就 touch+mouse 双套事件，页面
    // （Vue @click 与手势库并存）必须且已处理好双流，与真实手机一致。
    var tkStart = null, tkMouseSeen = false, tkSynthing = false;
    window.addEventListener('mousedown', function(ev){
      if (ev.isTrusted) tkMouseSeen = true;  // Chrome 自己合成的 mouse（若存在）
    }, true);
    window.addEventListener('touchstart', function(ev){
      if (!ev.isTrusted) return;
      if (ev.touches.length > 1) { tkStart = null; return; }  // 多指手势不合成
      var t = ev.touches[0];
      tkStart = { x: t.clientX, y: t.clientY, at: Date.now() };
    }, true);
    window.addEventListener('touchend', function(ev){
      if (!ev.isTrusted || !tkStart) return;
      var st = tkStart; tkStart = null;
      var t = ev.changedTouches && ev.changedTouches[0];
      if (!t) return;
      var dt = Date.now() - st.at;
      var dist = Math.abs(t.clientX - st.x) + Math.abs(t.clientY - st.y);
      if (dt > 600 || dist > 30) return;  // 拖动/长按不是轻点
      tkMouseSeen = false;  // touchend 之后到达的 mousedown 才算合成链
      setTimeout(function(){
        try {
          if (tkMouseSeen || tkSynthing) return;  // 合成链在（或正在补）→ 跳过
          var el = document.elementFromPoint(st.x, st.y);
          if (!el || tsSkip(el)) return;
          function mk(type, buttons){
            return new MouseEvent(type, { bubbles: true, cancelable: true, view: window,
              clientX: st.x, clientY: st.y, screenX: st.x, screenY: st.y,
              button: 0, buttons: buttons, detail: type === 'click' ? 1 : 0 });
          }
          tkSynthing = true;
          try {
            el.dispatchEvent(mk('mousedown', 1));
            el.dispatchEvent(mk('mouseup', 0));
            el.dispatchEvent(mk('click', 0));
          } finally { tkSynthing = false; }
        } catch(e) {}
      }, 90);
    }, true);
    // 豁免区保持原生鼠标行为：页面约定的 [data-no-touch-simulate] +
    // 表单/可编辑元素（输入框需要原生焦点与选字）
    function tsSkip(el){
      try {
        return !!(el && el.closest && el.closest('[data-no-touch-simulate], input, textarea, select, [contenteditable]'));
      } catch(e) { return false; }
    }
    function tsTouch(me){
      try {
        return new Touch({ identifier: 1, target: tsEl, clientX: me.clientX, clientY: me.clientY,
          screenX: me.screenX || me.clientX, screenY: me.screenY || me.clientY,
          pageX: me.pageX, pageY: me.pageY, radiusX: 1, radiusY: 1, rotationAngle: 0, force: 1 });
      } catch(e) { return null; }
    }
    // 页面 polyfill 同款 touch list（带 item() 方法）
    function tsList(me, ended){
      var l = [];
      if (!ended) { var t = tsTouch(me); if (t) l.push(t); }
      l.item = function(i){ return this[i] || null; };
      return l;
    }
    function tsFire(type, me){
      if (!tsEl || !tsEl.dispatchEvent) return;
      var ended = (type === 'touchend' || type === 'touchcancel');
      // 标准语义：changedTouches 始终含该触点（end 时 = 被移除的那个，
      // 页面靠 e.changedTouches[0] 判定点击/滑动落点）；touches/targetTouches
      // 在 end 后才为空列表
      var changed = tsList(me, false);
      var live = ended ? tsList(me, true) : changed;
      var ev;
      try { ev = new TouchEvent(type, { bubbles: true, cancelable: true }); }
      catch(e) {
        try { ev = document.createEvent('Event'); ev.initEvent(type, true, true); } catch(e2) { return; }
      }
      try {
        Object.defineProperty(ev, 'touches', { value: live });
        Object.defineProperty(ev, 'targetTouches', { value: live });
        Object.defineProperty(ev, 'changedTouches', { value: changed });
      } catch(e) {}
      tsEl.dispatchEvent(ev);
    }
    function tsHandler(type){
      return function(me){
        try {
          // 触摸轻点合成的鼠标事件：不再转回触摸（防双发，见上方说明）
          if (tsFromTouchSynth()) return;
          // 本脚本合成 click 兜底的派发中：不转触摸（否则双套 touchstart）
          if (tkSynthing) return;
          if (me.button !== undefined && me.button !== 0) return; // 只处理左键
          if (me.type === 'mousedown') tsDown = true;
          if (me.type === 'mouseup') tsDown = false;
          if (me.type === 'mousemove' && !tsDown) return;
          // 与页面 polyfill 一致：以按下时的目标元素为派发目标，全程不换
          if (me.type === 'mousedown' || !tsEl || !tsEl.dispatchEvent) tsEl = me.target;
          if (!tsSkip(tsEl)) {
            tsFire(type, me);
            // 模拟触摸按下时禁掉原生拖选文本/图片（否则「拖动全是复制文本」）
            if (me.type === 'mousedown') me.preventDefault();
          }
          if (me.type === 'mouseup') tsEl = null;
        } catch(e) {}
      };
    }
    window.addEventListener('mousedown', tsHandler('touchstart'), true);
    window.addEventListener('mousemove', tsHandler('touchmove'), true);
    window.addEventListener('mouseup', tsHandler('touchend'), true);
    // 拖动中彻底禁止选字/拖拽（触摸语义）
    document.addEventListener('selectstart', function(e){ if (tsDown) e.preventDefault(); }, true);
    document.addEventListener('dragstart', function(e){ if (tsDown) e.preventDefault(); }, true);
    // 复位保护：在页面外松开鼠标收不到 mouseup 时 tsDown 会卡在 true，
    // 之后所有 mousemove 都被当成拖动、点击全部失灵
    window.addEventListener('blur', function(){ tsDown = false; tsEl = null; }, true);
    document.addEventListener('mouseleave', function(){ tsDown = false; tsEl = null; }, true);
  }

  // 地址栏（Ctrl+U 呼出/收起，回车跳转，Esc 关闭）—— 还原原版 address.aardio
  // data-no-touch-simulate：页面触摸模拟器的约定豁免属性，地址栏保持原生鼠标行为
  var addrBar = document.createElement('div');
  addrBar.id = 'cpk-addr-bar';
  addrBar.setAttribute('data-no-touch-simulate', '1');
  addrBar.style.cssText = 'display:none;position:fixed;top:0;left:0;right:0;z-index:2147483647;background:#f5f5f5;border-bottom:1px solid #999;padding:2px 3px;box-sizing:border-box;';
  var addrInput = document.createElement('input');
  addrInput.type = 'text';
  addrInput.placeholder = '输入网址后回车跳转，Esc 关闭';
  addrInput.style.cssText = 'width:100%;height:26px;font-size:14px;border:1px solid #888;padding:0 4px;box-sizing:border-box;outline:none;background:#fff;';
  addrInput.addEventListener('keydown', function(ev){
    // 输入法组词中（如中文候选词确认的回车）不触发跳转/关闭
    if (ev.isComposing || ev.keyCode === 229) return;
    if (ev.key === 'Enter'){
      var u = addrInput.value.trim();
      if (u) { try { location.href = u; } catch(e){} }
      addrBar.style.display = 'none';
    } else if (ev.key === 'Escape'){
      addrBar.style.display = 'none';
    }
    ev.stopPropagation();
  });
  addrBar.appendChild(addrInput);
  whenDom(function(){ (document.body || document.documentElement).appendChild(addrBar); });
  window.__CPK_ADDR__ = function(on){
    addrBar.style.display = on ? 'block' : 'none';
    if (on) { addrInput.value = location.href; addrInput.focus(); addrInput.select(); }
  };

  function vis(el){ return !!(el && (el.offsetWidth || el.offsetHeight || el.getClientRects().length)); }
  function q(s){ try { return document.querySelector(s); } catch(e) { return null; } }

  // ===== 上下文识别（修正心跳误报「疑似改版」）=====
  // 云机页面把手机画面嵌在跨域 iframe（yun.139.com/ai-helper-phone）里，
  // 注入脚本在每个文档都会执行，但保活选择器（.van-dialog__confirm/#tabbar 等）
  // 属于顶层页面 DOM，iframe 里 querySelector 永远查不到 → 心跳恒全 0，属正常；
  // 同理 #/instance 云机内路由本身就没有 tabbar/解锁区/弹窗，全 0 也是正常态。
  var IS_FRAME = false;
  try { IS_FRAME = (window.top !== window); } catch(e) { IS_FRAME = true; }
  function routeOf(){ try { return (location.hash || '').split('?')[0]; } catch(e) { return ''; } }
  function inPhoneRoute(){ var r = routeOf(); return r.indexOf('/instance') >= 0 || (CFG.platform === 'unicom' && r.indexOf('/phone') >= 0); }
  function onHomeRoute(){ return routeOf().indexOf('/cloudAppList') >= 0; }
  // 每个路由首次心跳时落一份 DOM class 清单：真改版时日志里直接有证据可对照换选择器
  var sampledRoutes = {};
  function routeSampleOnce(){
    var key = IS_FRAME ? 'frame:' + location.pathname : routeOf();
    if (sampledRoutes[key]) return '';
    sampledRoutes[key] = 1;
    return ' 首见采样: ' + domSample().slice(0, 900);
  }

  function findBtn(root, texts){
    try {
      var els = root.querySelectorAll('button, [class*=btn], [class*=Btn], [role=button], div, span');
      for (var i = 0; i < els.length; i++){
        var t = (els[i].innerText || '').trim();
        for (var j = 0; j < texts.length; j++){ if (t === texts[j] && vis(els[i])) return els[i]; }
      }
    } catch(e){}
    return null;
  }

  // ===== v1.9.0 断连弹窗匹配强化（背景：2026-08-22 联通断连弹窗按钮文案与已知词
  // 不再全等，findBtn 精确匹配连续 miss 15 分钟保活失效，见 cpk-20260822.log）=====

  // 宽松包含匹配：只认按钮类元素（button/[role=button]/class 含 btn）且文本很短，
  // 避免命中含「连接」「重试」字样的整段提示文字（消息 div 文本长度必然超限）
  function findBtnLoose(root, texts){
    try {
      var els = root.querySelectorAll('button, [role=button], [class*=btn], [class*=Btn]');
      for (var i = 0; i < els.length; i++){
        var t = (els[i].innerText || '').trim();
        if (!t || t.length > 12) continue;
        for (var j = 0; j < texts.length; j++){ if (t.indexOf(texts[j]) >= 0 && vis(els[i])) return els[i]; }
      }
    } catch(e){}
    return null;
  }

  // 弹窗内按钮清单：miss 诊断核心数据（旧 miss 日志只截 20 字符，看不到弹窗里有什么按钮）
  function btnTexts(root){
    try {
      var els = root.querySelectorAll('button, [role=button], [class*=btn], [class*=Btn]');
      var out = [];
      for (var i = 0; i < els.length && out.length < 8; i++){ if (vis(els[i])) out.push(desc(els[i])); }
      return out.length ? out.join(' | ') : '(无按钮类元素)';
    } catch(e) { return 'btnTexts-err'; }
  }

  // 兜底点击目标：弹窗内任意可见、文本很短、且不含退出/取消类字样的按钮
  var NEG_WORDS = ['退出', '取消', '关闭', '返回', '注销', '删除'];
  function safeBtnIn(root){
    try {
      var els = root.querySelectorAll('button, [role=button], [class*=btn], [class*=Btn]');
      for (var i = 0; i < els.length; i++){
        var el = els[i], t = (el.innerText || '').trim();
        if (!t || t.length > 12 || !vis(el)) continue;
        var neg = false;
        for (var k = 0; k < NEG_WORDS.length; k++){ if (t.indexOf(NEG_WORDS[k]) >= 0) { neg = true; break; } }
        if (!neg) return el;
      }
    } catch(e){}
    return null;
  }

  // ===== 被踢退出自动重进（v1.12.1）=====
  // 退出检测（#tabbar / .title-bar）此前只上报 exited + 系统通知就停用保活，
  // 人不在场手机就此离线到天亮。退出多为被踢/会话抖动，登录态在本地数据目录，
  // 整页重载后站点自动重进云机（cpk-20261004/05.log 多次实证：cloudAppList→
  // cloudphone→instance / restoreData→restoreEnter），重进即恢复。
  // 防死循环：跨文档计数走 localStorage（同域持久），1 小时窗口内最多 3 次，
  // 连续失败第 3 次后放弃并留痕（疑似会话真失效，等人工登录，通知仍照发）；
  // 存活清零：非退出态稳定 60 个动作周期（约 5 分钟）计数归零。
  var CPK_REENTRY_KEY = 'cpk_reentry_v1';
  function reenterReset(){
    try { localStorage.setItem(CPK_REENTRY_KEY, JSON.stringify({ n: 0, t: Date.now() })); } catch(e){}
  }
  function tryAutoReenter(reason){
    try {
      var now = Date.now(), rec = null;
      try { rec = JSON.parse(localStorage.getItem(CPK_REENTRY_KEY) || 'null'); } catch(e){}
      if (!rec || typeof rec.n !== 'number' || now - (rec.t || 0) > 3600e3) rec = { n: 0, t: now };
      if (rec.n >= 3) {
        diag('sys', '自动重进已达上限（1 小时内 ' + rec.n + ' 次），暂停自动重进——疑似会话失效，需人工重新登录；系统通知已发');
        return;
      }
      rec.n += 1; rec.t = now;
      try { localStorage.setItem(CPK_REENTRY_KEY, JSON.stringify(rec)); } catch(e2){}
      diag('sys', '退出云机自动重进 ' + rec.n + '/3（' + reason + '）：整页重载——登录态在本地数据目录，重载后站点自动回云机');
      // 延迟 800ms 再重载：exited 上报与日志的回环 fetch 先发出，避免被 reload 取消
      setTimeout(function(){
        try { location.reload(); return; } catch(e3){}
        try { location.href = CFG.homeUri; } catch(e4){}
      }, 800);
    } catch(e) {}
  }

  // ===== stopTimer 语义（原版 1000ms）：退出/到期检测 =====
  // 还原原版：任一分支触发后 topTimerStatus=true，stopTimer 停用（本会话内只检测一次）
  function stopCheck(){
    if (state.stopDone) return;
    if (CFG.platform === 'mobile') {
      // 路由登记：进入过 #/instance 才算「进过云机」，路由级退出检测以此为准，
      // 避免窗口启动时本来就停在首页被误判成「已退出」
      if (inPhoneRoute()) state.entered = true;
      var tb = q('#tabbar');
      // 退出检测双保险：#tabbar 首页特征（原版）+ 路由从云机回到首页
      // （站点改版已去掉 #tabbar，实测首页心跳 tabbar 恒 0，仅靠原选择器检测不到退出）
      if (vis(tb) || (state.entered && onHomeRoute())) {
        state.stopDone = true; state.wasExited = true;
        diag('exit', '已退出云机（' + (vis(tb) ? '检测到 #tabbar 首页特征' : '路由从云机回到首页') + '），DOM 采样: ' + domSample());
        send('exited');
        // 被/疑似被踢：整页重载自动重进（1 小时 3 次上限，见 tryAutoReenter）
        tryAutoReenter(vis(tb) ? '#tabbar 首页特征' : '路由从云机回到首页');
        return;
      }
      var cf = q('.van-dialog__confirm');
      var cfTxt = vis(cf) ? ((cf.innerText || '').trim()) : '';
      if (cfTxt && cfTxt.indexOf('知道了') >= 0) {
        state.stopDone = true;
        cf.click();
        state.clicks++;
        diag('click', 'expired(知道了) -> ' + desc(cf));
        send('expired');
      }
    } else if (CFG.platform === 'unicom') {
      if (vis(q('.title-bar'))) {
        state.stopDone = true; state.wasExited = true;
        diag('exit', '已退出云机（检测到 .title-bar 首页特征），DOM 采样: ' + domSample());
        send('exited');
        // 被/疑似被踢：整页重载自动重进（1 小时 3 次上限，见 tryAutoReenter）
        tryAutoReenter('.title-bar 首页特征');
        return;
      }
      var cf2 = q('.van-dialog__confirm');
      if (vis(cf2)) {
        state.stopDone = true;
        cf2.click();
        state.clicks++;
        diag('click', 'expired(confirm) -> ' + desc(cf2));
        send('expired');
      }
    }
    // custom（自定义 URL 通用保活）：无退出/到期检测——退出与到期特征是
    // 移动/联通站点特定的（#tabbar / .title-bar / 「知道了」），对任意 URL
    // 无从判定，宁可不检测也不误报误点；页面是否健康由宿主看门狗
    // （心跳超时）与 CLI 引擎 chrome-error 检测兜底
  }

  // ===== runTimer 语义（原版 5000ms）：保活动作 =====
  function actionTick(){
    state.ticks++;
    if (!CFG.keepAlive) { send('paused'); return; }
    // 自动重进计数存活清零：非退出态稳定 60 拍（约 5 分钟）说明重进已成功/本来健康
    if (state.ticks % 60 === 0 && !state.wasExited) reenterReset();
    var acted = '';
    var exitedNow = state.wasExited;
    // 选择器命中摘要：beat 日志核心数据（全 0 是否异常由上下文决定，见下方心跳块）
    var hits = [];
    try {
      if (CFG.platform === 'mobile') {
        // ===== 移动云手机（cloudphoneh5.buy.139.com，逻辑忠实还原原作者 aardio 源码）=====
        // .van-dialog__confirm 万能确认按钮，按文字包含匹配（还原原版 string.keywords）：
        //   含 重连 → 断线重连；含 进入 → 超时重进；含 确认 → 到期/提示确认
        var cf = q('.van-dialog__confirm');
        var cfTxt = vis(cf) ? ((cf.innerText || '').trim()) : '';
        var ul = q('.unlocked');
        var ei = q('.enter-intance');
        hits.push(
          'confirm(' + (cfTxt ? cfTxt.slice(0, 6) : '-') + '):' + (cfTxt ? 1 : 0),
          'unlocked:' + (vis(ul) ? 1 : 0),
          'enter-intance:' + (vis(ei) ? 1 : 0),
          'tabbar:' + (vis(q('#tabbar')) ? 1 : 0)
        );

        if (cfTxt) {
          if (cfTxt.indexOf('重连') >= 0) {
            cf.click(); acted = 'retry'; diag('click', 'retry(confirm) -> ' + desc(cf));
          } else if (cfTxt.indexOf('进入') >= 0) {
            cf.click(); acted = 'retry'; diag('click', 're-enter(confirm) -> ' + desc(cf));
          } else if (cfTxt.indexOf('确认') >= 0) {
            cf.click(); acted = 'confirm'; diag('click', 'confirm -> ' + desc(cf));
          } else {
            var dg = q('.van-dialog') || cf;
            var dgTxt = ((dg.innerText || '').trim().slice(0, 400)).replace(/\s+/g, ' ');
            // ===== 已知场景：云机更新/维护弹窗（见 cpk-20261005.log 08:13）=====
            // 「云机更新中，请稍后再试」+ 唯一按钮「返回首页」。按钮不能点：点击是
            // SPA 路由跳回首页（非整页加载，不走 restoreEnter 自动重进），退出检测
            // 会判定「已退出云机」并停用保活，手机就此挂机。正确动作是整页重载——
            // 实测重载后站点自动重进云机（cloudAppList→cloudphone→instance，
            // 登录态在本地数据目录）：更新完成即恢复；未完成弹窗复现，60 秒节流
            // 再重载，直至恢复。匹配词取弹窗正文（更新/维护/稍后再试），按钮文字
            // 「返回首页」不含重连/进入/确认，天然只会落到这里。
            if (dgTxt.indexOf('更新') >= 0 || dgTxt.indexOf('维护') >= 0 || dgTxt.indexOf('稍后再试') >= 0) {
              state.updMiss++;
              // 只在首见与触发重载时留痕：持续期每 5 秒一条重复 miss 无信息量
              if (state.updMiss === 1) {
                diag('miss', '云机更新/维护弹窗(首见)（不点按钮"' + cfTxt +
                     '"——点击会退回首页停用保活，60 秒后整页重载重试） | 弹窗全文="' + dgTxt + '"');
              }
              if (state.updMiss >= 12) {
                state.updMiss = 0;
                diag('sys', '云机更新/维护持续 60 秒，自动重载页面重试（重载后站点自动重进云机） ' + safeUrl(location.href).slice(0, 120));
                try { location.reload(); } catch(e) {}
              }
            } else {
              // 未知文字的确认弹窗：不盲点（还原原版决策），但补齐 v1.9.0 联通同款
              // 诊断与分级兜底——miss 附弹窗全文与按钮清单（改版最直接证据），
              // 持续 3 分钟未识别自动整页重载（登录态在本地数据目录，重载自动回云机页）
              state.cfMiss++;
              diag('miss', 'confirm 按钮出现未知文字 "' + cfTxt + '"(第' + state.cfMiss + '次) | 弹窗全文="' +
                   dgTxt + '" 按钮清单=' + btnTexts(dg));
              if (state.cfMiss >= 36) {
                state.cfMiss = 0;
                diag('sys', '确认弹窗持续 3 分钟未识别（疑似改版），自动重载页面 ' + safeUrl(location.href).slice(0, 120));
                try { location.reload(); } catch(e) {}
              }
            }
          }
        } else {
          // 弹窗已消失：未知弹窗兜底计数复位
          state.cfMiss = 0;
          state.updMiss = 0;
        }

        // 解锁区：原作者直接点击 .unlocked 容器本身（文字含 进入 即可点）
        if (!acted && vis(ul) && ((ul.innerText || '').indexOf('进入') >= 0)) {
          ul.click(); acted = 'enter'; diag('click', 'enter(unlocked) -> ' + desc(ul));
        }

        // 进入云机按钮
        if (!acted && vis(ei) && ((ei.innerText || '').indexOf('进入云机') >= 0)) {
          ei.click(); acted = 'enter'; diag('click', 'enter(enter-intance) -> ' + desc(ei));
        }
      } else if (CFG.platform === 'unicom') {
        // ===== 联通云手机（uphone.wo-adv.cn）=====
        // 1. 试用弹窗 -> 立即启用云手机
        var tc = q('.try-content');
        hits.push('try-content:' + (vis(tc) ? 1 : 0));
        if (vis(tc)) {
          var b = q('.nut-popup--center .try-btn') || q('.try-btn') || findBtn(document.body, ['立即启用云手机']);
          if (b) { b.click(); acted = 'try-enable'; diag('click', 'try-enable -> ' + desc(b)); }
          else diag('miss', '.try-content 可见但未找到 .try-btn / [立即启用云手机] 按钮，疑似改版 | ' + desc(tc));
        }
        // 2. 无法连接 -> 再次尝试（v1.9.0：精确→宽松→确认词匹配 + 按钮清单诊断 + 分级兜底）
        var pdw = q('.phone-dialog-wrap');
        hits.push('phone-dialog:' + (vis(pdw) ? 1 : 0));
        if (vis(pdw)) {
          var RETRY_WORDS = ['再次尝试', '重试', '重新连接', '重新载入', '重新加载', '重新进入'];
          var OK_WORDS = ['确定', '确认', '知道了', '好的'];
          var rb2 = findBtn(pdw, RETRY_WORDS) || findBtnLoose(pdw, RETRY_WORDS)
                 || findBtn(pdw, OK_WORDS) || findBtnLoose(pdw, OK_WORDS);
          if (rb2) {
            rb2.click(); if (!acted) acted = 'retry'; state.pdwMiss = 0;
            diag('click', 'retry -> ' + desc(rb2));
          } else {
            state.pdwMiss++;
            diag('miss', '.phone-dialog-wrap 可见但未找到重试按钮(第' + state.pdwMiss + '次) | 弹窗全文="' +
                 ((pdw.innerText || '').trim().slice(0, 160)).replace(/\s+/g, ' ') +
                 '" 按钮清单=' + btnTexts(pdw));
            if (state.pdwMiss === 12) {
              // 兜底一：60 秒仍无已知按钮 -> 点弹窗内任意非退出类按钮（超时弹窗按钮几乎都是正向动作）
              var fb = safeBtnIn(pdw);
              if (fb) { fb.click(); if (!acted) acted = 'retry'; diag('click', 'retry(兜底任意按钮) -> ' + desc(fb)); }
            } else if (state.pdwMiss >= 36) {
              // 兜底二：3 分钟未恢复 -> 整页重载（登录态在本地数据目录，重载自动回云机页）
              state.pdwMiss = 0;
              diag('sys', '断连弹窗持续 3 分钟未恢复，自动重载页面 ' + safeUrl(location.href).slice(0, 120));
              try { location.reload(); } catch(e) {}
            }
          }
        } else {
          state.pdwMiss = 0;
        }
        // 3. 详情页 -> 进入云机
        var dic = q('.detail-info-container');
        hits.push('detail-info:' + (vis(dic) ? 1 : 0));
        if (vis(dic)) {
          var eb2 = q('.enter-intance') || q('.enter') || findBtn(dic, ['进入云机', '进入', '确认', '重连']);
          if (eb2) { eb2.click(); if (!acted) acted = 'enter'; diag('click', 'enter -> ' + desc(eb2)); }
          else diag('miss', '.detail-info-container 可见但未找到进入按钮，疑似改版 | ' + desc(dic));
        }
        hits.push('title-bar:' + (vis(q('.title-bar')) ? 1 : 0));
      }
      // custom（自定义 URL 通用保活）：不检测不点击任何站点弹窗——任意 URL
      // 的弹窗语义无从判定（哪颗按钮是「重连」哪颗是「退出」），通用版规则
      // 只做与站点无关的保活：心跳上报（下方向宿主证明页面活着）、空闲鼠标
      // 模拟（防空闲会话回收，见下方共用块）、路由 nav 留痕（改版排查线索）、
      // readyState/空白页检查（15 秒档）。页面异常由宿主看门狗与 CLI 引擎
      // chrome-error 检测兜底。站点特定规则将来要加：在本文件加 custom 分支
      // + popup_decide.js 加用例（见 cli/tests/README.md）。

      // 状态上报
      if (exitedNow) {
        send('exited');
      } else if (acted) {
        state.clicks++;
        send(acted);
      } else {
        send('alive');
        // 心跳采样：每 20 次动作周期（约 100 秒）记录一次选择器命中全貌。
        // 「全0即疑似改版」仅对顶层页面的首页/未识别路由成立；iframe（手机画面）
        // 与云机内路由全 0 是标注过的正常态——这两类上下文全 0 时单条无任何诊断
        // 信息，却占日志量 ~95%，降为 360 周期（约 30 分钟）一条存活节拍（仍证明
        // 脚本在跑、tick 在走）；其余上下文（非 0 命中 / 首页全 0 / 未识别路由）
        // 维持 100 秒全量采样一屏不漏，改版预警能力不缩水。360 是 20 的整倍数，
        // 与既有节拍天然对齐；tick=1 的首条心跳两档都会发出（注入即留痕）。
        if (state.ticks % 20 === 1) {
          var all0 = true;
          for (var hi = 0; hi < hits.length; hi++){ if (/:[1-9][0-9]*$/.test(hits[hi])) { all0 = false; break; } }
          // custom 无选择器：hits 恒空属正常（无站点规则可命中），不能按
          // 「全 0 疑似改版」解读——归入 30 分钟存活节拍，verdict 恒空
          var isCustom = CFG.platform === 'custom';
          var ctx = isCustom ? '自定义URL(无选择器,hits恒空属正常)'
                  : (IS_FRAME ? 'iframe手机画面(选择器属外层页面,全0恒正常)'
                  : (inPhoneRoute() ? '云机内(无弹窗无待点按钮,全0正常)'
                  : (onHomeRoute() ? '首页' : ('路由' + (routeOf() || '/') + '(未识别)'))));
          var verdict = (!isCustom && all0 && !IS_FRAME && !inPhoneRoute()) ? ' 全0即疑似改版' : '';
          var quiet = all0 && (isCustom || IS_FRAME || inPhoneRoute());
          if (!quiet || state.ticks % 360 === 1) {
            diag('beat', 'tick=' + state.ticks + ' url=' + (location.pathname + location.hash).slice(0, 90) +
                 ' platform=' + CFG.platform + ' 上下文=' + ctx + ' hits=[' + hits.join(',') + ']' + verdict + routeSampleOnce());
          }
        }
      }

      // 空闲时模拟轻微鼠标活动，防止会话闲置断开
      if (CFG.simulateActivity && !acted) {
        try {
          document.dispatchEvent(new MouseEvent('mousemove', {
            bubbles: true,
            clientX: 10 + Math.random() * (window.innerWidth - 20),
            clientY: 10 + Math.random() * (window.innerHeight - 20)
          }));
        } catch(e){}
      }
      if (acted) state.last = acted;
    } catch(e) {
      send('error');
      diag('error', 'tick 异常: ' + (e && e.message) + ' | ' + String(e && e.stack).slice(0, 200));
    }
  }

  // 窗口 resize 期间暂停 DOM 密集操作：vis() 用 offsetWidth/offsetHeight/getClientRects
  // 会强制同步布局，resize 时 WebView2 持续 reflow，叠加保活脚本每秒多次强制布局造成
  // layout thrashing，把 Chromium resize 固有卡顿放大成明显掉帧。resize 结束 300ms 后
  // 恢复——保活最多暂停零点几秒，不影响效果。窗口拖动移动不触发 resize、不产生 reflow，
  // 强制布局开销极小，故只处理 resize。
  // 【Linux】无头模式无窗口 resize，此段为 Windows 逻辑保留（无副作用）。
  var resizing = false, resizeTimer = null;
  window.addEventListener('resize', function(){
    resizing = true;
    if (resizeTimer) clearTimeout(resizeTimer);
    resizeTimer = setTimeout(function(){ resizing = false; }, 300);
  });

  // ===== 双定时器调度（还原原版 runTimer/stopTimer 周期）=====
  function tick(){
    // 路由变化检测（SPA 页面改版定位的第一线索）——纯 location 读取，不触发布局，resize 期间也保留
    if (state.lastUrl !== location.href) {
      state.lastUrl = location.href;
      diag('nav', '进入 ' + safeUrl(location.href).slice(0, 300) + ' title=' + (document.title || '').slice(0, 40));
    }
    if (resizing) return;  // resize 期间跳过 DOM 强制布局操作（stopCheck/actionTick 的 vis()）
    stopCheck();                                    // 原版 stopTimer：每 1 秒
    // 原版 runTimer：每 intervalMs（5 秒）。旧计数门控（++state.n >= every）
    // 假设 tick 恒 1s；宿主空闲降频（Linux 无观看时 tick 5s）下会把动作周期
    // 拉长 every 倍——改墙钟门控：任意 tick 周期（≤ intervalMs）下动作周期
    // 恒 ≈ intervalMs。页内 setInterval 1s 驱动（Windows 可见态）行为不变。
    var period = CFG.intervalMs || 5000;
    var nowMs = Date.now();
    if (!state.nextActionAt) state.nextActionAt = nowMs + period;
    if (nowMs >= state.nextActionAt) { state.nextActionAt = nowMs + period; actionTick(); }
  }

  // 手动诊断入口：输出当前页面结构快照
  window.__CPK_PROBE__ = function(){
    diag('probe', '手动采样: ' + domSample());
    actionTick();
    return 'ok';
  };

  // ===== 页面加载诊断：白屏/加载失败时给出可见的重试入口，不再让用户对着空白页 =====
  window.addEventListener('error', function(ev){
    // message 为空的是跨域资源加载失败（img/script），无排查价值且量大，跳过
    try { if (!ev.message) return; diag('error', '页面异常: ' + ev.message + ' @' + (ev.filename || '').slice(0, 80) + ':' + (ev.lineno || 0)); } catch(e){}
  }, true);

  function showLoadBar(msg){
    var bar = document.getElementById('cpk-load-bar');
    if (!bar) {
      bar = document.createElement('div');
      bar.id = 'cpk-load-bar';
      bar.style.cssText = 'display:none;position:fixed;left:0;right:0;bottom:0;z-index:2147483647;background:#fff3cd;border-top:1px solid #d39e00;color:#664d03;font:13px/1.6 sans-serif;padding:8px 10px;text-align:center;';
      bar.innerHTML = '<div id="cpk-load-msg" style="margin-bottom:6px;"></div>' +
        '<button id="cpk-load-retry" style="margin:0 6px;padding:4px 14px;cursor:pointer;">重新加载</button>' +
        '<button id="cpk-load-home" style="margin:0 6px;padding:4px 14px;cursor:pointer;">回云手机首页</button>' +
        '<button id="cpk-load-close" style="margin:0 6px;padding:4px 14px;cursor:pointer;">忽略</button>';
      (document.body || document.documentElement).appendChild(bar);
      document.getElementById('cpk-load-retry').onclick = function(){ diag('sys', '用户点击「重新加载」'); try{ location.reload(); }catch(e){} };
      document.getElementById('cpk-load-home').onclick = function(){ diag('sys', '用户点击「回云手机首页」'); try{ location.href = CFG.homeUri; }catch(e){} };
      document.getElementById('cpk-load-close').onclick = function(){ bar.style.display = 'none'; };
    }
    var m = document.getElementById('cpk-load-msg');
    if (m) m.textContent = msg;
    bar.style.display = 'block';
  }

  setTimeout(function(){
    try {
      var rs = document.readyState;
      var kids = document.body ? document.body.children.length : -1;
      if ((rs !== 'complete' && rs !== 'interactive') || kids <= 0) {
        diag('error', '页面未正常加载 readyState=' + rs + ' body子元素=' + kids + ' url=' + safeUrl(location.href).slice(0, 160));
        showLoadBar('页面似乎没有加载出来（空白）。请检查网络后重试：');
      } else {
        diag('sys', '页面加载正常 readyState=' + rs + ' body子元素=' + kids + ' url=' + safeUrl(location.href).slice(0, 120));
      }
    } catch(e) {}
  }, 15000);

  window.__CPK_TICK__ = tick;
  // 页面内定时器：Windows 版窗口可见时由它驱动（1 秒），隐藏时由 Rust 看门狗驱动。
  // Linux 版 CFG.pageTimer=false：无头页面恒由宿主 Rust 看门狗经 CDP 驱动
  // __CPK_TICK__（与 Windows 隐藏态同一通道），避免双驱动把动作周期缩短一半；
  // Windows 传 true，页内驱动保持可用（配合禁用定时器节流亦为双保险）。
  if (CFG.pageTimer !== false) {
    setInterval(function(){ try { tick(); } catch(e){} }, 1000);
  }
  send('installed');
  diag('sys', '保活脚本已注入 platform=' + CFG.platform + ' 动作周期=' + (CFG.intervalMs || 5000) + 'ms 检测周期=宿主tick(墙钟门控)' +
       ' 触点光标=' + (CFG.customCursor ? '开' : '关') +
       ' 鼠标操控模拟=' + (tsOn ? '已安装(ontouchstart存在,页面自带模拟器未加载)' : '未安装(页面自带模拟器生效)') +
       ' 驱动=' + (CFG.pageTimer !== false ? '页内定时器' : '宿主CDP看门狗') +
       ' url=' + safeUrl(location.href).slice(0, 120));
})();
