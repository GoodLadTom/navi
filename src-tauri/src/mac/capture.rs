//! Pictures of the screen with ScreenCaptureKit, for `screen` (the display the window in front is
//! on) and `context::ocr_window` (one window), plus the conversions between Core Graphics images
//! and the top-down BGRA the rest of Navi works in.
//!
//! * Both captures need the Screen Recording permission. Without it ScreenCaptureKit answers with
//!   an error (and macOS may ask the user), and these return None.
//! * ScreenCaptureKit answers on a queue of its own; each call here waits at most `WAIT` for it, so
//!   a stuck capture costs the calling thread a few seconds, never a hang.
//! * A display picture leaves out whole apps (Navi itself and "Never read these apps") rather than
//!   blacking them out: ScreenCaptureKit draws what is behind them instead.
//!
//! Navi for Mac. Copyright (c) 2026 GoodLad. GNU GPL v3 only (see LICENSE and NOTICE).

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::AllocAnyThread;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo, CGInterpolationQuality,
    CGMainDisplayID,
};
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{SCContentFilter, SCDisplay, SCRunningApplication, SCScreenshotManager, SCShareableContent, SCStreamConfiguration, SCWindow};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::mpsc;
use std::time::Duration;

/// How long each ScreenCaptureKit call may take.
const WAIT: Duration = Duration::from_secs(3);
/// 'BGRA', kCVPixelFormatType_32BGRA.
const PIXEL_FORMAT_BGRA: u32 = 0x4247_5241;

/// A picture: top-down BGRA, opaque.
pub struct Shot {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// Carries an Objective-C object out of ScreenCaptureKit's completion queue. It is handed over
/// once and only used on the receiving thread afterwards.
struct Handover<T>(T);
// SAFETY: the value is moved, never shared: the queue that made it lets go of it on sending.
unsafe impl<T> Send for Handover<T> {}

/// Byte order little + alpha first: B, G, R, A in memory.
fn bgra_info() -> u32 {
    CGImageByteOrderInfo::Order32Little.0 | CGImageAlphaInfo::PremultipliedFirst.0
}

/// The screen's windows, displays and apps, on screen only.
fn shareable_content() -> Option<Retained<SCShareableContent>> {
    // Content filters abort the process unless this process has a Core Graphics session. The app
    // has one through AppKit; asking for the main display opens it anywhere else (the tests).
    let _ = CGMainDisplayID();
    let (tx, rx) = mpsc::channel::<Result<Handover<Retained<SCShareableContent>>, String>>();
    let block = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        // SAFETY: ScreenCaptureKit hands over either a valid content object or an error, both
        // alive for the length of the callback; retaining keeps them past it.
        let result = match unsafe { Retained::retain(content) } {
            Some(content) => Ok(Handover(content)),
            None => Err(unsafe { Retained::retain(error) }.map(|e| e.localizedDescription().to_string()).unwrap_or_default()),
        };
        let _ = tx.send(result);
    });
    unsafe { SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(true, true, &block) };
    match rx.recv_timeout(WAIT) {
        Ok(Ok(content)) => Some(content.0),
        Ok(Err(why)) => {
            log::debug!("screen capture unavailable (Screen Recording permission?): {why}");
            None
        }
        Err(_) => {
            log::debug!("screen capture: no answer about what is on screen");
            None
        }
    }
}

/// One picture of `filter` at `width` x `height` pixels, without the pointer.
fn take(filter: &SCContentFilter, width: usize, height: usize) -> Option<CFRetained<CGImage>> {
    let config = unsafe { SCStreamConfiguration::new() };
    unsafe {
        config.setWidth(width);
        config.setHeight(height);
        config.setPixelFormat(PIXEL_FORMAT_BGRA);
        config.setShowsCursor(false);
    }
    let (tx, rx) = mpsc::channel::<Result<Handover<CFRetained<CGImage>>, String>>();
    let block = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
        let result = match NonNull::new(image) {
            // SAFETY: a valid image for the length of the callback; retained to keep it.
            Some(image) => Ok(Handover(unsafe { CFRetained::retain(image) })),
            None => Err(unsafe { Retained::retain(error) }.map(|e| e.localizedDescription().to_string()).unwrap_or_default()),
        };
        let _ = tx.send(result);
    });
    unsafe { SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(filter, &config, Some(&block)) };
    match rx.recv_timeout(WAIT) {
        Ok(Ok(image)) => Some(image.0),
        Ok(Err(why)) => {
            log::debug!("screenshot failed: {why}");
            None
        }
        Err(_) => {
            log::debug!("screenshot: no answer");
            None
        }
    }
}

/// Draws `image` into a `width` x `height` picture: top-down BGRA, opaque (whatever was see-through
/// comes out black, as on Windows).
pub fn image_to_bgra(image: &CGImage, width: u32, height: u32) -> Option<Vec<u8>> {
    if width == 0 || height == 0 {
        return None;
    }
    let (w, h) = (width as usize, height as usize);
    let mut bgra = vec![0u8; w * h * 4];
    let space = CGColorSpace::new_device_rgb()?;
    // SAFETY: `bgra` holds w * h * 4 bytes and outlives the context, which is dropped below.
    let context = unsafe { CGBitmapContextCreate(bgra.as_mut_ptr() as *mut c_void, w, h, 8, w * 4, Some(&space), bgra_info()) }?;
    CGContext::set_interpolation_quality(Some(&context), CGInterpolationQuality::High);
    let rect = CGRect { origin: CGPoint { x: 0.0, y: 0.0 }, size: CGSize { width: w as f64, height: h as f64 } };
    CGContext::draw_image(Some(&context), rect, Some(image));
    drop(context);
    bgra.iter_mut().skip(3).step_by(4).for_each(|a| *a = 255);
    Some(bgra)
}

/// A Core Graphics image of a top-down BGRA picture (for Vision).
pub fn bgra_to_image(width: u32, height: u32, bgra: &[u8]) -> Option<CFRetained<CGImage>> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || bgra.len() < w * h * 4 {
        return None;
    }
    let mut copy = bgra[..w * h * 4].to_vec();
    let space = CGColorSpace::new_device_rgb()?;
    // SAFETY: `copy` holds w * h * 4 bytes and outlives the context; the image made from it is a
    // copy (copy-on-write), so it doesn't point into `copy` once the context is gone.
    let context = unsafe { CGBitmapContextCreate(copy.as_mut_ptr() as *mut c_void, w, h, 8, w * 4, Some(&space), bgra_info()) }?;
    let image = CGBitmapContextCreateImage(Some(&context));
    drop(context);
    image
}

/// The process names (as `mac::front` gives them) of a running app: the name Navi keys on, and
/// the app's own name, so either spelling in "Never read these apps" leaves it out.
fn names_of(app: &SCRunningApplication) -> [String; 2] {
    let (bundle, name, pid) = unsafe { (app.bundleIdentifier().to_string(), app.applicationName().to_string(), app.processID()) };
    let exec = objc2_app_kit::NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        .and_then(|a| a.executableURL())
        .and_then(|u| u.lastPathComponent())
        .map(|s| s.to_string())
        .unwrap_or_default();
    [super::front::process_name_for(&bundle, &exec, &name), name.trim().to_lowercase()]
}

fn centre(r: &CGRect) -> (f64, f64) {
    (r.origin.x + r.size.width / 2.0, r.origin.y + r.size.height / 2.0)
}

fn contains(r: &CGRect, (x, y): (f64, f64)) -> bool {
    x >= r.origin.x && x < r.origin.x + r.size.width && y >= r.origin.y && y < r.origin.y + r.size.height
}

/// (width, height) in pixels of `filter`'s content, scaled down to at most `max_width` wide.
fn output_size(filter: &SCContentFilter, max_width: u32) -> Option<(u32, u32)> {
    let (rect, scale) = unsafe { (filter.contentRect(), filter.pointPixelScale() as f64) };
    let scale = if scale > 0.0 { scale } else { 1.0 };
    let (src_w, src_h) = ((rect.size.width * scale).round() as i64, (rect.size.height * scale).round() as i64);
    if src_w < 64 || src_h < 64 {
        return None;
    }
    let max = max_width.max(1) as i64;
    Some(if src_w > max { (max as u32, (src_h * max / src_w) as u32) } else { (src_w as u32, src_h as u32) })
}

/// The whole display that window `front_window` (a CGWindowID) is on, else the main display, at
/// most `max_width` pixels wide. Navi's own windows and those of every app `excluded` names (it is
/// given each app's process names) are left out of the picture.
pub fn display(front_window: Option<u32>, excluded: impl Fn(&str) -> bool, max_width: u32) -> Option<Shot> {
    let content = shareable_content()?;
    let displays = unsafe { content.displays() };
    let windows = unsafe { content.windows() };
    let at = front_window
        .and_then(|id| windows.iter().find(|w| unsafe { w.windowID() } == id))
        .map(|w| centre(&unsafe { w.frame() }));
    let main = CGMainDisplayID();
    let display: Retained<SCDisplay> = at
        .and_then(|p| displays.iter().find(|d| contains(&unsafe { d.frame() }, p)))
        .or_else(|| displays.iter().find(|d| unsafe { d.displayID() } == main))
        .or_else(|| displays.iter().next())?;
    let own = std::process::id() as i32;
    let left_out: Vec<Retained<SCRunningApplication>> = unsafe { content.applications() }
        .iter()
        .filter(|app| unsafe { app.processID() } == own || names_of(app).iter().any(|n| !n.is_empty() && excluded(n)))
        .collect();
    let no_windows: Retained<NSArray<SCWindow>> = NSArray::new();
    let filter = unsafe {
        SCContentFilter::initWithDisplay_excludingApplications_exceptingWindows(
            SCContentFilter::alloc(),
            &display,
            &NSArray::from_retained_slice(&left_out),
            &no_windows,
        )
    };
    let (width, height) = output_size(&filter, max_width)?;
    let image = take(&filter, width as usize, height as usize)?;
    let bgra = image_to_bgra(&image, width, height)?;
    Some(Shot { width, height, bgra })
}

/// Window `window_id` alone (a CGWindowID), at most `max_side` pixels on its longer side.
pub fn window(window_id: u32, max_side: u32) -> Option<Shot> {
    if window_id == 0 {
        return None;
    }
    let content = shareable_content()?;
    let windows = unsafe { content.windows() };
    let window = windows.iter().find(|w| unsafe { w.windowID() } == window_id)?;
    let filter = unsafe { SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &window) };
    let (w, h) = output_size(&filter, u32::MAX)?;
    let (w, h) = crate::context::fit_within(w as i32, h as i32, max_side as i32);
    let (width, height) = (w.max(1) as u32, h.max(1) as u32);
    let image = take(&filter, width as usize, height as usize)?;
    let bgra = image_to_bgra(&image, width, height)?;
    Some(Shot { width, height, bgra })
}
