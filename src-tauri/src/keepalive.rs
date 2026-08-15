use crate::config::SlotConfig;

/// 内嵌的触点光标 PNG（28×28，青圈白点，热点居中），避免引用任何第三方资源
const CURSOR_PNG_B64: &str = include_str!("../assets/cursor.b64");

/// 生成注入到云手机页面的保活初始化脚本。
///
/// 按槽位配置的 platform 分流（unicom 联通 / mobile 移动）：
/// 联通：试用弹窗(.try-content/.try-btn)、无法连接(.phone-dialog-wrap)、
///       详情页进入云机(.detail-info-container/.enter-intance)、到期(.van-dialog__confirm)、
///       退回首页检测(.title-bar)
/// 移动：解锁区进入云机(.unlocked/.enter-intance)、重连按钮按文字匹配、
///       到期(.van-dialog__confirm)、退回 H5 首页检测(#tabbar)
/// 通用：注入触点光标、屏蔽右键、可选 Cookie 注入、空闲鼠标活动模拟、
///       状态通过 127.0.0.1 回环 HTTP 上报给 Rust 侧（绕过跨域与远程 IPC 限制）
pub fn build_init_script(cfg: &SlotConfig, port: u16) -> String {
    let cookies: Vec<String> = cfg
        .cookies
        .lines()
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
        "keepAlive": cfg.keep_alive,
        "intervalMs": cfg.interval_ms,
        "simulateActivity": cfg.simulate_activity,
        "customCursor": cfg.custom_cursor,
        "blockContextMenu": cfg.block_context_menu,
        "cookies": cookies,
    });

    let cfg_json = serde_json::to_string(&inject).unwrap_or_else(|_| "{}".into());
    let cursor_b64 = CURSOR_PNG_B64.trim();

    format!(
        r#"(function(){{
  if (window.__CPK_INSTALLED__) return;
  window.__CPK_INSTALLED__ = true;
  var CFG = {cfg_json};
  var PORT = CFG.port, SLOT = CFG.slot;

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
      }}).catch(function(){{}});
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

  if (CFG.cookies && CFG.cookies.length) {{
    var applyCookies = function(){{
      for (var i = 0; i < CFG.cookies.length; i++){{
        var line = CFG.cookies[i];
        var eq = line.indexOf('=');
        if (eq < 1) continue;
        var name = line.slice(0, eq).trim();
        var value = line.slice(eq + 1).split(';')[0].trim();
        var kv = name + '=' + value + '; path=/';
        try {{ document.cookie = kv; }} catch(e){{}}
      }}
    }};
    applyCookies();
    setInterval(applyCookies, 60000);
  }}

  var state = {{ ticks: 0, clicks: 0, last: '', diagAt: {{}}, lastUrl: '', wasExited: false }};
  window.__CPK_STATE__ = state;

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

  function tick(){{
    state.ticks++;
    // 路由变化检测（SPA 页面改版定位的第一线索）
    if (state.lastUrl !== location.href) {{
      state.lastUrl = location.href;
      diag('nav', '进入 ' + location.href.slice(0, 300) + ' title=' + (document.title || '').slice(0, 40));
    }}
    if (!CFG.keepAlive) {{ send('paused'); return; }}
    var acted = '';
    var exited = false;
    // 选择器命中摘要：beat 日志核心数据，全 0 = 疑似改版
    var hits = [];
    try {{
      if (CFG.platform === 'mobile') {{
        // ===== 移动云手机（cloudphoneh5.buy.139.com，逻辑忠实还原原作者 aardio 源码）=====
        // .van-dialog__confirm 是万能确认按钮，按按钮文字区分语义：
        //   重连/重新连接→断线重连  进入→超时重连  确认/知道了→到期与提示
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
          if (cfTxt.indexOf('重连') >= 0 || cfTxt.indexOf('重新连接') >= 0) {{
            cf.click(); acted = 'retry'; diag('click', 'retry(confirm) -> ' + desc(cf));
          }} else if (cfTxt.indexOf('进入') >= 0) {{
            cf.click(); acted = 'retry'; diag('click', 're-enter(confirm) -> ' + desc(cf));
          }} else if (cfTxt === '确认' || cfTxt === '知道了') {{
            cf.click(); acted = 'expired-confirm'; diag('click', 'confirm(' + cfTxt + ') -> ' + desc(cf));
          }} else {{
            // 未知文字的确认弹窗：记录下来供改版分析，不盲点
            diag('miss', 'confirm 按钮出现未知文字 "' + cfTxt + '"，未点击');
          }}
        }}

        // 解锁区：原作者直接点击 .unlocked 容器本身（整个区域可点）
        if (!acted && vis(ul) && ((ul.innerText || '').indexOf('进入') >= 0)) {{
          ul.click(); acted = 'enter'; diag('click', 'enter(unlocked) -> ' + desc(ul));
        }}

        // 进入云机按钮
        if (!acted && vis(ei) && ((ei.innerText || '').indexOf('进入云机') >= 0)) {{
          ei.click(); acted = 'enter'; diag('click', 'enter(enter-intance) -> ' + desc(ei));
        }}

        // 检测到 #tabbar = 退回 H5 首页，即云机已退出
        exited = vis(q('#tabbar'));
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
        // 4. 到期/提示弹窗 -> 知道了
        var cf2 = q('.van-dialog__confirm');
        hits.push('van-confirm:' + (vis(cf2) ? 1 : 0), 'title-bar:' + (vis(q('.title-bar')) ? 1 : 0));
        if (vis(cf2)) {{ cf2.click(); if (!acted) acted = 'expired-confirm'; }}
        // 5. 检测到 .title-bar = 退回首页，即云机已退出
        exited = vis(q('.title-bar'));
      }}

      // 状态上报
      if (exited) {{
        if (!state.wasExited) {{
          state.wasExited = true;
          diag('exit', '已退出云机（检测到退回首页特征），DOM 采样: ' + domSample());
        }}
        send('exited');
      }} else {{
        state.wasExited = false;
        if (acted) {{
          state.clicks++;
          send(acted);
        }} else {{
          send('alive');
          // 心跳采样：每 20 tick 记录一次选择器命中全貌
          if (state.ticks % 20 === 1) {{
            diag('beat', 'tick=' + state.ticks + ' url=' + location.pathname.slice(0, 60) +
                 ' platform=' + CFG.platform + ' hits=[' + hits.join(',') + '] 全0即疑似改版');
          }}
        }}
      }}

      // 6. 空闲时模拟轻微鼠标活动，防止会话闲置断开
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

  // 手动诊断入口：前端「DOM 采样」按钮触发，输出当前页面结构快照
  window.__CPK_PROBE__ = function(){{
    diag('probe', '手动采样: ' + domSample());
    tick();
    return 'ok';
  }};

  window.__CPK_TICK__ = tick;
  // 页面内定时器（窗口可见时生效）
  setInterval(function(){{ try {{ tick(); }} catch(e){{}} }}, CFG.intervalMs || 5000);
  send('installed');
  diag('sys', '保活脚本已注入 platform=' + CFG.platform + ' interval=' + (CFG.intervalMs || 5000) + 'ms url=' + location.href.slice(0, 120));
}})();"#,
        cfg_json = cfg_json,
        __CPK_CURSOR__ = cursor_b64
    )
}
