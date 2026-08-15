use crate::browser;
use crate::config::SlotConfig;
use crate::logger;
use crate::AppState;
use tauri::{AppHandle, Manager};

/// 前端调试日志入口（login.js 把点击/调用结果回传到这里落盘，定位「点了没反应」）
#[tauri::command]
pub fn debug_log(app: AppHandle, msg: String) {
    logger::log(&app, 0, "debug", &msg);
}

/// 读取某个槽位配置（设置窗口预填）
#[tauri::command]
pub fn get_slot(app: AppHandle, slot: u32) -> Result<SlotConfig, String> {
    let state: tauri::State<AppState> = app.state();
    let cfg = state.config.lock().unwrap();
    let s = cfg
        .slots
        .iter()
        .find(|s| s.slot == slot)
        .cloned()
        .unwrap_or_else(|| {
            let mut d = SlotConfig::default();
            d.slot = slot;
            d
        });
    Ok(s.normalized())
}

/// 设置窗口「进入」：保存配置并启动窗口（还原原版 login.aardio showWebForm 流程）。
///
/// 【异步命令】必须 async：Tauri 同步命令在主线程执行，而 WebView2 窗口创建耗时数秒，
/// 会冻结整个 UI（= 用户反馈的「点击进入设置不消失」）。真正的创建工作再经
/// spawn_blocking 丢到独立线程，主线程完全不被阻塞。
#[tauri::command]
pub async fn launch_slot(app: AppHandle, cfg: SlotConfig) -> Result<Vec<String>, String> {
    logger::log(
        &app,
        0,
        "debug",
        &format!(
            "前端调用 launch_slot：slot={} 目录名=\"{}\" 平台={} 分辨率={}x{}",
            cfg.slot, cfg.name, cfg.platform, cfg.width, cfg.height
        ),
    );
    if cfg.name.trim().is_empty() {
        logger::log(&app, cfg.slot, "error", "进入被拒：缓存数据目录名为空");
        return Err("缓存数据目录名不能为空".into());
    }

    {
        let state: tauri::State<AppState> = app.state();
        let mut app_cfg = state.config.lock().unwrap();
        crate::config::upsert_slot(&mut app_cfg, cfg.clone());
        // 配置落盘失败不阻塞启动（例如 exe 放在只读目录），记日志继续
        if let Err(e) = crate::config::save(&app_cfg) {
            logger::log(
                &app,
                cfg.slot,
                "error",
                &format!("配置持久化失败（不影响本次启动，但重启后配置会丢失）：{e}"),
            );
        }
    }

    logger::log(&app, cfg.slot, "debug", "开始创建窗口（异步线程，不阻塞界面）");
    let slot = cfg.slot;
    let task_app = app.clone();
    let warnings = match tauri::async_runtime::spawn_blocking(move || {
        browser::start_slot_ex(&task_app, slot)
    })
    .await
    {
        Ok(Ok(w)) => w,
        Ok(Err(e)) => {
            // 启动失败：落盘 + 系统通知双保险，用户绝不可能毫无感知
            logger::log(&app, cfg.slot, "error", &format!("窗口启动失败：{e}"));
            use tauri_plugin_notification::NotificationExt;
            let _ = app
                .notification()
                .builder()
                .title("启动失败")
                .body(e.clone())
                .show();
            return Err(e);
        }
        Err(e) => {
            let msg = format!("启动任务异常（窗口创建线程崩溃）：{e}");
            logger::log(&app, cfg.slot, "error", &msg);
            return Err(msg);
        }
    };

    // 启动成功后隐藏设置窗口（与原版一致：loginForm.show(false)）
    logger::log(&app, cfg.slot, "debug", "窗口启动完成，隐藏设置窗口");
    if let Some(win) = app.get_webview_window("login") {
        let _ = win.hide();
    }
    Ok(warnings)
}

/// 正在运行的槽位列表（设置窗口提示用）
#[tauri::command]
pub fn get_running(app: AppHandle) -> Vec<u32> {
    let state: tauri::State<AppState> = app.state();
    let states = state.states.lock().unwrap();
    let mut v: Vec<u32> = states
        .iter()
        .filter(|(_, s)| s.running)
        .map(|(k, _)| *k)
        .collect();
    v.sort();
    v
}

/// 当前生效分辨率（会话内覆盖优先，其次配置）——「窗口设置」小窗预填用
#[tauri::command]
pub fn get_slot_size(app: AppHandle, slot: u32) -> Result<(f64, f64), String> {
    Ok(browser::slot_size(&app, slot))
}

/// 「窗口设置」小窗保存按钮（还原原版 settingWin：只改窗口与内存，不落盘）
#[tauri::command]
pub fn set_slot_size(app: AppHandle, slot: u32, w: f64, h: f64) -> Result<(), String> {
    if !(280.0..=4096.0).contains(&w) || !(400.0..=4096.0).contains(&h) {
        return Err("分辨率超出范围（宽 280~4096，高 400~4096）".into());
    }
    browser::apply_slot_size(&app, slot, w, h)
}
