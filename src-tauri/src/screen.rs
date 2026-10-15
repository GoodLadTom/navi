//! Screenshots for call notes and "Hey Navi, watch" (watch.rs): a look at the screen now and then,
//! kept only when enough of it has changed, with the text on it read on this computer.
//!
//! * `capture` copies the monitor the window in front is on, scaled to at most `max_width` pixels
//!   wide (GDI). Apps listed under Settings > Context and privacy > "Never read these apps"
//!   (password managers) are never captured: no picture is taken while one is in front, and
//!   wherever else their windows show (beside the window in front, or peeking out from behind it)
//!   is blacked out before the picture is kept or read. A window hidden behind others is left be.
//! * `Frame::signature` is a 64x36 greyscale thumbnail. `changed` is the share of it that differs
//!   noticeably from another, so a ticking clock or a moving cursor never counts as a new screen.
//!   Compare with the last screenshot KEPT, so slow changes (a page being typed) still add up.
//! * `save_jpeg` writes the picture under the app data folder (shots/<note id>/), where the
//!   interface can show it. `read_text` is Windows' own OCR.
//! * Nothing here leaves the computer. The AI is only ever given the text read off a screenshot
//!   and the name of the window, never the picture.

use crate::state::App;
use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

/// Pictures are kept at most this wide: readable, and small enough to keep dozens.
pub const MAX_WIDTH: i32 = 1600;
/// A screenshot counts as new when this share of the thumbnail has changed.
pub const NEW_SCREEN: f32 = 0.04;
/// How much a thumbnail cell must change (out of 255) to count.
const CELL_CHANGE: u8 = 18;
const SIG_W: usize = 64;
const SIG_H: usize = 36;
/// Text kept from each screenshot.
const TEXT_CHARS: usize = 1_200;

pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Top-down BGRA, opaque.
    pub bgra: Vec<u8>,
    /// The app and window in front when it was taken.
    pub app: String,
    pub window: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature(Vec<u8>);

impl Frame {
    /// A 64x36 greyscale thumbnail of the picture.
    pub fn signature(&self) -> Signature {
        let (w, h) = (self.width as usize, self.height as usize);
        let mut out = vec![0u8; SIG_W * SIG_H];
        if w == 0 || h == 0 || self.bgra.len() < w * h * 4 {
            return Signature(out);
        }
        for cy in 0..SIG_H {
            let (y0, y1) = (cy * h / SIG_H, ((cy + 1) * h / SIG_H).max(cy * h / SIG_H + 1).min(h));
            for cx in 0..SIG_W {
                let (x0, x1) = (cx * w / SIG_W, ((cx + 1) * w / SIG_W).max(cx * w / SIG_W + 1).min(w));
                let (mut sum, mut n) = (0u64, 0u64);
                for y in (y0..y1).step_by(2) {
                    for x in (x0..x1).step_by(2) {
                        let i = (y * w + x) * 4;
                        let (b, g, r) = (self.bgra[i] as u64, self.bgra[i + 1] as u64, self.bgra[i + 2] as u64);
                        sum += (r * 299 + g * 587 + b * 114) / 1000;
                        n += 1;
                    }
                }
                out[cy * SIG_W + cx] = (sum / n.max(1)) as u8;
            }
        }
        Signature(out)
    }
}

/// The share of the thumbnail (0 to 1) that differs noticeably between two screenshots.
pub fn changed(a: &Signature, b: &Signature) -> f32 {
    if a.0.len() != b.0.len() || a.0.is_empty() {
        return 1.0;
    }
    let differing = a.0.iter().zip(&b.0).filter(|(x, y)| x.abs_diff(**y) > CELL_CHANGE).count();
    differing as f32 / a.0.len() as f32
}

/// The screen the window in front is on, unless that window belongs to an app that is never read.
/// Windows of those apps elsewhere on the screen are blacked out.
pub fn capture(excluded_apps: &[String]) -> Option<Frame> {
    let (app, window, process) = crate::context::foreground_app();
    if !process.is_empty() && crate::context::is_excluded(&process, excluded_apps) {
        return None;
    }
    // A screenshot of Navi itself says nothing (early test calls were full of them).
    if process.eq_ignore_ascii_case("navi.exe") {
        return None;
    }
    #[cfg(windows)]
    {
        let (width, height, bgra, monitor) = win::capture_monitor(MAX_WIDTH)?;
        let mut frame = Frame { width: width as u32, height: height as u32, bgra, app, window };
        // Apps that block screen capture come out black, and so does a sleeping screen. Checked
        // before Navi blacks anything out itself, so that never makes a real screenshot look blocked.
        if mostly_black(&frame) {
            log::debug!("screenshot skipped: it came out black");
            return None;
        }
        // Never read means never kept or read, wherever their windows show: beside the window in
        // front, or partly behind it. The parts other windows cover don't show, so stay as they are.
        if !excluded_apps.is_empty() {
            for rect in win::shown_parts_of(excluded_apps) {
                if let Some(area) = scale_rect(rect, monitor, (width, height)) {
                    black_out(&mut frame, area);
                }
            }
        }
        Some(frame)
    }
    #[cfg(not(windows))]
    {
        let _ = (app, window);
        None
    }
}

/// A window's rectangle on the screen (left, top, right, bottom) as an area of the picture of
/// `monitor` at `size`, rounded outwards with a pixel to spare so no sliver of it survives the
/// scaling. None when the window isn't on that monitor.
pub fn scale_rect(rect: (i32, i32, i32, i32), monitor: (i32, i32, i32, i32), size: (i32, i32)) -> Option<(usize, usize, usize, usize)> {
    let (mw, mh) = ((monitor.2 - monitor.0) as i64, (monitor.3 - monitor.1) as i64);
    let (w, h) = (size.0.max(0) as i64, size.1.max(0) as i64);
    if mw <= 0 || mh <= 0 || w == 0 || h == 0 {
        return None;
    }
    let (left, top) = (rect.0.max(monitor.0), rect.1.max(monitor.1));
    let (right, bottom) = (rect.2.min(monitor.2), rect.3.min(monitor.3));
    if left >= right || top >= bottom {
        return None;
    }
    let down = |v: i32, origin: i32, span: i64, out: i64| ((v - origin) as i64 * out / span - 1).clamp(0, out) as usize;
    let up = |v: i32, origin: i32, span: i64, out: i64| (((v - origin) as i64 * out + span - 1) / span + 1).clamp(0, out) as usize;
    Some((down(left, monitor.0, mw, w), down(top, monitor.1, mh, h), up(right, monitor.0, mw, w), up(bottom, monitor.1, mh, h)))
}

/// The parts of `rect` (left, top, right, bottom) that none of `above` covers. With more pieces
/// than is worth working out (dozens of windows stacked over it), the rest is kept whole: blacking
/// out a little too much is safe, missing a sliver isn't.
pub fn uncovered(rect: (i32, i32, i32, i32), above: &[(i32, i32, i32, i32)]) -> Vec<(i32, i32, i32, i32)> {
    const MOST_PIECES: usize = 256;
    let mut parts = vec![rect];
    for a in above {
        if parts.is_empty() || parts.len() > MOST_PIECES {
            break;
        }
        let mut next = Vec::with_capacity(parts.len() + 3);
        for p in parts {
            let (l, t, r, b) = (p.0.max(a.0), p.1.max(a.1), p.2.min(a.2), p.3.min(a.3));
            if l >= r || t >= b {
                next.push(p);
                continue;
            }
            // What is left around the covered part: the bands above and below it, then the pieces
            // to its left and right.
            if p.1 < t {
                next.push((p.0, p.1, p.2, t));
            }
            if b < p.3 {
                next.push((p.0, b, p.2, p.3));
            }
            if p.0 < l {
                next.push((p.0, t, l, b));
            }
            if r < p.2 {
                next.push((r, t, p.2, b));
            }
        }
        parts = next;
    }
    parts
}

/// Paints an area of the picture (left, top, right, bottom) black.
pub fn black_out(frame: &mut Frame, (left, top, right, bottom): (usize, usize, usize, usize)) {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let (right, bottom) = (right.min(w), bottom.min(h));
    if left >= right || top >= bottom || frame.bgra.len() < w * h * 4 {
        return;
    }
    for y in top..bottom {
        for px in frame.bgra[(y * w + left) * 4..(y * w + right) * 4].chunks_exact_mut(4) {
            px.copy_from_slice(&[0, 0, 0, 255]);
        }
    }
}

/// True when nearly all of the picture is black (an app that blocks capture, or the screen asleep).
/// Those come out as pure black; a dark theme (Windows Terminal is 12, 12, 12) is kept.
pub fn mostly_black(frame: &Frame) -> bool {
    let px = frame.bgra.chunks_exact(4).step_by(7);
    let (mut dark, mut all) = (0usize, 0usize);
    for p in px {
        all += 1;
        if p[0].max(p[1]).max(p[2]) < 6 {
            dark += 1;
        }
    }
    all > 0 && dark * 100 >= all * 93
}

/// The text on the screenshot (Windows OCR), shortened.
pub fn read_text(frame: &Frame) -> String {
    let text = crate::context::ocr_bgra(frame.width, frame.height, frame.bgra.clone()).unwrap_or_default();
    let tidy = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
    crate::context::head_chars(&tidy, TEXT_CHARS).to_string()
}

pub fn save_jpeg(frame: &Frame, path: &Path) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let (w, h) = (u16::try_from(frame.width).map_err(|_| anyhow!("too wide"))?, u16::try_from(frame.height).map_err(|_| anyhow!("too tall"))?);
    let encoder = jpeg_encoder::Encoder::new_file(path, 82).map_err(|e| anyhow!("couldn't create {}: {e}", path.display()))?;
    encoder.encode(&frame.bgra, w, h, jpeg_encoder::ColorType::Bgra).map_err(|e| anyhow!("couldn't save the screenshot: {e}"))?;
    Ok(())
}

/// Where a note's screenshots go.
pub fn dir_for(app: &App, note_id: &str) -> PathBuf {
    app.paths.data.join("shots").join(note_id)
}

/// Removes a note's screenshots (they are Navi's own files in its data folder).
pub fn remove_dir_for(app: &App, note_id: &str) {
    let dir = dir_for(app, note_id);
    if dir.is_dir() {
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            log::warn!("couldn't remove the screenshots in {}: {e}", dir.display());
        }
    }
}

#[cfg(windows)]
mod win {
    use std::collections::HashMap;
    use std::ffi::c_void;
    use windows::core::{BOOL, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, GetDC, GetMonitorInfoW, MonitorFromWindow, ReleaseDC, SelectObject,
        SetStretchBltMode, StretchBlt, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, HALFTONE, HGDIOBJ, MONITORINFO,
        MONITOR_DEFAULTTOPRIMARY, SRCCOPY,
    };
    use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible, GWL_EXSTYLE, WINDOW_EX_STYLE,
        WS_EX_LAYERED, WS_EX_TRANSPARENT,
    };

    type Rect = (i32, i32, i32, i32);

    /// The parts of the screen (left, top, right, bottom) where windows of `apps` show: in front,
    /// or partly behind other windows, never the parts other windows cover. Minimised windows,
    /// ones on another virtual desktop and click-through overlays show nothing, so they are left
    /// out. A window only counts as covering what is behind it when it is surely opaque: windows
    /// that may be see-through (layered ones, and Navi's own, which are clear around the edges)
    /// never do, so at worst a little too much is blacked out.
    pub fn shown_parts_of(apps: &[String]) -> Vec<Rect> {
        struct Search {
            apps: Vec<String>,
            /// Whether each process seen so far is one of `apps`.
            seen: HashMap<u32, bool>,
            own: u32,
            /// Windows met so far, which are in front of the ones still to come.
            above: Vec<Rect>,
            found: Vec<Rect>,
        }
        unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
            // SAFETY: lparam is the &mut Search passed below, alive for the whole enumeration.
            let search = unsafe { &mut *(lparam.0 as *mut Search) };
            unsafe {
                if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
                    return BOOL(1);
                }
                let ex = WINDOW_EX_STYLE(GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32);
                if ex.contains(WS_EX_TRANSPARENT) {
                    return BOOL(1);
                }
                let mut cloaked = 0u32;
                if DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut c_void, std::mem::size_of::<u32>() as u32).is_ok() && cloaked != 0 {
                    return BOOL(1);
                }
                let mut r = RECT::default();
                if GetWindowRect(hwnd, &mut r).is_err() || r.right <= r.left || r.bottom <= r.top {
                    return BOOL(1);
                }
                let mut pid = 0u32;
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                let apps = &search.apps;
                let listed = *search.seen.entry(pid).or_insert_with(|| {
                    let name = process_name(pid);
                    !name.is_empty() && crate::context::is_excluded(&name, apps)
                });
                let whole = (r.left, r.top, r.right, r.bottom);
                if listed {
                    // Its whole rectangle, shadow and all, less what is in front of it. Blacked out,
                    // it then hides whatever is behind it too.
                    search.found.extend(super::uncovered(whole, &search.above));
                    search.above.push(whole);
                } else if pid != search.own && !ex.contains(WS_EX_LAYERED) {
                    // What it covers is its visible frame: Windows pads the rectangle with an
                    // invisible border for resizing.
                    let mut frame = RECT::default();
                    let got = DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut frame as *mut RECT as *mut c_void, std::mem::size_of::<RECT>() as u32).is_ok();
                    let covers = if got && frame.right > frame.left && frame.bottom > frame.top { (frame.left, frame.top, frame.right, frame.bottom) } else { whole };
                    search.above.push(covers);
                }
            }
            BOOL(1)
        }
        let mut search = Search { apps: apps.to_vec(), seen: HashMap::new(), own: std::process::id(), above: Vec::new(), found: Vec::new() };
        unsafe {
            let _ = EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize));
        }
        search.found
    }

    /// A process's file name in lower case ("keepass.exe"), or empty if it can't be read.
    fn process_name(pid: u32) -> String {
        if pid == 0 {
            return String::new();
        }
        unsafe {
            let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
            let mut buf = vec![0u16; 1_024];
            let mut len = buf.len() as u32;
            let read = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
            let _ = CloseHandle(handle);
            if !read {
                return String::new();
            }
            String::from_utf16_lossy(&buf[..len as usize]).rsplit(['\\', '/']).next().unwrap_or("").to_lowercase()
        }
    }

    /// The whole monitor the foreground window is on, scaled to fit `max_width`: (width, height,
    /// top-down opaque BGRA, the monitor's rectangle on the screen).
    pub fn capture_monitor(max_width: i32) -> Option<(i32, i32, Vec<u8>, (i32, i32, i32, i32))> {
        unsafe {
            let monitor = MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY);
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                return None;
            }
            let r = info.rcMonitor;
            let (src_w, src_h) = (r.right - r.left, r.bottom - r.top);
            if src_w < 64 || src_h < 64 {
                return None;
            }
            let (width, height) = if src_w > max_width { (max_width, ((src_h as i64 * max_width as i64) / src_w as i64) as i32) } else { (src_w, src_h) };
            let screen = GetDC(None);
            if screen.is_invalid() {
                return None;
            }
            let memory = CreateCompatibleDC(Some(screen));
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let mut pixels = None;
            if let Ok(dib) = CreateDIBSection(Some(memory), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                if !bits.is_null() {
                    let previous = SelectObject(memory, HGDIOBJ(dib.0));
                    SetStretchBltMode(memory, HALFTONE);
                    if StretchBlt(memory, 0, 0, width, height, Some(screen), r.left, r.top, src_w, src_h, SRCCOPY | CAPTUREBLT).as_bool() {
                        let _ = GdiFlush();
                        let mut px = std::slice::from_raw_parts(bits as *const u8, (width as usize) * (height as usize) * 4).to_vec();
                        px.iter_mut().skip(3).step_by(4).for_each(|a| *a = 255);
                        pixels = Some(px);
                    }
                    SelectObject(memory, previous);
                }
                let _ = DeleteObject(HGDIOBJ(dib.0));
            }
            let _ = DeleteDC(memory);
            ReleaseDC(None, screen);
            pixels.map(|p| (width, height, p, (r.left, r.top, r.right, r.bottom)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: u32, h: u32, f: impl Fn(u32, u32) -> u8) -> Frame {
        let mut bgra = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let v = f(x, y);
                bgra.extend_from_slice(&[v, v, v, 255]);
            }
        }
        Frame { width: w, height: h, bgra, app: String::new(), window: String::new() }
    }

    #[test]
    fn small_changes_are_not_a_new_screen() {
        let page = frame(640, 360, |x, y| if (x / 40 + y / 40) % 2 == 0 { 230 } else { 40 });
        // A clock in the corner ticks: a few pixels change.
        let clock = frame(640, 360, |x, y| if x > 600 && y > 340 { 0 } else if (x / 40 + y / 40) % 2 == 0 { 230 } else { 40 });
        assert!(changed(&page.signature(), &clock.signature()) < NEW_SCREEN);
        // A new slide: most of it changes.
        let slide = frame(640, 360, |x, _| if x < 320 { 250 } else { 10 });
        assert!(changed(&page.signature(), &slide.signature()) > 0.3);
        assert_eq!(changed(&page.signature(), &page.signature()), 0.0);
    }

    #[test]
    fn black_screenshots_are_noticed() {
        assert!(mostly_black(&frame(320, 180, |_, _| 3)));
        // A dark app with a light window in it is kept.
        assert!(!mostly_black(&frame(320, 180, |x, y| if x > 200 && y < 90 { 230 } else { 4 })));
        assert!(!mostly_black(&frame(320, 180, |x, _| (x % 255) as u8)));
        // A full-screen terminal in Windows Terminal's near-black, with a few lines of text.
        assert!(!mostly_black(&frame(320, 180, |x, y| if y < 12 && x < 120 { 200 } else { 12 })));
    }

    #[test]
    fn windows_never_read_are_blacked_out() {
        // A 3200 x 1800 monitor to the right of another, pictured at 1600 x 900.
        let monitor = (1920, 0, 5120, 1800);
        // A password manager in the middle of it: halved, rounded outwards with a pixel to spare.
        assert_eq!(scale_rect((2920, 400, 3920, 1000), monitor, (1600, 900)), Some((499, 199, 1001, 501)));
        // Hanging off the edge: only the part on this monitor.
        assert_eq!(scale_rect((1000, 100, 2120, 300), monitor, (1600, 900)), Some((0, 49, 101, 151)));
        // On the other monitor: nothing.
        assert_eq!(scale_rect((0, 0, 1900, 1000), monitor, (1600, 900)), None);
        let mut f = frame(40, 20, |_, _| 200);
        black_out(&mut f, (10, 5, 20, 15));
        let at = |f: &Frame, x: usize, y: usize| f.bgra[(y * 40 + x) * 4];
        assert_eq!((at(&f, 10, 5), at(&f, 19, 14), at(&f, 9, 5), at(&f, 20, 14), at(&f, 10, 15)), (0, 0, 200, 200, 200));
        // Out of range is clipped, never a panic.
        black_out(&mut f, (30, 10, 400, 400));
        assert_eq!(at(&f, 39, 19), 0);
    }

    #[test]
    fn only_the_parts_of_a_window_that_show_are_blacked_out() {
        let keepass = (100, 100, 500, 400);
        // Nothing in front of it: all of it.
        assert_eq!(uncovered(keepass, &[]), vec![keepass]);
        // A maximised browser in front: none of it, so nothing is punched into the browser.
        assert!(uncovered(keepass, &[(0, 0, 1920, 1040)]).is_empty());
        // A window over its middle: the four pieces around it, covering exactly what still shows.
        let parts = uncovered(keepass, &[(200, 200, 300, 300)]);
        assert_eq!(parts, vec![(100, 100, 500, 200), (100, 300, 500, 400), (100, 200, 200, 300), (300, 200, 500, 300)]);
        let area = |r: &(i32, i32, i32, i32)| (r.2 - r.0) as i64 * (r.3 - r.1) as i64;
        assert_eq!(parts.iter().map(area).sum::<i64>(), area(&keepass) - 100 * 100);
        // Two windows over its left and right halves leave the strip between them.
        assert_eq!(uncovered(keepass, &[(0, 0, 250, 1000), (350, 0, 1000, 1000)]), vec![(250, 100, 350, 400)]);
        // A window elsewhere changes nothing.
        assert_eq!(uncovered(keepass, &[(900, 900, 1000, 1000)]), vec![keepass]);
    }

    #[test]
    fn screenshots_save_as_jpeg() {
        let dir = std::env::temp_dir().join(format!("navi-shot-test-{}", std::process::id()));
        let path = dir.join("a").join("1.jpg");
        save_jpeg(&frame(320, 180, |x, y| ((x + y) % 255) as u8), &path).expect("save");
        let bytes = std::fs::read(&path).expect("read");
        assert_eq!(&bytes[..2], &[0xFF, 0xD8], "JPEG start marker");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Takes a real screenshot and reads it. Run by hand:
    /// cargo test --lib screen::tests::live_capture -- --ignored --nocapture
    #[test]
    #[ignore]
    fn live_capture() {
        let t = std::time::Instant::now();
        let f = capture(&[]).expect("capture");
        println!("{}x{} from {} ({}) in {} ms", f.width, f.height, f.app, f.window, t.elapsed().as_millis());
        let t = std::time::Instant::now();
        let text = read_text(&f);
        println!("OCR {} chars in {} ms: {:?}", text.len(), t.elapsed().as_millis(), text.chars().take(120).collect::<String>());
    }
}

#[cfg(all(test, windows))]
mod diag {
    /// Draws known words into an off-screen picture and reads them back with Windows OCR, so the
    /// text reading is checked without touching the screen. Run by hand:
    /// cargo test --lib screen::diag::ocr_reads_drawn_text -- --ignored --nocapture
    #[test]
    #[ignore]
    fn ocr_reads_drawn_text() {
        use std::ffi::c_void;
        use windows::core::w;
        use windows::Win32::Graphics::Gdi::*;
        let (width, height) = (1000i32, 220i32);
        let pixels = unsafe {
            let dc = CreateCompatibleDC(None);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: width, biHeight: -height, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let dib = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).expect("picture");
            let old = SelectObject(dc, HGDIOBJ(dib.0));
            let _ = PatBlt(dc, 0, 0, width, height, WHITENESS);
            let font = CreateFontW(72, 0, 0, 0, 600, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI"));
            let old_font = SelectObject(dc, HGDIOBJ(font.0));
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, windows::Win32::Foundation::COLORREF(0));
            let words: Vec<u16> = "Quarterly pricing review 2026".encode_utf16().collect();
            let _ = TextOutW(dc, 30, 60, &words);
            let _ = GdiFlush();
            let mut px = std::slice::from_raw_parts(bits as *const u8, (width * height * 4) as usize).to_vec();
            px.iter_mut().skip(3).step_by(4).for_each(|a| *a = 255);
            SelectObject(dc, old_font);
            let _ = DeleteObject(HGDIOBJ(font.0));
            SelectObject(dc, old);
            let _ = DeleteObject(HGDIOBJ(dib.0));
            let _ = DeleteDC(dc);
            px
        };
        let frame = super::Frame { width: width as u32, height: height as u32, bgra: pixels, app: String::new(), window: String::new() };
        let text = super::read_text(&frame);
        println!("read: {text:?}");
        assert!(text.to_lowercase().contains("pricing"), "OCR read {text:?}");
    }
}
