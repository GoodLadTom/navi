//! Checks that the meeting card (the "prompt" window) shows with no title bar. It builds the window
//! exactly as Navi does, shows it far off-screen (so nothing appears on the desktop) first the old
//! way (SetWindowPos + ShowWindow only) and then through `meeting::check_card_frame`, and prints
//! the window's outer and client rectangles each time. "frame: none" means no title bar.
//!
//! Run: cargo run --example card_check
//! It uses its own WebView2 data folder and no single-instance plugin, so a running Navi is left
//! alone.

use std::time::Duration;
use tauri::{WebviewUrl, WebviewWindowBuilder};

fn main() {
    tauri::Builder::default()
        .device_event_filter(tauri::DeviceEventFilter::Always)
        .setup(|app| {
            let w = WebviewWindowBuilder::new(app, "prompt", WebviewUrl::App("prompt.html".into()))
                .data_directory(std::env::temp_dir().join("navi-card-check-webview"))
                .title("")
                .inner_size(372.0, 132.0)
                .decorations(false)
                .transparent(true)
                .shadow(false)
                .always_on_top(true)
                .skip_taskbar(true)
                .resizable(false)
                .maximizable(false)
                .minimizable(false)
                .focused(false)
                .visible(false)
                .build()?;
            let raw = w.hwnd()?.0 as isize;
            navi_lib::meeting::guard_card(raw);
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(1500));
                println!("as built:    {}", report(raw));
                let mode = std::env::args().nth(1).unwrap_or_default();
                if mode == "old" {
                    // The first card at the size the window was built at: no size change, so
                    // Windows never recalculates the frame. This is the original bug.
                    old_way(raw, 372, 132);
                    std::thread::sleep(Duration::from_millis(400));
                    println!("old way:     {}", report(raw));
                    old_hide(raw);
                } else {
                    println!("new way:     {}", navi_lib::meeting::check_card_frame(raw, -9000, -9000, 372, 132));
                    std::thread::sleep(Duration::from_millis(400));
                    println!("a bit later: {}", report(raw));
                    println!("other size:  {}", navi_lib::meeting::check_card_frame(raw, -9000, -9000, 336, 76));
                    // The later bug: something gives the window its title bar back.
                    tao_again(raw);
                    std::thread::sleep(Duration::from_millis(300));
                    println!("title bar put back: {}", report(raw));
                }
                handle.exit(0);
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("the card check couldn't start");
}

/// Puts the title bar and sizing frame styles back on the window and has Windows redo its frame,
/// as tao does now and then. The card's guard should refuse both.
fn tao_again(raw: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_STYLE, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WS_CAPTION, WS_THICKFRAME,
    };
    unsafe {
        let hwnd = HWND(raw as *mut std::ffi::c_void);
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        SetWindowLongPtrW(hwnd, GWL_STYLE, (style | WS_CAPTION.0 | WS_THICKFRAME.0) as isize);
        let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

/// What Navi's first version did: place and show the window, nothing more.
fn old_way(raw: isize, w: i32, h: i32) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowPos, ShowWindow, HWND_TOPMOST, SWP_NOACTIVATE, SW_SHOWNOACTIVATE};
    unsafe {
        let hwnd = HWND(raw as *mut std::ffi::c_void);
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), -9000, -9000, w, h, SWP_NOACTIVATE);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
}

fn old_hide(raw: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
    unsafe {
        let _ = ShowWindow(HWND(raw as *mut std::ffi::c_void), SW_HIDE);
    }
}

fn report(raw: isize) -> String {
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, GetWindowRect};
    unsafe {
        let hwnd = HWND(raw as *mut std::ffi::c_void);
        let (mut outer, mut inner) = (RECT::default(), RECT::default());
        let _ = GetWindowRect(hwnd, &mut outer);
        let _ = GetClientRect(hwnd, &mut inner);
        let mut origin = POINT::default();
        let _ = ClientToScreen(hwnd, &mut origin);
        let same = (outer.right - outer.left, outer.bottom - outer.top, outer.left, outer.top) == (inner.right, inner.bottom, origin.x, origin.y);
        format!(
            "outer {}x{} at ({}, {}); client {}x{} at ({}, {}); frame: {}",
            outer.right - outer.left,
            outer.bottom - outer.top,
            outer.left,
            outer.top,
            inner.right,
            inner.bottom,
            origin.x,
            origin.y,
            if same { "none" } else { "PRESENT" }
        )
    }
}
