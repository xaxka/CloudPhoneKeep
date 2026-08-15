use crate::config::{self, SlotConfig};

/// 内嵌的触点光标 PNG（28×28，青圈白点，热点居中），避免引用任何第三方资源
const CURSOR_PNG_B64: &str = include_str!("../assets/cursor.b64");

/// 生成注入到云手机页面的保活初始化脚本。
///
/// 定时器结构忠实还原原版 web.aardio 的双定时器：
///   stopTimer 1000ms → 本脚本 stopCheck()：退出检测(#tabbar/.title-bar) + 到期「知道了」
///   runTimer  5000ms → 本脚本 actionTick()：重连/进入/确认弹窗点击 + 解锁区/进入云机
/// （窗口隐藏时由 Rust 看门狗每 1 秒 eval __CPK_TICK__ 驱动，tick 内自行按周期分流）
///
/// 按槽位配置的 platform 分流（unicom 联通 / mobile 移动）：
/// 联通：试用弹窗(.try-content/.try-btn)、无法连接(.phone-dialog-wrap)、
///       详情页进入云机(.detail-info-container/.enter-intance)、到期(.van-dialog__confirm)、
///       退回首页检测(.title-bar)
/// 移动：解锁区进入云机(.unlocked/.enter-intance)、重连/进入/确认按钮按文字包含匹配、
///       到期「知道了」(.van-dialog__confirm)、退回 H5 首页检测(#tabbar)
/// 通用：注入触点光标、屏蔽右键、Cookie 一次性按平台域注入、空闲鼠标活动模拟、
///       状态通过 127.0.0.1 回环 HTTP 上报给 Rust 侧（绕过跨域与远程 IPC 限制）
pub fn build_init_script(cfg: &SlotConfig, port: u16) -> String {
    // 还原原版 string.lines(cookieStr, ";\s*")：同时兼容「a=b; c=d」单行与「a=b」多行
    let cookies: Vec<String> = cfg
        .cookies
        .split([';', '\n'])
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();

    let platform = if cfg.platform.trim().is_empty() {
        "unicom".to_string()
    } else {
        cfg.platform.trim().to_string()
    };

    let inject = serde_json::json!({
        "slot": cfg.slot,
        "port": port,
        "platform": platform,
        "homeUri": cfg.web_uri,
        "keepAlive": cfg.keep_alive,
        "intervalMs": cfg.interval_ms,
        "simulateActivity": cfg.simulate_activity,
        "customCursor": cfg.custom_cursor,
        "blockContextMenu": cfg.block_context_menu,
        "cookies": cookies,
        // 还原原版 CDP Network.setCookies 的 domain 参数（.139.com / .wo-adv.cn）
        "cookieDomain": config::platform_cookie_domain(&platform),
    });

    let cfg_json = serde_json::to_string(&inject).unwrap_or_else(|_| "{}".into());
    let cursor_b64 = CURSOR_PNG_B64.trim();

    format!(
        r#"(function(){{
  if (window.__CPK_INSTALLED__) return;
  window.__CPK_INSTALLED__ = true;
  var CFG = {cfg_json};
  var PORT = CFG.port, SLOT = CFG.slot;

  var state = {{ ticks: 0, clicks: 0, last: '', diagAt: {{}}, lastUrl: '', wasExited: false, stopDone: false, n: 0 }};
  window.__CPK_STATE__ = state;

  function send(status){{
    try{{
      var qs = 'slot=' + SLOT + '&status=' + encodeURIComponent(status) + '&t=' + Date.now();
      fetch('http://127.0.0.1:' + PORT + '/report?' + qs, {{ mode: 'no-cors', cache: 'no-store' }}).catch(function(){{}});
    }}catch(e){{}}
  }}

  // ===== 诊断日志：POST 到本地 /log，由 Rust 侧落盘（同内容 5 秒内去重）=====
  function diag(level, msg){{
    try {{
      var now = Date.now();
      var key = level + '|' + String(msg).slice(0, 80);
      if (state.diagAt[key] && now - state.diagAt[key] < 5000) return;
      state.diagAt[key] = now;
      var body = 'slot=' + SLOT + '&level=' + encodeURIComponent(level) + '&msg=' + encodeURIComponent(String(msg).slice(0, 3800));
      fetch('http://127.0.0.1:' + PORT + '/log', {{
        method: 'POST',
        headers: {{ 'Content-Type': 'application/x-www-form-urlencoded' }},
        body: body, mode: 'no-cors', cache: 'no-store'
      }}).catch(function(e){{}});
    }} catch(e){{}}
  }}

  // 元素描述：tag.class("text")，用于日志中还原点击目标
  function desc(el){{
    if (!el) return 'null';
    try {{
      var t = el.tagName ? el.tagName.toLowerCase() : '?';
      var c = el.className;
      c = (c && c.baseVal !== undefined) ? c.baseVal : String(c || '');
      c = c.trim().split(/\s+/).slice(0, 4).join('.');
      var txt = (el.innerText || '').trim().slice(0, 20);
      return t + (c ? '.' + c : '') + (txt ? '("' + txt + '")' : '');
    }} catch(e) {{ return 'desc-err'; }}
  }}

  // DOM 采样：当前页面全部 class 去重清单（改版分析的核心数据）
  function domSample(){{
    try {{
      var seen = {{}}, n = 0;
      var els = document.querySelectorAll('*');
      for (var i = 0; i < els.length && i < 4000; i++){{
        var c = els[i].className;
        c = (c && c.baseVal !== undefined) ? c.baseVal : String(c || '');
        var parts = c.trim().split(/\s+/);
        for (var j = 0; j < parts.length; j++) if (parts[j]) {{ seen[parts[j]] = 1; n++; }}
      }}
      var arr = Object.keys(seen).sort();
      return 'url=' + location.pathname + location.hash +
             ' title=' + (document.title || '').slice(0, 30) +
             ' els=' + Math.min(els.length, 4000) +
             ' classes(' + arr.length + ')= ' + arr.join(' ').slice(0, 3400);
    }} catch(e) {{ return 'sample-err ' + (e && e.message); }}
  }}

  if (CFG.blockContextMenu) {{
    document.addEventListener('contextmenu', function(e){{ e.preventDefault(); }}, true);
  }}

  if (CFG.customCursor) {{
    try {{
      // 本地内嵌触点光标（无任何第三方网络依赖）
      var st = document.createElement('style');
      st.type = 'text/css';
      st.innerHTML = '*{{cursor:url("data:image/png;base64,{__CPK_CURSOR__}") 14 14, default;}}';
      (document.head || document.documentElement).appendChild(st);
    }} catch(e){{}}
  }}

  // 地址栏（Ctrl+U 呼出/收起，回车跳转，Esc 关闭）—— 还原原版 address.aardio
  var addrBar = document.createElement('div');
  addrBar.id = 'cpk-addr-bar';
  addrBar.style.cssText = 'display:none;position:fixed;top:0;left:0;right:0;z-index:2147483647;background:#f5f5f5;border-bottom:1px solid #999;padding:2px 3px;box-sizing:border-box;';
  var addrInput = document.createElement('input');
  addrInput.type = 'text';
  addrInput.placeholder = '输入网址后回车跳转，Esc 关闭';
  addrInput.style.cssText = 'width:100%;height:26px;font-size:14px;border:1px solid #888;padding:0 4px;box-sizing:border-box;outline:none;background:#fff;';
  addrInput.addEventListener('keydown', function(ev){{
    if (ev.key === 'Enter'){{
      var u = addrInput.value.trim();
      if (u) {{ try {{ location.href = u; }} catch(e){{}} }}
      addrBar.style.display = 'none';
    }} else if (ev.key === 'Escape'){{
      addrBar.style.display = 'none';
    }}
    ev.stopPropagation();
  }});
  addrBar.appendChild(addrInput);
  (document.body || document.documentElement).appendChild(addrBar);
  window.__CPK_ADDR__ = function(on){{
    addrBar.style.display = on ? 'block' : 'none';
    if (on) {{ addrInput.value = location.href; addrInput.focus(); addrInput.select(); }}
  }};

  // ===== Cookie 注入（还原原版：go() 之前经 CDP Network.setCookies 一次性写入，
  // domain=.139.com / .wo-adv.cn）。这里在文档开始时一次性写入，随后刷新一次，
  // 让首个真正加载的请求就携带 Cookie（等价于原版「导航前生效」）。
  // 不做周期性重刷：避免覆盖用户会话中更新的登录态。 =====
  if (CFG.cookies && CFG.cookies.length) {{
    try {{
      var wrote = 0;
      for (var i = 0; i < CFG.cookies.length; i++){{
        var line = CFG.cookies[i];
        var eq = line.indexOf('=');
        if (eq < 1) continue;
        var name = line.slice(0, eq).trim();
        var value = line.slice(eq + 1).split(';')[0].trim();
        document.cookie = name + '=' + value + '; path=/; domain=' + CFG.cookieDomain;
        wrote++;
      }}
      if (wrote && !sessionStorage.getItem('cpk_ck')) {{
        sessionStorage.setItem('cpk_ck', '1');
        diag('sys', 'Cookie 已按域 ' + CFG.cookieDomain + ' 注入 ' + wrote + ' 条，刷新使首个请求即携带');
        location.reload();
        return; // 本文档终止执行，刷新后的文档继续安装保活脚本
      }}
      diag('sys', 'Cookie 注入（本次导航）' + wrote + ' 条 domain=' + CFG.cookieDomain);
    }} catch(e){{}}
  }}

  function vis(el){{ return !!(el && (el.offsetWidth || el.offsetHeight || el.getClientRects().length)); }}
  function q(s){{ try {{ return document.querySelector(s); }} catch(e) {{ return null; }} }}
  function findBtn(root, texts){{
    try {{
      var els = root.querySelectorAll('button, [class*=btn], [class*=Btn], [role=button], div, span');
      for (var i = 0; i < els.length; i++){{
        var t = (els[i].innerText || '').trim();
        for (var j = 0; j < texts.length; j++){{ if (t === texts[j] && vis(els[i])) return els[i]; }}
      }}
    }} catch(e){{}}
    return null;
  }}

  // ===== stopTimer 语义（原版 1000ms）：退出/到期检测 =====
  // 还原原版：任一分支触发后 topTimerStatus=true，stopTimer 停用（本会话内只检测一次）
  function stopCheck(){{
    if (state.stopDone) return;
    if (CFG.platform === 'mobile') {{
      var tb = q('#tabbar');
      if (vis(tb)) {{
        state.stopDone = true; state.wasExited = true;
        diag('exit', '已退出云机（检测到 #tabbar 首页特征），DOM 采样: ' + domSample());
        send('exited');
        return;
      }}
      var cf = q('.van-dialog__confirm');
      var cfTxt = vis(cf) ? ((cf.innerText || '').trim()) : '';
      if (cfTxt && cfTxt.indexOf('知道了') >= 0) {{
        state.stopDone = true;
        cf.click();
        state.clicks++;
        diag('click', 'expired(知道了) -> ' + desc(cf));
        send('expired');
      }}
    }} else {{
      if (vis(q('.title-bar'))) {{
        state.stopDone = true; state.wasExited = true;
        diag('exit', '已退出云机（检测到 .title-bar 首页特征），DOM 采样: ' + domSample());
        send('exited');
        return;
      }}
      var cf2 = q('.van-dialog__confirm');
      if (vis(cf2)) {{
        state.stopDone = true;
        cf2.click();
        state.clicks++;
        diag('click', 'expired(confirm) -> ' + desc(cf2));
        send('expired');
      }}
    }}
  }}

  // ===== runTimer 语义（原版 5000ms）：保活动作 =====
  function actionTick(){{
    state.ticks++;
    if (!CFG.keepAlive) {{ send('paused'); return; }}
    var acted = '';
    var exitedNow = state.wasExited;
    // 选择器命中摘要：beat 日志核心数据，全 0 = 疑似改版
    var hits = [];
    try {{
      if (CFG.platform === 'mobile') {{
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

        if (cfTxt) {{
          if (cfTxt.indexOf('重连') >= 0) {{
            cf.click(); acted = 'retry'; diag('click', 'retry(confirm) -> ' + desc(cf));
          }} else if (cfTxt.indexOf('进入') >= 0) {{
            cf.click(); acted = 'retry'; diag('click', 're-enter(confirm) -> ' + desc(cf));
          }} else if (cfTxt.indexOf('确认') >= 0) {{
            cf.click(); acted = 'confirm'; diag('click', 'confirm -> ' + desc(cf));
          }} else {{
            // 未知文字的确认弹窗：记录下来供改版分析，不盲点
            diag('miss', 'confirm 按钮出现未知文字 "' + cfTxt + '"，未点击');
          }}
        }}

        // 解锁区：原作者直接点击 .unlocked 容器本身（文字含 进入 即可点）
        if (!acted && vis(ul) && ((ul.innerText || '').indexOf('进入') >= 0)) {{
          ul.click(); acted = 'enter'; diag('click', 'enter(unlocked) -> ' + desc(ul));
        }}

        // 进入云机按钮
        if (!acted && vis(ei) && ((ei.innerText || '').indexOf('进入云机') >= 0)) {{
          ei.click(); acted = 'enter'; diag('click', 'enter(enter-intance) -> ' + desc(ei));
        }}
      }} else {{
        // ===== 联通云手机（uphone.wo-adv.cn）=====
        // 1. 试用弹窗 -> 立即启用云手机
        var tc = q('.try-content');
        hits.push('try-content:' + (vis(tc) ? 1 : 0));
        if (vis(tc)) {{
          var b = q('.nut-popup--center .try-btn') || q('.try-btn') || findBtn(document.body, ['立即启用云手机']);
          if (b) {{ b.click(); acted = 'try-enable'; diag('click', 'try-enable -> ' + desc(b)); }}
          else diag('miss', '.try-content 可见但未找到 .try-btn / [立即启用云手机] 按钮，疑似改版 | ' + desc(tc));
        }}
        // 2. 无法连接 -> 再次尝试
        var pdw = q('.phone-dialog-wrap');
        hits.push('phone-dialog:' + (vis(pdw) ? 1 : 0));
        if (vis(pdw)) {{
          var rb2 = findBtn(pdw, ['再次尝试', '重试', '重新连接', '重新载入']);
          if (rb2) {{ rb2.click(); if (!acted) acted = 'retry'; diag('click', 'retry -> ' + desc(rb2)); }}
          else diag('miss', '.phone-dialog-wrap 可见但未找到重试按钮，疑似改版 | ' + desc(pdw));
        }}
        // 3. 详情页 -> 进入云机
        var dic = q('.detail-info-container');
        hits.push('detail-info:' + (vis(dic) ? 1 : 0));
        if (vis(dic)) {{
          var eb2 = q('.enter-intance') || q('.enter') || findBtn(dic, ['进入云机', '进入', '确认', '重连']);
          if (eb2) {{ eb2.click(); if (!acted) acted = 'enter'; diag('click', 'enter -> ' + desc(eb2)); }}
          else diag('miss', '.detail-info-container 可见但未找到进入按钮，疑似改版 | ' + desc(dic));
        }}
        hits.push('title-bar:' + (vis(q('.title-bar')) ? 1 : 0));
      }}

      // 状态上报
      if (exitedNow) {{
        send('exited');
      }} else if (acted) {{
        state.clicks++;
        send(acted);
      }} else {{
        send('alive');
        // 心跳采样：每 20 次动作周期记录一次选择器命中全貌
        if (state.ticks % 20 === 1) {{
          diag('beat', 'tick=' + state.ticks + ' url=' + location.pathname.slice(0, 60) +
               ' platform=' + CFG.platform + ' hits=[' + hits.join(',') + '] 全0即疑似改版');
        }}
      }}

      // 空闲时模拟轻微鼠标活动，防止会话闲置断开
      if (CFG.simulateActivity && !acted) {{
        try {{
          document.dispatchEvent(new MouseEvent('mousemove', {{
            bubbles: true,
            clientX: 10 + Math.random() * (window.innerWidth - 20),
            clientY: 10 + Math.random() * (window.innerHeight - 20)
          }}));
        }} catch(e){{}}
      }}
      if (acted) state.last = acted;
    }} catch(e) {{
      send('error');
      diag('error', 'tick 异常: ' + (e && e.message) + ' | ' + String(e && e.stack).slice(0, 200));
    }}
  }}

  // ===== 双定时器调度（还原原版 runTimer/stopTimer 周期）=====
  function tick(){{
    // 路由变化检测（SPA 页面改版定位的第一线索）
    if (state.lastUrl !== location.href) {{
      state.lastUrl = location.href;
      diag('nav', '进入 ' + location.href.slice(0, 300) + ' title=' + (document.title || '').slice(0, 40));
    }}
    stopCheck();                                    // 原版 stopTimer：每 1 秒
    var every = Math.max(1, Math.round((CFG.intervalMs || 5000) / 1000));
    if (++state.n >= every) {{ state.n = 0; actionTick(); }}  // 原版 runTimer：每 5 秒
  }}

  // 手动诊断入口：输出当前页面结构快照
  window.__CPK_PROBE__ = function(){{
    diag('probe', '手动采样: ' + domSample());
    actionTick();
    return 'ok';
  }};

  // ===== 页面加载诊断：白屏/加载失败时给出可见的重试入口，不再让用户对着空白页 =====
  window.addEventListener('error', function(ev){{
    try {{ diag('error', '页面异常: ' + (ev.message || '') + ' @' + (ev.filename || '').slice(0, 80) + ':' + (ev.lineno || 0)); }} catch(e){{}}
  }}, true);

  function showLoadBar(msg){{
    var bar = document.getElementById('cpk-load-bar');
    if (!bar) {{
      bar = document.createElement('div');
      bar.id = 'cpk-load-bar';
      bar.style.cssText = 'display:none;position:fixed;left:0;right:0;bottom:0;z-index:2147483647;background:#fff3cd;border-top:1px solid #d39e00;color:#664d03;font:13px/1.6 sans-serif;padding:8px 10px;text-align:center;';
      bar.innerHTML = '<div id="cpk-load-msg" style="margin-bottom:6px;"></div>' +
        '<button id="cpk-load-retry" style="margin:0 6px;padding:4px 14px;cursor:pointer;">重新加载</button>' +
        '<button id="cpk-load-home" style="margin:0 6px;padding:4px 14px;cursor:pointer;">回云手机首页</button>' +
        '<button id="cpk-load-close" style="margin:0 6px;padding:4px 14px;cursor:pointer;">忽略</button>';
      (document.body || document.documentElement).appendChild(bar);
      document.getElementById('cpk-load-retry').onclick = function(){{ diag('sys', '用户点击「重新加载」'); try{{ location.reload(); }}catch(e){{}} }};
      document.getElementById('cpk-load-home').onclick = function(){{ diag('sys', '用户点击「回云手机首页」'); try{{ location.href = CFG.homeUri; }}catch(e){{}} }};
      document.getElementById('cpk-load-close').onclick = function(){{ bar.style.display = 'none'; }};
    }}
    var m = document.getElementById('cpk-load-msg');
    if (m) m.textContent = msg;
    bar.style.display = 'block';
  }}

  setTimeout(function(){{
    try {{
      var rs = document.readyState;
      var kids = document.body ? document.body.children.length : -1;
      if ((rs !== 'complete' && rs !== 'interactive') || kids <= 0) {{
        diag('error', '页面未正常加载 readyState=' + rs + ' body子元素=' + kids + ' url=' + location.href.slice(0, 160));
        showLoadBar('页面似乎没有加载出来（空白）。请检查网络后重试：');
      }} else {{
        diag('sys', '页面加载正常 readyState=' + rs + ' body子元素=' + kids + ' url=' + location.href.slice(0, 120));
      }}
    }} catch(e) {{}}
  }}, 15000);

  window.__CPK_TICK__ = tick;
  // 页面内定时器（窗口可见时生效，1 秒驱动；隐藏时由 Rust 看门狗驱动）
  setInterval(function(){{ try {{ tick(); }} catch(e){{}} }}, 1000);
  send('installed');
  diag('sys', '保活脚本已注入 platform=' + CFG.platform + ' 动作周期=' + (CFG.intervalMs || 5000) + 'ms 检测周期=1000ms url=' + location.href.slice(0, 120));
}})();"#,
        cfg_json = cfg_json,
        __CPK_CURSOR__ = cursor_b64
    )
}
