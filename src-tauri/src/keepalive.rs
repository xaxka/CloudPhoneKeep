use crate::config::SlotConfig;

/// 生成注入到云手机页面的保活初始化脚本。
///
/// 与原版 aardio 程序等价的能力：
/// 1. 等待并自动点击「试用 / 立即启用云手机」弹窗（.try-content / .try-btn）
/// 2. 「无法连接」弹窗自动点「再次尝试」（.phone-dialog-wrap）
/// 3. 详情页自动点「进入云机」（.detail-info-container / .enter / .enter-intance）
/// 4. 到期提示自动点「知道了」（.van-dialog__confirm）
/// 5. 检测到 .title-bar（已退回首页=云机退出）→ 上报 exited
/// 6. 注入触点光标、屏蔽右键、可选 Cookie 注入、空闲鼠标活动模拟
/// 7. 状态通过 127.0.0.1 回环 HTTP 上报给 Rust 侧（绕过跨域与远程 IPC 限制）
pub fn build_init_script(cfg: &SlotConfig, port: u16) -> String {
    let cookies: Vec<String> = cfg
        .cookies
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();

    let inject = serde_json::json!({
        "slot": cfg.slot,
        "port": port,
        "keepAlive": cfg.keep_alive,
        "intervalMs": cfg.interval_ms,
        "simulateActivity": cfg.simulate_activity,
        "customCursor": cfg.custom_cursor,
        "blockContextMenu": cfg.block_context_menu,
        "cookies": cookies,
    });

    let cfg_json = serde_json::to_string(&inject).unwrap_or_else(|_| "{}".into());

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

  if (CFG.blockContextMenu) {{
    document.addEventListener('contextmenu', function(e){{ e.preventDefault(); }}, true);
  }}

  if (CFG.customCursor) {{
    try {{
      var st = document.createElement('style');
      st.type = 'text/css';
      st.innerHTML = '*{{cursor:url("https://fs-im-kefu.7moor-fs1.com/ly/4d2c3f00-7d4c-11e5-af15-41bf63ae4ea0/1715189790852/Dotter.cur"),default;}}';
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

  var state = {{ ticks: 0, clicks: 0, last: '' }};
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
    if (!CFG.keepAlive) {{ send('paused'); return; }}
    var acted = '';
    try {{
      // 1. 试用弹窗 -> 立即启用云手机
      if (vis(q('.try-content'))) {{
        var b = q('.nut-popup--center .try-btn') || q('.try-btn') || findBtn(document.body, ['立即启用云手机']);
        if (b) {{ b.click(); acted = 'try-enable'; }}
      }}
      // 2. 无法连接 -> 再次尝试
      var pdw = q('.phone-dialog-wrap');
      if (vis(pdw)) {{
        var rb = findBtn(pdw, ['再次尝试', '重试', '重新连接', '重新载入']);
        if (rb) {{ rb.click(); if (!acted) acted = 'retry'; }}
      }}
      // 3. 详情页 -> 进入云机
      var dic = q('.detail-info-container');
      if (vis(dic)) {{
        var eb = q('.enter-intance') || q('.enter') || findBtn(dic, ['进入云机', '进入', '确认', '重连']);
        if (eb) {{ eb.click(); if (!acted) acted = 'enter'; }}
      }}
      // 4. 到期/提示弹窗 -> 知道了
      var cf = q('.van-dialog__confirm');
      if (vis(cf)) {{ cf.click(); if (!acted) acted = 'expired-confirm'; }}

      // 5. 状态上报
      if (vis(q('.title-bar'))) {{
        send('exited');
      }} else if (acted) {{
        state.clicks++;
        send(acted);
      }} else {{
        send('alive');
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
    }}
  }}

  window.__CPK_TICK__ = tick;
  // 页面内定时器（窗口可见时生效）
  setInterval(function(){{ try {{ tick(); }} catch(e){{}} }}, CFG.intervalMs || 5000);
  send('installed');
}})();"#
    )
}
