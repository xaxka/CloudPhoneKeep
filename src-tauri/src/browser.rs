use crate::config::{self, SlotConfig};
use crate::keepalive;
use crate::logger;
use crate::state::SlotState;
use crate::AppState;
use std::sync::atomic::Ordering;
use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_notification::NotificationExt;

pub const RELEASES_URL: &str = "https://github.com/xixka/CloudPhoneKeep/releases";
/// WebView2 运行时官方下载页（窗口创建失败且疑似缺运行时时打开）
pub const WEBVIEW2_URL: &str = "https://developer.microsoft.com/microsoft-edge/webview2/";

pub fn slot_label(slot: u32) -> String {
    format!("browser-{slot}")
}

// ---------------------------------------------------------------------------
// 窗口生命周期
// ---------------------------------------------------------------------------

/// 启动（或聚焦）某个槽位的云手机窗口
pub fn start_slot(app: &AppHandle, slot: u32) -> Result<(), String> {
    start_slot_ex(app, slot).map(|_| ())
}

/// 启动（或聚焦）某个槽位的云手机窗口。
/// 返回 Ok(warnings)：启动成功但存在非致命问题（老板键被占用等），由前端醒目提示；
/// 仅当窗口确实无法创建时才返回 Err。
pub fn start_slot_ex(app: &AppHandle, slot: u32) -> Result<Vec<String>, String> {
    if !(1..=9).contains(&slot) {
        return Err("老板键索引必须在 1~9 之间".into());
    }
    let cfg: SlotConfig = {
        let state: tauri::State<AppState> = app.state();
        let cfg = state.config.lock().unwrap();
        cfg.slots
            .iter()
            .find(|s| s.slot == slot)
            .cloned()
            .ok_or_else(|| format!("帐号{slot} 配置不存在"))?
    };

    // 已打开则直接显示（与原版一致：重复进入只聚焦）
    if let Some(win) = app.get_webview_window(&slot_label(slot)) {
        let _ = win.show();
        let _ = win.set_focus();
        sync_state(app, slot, |s| {
            s.running = true;
            s.visible = true;
        });
        return Ok(Vec::new());
    }

    // 老板键注册失败【不再阻塞启动】：保活不依赖老板键，降级为警告继续开窗
    let mut warnings: Vec<String> = Vec::new();
    if let Err(e) = register_slot_shortcut(app, slot) {
        warnings.push(format!("老板键 Ctrl+{slot} 不可用：{e}"));
        logger::log(app, slot, "error", &format!("老板键注册失败（窗口仍将启动，无老板键模式）：{e}"));
    }
    ensure_addr_shortcut(app);

    let port = *app.state::<AppState>().port.lock().unwrap();
    let init_script = keepalive::build_init_script(&cfg, port);

    let url = cfg.web_uri.trim().to_string();
    if url.is_empty() {
        unregister_slot_shortcut(app, slot);
        return Err("浏览器地址不能为空".into());
    }
    let web_url: tauri::Url = url
        .parse()
        .map_err(|e| {
            unregister_slot_shortcut(app, slot);
            format!("浏览器地址无效: {e}")
        })?;

    let name = if cfg.name.trim().is_empty() {
        format!("帐号{slot}")
    } else {
        cfg.name.trim().to_string()
    };
    let platform_label = config::platform_preset(&cfg.platform).label;
    let has_boss_key = warnings.is_empty();
    let title = if has_boss_key {
        format!("{platform_label} - {name} - 老板键：Ctrl + {slot}")
    } else {
        format!("{platform_label} - {name} - 老板键不可用（Ctrl+{slot} 被占用）")
    };

    // 原生菜单栏（还原原版 aardio 主菜单）：创建失败同样降级为无菜单模式，不阻塞
    let menu = match build_window_menu(app, slot) {
        Ok(m) => Some(m),
        Err(e) => {
            warnings.push(format!("菜单创建失败：{e}"));
            logger::log(app, slot, "error", &format!("菜单创建失败（窗口将以无菜单模式启动）：{e}"));
            None
        }
    };

    let profile = config::profile_dir(slot, &name);
    let profile_name = profile
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut builder = WebviewWindowBuilder::new(app, slot_label(slot), WebviewUrl::External(web_url))
        .title(&title)
        .inner_size(cfg.width, cfg.height)
        .min_inner_size(280.0, 400.0)
        .resizable(true)
        .initialization_script(&init_script)
        // 每个帐号独立数据目录（Cookie/缓存隔离），保存在 exe 目录 data/ 下
        .data_directory(profile);
    if let Some(m) = menu {
        builder = builder.menu(m);
    }

    let win = match builder.build() {
        Ok(w) => w,
        Err(e) => {
            unregister_slot_shortcut(app, slot);
            let es = e.to_string().to_lowercase();
            let looks_like_webview2 = es.contains("webview")
                || es.contains("runtime")
                || es.contains("corewebview");
            logger::log(
                app,
                slot,
                "error",
                &format!("窗口创建失败：{e}（疑似缺 WebView2 运行时：{looks_like_webview2}）"),
            );
            if looks_like_webview2 {
                // 直接把官方下载页打开到用户面前，不再让用户对着报错发呆
                use tauri_plugin_opener::OpenerExt;
                let _ = app.opener().open_url(WEBVIEW2_URL, None::<&str>);
                return Err(format!(
                    "窗口创建失败：系统缺少 WebView2 运行时。\n已自动打开官方下载页，安装「Evergreen 独立安装包」后重开程序即可。"
                ));
            }
            let hint = if es.contains("denied") || es.contains("lock") {
                format!("创建窗口失败: {e}（该帐号数据目录可能正被另一个实例占用，请勿同时运行两份程序）")
            } else {
                format!("创建窗口失败: {e}")
            };
            return Err(hint);
        }
    };

    logger::log(
        app,
        slot,
        "sys",
        &format!("窗口已启动 platform={} url={} profile={profile_name} 老板键={}", cfg.platform, url, if has_boss_key { "已注册" } else { "不可用" }),
    );

    {
        let w = win.clone();
        win.on_window_event(move |e| match e {
            // 关闭按钮 → 隐藏（保活继续），老板键/托盘再次显示（与原版一致）
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = w.hide();
                let app = w.app_handle().clone();
                let s = slot_of_label(&w.label());
                sync_state(&app, s, |st| st.visible = false);
                logger::log(&app, s, "sys", "窗口已隐藏（保活继续，看门狗驱动模式）");
                // 首次隐藏弹通知，明确告知去哪里找回/退出，避免“关了就找不到、退不掉”
                let first_hide = {
                    let state: tauri::State<AppState> = app.state();
                    let mut states = state.states.lock().unwrap();
                    match states.get_mut(&s) {
                        Some(st) => std::mem::replace(&mut st.hide_notified, true),
                        None => false,
                    }
                };
                if !first_hide {
                    let _ = app
                        .notification()
                        .builder()
                        .title("已隐藏到托盘，保活继续")
                        .body(format!("Ctrl+{s} 或托盘图标可再次呼出；托盘菜单「退出」可彻底退出程序"))
                        .show();
                }
                rebuild_tray(&app);
            }
            WindowEvent::Focused(true) => {
                let app = w.app_handle().clone();
                *app.state::<AppState>().focused.lock().unwrap() = slot_of_label(&w.label());
            }
            WindowEvent::Destroyed => {
                let app = w.app_handle().clone();
                cleanup_slot(&app, slot_of_label(&w.label()));
            }
            _ => {}
        });
    }

    sync_state(app, slot, |s| {
        s.running = true;
        s.visible = true;
        s.hide_notified = false;
        if s.last_status == "未启动" {
            s.last_status = "installed".into();
        }
    });
    spawn_watchdog(app.clone(), slot);
    rebuild_tray(app);
    Ok(warnings)
}

fn slot_of_label(label: &str) -> u32 {
    label.strip_prefix("browser-").and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// 原生菜单栏（每个窗口独立，id 带槽位后缀）
fn build_window_menu(app: &AppHandle, slot: u32) -> Result<tauri::menu::Menu<tauri::Wry>, tauri::Error> {
    MenuBuilder::new(app)
        .item(&MenuItemBuilder::with_id(format!("home-{slot}"), "云手机首页").build(app)?)
        .item(&MenuItemBuilder::with_id(format!("rotate-{slot}"), "旋转").build(app)?)
        .item(&MenuItemBuilder::with_id(format!("top-{slot}"), "窗口置顶").build(app)?)
        .item(&MenuItemBuilder::with_id(format!("settings-{slot}"), "设置").build(app)?)
        .item(&MenuItemBuilder::with_id("update-check", "检查更新").build(app)?)
        .item(&MenuItemBuilder::with_id(format!("quitapp-{slot}"), "退出程序").build(app)?)
        .build()
}

/// 应用级菜单事件（窗口菜单 + 托盘菜单统一在此处理）
pub fn handle_menu_event(app: &AppHandle, id: &str) {
    // 全局项
    match id {
        "open-settings" => return show_login(app, None),
        "show-all" => {
            for slot in running_slots(app) {
                show_slot(app, slot, true);
            }
            return;
        }
        "hide-all" => {
            for slot in running_slots(app) {
                show_slot(app, slot, false);
            }
            return;
        }
        "open-logdir" => return open_log_dir(app),
        "probe-all" => {
            for slot in running_slots(app) {
                if let Some(win) = app.get_webview_window(&slot_label(slot)) {
                    let _ = win.eval("try{window.__CPK_PROBE__&&window.__CPK_PROBE__()}catch(e){}");
                }
            }
            logger::log(app, 0, "sys", "已触发全部窗口 DOM 采样，结果见 logs/ 目录");
            return;
        }
        "quit" => {
            quit_all(app);
            return;
        }
        "update-check" => {
            use tauri_plugin_opener::OpenerExt;
            let _ = app.opener().open_url(RELEASES_URL, None::<&str>);
            return;
        }
        _ => {}
    }

    // 带槽位后缀的项：home-3 / rotate-3 / top-3 / settings-3 / show-3 / hide-3 / close-3 / quitapp-3
    if let Some((action, n)) = id.rsplit_once('-') {
        if let Ok(slot) = n.parse::<u32>() {
            match action {
                "home" => {
                    let preset = {
                        let state: tauri::State<AppState> = app.state();
                        let cfg = state.config.lock().unwrap();
                        cfg.slots
                            .iter()
                            .find(|s| s.slot == slot)
                            .map(|s| s.web_uri.clone())
                            .unwrap_or_default()
                    };
                    if !preset.is_empty() {
                        let _ = nav_slot(app, slot, &preset);
                    }
                }
                "rotate" => {
                    let _ = rotate_slot(app, slot);
                }
                "top" => {
                    let top = !app
                        .state::<AppState>()
                        .states
                        .lock()
                        .unwrap()
                        .get(&slot)
                        .map(|s| s.topmost)
                        .unwrap_or(false);
                    let _ = set_topmost(app, slot, top);
                }
                "settings" => show_login(app, Some(slot)),
                "show" => show_slot(app, slot, true),
                "hide" => show_slot(app, slot, false),
                "close" => {
                    let _ = stop_slot(app, slot);
                }
                "quitapp" => quit_all(app),
                _ => {}
            }
        }
    }
}

/// 彻底退出程序：置退出标志（停用托盘重建等收尾动作，避免退出流程卡死），
/// 销毁全部云手机窗口后退出进程。
pub fn quit_all(app: &AppHandle) {
    if app
        .state::<AppState>()
        .quitting
        .swap(true, Ordering::SeqCst)
    {
        return; // 已在退出流程中，避免重复触发
    }
    logger::log(app, 0, "sys", "开始退出程序：销毁全部窗口");
    for (label, win) in app.webview_windows() {
        if label.starts_with("browser-") {
            let _ = win.destroy();
        }
    }
    if let Some(login) = app.get_webview_window("login") {
        let _ = login.destroy();
    }
    app.exit(0);
}

/// 显示设置窗口（prefill 指定槽位）
pub fn show_login(app: &AppHandle, slot: Option<u32>) {
    if let Some(win) = app.get_webview_window("login") {
        let _ = win.show();
        let _ = win.set_focus();
        if let Some(n) = slot {
            let _ = win.eval(&format!(
                "try{{window.__CPK_LOAD__&&window.__CPK_LOAD__({n})}}catch(e){{}}"
            ));
        }
    }
}

fn running_slots(app: &AppHandle) -> Vec<u32> {
    let state: tauri::State<AppState> = app.state();
    let states = state.states.lock().unwrap();
    let mut v: Vec<u32> = states.iter().filter(|(_, s)| s.running).map(|(k, _)| *k).collect();
    v.sort();
    v
}

/// 槽位清理：热键注销、看门狗终止、托盘刷新
fn cleanup_slot(app: &AppHandle, slot: u32) {
    if !(1..=9).contains(&slot) {
        return;
    }
    unregister_slot_shortcut(app, slot);
    let old_watchdog = {
        let state: tauri::State<AppState> = app.state();
        let old = state.watchdogs.lock().unwrap().remove(&slot);
        old
    };
    if let Some(h) = old_watchdog {
        h.abort();
    }
    sync_state(app, slot, |s| {
        s.running = false;
        s.visible = false;
        s.last_status = "已停止".into();
    });
    logger::log(app, slot, "sys", "窗口已关闭");
    if !app.state::<AppState>().quitting.load(Ordering::SeqCst) {
        rebuild_tray(app);
    }
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

    let task_app = app.clone();
    let handle = tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(interval_ms));
        loop {
            ticker.tick().await;
            let win = match task_app.get_webview_window(&slot_label(slot)) {
                Some(w) => w,
                None => break,
            };
            let visible = win.is_visible().unwrap_or(true);
            sync_state(&task_app, slot, |s| {
                s.running = true;
                s.visible = visible;
            });
            if let Err(e) = win.eval("try{window.__CPK_TICK__&&window.__CPK_TICK__()}catch(e){}") {
                logger::log(&task_app, slot, "error", &format!("看门狗 eval 失败: {e}"));
            }
        }
    });

    let old = {
        let state: tauri::State<AppState> = app.state();
        let old = state.watchdogs.lock().unwrap().insert(slot, handle);
        old
    };
    if let Some(old) = old {
        old.abort();
        logger::log(&app, slot, "sys", "看门狗已替换（旧任务终止）");
    }
}

/// 停止槽位：真正关闭窗口并退出该帐号保活
pub fn stop_slot(app: &AppHandle, slot: u32) -> Result<(), String> {
    if let Some(win) = app.get_webview_window(&slot_label(slot)) {
        let _ = win.destroy();
    } else {
        cleanup_slot(app, slot);
    }
    Ok(())
}

/// 老板键：可见则瞬间隐藏，隐藏则显示并置前
pub fn toggle_slot(app: &AppHandle, slot: u32) {
    if let Some(win) = app.get_webview_window(&slot_label(slot)) {
        match win.is_visible() {
            Ok(true) => {
                let _ = win.hide();
                let first_hide = {
                    let state: tauri::State<AppState> = app.state();
                    let mut states = state.states.lock().unwrap();
                    match states.get_mut(&slot) {
                        Some(st) => std::mem::replace(&mut st.hide_notified, true),
                        None => false,
                    }
                };
                sync_state(app, slot, |s| s.visible = false);
                logger::log(app, slot, "sys", "窗口已隐藏（保活切换为看门狗驱动模式）");
                if !first_hide {
                    let _ = app
                        .notification()
                        .builder()
                        .title("已隐藏到托盘，保活继续")
                        .body(format!("Ctrl+{slot} 可再次呼出；托盘菜单「退出」可彻底退出程序"))
                        .show();
                }
                rebuild_tray(app);
            }
            _ => {
                let _ = win.show();
                let _ = win.set_focus();
                sync_state(app, slot, |s| s.visible = true);
                logger::log(app, slot, "sys", "窗口已显示");
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
        logger::log(app, slot, "sys", if top { "窗口已置顶" } else { "已取消置顶" });
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
            .ok_or("帐号配置不存在")?;
        std::mem::swap(&mut s.width, &mut s.height);
        s.screen_model = if s.screen_model == "vertical" {
            "horizontal".into()
        } else {
            "vertical".into()
        };
        let size = (s.width, s.height);
        config::save(&cfg).ok();
        size
    };

    if let Some(win) = app.get_webview_window(&slot_label(slot)) {
        let _ = win.set_size(tauri::LogicalSize::new(w, h));
        let _ = win.eval("try{location.reload()}catch(e){}");
    }
    Ok((w, h))
}

/// 导航（云手机首页 / 地址栏跳转）
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

/// 在当前聚焦的窗口里切换地址栏（Ctrl+U，还原原版）
pub fn toggle_address_bar(app: &AppHandle) {
    let slot = *app.state::<AppState>().focused.lock().unwrap();
    if slot == 0 {
        return;
    }
    if let Some(win) = app.get_webview_window(&slot_label(slot)) {
        let _ = win.eval(
            "try{var b=document.getElementById('cpk-addr-bar');window.__CPK_ADDR__&&window.__CPK_ADDR__(b?b.style.display==='none':true)}catch(e){}",
        );
    }
}

fn open_log_dir(app: &AppHandle) {
    use tauri_plugin_opener::OpenerExt;
    let dir = crate::config::base_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let _ = app
        .opener()
        .open_path(dir.to_string_lossy().to_string(), None::<&str>);
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
// 托盘（动态：只显示运行中的帐号，与原版每窗口托盘一致）
// ---------------------------------------------------------------------------

pub fn create_tray(app: &AppHandle) -> Result<(), tauri::Error> {
    let menu = tray_menu(app)?;
    let mut builder = TrayIconBuilder::with_id("main-tray")
        .tooltip("云手机保活")
        .menu(&menu)
        .on_menu_event(|app, event| handle_menu_event(app, event.id().as_ref()));

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    let tray = builder.build(app)?;
    tray.on_tray_icon_event(|tray, event| {
        // 左键点击托盘 → 显示设置窗口（与原版行为一致）
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            let app = tray.app_handle().clone();
            show_login(&app, None);
        }
    });
    *app.state::<AppState>().tray.lock().unwrap() = Some(tray);
    Ok(())
}

/// 重建托盘菜单（窗口启停后调用）。退出流程中跳过：关闭窗口的 Destroyed 事件
/// 会级联触发本函数，退出时持锁重建菜单存在卡死风险，且毫无必要。
pub fn rebuild_tray(app: &AppHandle) {
    if app.state::<AppState>().quitting.load(Ordering::SeqCst) {
        return;
    }
    let menu = match tray_menu(app) {
        Ok(m) => m,
        Err(_) => return,
    };
    let state: tauri::State<AppState> = app.state();
    let guard = state.tray.lock().unwrap();
    if let Some(tray) = guard.as_ref() {
        let _ = tray.set_menu(Some(menu));
    }
}

fn tray_menu(app: &AppHandle) -> Result<tauri::menu::Menu<tauri::Wry>, tauri::Error> {
    let open_settings = MenuItemBuilder::with_id("open-settings", "打开设置（新开帐号）").build(app)?;
    let show_all = MenuItemBuilder::with_id("show-all", "显示全部窗口").build(app)?;
    let hide_all = MenuItemBuilder::with_id("hide-all", "隐藏全部窗口").build(app)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let probe = MenuItemBuilder::with_id("probe-all", "DOM 采样（全部窗口）").build(app)?;
    let logdir = MenuItemBuilder::with_id("open-logdir", "打开日志目录").build(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let quit = MenuItemBuilder::with_id("quit", "退出").build(app)?;

    let mut mb = MenuBuilder::new(app)
        .item(&open_settings)
        .item(&show_all)
        .item(&hide_all)
        .item(&sep1);

    // 运行中的帐号：子菜单 显示/隐藏/关闭（还原原版每窗口托盘）
    let slots = {
        let state: tauri::State<AppState> = app.state();
        let cfg = state.config.lock().unwrap();
        let states = state.states.lock().unwrap();
        (1..=9u32)
            .filter(|n| states.get(n).map(|s| s.running).unwrap_or(false))
            .map(|n| {
                let name = cfg
                    .slots
                    .iter()
                    .find(|s| s.slot == n)
                    .map(|s| {
                        if s.name.trim().is_empty() {
                            format!("帐号{n}")
                        } else {
                            s.name.trim().to_string()
                        }
                    })
                    .unwrap_or_else(|| format!("帐号{n}"));
                (n, name)
            })
            .collect::<Vec<_>>()
    };

    for (n, name) in &slots {
        let sub = SubmenuBuilder::new(app, format!("{name}（Ctrl+{n}）"))
            .item(&MenuItemBuilder::with_id(format!("show-{n}"), "显示").build(app)?)
            .item(&MenuItemBuilder::with_id(format!("hide-{n}"), "隐藏").build(app)?)
            .item(&MenuItemBuilder::with_id(format!("close-{n}"), "关闭窗口（退出保活）").build(app)?)
            .build()?;
        mb = mb.item(&sub);
    }
    if !slots.is_empty() {
        mb = mb.separator();
    }

    let menu = mb.item(&probe).item(&logdir).item(&sep2).item(&quit).build()?;
    Ok(menu)
}

// ---------------------------------------------------------------------------
// 全局热键：老板键 Ctrl+N（按槽位注册）+ Ctrl+U 地址栏
// ---------------------------------------------------------------------------

/// 注册某槽位的老板键。失败返回可读错误（例如被其他实例占用），绝不 panic。
pub fn register_slot_shortcut(app: &AppHandle, slot: u32) -> Result<(), String> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;

    let code = format!("Ctrl+{slot}");
    let sc: tauri_plugin_global_shortcut::Shortcut = code
        .parse()
        .map_err(|e| format!("解析热键 {code} 失败: {e}"))?;
    app.global_shortcut()
        .register(sc)
        .map_err(|e| format!("老板键 {code} 注册失败：{e}（可能已被其他程序或另一个实例占用，请换一个索引）"))?;
    app.state::<AppState>()
        .shortcut_ids
        .lock()
        .unwrap()
        .insert(sc.id(), slot);
    Ok(())
}

pub fn unregister_slot_shortcut(app: &AppHandle, slot: u32) {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;

    if let Ok(sc) = format!("Ctrl+{slot}").parse::<tauri_plugin_global_shortcut::Shortcut>() {
        let _ = app.global_shortcut().unregister(sc);
    }
    let state: tauri::State<AppState> = app.state();
    state
        .shortcut_ids
        .lock()
        .unwrap()
        .retain(|_, v| *v != slot);
}

/// 注册地址栏热键 Ctrl+U（首个窗口打开时注册一次，应用生命周期内有效）
fn ensure_addr_shortcut(app: &AppHandle) {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;

    let has = {
        let state: tauri::State<AppState> = app.state();
        let map = state.shortcut_ids.lock().unwrap();
        map.values().any(|v| *v == 0)
    };
    if has {
        return;
    }
    if let Ok(sc) = "Ctrl+U".parse::<tauri_plugin_global_shortcut::Shortcut>() {
        if app.global_shortcut().register(sc).is_ok() {
            app.state::<AppState>()
                .shortcut_ids
                .lock()
                .unwrap()
                .insert(sc.id(), 0);
        }
    }
}

/// 全局热键回调：Ctrl+N 切换对应窗口显隐；Ctrl+U 切换地址栏
pub fn shortcut_handler(
    app: &AppHandle,
    shortcut: &tauri_plugin_global_shortcut::Shortcut,
    event: tauri_plugin_global_shortcut::ShortcutEvent,
) {
    use tauri_plugin_global_shortcut::ShortcutState;
    if event.state != ShortcutState::Pressed {
        return;
    }
    let id = shortcut.id();
    let slot = {
        let state: tauri::State<AppState> = app.state();
        let map = state.shortcut_ids.lock().unwrap();
        map.get(&id).copied()
    };
    match slot {
        Some(0) => toggle_address_bar(app),
        Some(n) => toggle_slot(app, n),
        None => {}
    }
}
