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
    var exited = false;
    try {{
      if (CFG.platform === 'mobile') {{
        // ===== 移动云手机（cloudphoneh5.buy.139.com）=====
        // 1. 详情页解锁区 -> 进入云机
        if (vis(q('.unlocked')) || vis(q('.enter-intance'))) {{
          var eb = q('.enter-intance') || q('.enter') || findBtn(document.body, ['进入云机', '进入']);
          if (eb) {{ eb.click(); acted = 'enter'; }}
        }}
        // 2. 连接断开/重连弹窗 -> 按文字匹配重连按钮
        if (!acted) {{
          var rb = findBtn(document.body, ['重连', '重新连接', '再次尝试', '重试']);
          if (rb) {{ rb.click(); acted = 'retry'; }}
        }}
        // 3. 到期/提示弹窗 -> 知道了
        var cf = q('.van-dialog__confirm');
        if (vis(cf)) {{ cf.click(); if (!acted) acted = 'expired-confirm'; }}
        // 4. 检测到 #tabbar = 退回 H5 首页，即云机已退出
        exited = vis(q('#tabbar'));
      }} else {{
        // ===== 联通云手机（uphone.wo-adv.cn）=====
        // 1. 试用弹窗 -> 立即启用云手机
        if (vis(q('.try-content'))) {{
          var b = q('.nut-popup--center .try-btn') || q('.try-btn') || findBtn(document.body, ['立即启用云手机']);
          if (b) {{ b.click(); acted = 'try-enable'; }}
        }}
        // 2. 无法连接 -> 再次尝试
        var pdw = q('.phone-dialog-wrap');
        if (vis(pdw)) {{
          var rb2 = findBtn(pdw, ['再次尝试', '重试', '重新连接', '重新载入']);
          if (rb2) {{ rb2.click(); if (!acted) acted = 'retry'; }}
        }}
        // 3. 详情页 -> 进入云机
        var dic = q('.detail-info-container');
        if (vis(dic)) {{
          var eb2 = q('.enter-intance') || q('.enter') || findBtn(dic, ['进入云机', '进入', '确认', '重连']);
          if (eb2) {{ eb2.click(); if (!acted) acted = 'enter'; }}
        }}
        // 4. 到期/提示弹窗 -> 知道了
        var cf2 = q('.van-dialog__confirm');
        if (vis(cf2)) {{ cf2.click(); if (!acted) acted = 'expired-confirm'; }}
        // 5. 检测到 .title-bar = 退回首页，即云机已退出
        exited = vis(q('.title-bar'));
      }}

      // 状态上报
      if (exited) {{
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
}})();"#,
        cfg_json = cfg_json,
        __CPK_CURSOR__ = cursor_b64
    )
}
