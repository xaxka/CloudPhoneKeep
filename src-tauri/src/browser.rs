use crate::config::{self, SlotConfig};
use crate::keepalive;
use crate::state::SlotState;
use crate::AppState;
use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

pub fn slot_label(slot: u32) -> String {
    format!("browser-{slot}")
}

/// 启动（或显示）某个槽位的云手机窗口
pub fn start_slot(app: &AppHandle, slot: u32) -> Result<(), String> {
    if !(1..=9).contains(&slot) {
        return Err("槽位编号必须在 1~9 之间".into());
    }
    let cfg: SlotConfig = {
        let state: tauri::State<AppState> = app.state();
        let cfg = state.config.lock().unwrap();
        cfg.slots
            .iter()
            .find(|s| s.slot == slot)
            .cloned()
            .ok_or_else(|| format!("槽位 {slot} 配置不存在"))?
    };

    // 已打开则直接显示
    if let Some(win) = app.get_webview_window(&slot_label(slot)) {
        let _ = win.show();
        let _ = win.set_focus();
        sync_state(app, slot, |s| {
            s.running = true;
            s.visible = true;
        });
        return Ok(());
    }

    let port = *app.state::<AppState>().port.lock().unwrap();
    let init_script = keepalive::build_init_script(&cfg, port);

    let url = cfg.web_uri.trim().to_string();
    if url.is_empty() {
        return Err("浏览器地址不能为空".into());
    }
    let web_url: tauri::Url = url
        .parse()
        .map_err(|e| format!("浏览器地址无效: {e}"))?;

    let name = if cfg.name.trim().is_empty() {
        format!("帐号{slot}")
    } else {
        cfg.name.trim().to_string()
    };
    let title = format!("联通云手机 - {name} - 老板键 Ctrl+{slot}");

    let mut builder = WebviewWindowBuilder::new(app, slot_label(slot), WebviewUrl::External(web_url))
        .title(&title)
        .inner_size(cfg.width, cfg.height)
        .min_inner_size(280.0, 400.0)
        .resizable(true)
        .initialization_script(&init_script);

    // Windows / macOS: 每个帐号独立数据目录，实现 Cookie/缓存隔离
    let profile = config::profile_dir(app, slot, &name);
    builder = builder.data_directory(profile);

    let win = builder.build().map_err(|e| format!("创建窗口失败: {e}"))?;

    // 关闭按钮 → 隐藏（保活继续），通过托盘/老板键再次显示
    {
        let w = win.clone();
        win.on_window_event(move |e| {
            if let WindowEvent::CloseRequested { api, .. } = e {
                api.prevent_close();
                let _ = w.hide();
            }
        });
    }

    sync_state(app, slot, |s| {
        s.running = true;
        s.visible = true;
        if s.last_status == "未启动" {
            s.last_status = "installed".into();
        }
    });

    spawn_watchdog(app.clone(), slot);
    let _ = app.emit(
        "cpk://log",
        serde_json::json!({"slot": slot, "msg": format!("窗口已启动：{title}")}),
    );
    Ok(())
}

/// 原生看门狗：窗口隐藏时页面定时器可能被浏览器节流，
/// 由 Rust 侧周期性 eval 调用页面内 tick，保证后台保活不掉线。
fn spawn_watchdog(app: AppHandle, slot: u32) {
    let interval_ms = {
        let state: tauri::State<AppState> = app.state();
        let cfg = state.config.lock().unwrap();
        cfg.slots
            .iter()
            .find(|s| s.slot == slot)
            .map(|s| s.interval_ms)
            .unwrap_or(5000)
            .max(1000)
    };

    let handle = tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(interval_ms));
        loop {
            ticker.tick().await;
            let win = match app.get_webview_window(&slot_label(slot)) {
                Some(w) => w,
                None => break,
            };
            // 更新可见性状态
            let visible = win.is_visible().unwrap_or(true);
            sync_state(&app, slot, |s| {
                s.running = true;
                s.visible = visible;
            });
            // 驱动页面内保活 tick（eval 直接执行，不受页面定时器节流影响）
            let _ = win.eval("try{window.__CPK_TICK__&&window.__CPK_TICK__()}catch(e){}");
        }
    });

    let state: tauri::State<AppState> = app.state();
    if let Some(old) = state.watchdogs.lock().unwrap().insert(slot, handle) {
        old.abort();
    }
}

/// 停止槽位：关闭窗口并终止看门狗
pub fn stop_slot(app: &AppHandle, slot: u32) -> Result<(), String> {
    let label = slot_label(slot);
    {
        let state: tauri::State<AppState> = app.state();
        if let Some(h) = state.watchdogs.lock().unwrap().remove(&slot) {
            h.abort();
        }
    }
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.destroy();
    }
    sync_state(app, slot, |s| {
        s.running = false;
        s.visible = false;
        s.last_status = "已停止".into();
    });
    let _ = app.emit(
        "cpk://log",
        serde_json::json!({"slot": slot, "msg": "窗口已停止"}),
    );
    Ok(())
}

/// 老板键 / 托盘切换：可见则瞬间隐藏，隐藏则显示并置前
pub fn toggle_slot(app: &AppHandle, slot: u32) {
    if let Some(win) = app.get_webview_window(&slot_label(slot)) {
        match win.is_visible() {
            Ok(true) => {
                let _ = win.hide();
                sync_state(app, slot, |s| s.visible = false);
            }
            _ => {
                let _ = win.show();
                let _ = win.set_focus();
                sync_state(app, slot, |s| s.visible = true);
            }
        }
    }
}

pub fn show_slot(app: &AppHandle, slot: u32, visible: bool) {
    if let Some(win) = app.get_webview_window(&slot_label(slot)) {
        if visible {
            let _ = win.show();
            let _ = win.set_focus();
        } else {
            let _ = win.hide();
        }
        sync_state(app, slot, |s| s.visible = visible);
    }
}

pub fn set_topmost(app: &AppHandle, slot: u32, top: bool) -> Result<(), String> {
    if let Some(win) = app.get_webview_window(&slot_label(slot)) {
        win.set_always_on_top(top)
            .map_err(|e| format!("置顶设置失败: {e}"))?;
        sync_state(app, slot, |s| s.topmost = top);
        Ok(())
    } else {
        Err("窗口未启动".into())
    }
}

/// 旋转：交换宽高（横竖屏切换）并刷新页面
pub fn rotate_slot(app: &AppHandle, slot: u32) -> Result<(f64, f64), String> {
    let (w, h) = {
        let state: tauri::State<AppState> = app.state();
        let mut cfg = state.config.lock().unwrap();
        let s = cfg
            .slots
            .iter_mut()
            .find(|s| s.slot == slot)
            .ok_or("槽位不存在")?;
        std::mem::swap(&mut s.width, &mut s.height);
        s.screen_model = if s.screen_model == "vertical" {
            "horizontal".into()
        } else {
            "vertical".into()
        };
        config::save(app, &cfg).ok();
        (s.width, s.height)
    };

    if let Some(win) = app.get_webview_window(&slot_label(slot)) {
        let _ = win.set_size(tauri::LogicalSize::new(w, h));
        let _ = win.eval("try{location.reload()}catch(e){}");
    }
    Ok((w, h))
}

/// 导航（首页 / 地址栏跳转）
pub fn nav_slot(app: &AppHandle, slot: u32, url: &str) -> Result<(), String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("地址不能为空".into());
    }
    let js = format!(
        "try{{location.href={}}}catch(e){{}}",
        serde_json::to_string(url).unwrap_or_else(|_| "\"\"".into())
    );
    if let Some(win) = app.get_webview_window(&slot_label(slot)) {
        win.eval(&js).map_err(|e| format!("导航失败: {e}"))?;
        Ok(())
    } else {
        Err("窗口未启动".into())
    }
}

fn sync_state(app: &AppHandle, slot: u32, f: impl FnOnce(&mut SlotState)) {
    let snapshot = {
        let state: tauri::State<AppState> = app.state();
        let mut states = state.states.lock().unwrap();
        let s = states.entry(slot).or_insert_with(|| SlotState::new(slot));
        f(s);
        s.clone()
    };
    let _ = app.emit("cpk://status", &snapshot);
}

// ---------------------------------------------------------------------------
// 托盘
// ---------------------------------------------------------------------------

pub fn create_tray(app: &AppHandle) -> Result<(), tauri::Error> {
    let open_main = MenuItemBuilder::with_id("open-main", "打开主面板").build(app)?;
    let show_all = MenuItemBuilder::with_id("show-all", "显示全部窗口").build(app)?;
    let hide_all = MenuItemBuilder::with_id("hide-all", "隐藏全部窗口").build(app)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let quit = MenuItemBuilder::with_id("quit", "退出").build(app)?;

    let mut slot_items: Vec<tauri::menu::MenuItem<tauri::Wry>> = Vec::new();
    for i in 1..=9u32 {
        let item = MenuItemBuilder::with_id(
            format!("slot-{i}-toggle"),
            format!("帐号{i}：显示/隐藏  (Ctrl+{i})"),
        )
        .build(app)?;
        slot_items.push(item);
    }

    let mut mb = MenuBuilder::new(app)
        .item(&open_main)
        .item(&show_all)
        .item(&hide_all)
        .item(&sep1);
    for it in &slot_items {
        mb = mb.item(it);
    }
    let menu = mb.item(&sep2).item(&quit).build()?;

    let mut builder = TrayIconBuilder::with_id("main-tray")
        .tooltip("云手机保活 CloudPhoneKeep")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open-main" => {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            "show-all" => {
                for i in 1..=9u32 {
                    show_slot(app, i, true);
                }
            }
            "hide-all" => {
                for i in 1..=9u32 {
                    show_slot(app, i, false);
                }
            }
            "quit" => {
                app.exit(0);
            }
            id => {
                if let Some(n) = id.strip_prefix("slot-") {
                    if let Some(idx) = n.strip_suffix("-toggle") {
                        if let Ok(slot) = idx.parse::<u32>() {
                            toggle_slot(app, slot);
                        }
                    }
                }
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    let tray = builder.build(app)?;

    tray.on_tray_icon_event(|tray, event| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            let app = tray.app_handle().clone();
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }
    });

    Ok(())
}

// ---------------------------------------------------------------------------
// 全局老板键 Ctrl+1..9
// ---------------------------------------------------------------------------

pub fn register_shortcuts(app: &AppHandle) -> Result<(), String> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;

    let digits = [
        "Ctrl+1", "Ctrl+2", "Ctrl+3", "Ctrl+4", "Ctrl+5", "Ctrl+6", "Ctrl+7", "Ctrl+8", "Ctrl+9",
    ];
    let mut map = std::collections::HashMap::new();
    for (idx, code) in digits.iter().enumerate() {
        let slot = (idx + 1) as u32;
        match code.parse::<tauri_plugin_global_shortcut::Shortcut>() {
            Ok(sc) => {
                map.insert(sc.id(), slot);
                app.global_shortcut()
                    .register(sc)
                    .map_err(|e| format!("注册老板键 {code} 失败: {e}"))?;
            }
            Err(_) => continue,
        }
    }
    *app.state::<AppState>().shortcut_ids.lock().unwrap() = map;
    Ok(())
}

/// 全局热键回调：Ctrl+N 切换对应窗口显隐
pub fn shortcut_handler(
    app: &AppHandle,
    shortcut: &tauri_plugin_global_shortcut::Shortcut,
    event: tauri_plugin_global_shortcut::ShortcutEvent,
) {
    use tauri_plugin_global_shortcut::ShortcutState;
    if event.state != ShortcutState::Pressed {
        return;
    }
    let slot = {
        let state: tauri::State<AppState> = app.state();
        let map = state.shortcut_ids.lock().unwrap();
        map.get(&shortcut.id()).copied()
    };
    if let Some(slot) = slot {
        toggle_slot(app, slot);
    }
}
