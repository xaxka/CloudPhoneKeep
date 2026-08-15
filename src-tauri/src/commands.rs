use crate::browser;
use crate::config::SlotConfig;
use crate::logger;
use crate::AppState;
use tauri::{AppHandle, Manager};

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
/// 还原原版校验：缓存数据目录名为空 → 阻止启动（原版弹 msgbox，这里返回错误由前端横幅展示）。
/// 返回非致命警告（老板键被占用等）由前端醒目展示；Err 仅在窗口无法创建时返回。
#[tauri::command]
pub fn launch_slot(app: AppHandle, cfg: SlotConfig) -> Result<Vec<String>, String> {
    if cfg.name.trim().is_empty() {
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

    let warnings = match browser::start_slot_ex(&app, cfg.slot) {
        Ok(w) => w,
        Err(e) => {
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
    };

    // 启动成功后隐藏设置窗口（与原版一致：loginForm.show(false)）
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
    let (w, h) = if w < 280.0 || h < 400.0 {
        return Err("分辨率过小（最小 280 x 400）".into());
    } else {
        (w, h)
    };
    browser::apply_slot_size(&app, slot, w, h)
}
