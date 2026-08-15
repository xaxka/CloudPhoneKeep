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

  var el = function (id) { return document.getElementById(id); };
  var PRESETS = {
    mobile: { width: 414, height: 896, url: "https://cloudphoneh5.buy.139.com" },
    unicom: { width: 405, height: 720, url: "https://uphone.wo-adv.cn/cloudphone/#/home" }
  };

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
    var n = parseInt(el("idx").value, 10);
    if (!(n >= 1 && n <= 9)) {
      el("running-hint").textContent = "老板键索引必须是 1~9";
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
      screenModel: "vertical",
      keepAlive: true,
      intervalMs: 5000,
      simulateActivity: true,
      customCursor: true,
      blockContextMenu: true
    };
    el("btn-go").disabled = true;
    invoke("launch_slot", { cfg: cfg })
      .then(function () {
        // 启动成功：设置窗口由后端隐藏
      })
      .catch(function (err) {
        el("running-hint").textContent = String(err);
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
