/* CloudPhoneKeep 管理面板逻辑（原生 JS，无构建依赖） */
"use strict";

const STATUS_TEXT = {
  "未启动": ["未启动", ""],
  "已停止": ["已停止", ""],
  "installed": ["已注入保活", "ok"],
  "alive": ["保活中", "ok"],
  "idle": ["保活中", "ok"],
  "paused": ["已暂停", "warn"],
  "try-enable": ["已点[立即启用]", "ok"],
  "retry": ["已点[再次尝试]", "ok"],
  "enter": ["已点[进入云机]", "ok"],
  "expired-confirm": ["已确认到期弹窗", "warn"],
  "exited": ["已退出云机", "err"],
  "error": ["脚本异常", "err"],
};

const tauri = () => window.__TAURI__;
let config = null;
let states = {};
let dirty = false;

function invoke(cmd, args) {
  const api = tauri();
  if (!api) return Promise.reject(new Error("Tauri API 不可用"));
  return api.core.invoke(cmd, args);
}

function listen(event, handler) {
  const api = tauri();
  if (!api) return;
  api.event.listen(event, (e) => handler(e.payload));
}

/* ---------------- utils ---------------- */

function $(id) { return document.getElementById(id); }

function toast(msg, ms = 2200) {
  const el = $("toast");
  el.textContent = msg;
  el.classList.remove("hidden");
  clearTimeout(el._t);
  el._t = setTimeout(() => el.classList.add("hidden"), ms);
}

function fmtTime(ts) {
  if (!ts) return "--";
  const d = new Date(ts);
  const p = (n) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

function log(tag, msg, cls = "") {
  const body = $("log");
  const line = document.createElement("div");
  line.className = "line";
  line.innerHTML =
    `<span class="time">${fmtTime(Date.now())}</span>` +
    `<span class="tag ${cls}">${tag}</span>` +
    `<span>${String(msg).replace(/</g, "&lt;")}</span>`;
  body.appendChild(line);
  while (body.children.length > 200) body.removeChild(body.firstChild);
  body.scrollTop = body.scrollHeight;
}

function statusInfo(s) {
  return STATUS_TEXT[s] || [s, "warn"];
}

function markDirty() {
  dirty = true;
  $("btn-save").textContent = "保存配置 *";
}

function clearDirty() {
  dirty = false;
  $("btn-save").textContent = "保存配置";
}

/* ---------------- render ---------------- */

function renderSlots() {
  const grid = $("slots");
  grid.innerHTML = "";
  for (const slot of config.slots) {
    grid.appendChild(renderSlotCard(slot));
  }
}

function renderSlotCard(s) {
  const st = states[s.slot] || {};
  const [txt, cls] = statusInfo(st.lastStatus || (st.running ? "alive" : "未启动"));
  const running = !!st.running;

  const card = document.createElement("div");
  card.className = "card slot" + (running ? " running" : "");
  card.dataset.slot = s.slot;

  card.innerHTML = `
    <div class="slot-head">
      <span class="slot-badge">帐号${s.slot}</span>
      <span class="bosskey">Ctrl+${s.slot}</span>
      <span class="status">
        <span class="dot ${cls}"></span>
        <span class="status-text">${txt}</span>
      </span>
    </div>

    <div class="field-grid">
      <div class="field span2">
        <label>缓存目录名 / 帐号标识（建议手机号，各窗口数据隔离）
          <input type="text" data-k="name" value="${escapeAttr(s.name)}" placeholder="帐号${s.slot}" />
        </label>
      </div>
      <div class="field span2">
        <label>浏览器地址
          <input type="text" data-k="webUri" value="${escapeAttr(s.webUri)}" />
        </label>
      </div>
      <div class="field">
        <label>窗口分辨率
          <div class="size-row">
            <input type="number" data-k="width" value="${s.width}" min="280" step="5" />
            <span class="x">×</span>
            <input type="number" data-k="height" value="${s.height}" min="400" step="5" />
          </div>
        </label>
      </div>
      <div class="field">
        <label>屏幕方向
          <select data-k="screenModel">
            <option value="vertical" ${s.screenModel === "vertical" ? "selected" : ""}>竖屏</option>
            <option value="horizontal" ${s.screenModel === "horizontal" ? "selected" : ""}>横屏</option>
          </select>
        </label>
      </div>
    </div>

    <details class="slot-details">
      <summary>保活与高级选项</summary>
      <div class="inner">
        <div class="field">
          <label>指定 Cookie（每行 name=value; 可选 domain=... 供参考，实际以窗口内登录为准）
            <textarea data-k="cookies" rows="3" placeholder="token=xxxx&#10;session=yyyy">${escapeText(s.cookies)}</textarea>
          </label>
        </div>
        <div class="field">
          <label>保活检测间隔（毫秒）
            <input type="number" data-k="intervalMs" value="${s.intervalMs}" min="1000" step="500" />
          </label>
        </div>
        <div class="toggle-row">
          <label><input type="checkbox" data-k="keepAlive" ${s.keepAlive ? "checked" : ""} /> 保活引擎</label>
          <label><input type="checkbox" data-k="enabled" ${s.enabled ? "checked" : ""} /> 启用(自动启动)</label>
          <label><input type="checkbox" data-k="simulateActivity" ${s.simulateActivity ? "checked" : ""} /> 模拟活动防掉线</label>
          <label><input type="checkbox" data-k="customCursor" ${s.customCursor ? "checked" : ""} /> 触点光标</label>
          <label><input type="checkbox" data-k="blockContextMenu" ${s.blockContextMenu ? "checked" : ""} /> 屏蔽右键</label>
        </div>
      </div>
    </details>

    <div class="slot-actions">
      <button class="btn primary" data-act="start">${running ? "显示" : "启动"}</button>
      <button class="btn danger" data-act="stop">停止</button>
      <button class="btn ghost" data-act="toggle">显/隐</button>
      <button class="btn ghost" data-act="topmost">置顶</button>
      <button class="btn ghost" data-act="rotate">旋转</button>
      <button class="btn ghost" data-act="home">首页</button>
    </div>

    <div class="slot-footer">
      <span>自动点击：<span class="clicks">${st.clicks || 0}</span> 次</span>
      <span>最近上报：<span class="last-at">${fmtTime(st.last_at || 0)}</span></span>
    </div>
  `;

  bindCardInputs(card, s);
  bindCardActions(card, s);
  return card;
}

function escapeAttr(v) { return String(v ?? "").replace(/"/g, "&quot;"); }
function escapeText(v) { return String(v ?? "").replace(/</g, "&lt;"); }

function bindCardInputs(card, s) {
  // 配置对象全程使用与 Rust serde camelCase 一致的字段名
  const map = {
    name: "name", webUri: "webUri", width: "width", height: "height",
    screenModel: "screenModel", cookies: "cookies", intervalMs: "intervalMs",
    keepAlive: "keepAlive", enabled: "enabled", simulateActivity: "simulateActivity",
    customCursor: "customCursor", blockContextMenu: "blockContextMenu",
  };
  card.querySelectorAll("[data-k]").forEach((el) => {
    const k = el.dataset.k;
    const field = map[k];
    const evName = el.type === "checkbox" ? "change" : "change";
    el.addEventListener(evName, () => {
      let v;
      if (el.type === "checkbox") v = el.checked;
      else if (el.type === "number") v = Number(el.value) || 0;
      else v = el.value;
      s[field] = v;
      markDirty();
    });
  });
}

function bindCardActions(card, s) {
  const slot = s.slot;
  card.querySelectorAll("[data-act]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const act = btn.dataset.act;
      try {
        if (act === "start") {
          if (dirty) await saveConfig(false);
          await invoke("start_slot", { slot });
        } else if (act === "stop") {
          await invoke("stop_slot", { slot });
        } else if (act === "toggle") {
          await invoke("toggle_slot", { slot });
        } else if (act === "topmost") {
          const st = states[slot] || {};
          const next = !(st.topmost);
          await invoke("topmost_slot", { slot, top: next });
          toast(next ? `帐号${slot} 已置顶` : `帐号${slot} 已取消置顶`);
        } else if (act === "rotate") {
          const r = await invoke("rotate_slot", { slot });
          s.width = r.width; s.height = r.height;
          s.screenModel = s.screenModel === "vertical" ? "horizontal" : "vertical";
          renderSlots();
          toast(`帐号${slot} 已切换为 ${r.width}×${r.height}`);
        } else if (act === "home") {
          const url = card.querySelector('[data-k="webUri"]').value.trim();
          if (dirty) await saveConfig(false);
          await invoke("home_slot", { slot, url: url || null });
        }
      } catch (e) {
        toast(`操作失败: ${e}`);
      }
    });
  });
}

function refreshCard(slot) {
  const st = states[slot];
  if (!st) return;
  const card = document.querySelector(`.slot[data-slot="${slot}"]`);
  if (!card) return;
  card.classList.toggle("running", !!st.running);
  const [txt, cls] = statusInfo(st.lastStatus || (st.running ? "alive" : "未启动"));
  const dot = card.querySelector(".dot");
  dot.className = `dot ${cls}`;
  card.querySelector(".status-text").textContent = st.running ? txt : "已停止";
  card.querySelector(".clicks").textContent = st.clicks || 0;
  card.querySelector(".last-at").textContent = fmtTime(st.lastAt || 0);
  const startBtn = card.querySelector('[data-act="start"]');
  startBtn.textContent = st.running ? "显示" : "启动";
}

/* ---------------- config io ---------------- */

async function loadConfig() {
  config = await invoke("get_config");
  states = await invoke("get_states");
  $("set-auto-start").checked = !!config.settings.autoStart;
  $("set-update-url").value = config.settings.updateUrl || "";
  $("set-download-page").value = config.settings.downloadPage || "";
  renderSlots();
  clearDirty();
}

async function saveConfig(notify = true) {
  config.settings.autoStart = $("set-auto-start").checked;
  config.settings.updateUrl = $("set-update-url").value.trim();
  config.settings.downloadPage = $("set-download-page").value.trim();
  try {
    await invoke("save_config", { cfg: config });
    clearDirty();
    if (notify) toast("配置已保存");
    log("SYS", "配置已保存", "sys");
  } catch (e) {
    toast(`保存失败: ${e}`);
  }
}

/* ---------------- global actions ---------------- */

function bindGlobal() {
  $("btn-save").addEventListener("click", () => saveConfig(true));

  $("btn-start-all").addEventListener("click", async () => {
    if (dirty) await saveConfig(false);
    let n = 0;
    for (const s of config.slots) {
      if (s.enabled) {
        try { await invoke("start_slot", { slot: s.slot }); n++; } catch (e) { /* ignore */ }
      }
    }
    toast(`已启动 ${n} 个帐号窗口`);
  });

  $("btn-stop-all").addEventListener("click", async () => {
    for (const s of config.slots) {
      try { await invoke("stop_slot", { slot: s.slot }); } catch (e) { /* ignore */ }
    }
    toast("已全部停止");
  });

  $("btn-show-all").addEventListener("click", () => {
    for (let i = 1; i <= 9; i++) invoke("show_slot", { slot: i, visible: true }).catch(() => {});
  });

  $("btn-hide-all").addEventListener("click", () => {
    for (let i = 1; i <= 9; i++) invoke("show_slot", { slot: i, visible: false }).catch(() => {});
    toast("已隐藏全部窗口，Ctrl+1~9 或托盘可呼出");
  });

  $("btn-clear-log").addEventListener("click", () => { $("log").innerHTML = ""; });

  ["set-auto-start", "set-update-url", "set-download-page"].forEach((id) => {
    $(id).addEventListener("change", markDirty);
  });

  $("btn-check-update").addEventListener("click", checkUpdate);
  $("btn-open-download").addEventListener("click", async () => {
    const url = $("btn-open-download").dataset.url;
    if (url) await invoke("open_url", { url }).catch((e) => toast(`打开失败: ${e}`));
  });
}

/* ---------------- update ---------------- */

async function checkUpdate() {
  const dlg = $("dlg-update");
  const body = $("update-body");
  const dl = $("btn-open-download");
  dlg.showModal();
  body.textContent = "正在检查更新...";
  dl.style.display = "none";
  try {
    const info = await invoke("check_update");
    if (info.error) {
      body.innerHTML = `<span class="warn">检查失败：${escapeText(info.error)}</span>`;
      return;
    }
    if (info.hasUpdate) {
      body.innerHTML =
        `发现新版本 <span class="ver">v${escapeText(info.remoteVersion)}</span>（当前 v${escapeText(info.currentVersion)}）\n\n` +
        `${escapeText(info.description || "暂无更新说明")}`;
      dl.style.display = "";
      dl.dataset.url = info.downloadUrl;
    } else {
      body.innerHTML = `<span class="ok">已经是最新版本 v${escapeText(info.currentVersion)}</span>`;
    }
  } catch (e) {
    body.innerHTML = `<span class="warn">检查失败：${escapeText(String(e))}</span>`;
  }
}

/* ---------------- events ---------------- */

function bindEvents() {
  listen("cpk://status", (st) => {
    states[st.slot] = st;
    refreshCard(st.slot);
    if (st.lastStatus === "exited") {
      log(`帐号${st.slot}`, "已退出云手机，请检查会话", "act");
    }
  });

  listen("cpk://log", (e) => {
    log(`帐号${e.slot}`, e.msg, "sys");
  });
}

/* ---------------- boot ---------------- */

window.addEventListener("DOMContentLoaded", async () => {
  if (!tauri()) {
    log("SYS", "未检测到 Tauri 环境，请通过 CloudPhoneKeep 应用打开本面板", "sys");
    return;
  }
  bindGlobal();
  bindEvents();
  try {
    await loadConfig();
    const info = await invoke("get_app_info");
    log("SYS", `CloudPhoneKeep v${info.version} 就绪，本地上报端口 ${info.port}`, "sys");
  } catch (e) {
    log("SYS", `初始化失败: ${e}`, "sys");
  }
});
