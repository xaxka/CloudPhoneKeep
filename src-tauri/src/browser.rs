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
        reapply_topmost(app, slot);
        sync_state(app, slot, |s| {
            s.running = true;
            s.visible = true;
        });
        // 托盘菜单的 ● 显隐标记需要同步刷新（此前漏掉，重开后标记停留在旧状态）
        refresh_slot_tray(app, slot);
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

    // 生效分辨率：会话内覆盖（旋转/窗口设置）优先，其次配置值（还原原版 userInfo 语义）
    let (w, h) = {
        let state: tauri::State<AppState> = app.state();
        let states = state.states.lock().unwrap();
        states
            .get(&slot)
            .and_then(|s| s.size_override)
            .unwrap_or((cfg.width, cfg.height))
    };
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
            let mut builder = WebviewWindowBuilder::new(
                app,
                slot_label(slot),
                WebviewUrl::External(web_url.clone()),
            )
            .title(&title)
            .inner_size(w, h)
            .min_inner_size(280.0, 400.0)
            .resizable(true)
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
            if let Some(m) = &menu {
                builder = builder.menu(m.clone());
            }
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
                // 仅隐藏成功才更新状态，避免托盘 ● 标记与实际显隐相反
                if hidden {
                    sync_state(&app, slot, |s| s.visible = false);
                    refresh_slot_tray(&app, slot);
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
    // 预创建「窗口设置」小窗（隐藏）：菜单点击时瞬时弹出，无 1~2 秒建窗等待
    precreate_winset(app, slot);
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

/// 原生菜单栏（每个窗口独立，id 带槽位后缀）。
/// 还原原版 aardio 主菜单五项：首页/旋转/窗口置顶/设置/检查更新（无退出项）。
fn build_window_menu(app: &AppHandle, slot: u32) -> Result<tauri::menu::Menu<tauri::Wry>, tauri::Error> {
    MenuBuilder::new(app)
        .item(&MenuItemBuilder::with_id(format!("home-{slot}"), "首页").build(app)?)
        .item(&MenuItemBuilder::with_id(format!("rotate-{slot}"), "旋转").build(app)?)
        .item(&MenuItemBuilder::with_id(format!("top-{slot}"), "窗口置顶").build(app)?)
        .item(&MenuItemBuilder::with_id(format!("settings-{slot}"), "窗口设置").build(app)?)
        .item(&MenuItemBuilder::with_id("update-check", "检查更新").build(app)?)
        .build()
}

/// 菜单事件去重：Windows 下每次菜单点击会触发【两次】menu event（muda 已知行为，
/// 实测两次间隔约 5~6ms）。不去重的后果（用户日志实锤）：
///   「旋转」连转两次 = 没转；「窗口置顶」开了立即被取消；
///   「窗口设置」一次点出两个小窗。
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
    // 全局项
    match id {
        "open-settings" => return show_login(app, None),
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

    // 带槽位后缀的项：home-3 / rotate-3 / top-3 / settings-3 / show-3 / hide-3 / data-3
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
                // 打开该帐号的数据目录（含 WebView2 数据与本帐号日志）
                "data" => open_slot_data_dir(app, slot),
                _ => {}
            }
        }
    }
}

/// 防重复标记：菜单快速连点时，「查窗口不存在 → 建窗」两步之间没有原子性，
/// 两个线程都会走到建窗 → 出现 2 个窗口设置。建窗期间按槽位加锁去重。
static WINSET_OPENING: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// 建窗期间用户点了菜单 → 记下「想要显示」，建好后立即显示（不再要求用户点第二下）
static WINSET_WANTED: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// 构建「窗口设置」小窗（隐藏状态；显示由调用方决定）。
/// 尺寸 280x264：内容实际高度约 217px + 出错提示换行余量，原 218px 会贴边裁掉底部
fn build_winset(app: &AppHandle, slot: u32) -> Result<tauri::WebviewWindow, tauri::Error> {
    let win = WebviewWindowBuilder::new(
        app,
        &winset_label(slot),
        WebviewUrl::App(std::path::PathBuf::from("winset.html")),
    )
    .title("窗口设置")
    .inner_size(280.0, 264.0)
    .resizable(false)
    .visible(false)
    .build()?;
    // 关闭 = 隐藏（窗口保留，再次打开瞬时响应；退出走 quit_all 的 destroy，不受影响）
    let w = win.clone();
    win.on_window_event(move |e| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = e {
            api.prevent_close();
            let _ = w.hide();
        }
    });
    Ok(win)
}

/// 预创建「窗口设置」小窗（隐藏）：帐号窗口启动后即在后台线程建好，
/// 用户点菜单时只需 show——消除 WebView2 建窗的 1~2 秒等待。
/// 失败仅记日志，不影响帐号窗口；下次点菜单时走 open_winset 现建兜底
fn precreate_winset(app: &AppHandle, slot: u32) {
    let app2 = app.clone();
    std::thread::spawn(move || {
        let _guard = WinsetOpeningGuard(slot);
        if app2.get_webview_window(&winset_label(slot)).is_some() {
            return;
        }
        match build_winset(&app2, slot) {
            Ok(_) => logger::log(&app2, slot, "debug", "窗口设置小窗已预创建（隐藏待用）"),
            Err(e) => logger::log(&app2, slot, "debug", &format!("窗口设置预创建失败（点菜单时将现建）：{e}")),
        }
        // 预创建完成时若用户已在等菜单响应，立即显示
        if WINSET_WANTED.lock().unwrap().contains(&slot) {
            WINSET_WANTED.lock().unwrap().retain(|s| *s != slot);
            if let Some(w) = app2.get_webview_window(&winset_label(slot)) {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }
    });
}

/// RAII 防重复：线程结束时自动移除槽位标记（无论建窗成败）。
/// 用法：`let _guard = WinsetOpeningGuard(slot);`（元组结构体直接构造）
struct WinsetOpeningGuard(u32);
impl Drop for WinsetOpeningGuard {
    fn drop(&mut self) {
        WINSET_OPENING.lock().unwrap().retain(|s| *s != self.0);
    }
}

/// 「窗口设置」小窗（还原原版 settingWin：仅分辨率输入 + 保存，会话内生效不落盘，
/// 同一窗口单实例，关闭=隐藏可反复瞬时打开）
pub fn open_winset(app: &AppHandle, slot: u32) {
    if let Some(win) = app.get_webview_window(&winset_label(slot)) {
        // 单实例：已打开则聚焦（还原原版 isWinHwnd 拦截重复打开）
        let _ = win.show();
        let _ = win.set_focus();
        return;
    }
    {
        let mut opening = WINSET_OPENING.lock().unwrap();
        if opening.contains(&slot) {
            // 预创建还在进行中：记下「想要显示」，建好立即弹出
            let mut wanted = WINSET_WANTED.lock().unwrap();
            if !wanted.contains(&slot) {
                wanted.push(slot);
            }
            return;
        }
    }
    // 已知 Windows 陷阱（wry#583）：在菜单事件回调里同步创建 WebView2 窗口可能死锁，
    // 丢到独立线程创建，回调立即返回
    let app2 = app.clone();
    std::thread::spawn(move || {
        let _guard = WinsetOpeningGuard(slot);
        match build_winset(&app2, slot) {
            Ok(w) => {
                let _ = w.show();
                let _ = w.set_focus();
                logger::log(&app2, slot, "sys", "打开窗口设置（分辨率，仅本会话生效）")
            }
            Err(e) => logger::log(&app2, slot, "error", &format!("窗口设置打开失败：{e}")),
        }
    });
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
                reapply_topmost(app, slot);
                sync_state(app, slot, |s| s.visible = true);
                logger::log(app, slot, "sys", "窗口已显示");
                refresh_slot_tray(app, slot);
            }
        }
    }
}

pub fn show_slot(app: &AppHandle, slot: u32, visible: bool) {
    if let Some(win) = app.get_webview_window(&slot_label(slot)) {
        let r = if visible {
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
        refresh_slot_tray(app, slot);
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

/// 旋转：交换宽高并刷新页面（还原原版：仅改内存不落盘，重启后回到配置分辨率）
pub fn rotate_slot(app: &AppHandle, slot: u32) -> Result<(f64, f64), String> {
    let (w, h) = slot_size(app, slot);
    // 交换后钳制到窗口最小尺寸（min_inner_size 280x400），避免 set_size 被 clamp 后与记录不一致
    let swapped = (h.max(280.0), w.max(400.0));
    sync_state(app, slot, |s| s.size_override = Some(swapped));

    if let Some(win) = app.get_webview_window(&slot_label(slot)) {
        let _ = win.set_size(tauri::LogicalSize::new(swapped.0, swapped.1));
        // 还原原版 myWebView.go(location)：刷新当前页。但必须等新的窗口尺寸
        // 真正应用到 WebView2 之后再刷——立即 reload 时页面读到的是旧视口
        // 尺寸，云手机画面仍按旧方向渲染（=「旋转不生效」）
        let app2 = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(400));
            if let Some(w) = app2.get_webview_window(&slot_label(slot)) {
                let _ = w.eval("try{location.reload()}catch(e){}");
            }
        });
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

/// 打开某帐号的数据目录（含 WebView2 数据与本帐号日志）。
/// 优先用「实际运行目录」登记表：数据目录被锁换用 -r2 兜底目录时，
/// 日志与 WebView2 数据都在兜底目录里——按配置名重算会打开旧目录（里面没有日志）
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
        &format!("打开数据目录：{}（本帐号日志 cpk-*.log 也在此目录）", dir.display()),
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
// 菜单：显示(●)/隐藏(●)/分隔/新开账号/打开数据目录/分隔/退出
// （● = 当前状态；左键单击=打开窗口，右键=弹出菜单）
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
        // 左键不再弹菜单（默认 true）：左键=打开窗口，右键=弹出菜单
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| handle_menu_event(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            // 左键单击（抬起）= 显示并聚焦该托盘对应的云手机窗口
            if let tauri::tray::TrayIconEvent::Click {
                button: tauri::tray::MouseButton::Left,
                button_state: tauri::tray::MouseButtonState::Up,
                ..
            } = event
            {
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
    // 当前状态用 ● 标记（○ = 未处于该状态）。原版用「&● 显示 / 空格 显示」对齐，
    // 全角空格在部分系统渲染成多余空格，改用 ○ 天然等宽对齐
    let show = MenuItemBuilder::with_id(format!("show-{slot}"), if visible { "● 显示" } else { "○ 显示" }).build(app)?;
    let hide = MenuItemBuilder::with_id(format!("hide-{slot}"), if !visible { "● 隐藏" } else { "○ 隐藏" }).build(app)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let open_settings = MenuItemBuilder::with_id("open-settings", "新开账号").build(app)?;
    let datadir = MenuItemBuilder::with_id(format!("data-{slot}"), "打开数据目录").build(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let quit = MenuItemBuilder::with_id("quit", "退出").build(app)?;

    let menu = MenuBuilder::new(app)
        .item(&show)
        .item(&hide)
        .item(&sep1)
        .item(&open_settings)
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
