use crate::config::{self, SlotConfig};
use crate::keepalive;
use crate::logger;
use crate::state::SlotState;
use crate::AppState;
use std::sync::atomic::Ordering;
use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem};
use tauri::tray::{TrayIconBuilder};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

pub const RELEASES_URL: &str = "https://github.com/xixka/CloudPhoneKeep/releases";
/// WebView2 运行时官方下载页（窗口创建失败且疑似缺运行时时打开）
pub const WEBVIEW2_URL: &str = "https://developer.microsoft.com/microsoft-edge/webview2/";

pub fn slot_label(slot: u32) -> String {
    format!("browser-{slot}")
}

fn winset_label(slot: u32) -> String {
    format!("winset-{slot}")
}

// ---------------------------------------------------------------------------
// 窗口生命周期
// ---------------------------------------------------------------------------

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
        // 托盘菜单的 ● 显隐标记需要同步刷新（此前漏掉，重开后标记停留在旧状态）
        refresh_slot_tray(app, slot);
        return Ok(Vec::new());
    }

    // 老板键注册失败【不再阻塞启动】：保活不依赖老板键，降级为警告继续开窗
    let mut warnings: Vec<String> = Vec::new();
    if let Err(e) = register_slot_shortcut(app, slot) {
        let msg = format!("老板键 Ctrl+{slot} 不可用（可能被其他程序占用），窗口将以无老板键模式启动");
        warnings.push(format!("{msg}：{e}"));
        logger::log(app, slot, "error", &format!("老板键注册失败（窗口仍将启动，无老板键模式）：{e}"));
        // 设置窗口启动成功后会被隐藏，横幅用户看不到 → 系统通知兜底
        use tauri_plugin_notification::NotificationExt;
        let _ = app
            .notification()
            .builder()
            .title("老板键不可用")
            .body(format!("{msg}，请换一个索引"))
            .show();
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
    // 标题格式还原原版：移动云手机 - {name} - 老板键：Ctrl + {idx}
    let title = format!("{platform_label} - {name} - 老板键：Ctrl + {slot}");

    // 原生菜单栏（还原原版 aardio 主菜单五项）：创建失败降级为无菜单模式，不阻塞
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

    // 生效分辨率：会话内覆盖（旋转/窗口设置）优先，其次配置值（还原原版 userInfo 语义）
    let (w, h) = {
        let state: tauri::State<AppState> = app.state();
        let states = state.states.lock().unwrap();
        states
            .get(&slot)
            .and_then(|s| s.size_override)
            .unwrap_or((cfg.width, cfg.height))
    };

    let mut builder = WebviewWindowBuilder::new(app, slot_label(slot), WebviewUrl::External(web_url))
        .title(&title)
        .inner_size(w, h)
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
        &format!("窗口已启动 platform={} url={} profile={profile_name}", cfg.platform, url),
    );

    {
        let w = win.clone();
        win.on_window_event(move |e| match e {
            // 还原原版 web.aardio onClose/onDestroy → win.quitMessage()：
            // 点 X 即退出整个程序（所有窗口、全部保活一并结束）
            WindowEvent::CloseRequested { .. } => {
                let app = w.app_handle().clone();
                logger::log(&app, slot_of_label(&w.label()), "sys", "用户关闭窗口 → 退出程序");
                quit_all(&app);
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
        if s.last_status == "未启动" {
            s.last_status = "installed".into();
        }
    });
    spawn_watchdog(app.clone(), slot);
    // 还原原版：每个窗口创建自己的托盘图标
    create_slot_tray(app, slot);
    Ok(warnings)
}

fn slot_of_label(label: &str) -> u32 {
    label.strip_prefix("browser-").and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// 原生菜单栏（每个窗口独立，id 带槽位后缀）。
/// 还原原版 aardio 主菜单五项：云手机首页/旋转/窗口置顶/设置/检查更新（无退出项）。
fn build_window_menu(app: &AppHandle, slot: u32) -> Result<tauri::menu::Menu<tauri::Wry>, tauri::Error> {
    MenuBuilder::new(app)
        .item(&MenuItemBuilder::with_id(format!("home-{slot}"), "云手机首页").build(app)?)
        .item(&MenuItemBuilder::with_id(format!("rotate-{slot}"), "旋转").build(app)?)
        .item(&MenuItemBuilder::with_id(format!("top-{slot}"), "窗口置顶").build(app)?)
        .item(&MenuItemBuilder::with_id(format!("settings-{slot}"), "设置").build(app)?)
        .item(&MenuItemBuilder::with_id("update-check", "检查更新").build(app)?)
        .build()
}

/// 应用级菜单事件（窗口菜单 + 托盘菜单统一在此处理）
pub fn handle_menu_event(app: &AppHandle, id: &str) {
    // 全局项
    match id {
        "open-settings" => return show_login(app, None),
        "open-logdir" => return open_log_dir(app),
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

    // 带槽位后缀的项：home-3 / rotate-3 / top-3 / settings-3 / show-3 / hide-3
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
                // 还原原版：菜单「设置」打开只调分辨率的「窗口设置」小窗
                "settings" => open_winset(app, slot),
                "show" => show_slot(app, slot, true),
                "hide" => show_slot(app, slot, false),
                _ => {}
            }
        }
    }
}

/// 「窗口设置」小窗（还原原版 settingWin：仅分辨率输入 + 保存，会话内生效不落盘，
/// 同一窗口单实例，关闭后可再次打开）
pub fn open_winset(app: &AppHandle, slot: u32) {
    let label = winset_label(slot);
    if let Some(win) = app.get_webview_window(&label) {
        // 单实例：已打开则聚焦（还原原版 isWinHwnd 拦截重复打开）
        let _ = win.show();
        let _ = win.set_focus();
        return;
    }
    let _ = WebviewWindowBuilder::new(
        app,
        label,
        WebviewUrl::App(std::path::PathBuf::from("winset.html")),
    )
    .title("窗口设置")
    .inner_size(275.0, 218.0)
    .resizable(false)
    .build();
    logger::log(app, slot, "sys", "打开窗口设置（分辨率，仅本会话生效）");
}

/// 当前生效分辨率（会话内覆盖优先，其次配置值）
pub fn slot_size(app: &AppHandle, slot: u32) -> (f64, f64) {
    let state: tauri::State<AppState> = app.state();
    let cfg = state.config.lock().unwrap();
    let base = cfg
        .slots
        .iter()
        .find(|s| s.slot == slot)
        .map(|s| (s.width, s.height))
        .unwrap_or((414.0, 896.0));
    drop(cfg);
    let states = state.states.lock().unwrap();
    states
        .get(&slot)
        .and_then(|s| s.size_override)
        .unwrap_or(base)
}

/// 「窗口设置」保存 / 立即改尺寸：只改窗口与内存，不写配置（还原原版 settingWin 语义）
pub fn apply_slot_size(app: &AppHandle, slot: u32, w: f64, h: f64) -> Result<(), String> {
    sync_state(app, slot, |s| s.size_override = Some((w, h)));
    if let Some(win) = app.get_webview_window(&slot_label(slot)) {
        win.set_size(tauri::LogicalSize::new(w, h))
            .map_err(|e| e.to_string())?;
    }
    logger::log(app, slot, "sys", &format!("分辨率已调整（仅本会话）：{w} x {h}"));
    Ok(())
}

/// 彻底退出程序：置退出标志（跳过收尾动作，避免退出流程被卡死），
/// 移除全部托盘、销毁全部窗口后退出进程（还原原版 win.quitMessage）。
pub fn quit_all(app: &AppHandle) {
    if app
        .state::<AppState>()
        .quitting
        .swap(true, Ordering::SeqCst)
    {
        return; // 已在退出流程中，避免重复触发
    }
    logger::log(app, 0, "sys", "开始退出程序：销毁全部窗口");
    // 移除全部托盘图标（还原原版退出前 tray.delete()）
    app.state::<AppState>().trays.lock().unwrap().clear();
    for (label, win) in app.webview_windows() {
        if label.starts_with("browser-") || label.starts_with("winset-") {
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

/// 槽位清理：热键注销、看门狗终止、移除该窗口托盘
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
    // 移除该窗口的托盘图标（还原原版窗口销毁时托盘随之移除）
    app.state::<AppState>().trays.lock().unwrap().remove(&slot);
    sync_state(app, slot, |s| {
        s.running = false;
        s.visible = false;
        s.last_status = "已停止".into();
    });
    logger::log(app, slot, "sys", "窗口已关闭");
}

/// 原生看门狗：窗口隐藏时 WebView2 会把页面 JS 定时器节流到分钟级，
/// 由 Rust 侧周期性 eval 调用页面内 tick 接管驱动，保证后台保活不掉线。
/// 注意：窗口可见时【必须跳过】——页面自身 setInterval(1000) 在跑，
/// 双驱动会让 actionTick 的 5 秒周期实际变成 2.5 秒（点击频率翻倍）。
/// 周期 1 秒 = 原版 stopTimer；页面内 tick 自行分流：
///   - 每 1 秒：退出/到期检测（stopTimer 语义）
///   - 每 intervalMs：保活动作（runTimer 5 秒语义）
fn spawn_watchdog(app: AppHandle, slot: u32) {
    let task_app = app.clone();
    let handle = tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(1000));
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
            // 仅隐藏时驱动：可见时页面定时器自己在跑，eval 会造成双倍 tick
            if !visible {
                if let Err(e) = win.eval("try{window.__CPK_TICK__&&window.__CPK_TICK__()}catch(e){}") {
                    logger::log(&task_app, slot, "error", &format!("看门狗 eval 失败: {e}"));
                }
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

/// 老板键：可见则瞬间隐藏，隐藏则显示并置前（还原原版 reghotkey Ctrl+N）
pub fn toggle_slot(app: &AppHandle, slot: u32) {
    if let Some(win) = app.get_webview_window(&slot_label(slot)) {
        match win.is_visible() {
            Ok(true) => {
                let _ = win.hide();
                sync_state(app, slot, |s| s.visible = false);
                logger::log(app, slot, "sys", "窗口已隐藏（保活切换为看门狗驱动模式）");
                refresh_slot_tray(app, slot);
            }
            _ => {
                let _ = win.show();
                let _ = win.set_focus();
                sync_state(app, slot, |s| s.visible = true);
                logger::log(app, slot, "sys", "窗口已显示");
                refresh_slot_tray(app, slot);
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
        refresh_slot_tray(app, slot);
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

/// 旋转：交换宽高并刷新页面（还原原版：仅改内存不落盘，重启后回到配置分辨率）
pub fn rotate_slot(app: &AppHandle, slot: u32) -> Result<(f64, f64), String> {
    let (w, h) = slot_size(app, slot);
    // 交换后钳制到窗口最小尺寸（min_inner_size 280x400），避免 set_size 被 clamp 后与记录不一致
    let swapped = (h.max(280.0), w.max(400.0));
    sync_state(app, slot, |s| s.size_override = Some(swapped));

    if let Some(win) = app.get_webview_window(&slot_label(slot)) {
        let _ = win.set_size(tauri::LogicalSize::new(swapped.0, swapped.1));
        // 还原原版 myWebView.go(location)：刷新当前页
        let _ = win.eval("try{location.reload()}catch(e){}");
    }
    logger::log(app, slot, "sys", &format!("旋转：{} x {}（仅本会话生效）", swapped.0, swapped.1));
    Ok(swapped)
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
// 托盘（还原原版：每个云手机窗口创建自己的 win.util.tray）
// 菜单：显示(●)/隐藏(●)/分隔/打开设置（新开帐号）/打开日志目录/分隔/退出
// （● 前缀标记当前状态，与原版 appConfig.windowShown 判断一致；左键无动作也与原版一致）
// ---------------------------------------------------------------------------

/// 为某个槽位窗口创建独立托盘图标
pub fn create_slot_tray(app: &AppHandle, slot: u32) {
    let menu = match slot_tray_menu(app, slot) {
        Ok(m) => m,
        Err(e) => {
            logger::log(app, slot, "error", &format!("托盘创建失败（不影响保活）：{e}"));
            return;
        }
    };
    let platform_label = {
        let state: tauri::State<AppState> = app.state();
        let cfg = state.config.lock().unwrap();
        cfg.slots
            .iter()
            .find(|s| s.slot == slot)
            .map(|s| config::platform_preset(&s.platform).label.to_string())
            .unwrap_or_else(|| "云手机保活".into())
    };
    let mut builder = TrayIconBuilder::with_id(format!("tray-{slot}"))
        .tooltip(&platform_label)
        .menu(&menu)
        .on_menu_event(|app, event| handle_menu_event(app, event.id().as_ref()));

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    match builder.build(app) {
        Ok(tray) => {
            app.state::<AppState>().trays.lock().unwrap().insert(slot, tray);
            logger::log(app, slot, "sys", "托盘已创建（本窗口）");
        }
        Err(e) => {
            logger::log(app, slot, "error", &format!("托盘创建失败（不影响保活）：{e}"));
        }
    }
}

/// 窗口显隐后刷新该槽位托盘菜单的 ● 状态标记
fn refresh_slot_tray(app: &AppHandle, slot: u32) {
    let menu = match slot_tray_menu(app, slot) {
        Ok(m) => m,
        Err(_) => return,
    };
    let state: tauri::State<AppState> = app.state();
    let trays = state.trays.lock().unwrap();
    if let Some(tray) = trays.get(&slot) {
        let _ = tray.set_menu(Some(menu));
    }
}

fn slot_tray_menu(app: &AppHandle, slot: u32) -> Result<tauri::menu::Menu<tauri::Wry>, tauri::Error> {
    let visible = {
        let state: tauri::State<AppState> = app.state();
        let states = state.states.lock().unwrap();
        states.get(&slot).map(|s| s.visible).unwrap_or(true)
    };
    // 还原原版菜单文案：appConfig.windowShown ? "&● 显示" : "&   显示"
    let show = MenuItemBuilder::with_id(format!("show-{slot}"), if visible { "● 显示" } else { "　 显示" }).build(app)?;
    let hide = MenuItemBuilder::with_id(format!("hide-{slot}"), if !visible { "● 隐藏" } else { "　 隐藏" }).build(app)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let open_settings = MenuItemBuilder::with_id("open-settings", "打开设置（新开帐号）").build(app)?;
    let logdir = MenuItemBuilder::with_id("open-logdir", "打开日志目录").build(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let quit = MenuItemBuilder::with_id("quit", "退出").build(app)?;

    let menu = MenuBuilder::new(app)
        .item(&show)
        .item(&hide)
        .item(&sep1)
        .item(&open_settings)
        .item(&logdir)
        .item(&sep2)
        .item(&quit)
        .build()?;
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
