//! The recording pill window: always on top, never takes focus, clicks pass through it.
//!
//! OWNER: Windows systems agent. STATUS: implemented.
//!
//! Contract:
//! * `prepare_pill` runs once after the "pill" webview window is built (transparent, no
//!   decorations, skip taskbar, always on top, hidden). It adds WS_EX_NOACTIVATE |
//!   WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT (and keeps WS_EX_LAYERED if Tauri set it) and makes the
//!   window ignore the cursor, so it can never steal focus from the app being dictated into.
//! * `show_pill` sizes the transparent window to the work area of the monitor containing
//!   `near_hwnd` (fall back to the cursor's monitor). The HTML centres the pill at the bottom,
//!   while the firefly can travel across the whole screen. It shows the window with
//!   SW_SHOWNOACTIVATE and HWND_TOPMOST without activating.
//! * `hide_pill` hides it (SW_HIDE). Both are cheap and safe to call repeatedly.
//!
//! Details worth knowing:
//! * Monitor bounds are in physical pixels. The HTML's CSS gap scales with the webview DPI.
//! * Showing and hiding go straight to Win32, not through Tauri's show()/hide(): Tauri's show uses
//!   SW_SHOW, which can activate the window. So `WebviewWindow::is_visible()` does not track the pill.
//! * tao rewrites the extended style from its own flags whenever they change, which would drop
//!   WS_EX_TOOLWINDOW; `show_pill` re-applies the styles every time (a no-op when they are intact).
//!   `prepare_pill` also sets Tauri's focusable(false) and ignore-cursor flags so tao keeps
//!   WS_EX_NOACTIVATE, WS_EX_TRANSPARENT and WS_EX_LAYERED itself.
//! * Moving the pill to a monitor with a different scale can make tao resize it (WM_DPICHANGED);
//!   `show_pill` applies the monitor bounds twice if needed.
//! * These make synchronous calls to the window's (main) thread: do not call them from a thread the
//!   main thread is blocked waiting on.

use anyhow::Result;
use tauri::WebviewWindow;

/// A rectangle in physical screen pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// The transparent overlay must cover the selected monitor's usable area for full-screen flight.
pub fn overlay_bounds(work: Rect) -> (i32, i32, i32, i32) {
    (work.left, work.top, work.right - work.left, work.bottom - work.top)
}

pub fn prepare_pill(window: &WebviewWindow) -> Result<()> {
    // Through Tauri first, so tao's own style bookkeeping agrees with what we set below.
    if let Err(e) = window.set_ignore_cursor_events(true) {
        log::warn!("Could not make the recording bubble click-through: {e}");
    }
    if let Err(e) = window.set_focusable(false) {
        log::warn!("Could not make the recording bubble unfocusable: {e}");
    }
    #[cfg(windows)]
    {
        let hwnd = win::pill_hwnd(window)?;
        win::apply_styles(hwnd);
    }
    Ok(())
}

pub fn show_pill(window: &WebviewWindow, near_hwnd: isize) -> Result<()> {
    #[cfg(windows)]
    {
        let hwnd = win::pill_hwnd(window)?;
        win::apply_styles(hwnd);
        win::position_and_show(hwnd, near_hwnd)
    }
    #[cfg(not(windows))]
    {
        let _ = near_hwnd;
        window.show()?;
        Ok(())
    }
}

pub fn hide_pill(window: &WebviewWindow) -> Result<()> {
    #[cfg(windows)]
    {
        let hwnd = win::pill_hwnd(window)?;
        win::hide(hwnd);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        window.hide()?;
        Ok(())
    }
}

#[cfg(windows)]
mod win {
    use super::*;
    use anyhow::anyhow;
    use parking_lot::Mutex;
    use std::ffi::c_void;
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, GetWindowLongPtrW, GetWindowRect, IsWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow, GWL_EXSTYLE, HWND_TOPMOST,
        SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_SHOWNOACTIVATE, WINDOW_EX_STYLE,
        WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
    };

    /// (window label, raw HWND) of the pill, so Tauri is asked only once.
    static PILL: Mutex<Option<(String, isize)>> = parking_lot::const_mutex(None);

    pub(super) fn pill_hwnd(window: &WebviewWindow) -> Result<HWND> {
        let label = window.label();
        if let Some((cached_label, raw)) = PILL.lock().as_ref() {
            let hwnd = HWND(*raw as *mut c_void);
            if cached_label == label && unsafe { IsWindow(Some(hwnd)) }.as_bool() {
                return Ok(hwnd);
            }
        }
        let tauri_hwnd = window.hwnd().map_err(|e| anyhow!("Navi couldn't find the recording bubble window ({e})."))?;
        // Tauri's HWND comes from its own copy of the windows crate; only the raw value carries over.
        let raw = tauri_hwnd.0 as isize;
        *PILL.lock() = Some((label.to_string(), raw));
        Ok(HWND(raw as *mut c_void))
    }

    pub(super) fn apply_styles(hwnd: HWND) {
        unsafe {
            let current = WINDOW_EX_STYLE(GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32);
            // APPWINDOW would force a taskbar button and defeat TOOLWINDOW; everything else is kept,
            // including WS_EX_LAYERED and WS_EX_TOPMOST.
            let wanted = (current | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT) & !WS_EX_APPWINDOW;
            if wanted != current {
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, wanted.0 as isize);
                // Cached style bits only refresh after a frame change.
                let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
            }
        }
    }

    fn target_monitor(near: isize) -> HMONITOR {
        unsafe {
            if near != 0 {
                let hwnd = HWND(near as *mut c_void);
                if IsWindow(Some(hwnd)).as_bool() {
                    let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL);
                    if !monitor.0.is_null() {
                        return monitor;
                    }
                }
            }
            let mut cursor = POINT::default();
            let _ = GetCursorPos(&mut cursor);
            MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST)
        }
    }

    fn work_area(monitor: HMONITOR) -> Option<Rect> {
        unsafe {
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                return None;
            }
            let r = info.rcWork;
            Some(Rect { left: r.left, top: r.top, right: r.right, bottom: r.bottom })
        }
    }

    fn window_rect(hwnd: HWND) -> Result<RECT> {
        let mut rect = RECT::default();
        unsafe { GetWindowRect(hwnd, &mut rect) }.map_err(|e| anyhow!("Navi couldn't measure the recording bubble ({e})."))?;
        Ok(rect)
    }

    pub(super) fn position_and_show(hwnd: HWND, near: isize) -> Result<()> {
        let work = work_area(target_monitor(near)).ok_or_else(|| anyhow!("Navi couldn't find a screen to show the recording bubble on."))?;
        let (x, y, width, height) = overlay_bounds(work);
        for _ in 0..2 {
            unsafe { SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, width, height, SWP_NOACTIVATE) }
                .map_err(|e| anyhow!("Navi couldn't size the recording overlay ({e})."))?;
            // Crossing to a monitor with another scale can make tao resize the window.
            let after = window_rect(hwnd)?;
            if (after.left, after.top, after.right, after.bottom) == (work.left, work.top, work.right, work.bottom) {
                break;
            }
        }
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            // Back to the top of the topmost band, now that it is visible.
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
        Ok(())
    }

    pub(super) fn hide(hwnd: HWND) {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_uses_the_monitor_work_area() {
        let work = Rect { left: 0, top: 0, right: 1920, bottom: 1040 };
        assert_eq!(overlay_bounds(work), (0, 0, 1920, 1040));
    }

    #[test]
    fn secondary_monitor_to_the_right() {
        let work = Rect { left: 1920, top: 0, right: 4480, bottom: 1400 };
        assert_eq!(overlay_bounds(work), (1920, 0, 2560, 1400));
    }

    #[test]
    fn monitor_left_of_and_above_the_primary() {
        let work = Rect { left: -2560, top: -200, right: 0, bottom: 1240 };
        assert_eq!(overlay_bounds(work), (-2560, -200, 2560, 1440));
    }

    #[test]
    fn taskbar_on_the_left_shifts_the_work_area() {
        let work = Rect { left: 62, top: 0, right: 1920, bottom: 1080 };
        assert_eq!(overlay_bounds(work), (62, 0, 1858, 1080));
    }
}
