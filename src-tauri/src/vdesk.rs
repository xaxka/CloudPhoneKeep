//! Windows 虚拟桌面绑定（多桌面修复）
//!
//! ## 问题背景
//! 云手机窗口 `skip_taskbar(true)`（tao 在 Windows 用 ITaskbarList::DeleteTab
//! 移除任务栏按钮）。资源管理器把「归属虚拟桌面」的登记挂在任务栏按钮的
//! 生命周期上：没有任务栏按钮的窗口不登记到任何桌面 → Win+Ctrl+←/→ 切换
//! 桌面时它在**每个**桌面都显示（多桌面用户报告：第二个桌面还能看到窗口）。
//!
//! ## 修复方案
//! 用文档化 Win32 API（Win10+，shobjidl_core）把窗口显式绑定到「当前虚拟
//! 桌面」：
//!   1. CoCreateInstance(CLSID_VirtualDesktopManager) → IVirtualDesktopManager
//!   2. 当前桌面 ID = GetWindowDesktopId(代表窗口)。代表窗口优先用 shell
//!      窗口（桌面上常驻的 explorer 窗口，必然登记在当前桌面），不可用时
//!      退回 GetForegroundWindow（本程序自己的窗口刚被聚焦时也成立）
//!   3. MoveWindowToDesktop(hwnd, 当前桌面 ID)
//!
//! 绑定到单个桌面后窗口具备正常桌面归属，但切到别的桌面时它会被 DWM
//! 「cloak」（保留 WS_VISIBLE 但不再呈现，见 Raymond Chen《How can I detect
//! that my window has been suppressed》）。由此派生两个配套行为：
//!   - is_cloaked()：DWMWA_CLOAKED 检测，看门狗把「被 cloak」按隐藏同等
//!     对待——WebView2 侧页面此时按后台页处理，JS 定时器被节流，必须由
//!     Rust 看门狗接管驱动，否则用户在其他桌面期间保活静默降级
//!   - toggle_slot 的老板键：窗口在别的桌面显示时按「呼出到当前桌面」
//!     处理，而不是误判为可见而隐藏
//!
//! ## 线程模型
//! COM 在调用线程初始化（COINIT_APARTMENTTHREADED）。本模块被主线程与
//! 老板键回调线程调用：每处 init 成功才配对 CoUninitialize；线程此前已用
//! 其他模式初始化过 COM（RPC_E_CHANGED_MODE）时直接沿用，不重复 uninit。
//!
//! 非 Windows 平台本模块整体 no-op（本项目仅发布 Windows 版，保留跨平台
//! 可编译性）。

#[cfg(windows)]
use windows::Win32::Foundation::HWND;
#[cfg(windows)]
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
#[cfg(windows)]
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
};
#[cfg(windows)]
use windows::Win32::UI::Shell::{IVirtualDesktopManager, VirtualDesktopManager};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetShellWindow};

/// 把窗口绑定到当前虚拟桌面（云手机窗口与设置窗口共用）。
/// 幂等：已在当前桌面时无操作。返回是否实际移动。
#[cfg(windows)]
pub fn pin_window(app: &tauri::AppHandle, win: &tauri::WebviewWindow, slot: u32) -> bool {
    let moved = unsafe { pin_hwnd(hwnd_of(win)) };
    match moved {
        Ok(true) => {
            crate::logger::log(app, slot, "sys", "窗口已绑定到当前虚拟桌面（skipTaskbar 窗口需显式登记，否则会在每个桌面都显示）");
            true
        }
        Ok(false) => false,
        Err(e) => {
            // 失败只降级日志：单桌面环境本就无感（桌面 ID 即唯一桌面）
            crate::logger::log(app, slot, "debug", &format!("虚拟桌面绑定未执行（无碍单桌面使用）：{e}"));
            false
        }
    }
}

/// 窗口是否在「当前」虚拟桌面（老板键判断用；出错按在当前桌面处理，
/// 维持既有的「可见→隐藏」语义，不改变单桌面行为）
#[cfg(windows)]
pub fn is_on_current_desktop(win: &tauri::WebviewWindow) -> bool {
    let hwnd = hwnd_of(win);
    if hwnd.is_invalid() {
        return true;
    }
    // 调用线程可能从未初始化过 COM（如全局热键回调线程）：配对 init/uninit
    let need_uninit = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    let result = unsafe {
        CoCreateInstance::<_, IVirtualDesktopManager>(&VirtualDesktopManager, None, CLSCTX_ALL)
    }
    .map(|vdm| {
        unsafe { vdm.IsWindowOnCurrentVirtualDesktop(hwnd) }
            .map(|b| b.as_bool())
            .unwrap_or(true)
    })
    .unwrap_or(true);
    if need_uninit {
        unsafe { CoUninitialize() };
    }
    result
}

/// 窗口是否被 DWM cloak（在其他虚拟桌面 / UWP 挂起）。看门狗据此接管驱动。
/// 出错按未 cloak 处理（保持原有行为）。
#[cfg(windows)]
pub fn is_cloaked(win: &tauri::WebviewWindow) -> bool {
    let mut cloaked: u32 = 0;
    let ok = unsafe {
        DwmGetWindowAttribute(
            hwnd_of(win),
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut core::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        )
    };
    ok.is_ok() && cloaked != 0
}

#[cfg(windows)]
fn hwnd_of(win: &tauri::WebviewWindow) -> HWND {
    win.hwnd().unwrap_or_default()
}

#[cfg(windows)]
unsafe fn pin_hwnd(hwnd: HWND) -> Result<bool, windows::core::Error> {
    if hwnd.is_invalid() {
        return Ok(false);
    }
    // COM init 配对：成功才 uninit；线程已用其他模式初始化（RPC_E_CHANGED_MODE）
    // 时沿用现状不重复 uninit
    let need_uninit = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    let result = unsafe { pin_hwnd_inner(hwnd) };
    if need_uninit {
        unsafe { CoUninitialize() };
    }
    result
}

#[cfg(windows)]
unsafe fn pin_hwnd_inner(hwnd: HWND) -> Result<bool, windows::core::Error> {
    let vdm: IVirtualDesktopManager =
        unsafe { CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL)? };

    // 窗口已在当前桌面：无事可做（老板键呼出路径可能就是这种情况）
    let on_current = unsafe { vdm.IsWindowOnCurrentVirtualDesktop(hwnd) }
        .map(|b| b.as_bool())
        .unwrap_or(false);
    if on_current {
        return Ok(false);
    }

    // 当前桌面 ID：优先 shell 窗口（桌面常驻，必然已登记）；不可用退回前台窗口
    let mut shell = unsafe { GetShellWindow() };
    if shell.is_invalid() {
        shell = unsafe { GetForegroundWindow() };
    }
    if shell.is_invalid() {
        // 探测不到代表窗口：拿不到「当前桌面」语义，按无事处理
        return Ok(false);
    }
    let current = match unsafe { vdm.GetWindowDesktopId(shell) } {
        Ok(id) => id,
        Err(_) => return Ok(false),
    };

    unsafe { vdm.MoveWindowToDesktop(hwnd, &current) }?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// 非 Windows：no-op 兜底（本项目仅发布 Windows 版，保持跨平台可编译）
// ---------------------------------------------------------------------------
#[cfg(not(windows))]
pub fn pin_window(_app: &tauri::AppHandle, _win: &tauri::WebviewWindow, _slot: u32) -> bool {
    false
}

#[cfg(not(windows))]
pub fn is_on_current_desktop(_win: &tauri::WebviewWindow) -> bool {
    true
}

#[cfg(not(windows))]
pub fn is_cloaked(_win: &tauri::WebviewWindow) -> bool {
    false
}
