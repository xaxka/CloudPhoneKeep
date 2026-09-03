use crate::config::{self, SlotConfig};
use crate::keepalive;
use crate::logger;
use crate::state::SlotState;
use crate::AppState;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem};
use tauri::tray::{TrayIconBuilder};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

/// WebView2 运行时官方下载页（窗口创建失败且疑似缺运行时时打开）
pub const WEBVIEW2_URL: &str = "https://developer.microsoft.com/microsoft-edge/webview2/";

pub fn slot_label(slot: u32) -> String {
    format!("browser-{slot}")
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
        reapply_topmost(app, slot);
        sync_state(app, slot, |s| {
            s.running = true;
            s.visible = true;
        });
        hide_login(app);
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

    // 窗口不再挂原生菜单栏（原「首页/旋转/窗口置顶/检查更新」五项已移除，
    // 「首页/窗口置顶」改到托盘菜单，见 slot_tray_menu）

    let profile = config::profile_dir(slot, &name);
    // 日志目录登记更新（改名重进 / 新帐号时以本次目录为准）
    logger::register_slot_dir(slot, profile.clone());
    // 旧版目录命名（slot-N-名字）一次性迁移为 data/名字，尽量保住已有登录态
    if let Some(msg) = config::migrate_legacy_profile(slot, &name) {
        logger::log(app, slot, "sys", &msg);
    }
    let profile_name = profile
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    // 初始分辨率使用配置值；用户可自由拖动窗口调整大小
    let (w, h) = (cfg.width, cfg.height);
    logger::log(
        app,
        slot,
        "debug",
        &format!("窗口参数就绪：{w}x{h} 数据目录={profile_name}（下一步 WebView2 创建；若此后长时间无日志=创建卡死，多为数据目录被残留进程锁定）"),
    );

    // 数据目录被锁（0x800700AA / 0x8007139F，残留的 WebView2 进程占用）时：
    // 先清扫残留进程重试一次，再不行换新目录重试一次——保证窗口一定能开出来
    let mut profile = profile;
    let win = {
        let mut attempt = 0u8;
        loop {
            attempt += 1;
            let t0 = std::time::Instant::now();
            let nav_app = app.clone();
            let title_app = app.clone();
            let builder = WebviewWindowBuilder::new(
                app,
                slot_label(slot),
                WebviewUrl::External(web_url.clone()),
            )
            .title(&title)
            .inner_size(w, h)
            .min_inner_size(280.0, 400.0)
            .resizable(true)
            // 只保留关闭按钮：去掉最小化/最大化（窗口跳过任务栏、仅由托盘管理，
            // 最小化/最大化均无意义；拖拽自由调整大小保留）
            .minimizable(false)
            .maximizable(false)
            // 不在任务栏显示：云手机窗口只通过托盘管理（点 X 隐藏到托盘；
            // 老板键 Ctrl+N 显隐；托盘左键显示）。任务栏无残留，符合「只在托盘显示」
            .skip_taskbar(true)
            .initialization_script(&init_script)
            // 每个帐号独立数据目录（Cookie/缓存隔离），保存在 AppData\LocalLow\CloudPhoneKeep\<目录名>
            .data_directory(profile.clone())
            // 页面加载事件落盘（debug 级）：「窗口开了但白屏/加载不出来」时，
            // 据此分辨是导航根本没开始（网络/站点问题）还是加载完了但渲染异常
            .on_page_load(move |_w, payload| {
                let ev = match payload.event() {
                    tauri::webview::PageLoadEvent::Started => "Started(开始加载)",
                    tauri::webview::PageLoadEvent::Finished => "Finished(加载完成)",
                };
                logger::log(&nav_app, slot, "debug", &format!("页面加载事件 {ev}：{}", payload.url()));
            })
            // SPA 路由切换会改标题：标题出现变化 = 页面确实渲染出来了
            .on_document_title_changed(move |_w, t| {
                logger::log(&title_app, slot, "debug", &format!("页面标题：{t}"));
            });
            match builder.build() {
                Ok(w) => {
                    logger::log(
                        app,
                        slot,
                        "debug",
                        &format!("WebView2 创建成功，耗时 {} ms（第 {attempt} 次尝试）", t0.elapsed().as_millis()),
                    );
                    break w;
                }
                Err(e) => {
                    let es = e.to_string().to_lowercase();
                    let locked = es.contains("0x800700aa")
                        || es.contains("0x8007139f")
                        || es.contains("in use")
                        || es.contains("lock");
                    if locked && attempt <= 2 {
                        // 第一步：清扫残留 WebView2 进程后原地重试。
                        // 多实例保护在 kill_zombie_webview2 内部：还有其它本程序实例
                        // 存活时不清扫（那把锁属于活实例，杀了会连人窗口一起杀），
                        // 直接返回 false → 走换目录分支
                        if attempt == 1 {
                            logger::log(
                                app,
                                slot,
                                "error",
                                &format!("数据目录被占用（{e}）：尝试清扫残留 WebView2 进程后重试"),
                            );
                            if crate::kill_zombie_webview2(app, &profile) {
                                // 被杀进程的文件句柄释放需要一点时间，稍等再重试
                                std::thread::sleep(std::time::Duration::from_millis(800));
                                continue;
                            }
                        }
                        // 第二步：换用新数据目录重试（原登录态留在旧目录，可能需重新登录）
                        let suffix = if attempt == 1 { "-r2" } else { "-r3" };
                        let fresh = config::profile_dir_with_suffix(slot, &name, suffix);
                        // 日志跟着新目录走
                        logger::register_slot_dir(slot, fresh.clone());
                        logger::log(
                            app,
                            slot,
                            "error",
                            &format!(
                                "数据目录仍被占用：换用新目录 {} 重试（原登录态留在旧目录，可能需重新登录；多开时请各实例使用不同目录名）",
                                fresh.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
                            ),
                        );
                        profile = fresh;
                        continue;
                    }
                    unregister_slot_shortcut(app, slot);
                    let looks_like_webview2 = es.contains("webview")
                        || es.contains("runtime")
                        || es.contains("corewebview");
                    logger::log(
                        app,
                        slot,
                        "error",
                        &format!("窗口创建失败（第 {attempt} 次，耗时 {} ms）：{e}", t0.elapsed().as_millis()),
                    );
                    if looks_like_webview2 {
                        // 直接把官方下载页打开到用户面前，不再让用户对着报错发呆
                        use tauri_plugin_opener::OpenerExt;
                        let _ = app.opener().open_url(WEBVIEW2_URL, None::<&str>);
                        return Err(format!(
                            "窗口创建失败：系统缺少 WebView2 运行时。\n已自动打开官方下载页，安装「Evergreen 独立安装包」后重开程序即可。"
                        ));
                    }
                    let hint = if locked {
                        format!("创建窗口失败: {e}（数据目录被残留进程锁定且自动恢复失败，请重启电脑后重试）")
                    } else {
                        format!("创建窗口失败: {e}")
                    };
                    return Err(hint);
                }
            }
        }
    };

    let final_profile_name = profile
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    logger::log(
        app,
        slot,
        "sys",
        &format!("窗口已启动 platform={} url={} profile={final_profile_name}", cfg.platform, url),
    );

    {
        let w = win.clone();
        win.on_window_event(move |e| match e {
            // 云手机窗口点 X = 隐藏到托盘继续保活（不再退出整个程序；
            // 要退出请用托盘菜单「退出」）
            WindowEvent::CloseRequested { api, .. } => {
                let app = w.app_handle().clone();
                let slot = slot_of_label(&w.label());
                api.prevent_close();
                let hidden = match w.hide() {
                    Ok(()) => {
                        logger::log(&app, slot, "sys", "用户点击关闭 → 隐藏到托盘（保活继续，退出请用托盘菜单）");
                        true
                    }
                    // 隐藏失败必须留痕：曾出现「已记录隐藏、随后还能点中窗口菜单」的疑似静默失败
                    Err(e) => {
                        logger::log(&app, slot, "error", &format!("点 X 隐藏窗口失败（窗口保持可见）：{e}"));
                        false
                    }
                };
                // 仅隐藏成功才更新状态
                if hidden {
                    sync_state(&app, slot, |s| s.visible = false);
                }
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
    // 启动成功：隐藏设置窗口（还原原版 loginForm.show(false)）。
    // 经主线程派发执行，避免从后台线程直接调窗口 API 的兼容性问题
    hide_login(app);
    Ok(warnings)
}

/// 隐藏设置窗口（进入成功后）。派发到主线程执行；失败会写 error 日志，
/// 不再静默吞掉（此前失败无感知，用户看到的就是「设置窗口不消失」）
pub fn hide_login(app: &AppHandle) {
    let h = app.clone();
    let res = app.run_on_main_thread(move || {
        if let Some(w) = h.get_webview_window("login") {
            if let Err(e) = w.hide() {
                logger::log(&h, 0, "error", &format!("设置窗口隐藏失败：{e}"));
            }
        } else {
            logger::log(&h, 0, "error", "设置窗口隐藏失败：找不到 login 窗口");
        }
    });
    if let Err(e) = res {
        logger::log(app, 0, "error", &format!("设置窗口隐藏失败（主线程派发）：{e}"));
    }
}

fn slot_of_label(label: &str) -> u32 {
    label.strip_prefix("browser-").and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// 菜单事件去重：Windows 下每次菜单点击会触发【两次】menu event（muda 已知行为，
/// 实测两次间隔约 5~6ms）。不去重的后果：「窗口置顶」开了立即被取消、
/// 「首页」连刷两次、显示/隐藏状态反复横跳。
/// 同一 id 在 300ms 内的重复事件直接丢弃。
fn menu_event_dup(id: &str) -> bool {
    static LAST: Mutex<Option<(String, std::time::Instant)>> = Mutex::new(None);
    let mut g = LAST.lock().unwrap();
    let now = std::time::Instant::now();
    if let Some((last_id, at)) = g.as_ref() {
        if last_id == id && now.duration_since(*at) < std::time::Duration::from_millis(300) {
            return true;
        }
    }
    *g = Some((id.to_string(), now));
    false
}

/// 应用级菜单事件（窗口菜单 + 托盘菜单统一在此处理）
pub fn handle_menu_event(app: &AppHandle, id: &str) {
    if menu_event_dup(id) {
        return;
    }
    // 全局项（「新开账号」入口已随多 exe 多开移除，不再有 open-settings）
    if id == "quit" {
        quit_all(app);
        return;
    }

    // 带槽位后缀的项：home-3 / top-3 / data-3
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
                // 打开该帐号的数据目录（含 WebView2 数据；日志统一在数据根目录）
                "data" => open_slot_data_dir(app, slot),
                _ => {}
            }
        }
    }
}



/// 彻底退出程序（还原原版 win.quitMessage 语义）：
/// 1. 常规路径：移除托盘 → 销毁全部窗口 → app.exit(0)
/// 2. 硬杀兜底：1.5 秒后进程仍未退出（插件/托盘/事件循环卡死）则 std::process::exit
///    直接终止——保证「退出」在任何异常下都一定生效
pub fn quit_all(app: &AppHandle) {
    if app
        .state::<AppState>()
        .quitting
        .swap(true, Ordering::SeqCst)
    {
        return; // 已在退出流程中，避免重复触发
    }
    logger::log(app, 0, "sys", "开始退出程序：销毁全部窗口");
    // 硬杀兜底（selftest 模式下退出码为 98，可区分「靠兜底才退掉」）
    let guard_code = app.state::<AppState>().force_exit_code.load(Ordering::SeqCst);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        std::process::exit(guard_code);
    });
    // 先逐个隐藏托盘图标（Windows 上立即 NIM_DELETE，从托盘区移除），再 drop。
    // 仅靠 clear() 的 drop 依赖「最后一个引用释放时移除」，但下方 1.5 秒硬杀兜底
    // （std::process::exit）会跳过 Tauri 清理流程，可能留下残留图标。显式
    // set_visible(false) 立即从托盘区消失，即便硬杀打断也不会有视觉残留。
    // 先绑定 state 延长生命周期：app.state() 返回临时值，直接链式 .trays.lock()
    // 的话临时 State 在语句末 drop，而 MutexGuard 跨 for 循环借用它 → E0716。
    {
        let state = app.state::<AppState>();
        let trays = state.trays.lock().unwrap();
        for tray in trays.values() {
            let _ = tray.set_visible(false);
        }
    }
    // 移除全部托盘图标（还原原版退出前 tray.delete()）
    app.state::<AppState>().trays.lock().unwrap().clear();
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

/// 原生看门狗：窗口隐藏或最小化时 WebView2/Chromium 会把页面 JS 定时器节流到
/// 分钟级（隐藏：后台定时器节流；最小化：页面按后台页处理，5 分钟后节流到
/// 约 1 次/分钟），由 Rust 侧周期性 eval 调用页面内 tick 接管驱动，保证后台
/// 保活不掉线。
/// 注意：窗口可见且未最小化时【必须跳过】——页面自身 setInterval(1000) 在跑，
/// 双驱动会让 actionTick 的 5 秒周期实际变成 2.5 秒（点击频率翻倍）。
/// 最小化必须与隐藏同等对待：Win32 对最小化窗口 IsWindowVisible 返回 TRUE
/// （tao 的 is_visible() 为 true），但页面已被 Chromium 当后台页节流——
/// 标题栏去掉最小化按钮（minimizable(false)）挡不住 Win+D / 显示桌面 /
/// Win+Down 等系统级最小化，此前这类窗口的保活会静默停摆且无任何日志。
/// 周期 1 秒 = 原版 stopTimer；页面内 tick 自行分流：
///   - 每 1 秒：退出/到期检测（stopTimer 语义）
///   - 每 intervalMs：保活动作（runTimer 5 秒语义）
fn spawn_watchdog(app: AppHandle, slot: u32) {
    let task_app = app.clone();
    let handle = tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(1000));
        // 状态转换只记一次日志（避免每秒刷屏）：最小化接管 / 恢复
        let mut was_minimized_driven = false;
        // eval 连续失败计数：首次与每 30 次落盘防刷屏；连续 10 次给出明确结论
        let mut eval_fails: u32 = 0;
        let mut escalated = false;
        loop {
            ticker.tick().await;
            let win = match task_app.get_webview_window(&slot_label(slot)) {
                Some(w) => w,
                None => break,
            };
            let visible = win.is_visible().unwrap_or(true);
            let minimized = win.is_minimized().unwrap_or(false);
            sync_state(&task_app, slot, |s| {
                s.running = true;
                s.visible = visible;
            });
            // 仅隐藏/最小化时驱动：可见且未最小化时页面定时器自己在跑，
            // eval 会造成双倍 tick
            let need_drive = !visible || minimized;
            if minimized && !was_minimized_driven {
                was_minimized_driven = true;
                logger::log(
                    &task_app,
                    slot,
                    "sys",
                    "窗口被最小化（Win+D/显示桌面等）：Chromium 将节流页内定时器，看门狗已接管保活驱动（恢复窗口请用托盘左键或老板键）",
                );
            } else if !minimized {
                was_minimized_driven = false;
            }
            if need_drive {
                if let Err(e) = win.eval("try{window.__CPK_TICK__&&window.__CPK_TICK__()}catch(e){}") {
                    eval_fails += 1;
                    // 每秒一条 error 会把日志刷成噪音（一天数万条）：首次与每 30 次
                    //（约 30 秒）各落一条，足够定位又不淹没问题日志
                    if eval_fails == 1 || eval_fails % 30 == 0 {
                        logger::log(&task_app, slot, "error", &format!("看门狗 eval 失败(连续第 {eval_fails} 次): {e}"));
                    }
                    if eval_fails >= 10 && !escalated {
                        escalated = true;
                        logger::log(
                            &task_app,
                            slot,
                            "error",
                            "保活驱动已中断（连续 eval 失败，页面可能已崩溃）：请重新显示窗口或重启程序",
                        );
                    }
                } else {
                    eval_fails = 0;
                    escalated = false;
                }
            } else {
                eval_fails = 0;
                escalated = false;
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
            }
            _ => {
                let _ = win.unminimize();
                let _ = win.show();
                let _ = win.set_focus();
                reapply_topmost(app, slot);
                sync_state(app, slot, |s| s.visible = true);
                logger::log(app, slot, "sys", "窗口已显示");
            }
        }
    }
}

pub fn show_slot(app: &AppHandle, slot: u32, visible: bool) {
    if let Some(win) = app.get_webview_window(&slot_label(slot)) {
        let r = if visible {
            // unminimize 必须先于 show：skip_taskbar 窗口最小化后直接 show 仍处于最小化态，
            // 导致「点托盘看似无反应」（实际窗口已显示但还在任务栏外最小化栏）
            let _ = win.unminimize();
            let a = win.show();
            let _ = win.set_focus();
            reapply_topmost(app, slot);
            a
        } else {
            win.hide()
        };
        if let Err(e) = r {
            logger::log(
                app,
                slot,
                "error",
                &format!("窗口{}失败：{e}", if visible { "显示" } else { "隐藏" }),
            );
        }
        sync_state(app, slot, |s| s.visible = visible);
    }
}

pub fn set_topmost(app: &AppHandle, slot: u32, top: bool) -> Result<(), String> {
    if let Some(win) = app.get_webview_window(&slot_label(slot)) {
        win.set_always_on_top(top)
            .map_err(|e| format!("置顶设置失败: {e}"))?;
        sync_state(app, slot, |s| s.topmost = top);
        logger::log(
            app,
            slot,
            "sys",
            if top {
                "窗口已置顶（悬浮于其他窗口之上；再次点击「窗口置顶」取消）"
            } else {
                "已取消置顶"
            },
        );
        Ok(())
    } else {
        Err("窗口未启动".into())
    }
}

/// 窗口重新显示后重新应用置顶。Windows 下 hide/show 循环会丢失
/// WS_EX_TOPMOST（老板键隐藏再呼出、托盘显隐后「置顶失效」的根因），
/// 每次显示时按记录状态补一次。
fn reapply_topmost(app: &AppHandle, slot: u32) {
    let top = {
        let state: tauri::State<AppState> = app.state();
        let states = state.states.lock().unwrap();
        states.get(&slot).map(|s| s.topmost).unwrap_or(false)
    };
    if top {
        if let Some(win) = app.get_webview_window(&slot_label(slot)) {
            let _ = win.set_always_on_top(true);
        }
    }
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

/// 打开某帐号的数据目录（WebView2 数据与该帐号日志所在；程序级日志在数据根目录）。
/// 优先用「实际运行目录」登记表：数据目录被锁换用 -r2 兜底目录时，
/// WebView2 数据在兜底目录里——按配置名重算会打开旧目录（里面没有数据）
fn open_slot_data_dir(app: &AppHandle, slot: u32) {
    use tauri_plugin_opener::OpenerExt;
    let name = {
        let state: tauri::State<AppState> = app.state();
        let cfg = state.config.lock().unwrap();
        cfg.slots
            .iter()
            .find(|s| s.slot == slot)
            .map(|s| s.name.clone())
            .unwrap_or_default()
    };
    let dir = logger::slot_dir(slot)
        .or_else(|| {
            if name.trim().is_empty() {
                None
            } else {
                Some(config::profile_dir(slot, &name))
            }
        })
        .unwrap_or_else(config::base_dir);
    logger::log(
        app,
        slot,
        "sys",
        &format!("打开数据目录：{}（该帐号日志就在本目录内，程序级日志在数据根目录 cpk-*.log）", dir.display()),
    );
    let _ = app
        .opener()
        .open_path(dir.to_string_lossy().to_string(), None::<&str>);
}

fn sync_state(app: &AppHandle, slot: u32, f: impl FnOnce(&mut SlotState)) {
    let state: tauri::State<AppState> = app.state();
    let mut states = state.states.lock().unwrap();
    f(states.entry(slot).or_insert_with(|| SlotState::new(slot)));
}

// ---------------------------------------------------------------------------
// 托盘（还原原版：每个云手机窗口创建自己的 win.util.tray）
// 菜单：首页/窗口置顶/分隔/打开数据目录/分隔/退出
// （各项均无 ●/○ 前缀，文字左对齐；左键单击或双击=打开窗口，右键=弹出菜单）
// ---------------------------------------------------------------------------

// 【多 exe 多开】不建全局托盘，托盘菜单也没有「新开账号」入口：每运行一个 exe
// 即一个独立实例（启动显示设置窗口 →「进入」后得到本实例自己的云手机窗口与
// 独立托盘图标）。需要多开一个云手机 = 再运行一个 exe 实例，各实例的窗口、
// 托盘、老板键状态互不干扰（数据目录冲突由 -r2/-r3 换目录兜底保护）。

/// 为某个槽位窗口创建独立托盘图标
pub fn create_slot_tray(app: &AppHandle, slot: u32) {
    let menu = match slot_tray_menu(app, slot) {
        Ok(m) => m,
        Err(e) => {
            logger::log(app, slot, "error", &format!("托盘创建失败（不影响保活）：{e}"));
            return;
        }
    };
    let (platform_label, account_name) = {
        let state: tauri::State<AppState> = app.state();
        let cfg = state.config.lock().unwrap();
        cfg.slots
            .iter()
            .find(|s| s.slot == slot)
            .map(|s| {
                (
                    config::platform_preset(&s.platform).label.to_string(),
                    s.name.trim().to_string(),
                )
            })
            .unwrap_or_else(|| ("云手机保活".into(), String::new()))
    };
    // 多 exe 多开时多个实例的托盘图标外观相同，提示文字带上帐号名便于区分
    let tooltip = if account_name.is_empty() {
        platform_label
    } else {
        format!("{platform_label} - {account_name}")
    };
    let mut builder = TrayIconBuilder::with_id(format!("tray-{slot}"))
        .tooltip(&tooltip)
        .menu(&menu)
        // 左键不再弹菜单（默认 true）：左键=打开窗口，右键=弹出菜单
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| handle_menu_event(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            // 左键单击（抬起）或双击 = 显示并聚焦该托盘对应的云手机窗口
            // 同时支持单击与双击：部分 Windows 触摸板/旧系统对托盘 Up 事件丢事件，
            // 双击作为兜底入口，保证「点托盘必出窗口」
            let fire = matches!(
                event,
                tauri::tray::TrayIconEvent::Click {
                    button: tauri::tray::MouseButton::Left,
                    button_state: tauri::tray::MouseButtonState::Up,
                    ..
                } | tauri::tray::TrayIconEvent::DoubleClick {
                    button: tauri::tray::MouseButton::Left,
                    ..
                }
            );
            if fire {
                let app = tray.app_handle().clone();
                let n = tray
                    .id()
                    .as_ref()
                    .strip_prefix("tray-")
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0);
                if (1..=9).contains(&n) {
                    logger::log(&app, n, "sys", "托盘左键 → 显示窗口");
                    show_slot(&app, n, true);
                }
            }
        });

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

fn slot_tray_menu(app: &AppHandle, slot: u32) -> Result<tauri::menu::Menu<tauri::Wry>, tauri::Error> {
    // 菜单项一律不加 ●/○ 前缀：带前缀时「首页/退出」等无标记项与有标记项
    // 文字起点错位不对齐，且用户不需要圆点标记。
    // 「显示窗口/隐藏窗口」入口已移除：左键单击/双击托盘图标即呼出窗口（更直观，
    // 也避免与老板键 Ctrl+N 的显隐切换语义重叠造成误解）。
    // 「新开账号」入口已移除：多开改为多 exe 运行（再启动一个程序实例即可），
    // 每个实例只管理自己的云手机窗口，托盘菜单不再承担开新帐号的职责。
    let home = MenuItemBuilder::with_id(format!("home-{slot}"), "首页").build(app)?;
    let top = MenuItemBuilder::with_id(format!("top-{slot}"), "窗口置顶").build(app)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let datadir = MenuItemBuilder::with_id(format!("data-{slot}"), "打开数据目录").build(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let quit = MenuItemBuilder::with_id("quit", "退出").build(app)?;

    let menu = MenuBuilder::new(app)
        .item(&home)
        .item(&top)
        .item(&sep1)
        .item(&datadir)
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
    logger::log(app, slot, "sys", &format!("老板键 {code} 已注册（全局热键：按一下隐藏窗口，再按一下显示）"));
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
        match app.global_shortcut().register(sc) {
            Ok(()) => {
                app.state::<AppState>()
                    .shortcut_ids
                    .lock()
                    .unwrap()
                    .insert(sc.id(), 0);
            }
            Err(e) => {
                logger::log(
                    app,
                    0,
                    "error",
                    &format!("Ctrl+U 地址栏热键注册失败（地址栏功能不可用，可能被其他程序占用）：{e}"),
                );
            }
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
    // 触发即留痕：老板键「没反应」时，凭日志能立刻区分
    // 「热键没注册上/被别的程序占了」还是「触发但切换逻辑有问题」
    if let Some(n) = slot {
        let desc = if n == 0 { "Ctrl+U 地址栏".to_string() } else { format!("老板键 Ctrl+{n}") };
        logger::log(app, n, "sys", &format!("全局热键触发：{desc}"));
    }
    match slot {
        Some(0) => toggle_address_bar(app),
        Some(n) => toggle_slot(app, n),
        None => {}
    }
}
