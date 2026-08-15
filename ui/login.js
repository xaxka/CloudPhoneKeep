// 设置窗口（还原原版 login.aardio 交互）：
// 选索引 → 预填该槽位配置 → 进入 = 保存 + 启动窗口
(function () {
  "use strict";

  var invoke;
  try {
    invoke = window.__TAURI__.core.invoke;
  } catch (e) {
    invoke = function () { return Promise.reject("TAURI API 不可用"); };
  }

  // 前端调试日志：把关键交互与调用结果回传 Rust 落盘（logs/cpk-*.log 的 [debug] 行）。
  // 有了它，「点了没反应 / 卡在哪一步」在日志里一眼可见
  function dlog(msg) {
    try { invoke("debug_log", { msg: "[login] " + msg }).catch(function () {}); } catch (e) {}
  }
  dlog("login.js 已加载");
  window.addEventListener("error", function (e) {
    dlog("JS错误：" + (e.message || String(e.error)) + " @" + (e.filename || "") + ":" + (e.lineno || 0));
  });
  window.addEventListener("unhandledrejection", function (e) {
    dlog("未处理的 Promise 拒绝：" + String(e.reason));
  });

  var el = function (id) { return document.getElementById(id); };
  var PRESETS = {
    mobile: { width: 414, height: 896, url: "https://cloudphoneh5.buy.139.com" },
    unicom: { width: 405, height: 720, url: "https://uphone.wo-adv.cn/cloudphone/#/home" }
  };

  // 醒目横幅：错误(红) / 警告(黄)。此前错误只显示在底部小灰字里，用户根本看不见
  function showBanner(id, msg) {
    var b = el(id);
    b.textContent = msg;
    b.hidden = false;
  }
  function hideBanners() {
    el("err").hidden = true;
    el("warn").hidden = true;
  }

  function refreshHint() {
    invoke("get_running")
      .then(function (slots) {
        if (slots && slots.length) {
          var names = slots.map(function (n) { return "帐号" + n + "(Ctrl+" + n + ")"; }).join("、");
          el("running-hint").textContent = "运行中：" + names;
        } else {
          el("running-hint").innerHTML = "&nbsp;";
        }
      })
      .catch(function () {});
  }

  function loadSlot(n) {
    invoke("get_slot", { slot: n })
      .then(function (cfg) {
        el("platform").value = PRESETS[cfg.platform] ? cfg.platform : "mobile";
        el("name").value = cfg.name || "";
        el("width").value = cfg.width;
        el("height").value = cfg.height;
        el("cookies").value = cfg.cookies || "";
        // 滚轮滑动默认开；触点光标默认关（老配置里存过 true 的会回显勾选，可手动取消）
        el("wheel").checked = cfg.wheelScroll !== false;
        el("cursor").checked = !!cfg.customCursor;
      })
      .catch(function () {});
  }

  // 平台切换 → 联动默认分辨率（原版两个 exe 各自的默认值）
  el("platform").addEventListener("change", function () {
    var p = PRESETS[this.value];
    if (!p) return;
    el("width").value = p.width;
    el("height").value = p.height;
  });

  el("idx").addEventListener("change", function () {
    var n = parseInt(this.value, 10);
    if (n >= 1 && n <= 9) loadSlot(n);
  });

  el("btn-go").addEventListener("click", launch);
  document.addEventListener("keydown", function (e) {
    if (e.key === "Enter") launch();
  });

  function launch() {
    hideBanners();
    // 还原原版校验：username 为空 → 「缓存数据目录名不能为空」并阻止启动
    if (!el("name").value.trim()) {
      showBanner("err", "缓存数据目录名不能为空");
      el("name").focus();
      return;
    }
    var n = parseInt(el("idx").value, 10);
    if (!(n >= 1 && n <= 9)) {
      showBanner("err", "老板键索引必须是 1~9 的数字");
      el("idx").focus();
      return;
    }
    var platform = el("platform").value;
    var p = PRESETS[platform] || PRESETS.mobile;
    var cfg = {
      slot: n,
      name: el("name").value.trim(),
      platform: platform,
      webUri: p.url,
      cookies: el("cookies").value,
      width: parseFloat(el("width").value) || p.width,
      height: parseFloat(el("height").value) || p.height,
      keepAlive: true,
      intervalMs: 5000,
      simulateActivity: true,
      customCursor: el("cursor").checked,
      wheelScroll: el("wheel").checked,
      blockContextMenu: true
    };
    el("btn-go").disabled = true;
    dlog("点击「进入」：slot=" + cfg.slot + " 目录名=" + cfg.name + " 平台=" + platform +
      " " + cfg.width + "x" + cfg.height + " cookie行数=" + (cfg.cookies ? cfg.cookies.split(/[\n;]/).filter(function (l) { return l.trim(); }).length : 0));
    invoke("launch_slot", { cfg: cfg })
      .then(function (warnings) {
        dlog("launch_slot 成功返回" + (warnings && warnings.length ? "（警告：" + warnings.join("；") + "）" : "（无警告）"));
        // 启动成功：设置窗口由后端隐藏；如有非致命警告（老板键被占用）下次打开时展示
        if (warnings && warnings.length) {
          showBanner("warn", warnings.join("\n"));
        }
      })
      .catch(function (err) {
        dlog("launch_slot 失败：" + String(err));
        showBanner("err", "启动失败：" + String(err));
      })
      .finally(function () {
        el("btn-go").disabled = false;
        refreshHint();
      });
  }

  // 菜单「设置」重新打开时预填指定槽位
  window.__CPK_LOAD__ = function (n) {
    el("idx").value = n;
    loadSlot(n);
  };

  loadSlot(1);
  refreshHint();
  setInterval(refreshHint, 4000);
})();
