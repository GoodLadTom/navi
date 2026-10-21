//! The Accessibility API (AXUIElement), the Mac's counterpart of UI Automation: the focused
//! element, its text, caret and selection, and window titles. Needs the Accessibility permission
//! (System Settings > Privacy & Security > Accessibility); without it every read gives None.
//!
//! Every call has a messaging timeout, because an app that is busy (or hung) would otherwise hold
//! the caller for up to six seconds.

use objc2_application_services::{AXError, AXIsProcessTrusted, AXIsProcessTrustedWithOptions, AXUIElement, AXValue, AXValueType};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRange, CFRetained, CFString, CFType};
use std::ffi::c_void;
use std::ptr::NonNull;

/// How long one AX call may wait for the app before giving up.
const MESSAGING_TIMEOUT_S: f32 = 0.25;

/// Whether Navi has the Accessibility permission (needed to type, read fields and hear keys).
pub fn trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// Asks macOS to show its "Navi would like to control this computer" prompt (once per app
/// version), and says whether the permission is already there.
pub fn ask_for_trust() -> bool {
    let key = CFString::from_static_str("AXTrustedCheckOptionPrompt");
    let yes = CFBoolean::new(true);
    let dict = CFDictionary::<CFString, CFBoolean>::from_slices(&[&*key], &[yes]);
    unsafe { AXIsProcessTrustedWithOptions(Some(dict.as_opaque())) }
}

/// An accessibility element with the reads Navi needs.
#[derive(Clone)]
pub struct Element(pub CFRetained<AXUIElement>);

// AXUIElement is a CF object; the API may be called from any thread (one call at a time per
// element is all Navi does).
unsafe impl Send for Element {}
unsafe impl Sync for Element {}

impl Element {
    pub fn system_wide() -> Element {
        let el = unsafe { AXUIElement::new_system_wide() };
        unsafe { el.set_messaging_timeout(MESSAGING_TIMEOUT_S) };
        Element(el)
    }

    pub fn application(pid: i32) -> Element {
        let el = unsafe { AXUIElement::new_application(pid) };
        unsafe { el.set_messaging_timeout(MESSAGING_TIMEOUT_S) };
        Element(el)
    }

    pub fn set_timeout(&self, seconds: f32) {
        unsafe { self.0.set_messaging_timeout(seconds) };
    }

    /// The raw value of `name`, or None (no value, not supported, permission missing, timed out).
    pub fn attr(&self, name: &str) -> Option<CFRetained<CFType>> {
        let name = CFString::from_str(name);
        let mut out: *const CFType = std::ptr::null();
        let err = unsafe { self.0.copy_attribute_value(&name, NonNull::from(&mut out)) };
        if err != AXError::Success || out.is_null() {
            return None;
        }
        Some(unsafe { CFRetained::from_raw(NonNull::new_unchecked(out as *mut CFType)) })
    }

    pub fn string(&self, name: &str) -> Option<String> {
        let value = self.attr(name)?;
        value.downcast_ref::<CFString>().map(|s| s.to_string())
    }

    pub fn element(&self, name: &str) -> Option<Element> {
        let value = self.attr(name)?;
        let el = value.downcast::<AXUIElement>().ok()?;
        unsafe { el.set_messaging_timeout(MESSAGING_TIMEOUT_S) };
        Some(Element(el))
    }

    pub fn bool(&self, name: &str) -> Option<bool> {
        let value = self.attr(name)?;
        if let Some(b) = value.downcast_ref::<CFBoolean>() {
            return Some(b.as_bool());
        }
        value.downcast_ref::<CFNumber>().and_then(|n| n.as_i64()).map(|n| n != 0)
    }

    pub fn number(&self, name: &str) -> Option<i64> {
        let value = self.attr(name)?;
        value.downcast_ref::<CFNumber>().and_then(|n| n.as_i64())
    }

    pub fn range(&self, name: &str) -> Option<CFRange> {
        let value = self.attr(name)?;
        let value = value.downcast_ref::<AXValue>()?;
        let mut range = CFRange { location: 0, length: 0 };
        let ok = unsafe { value.value(AXValueType::CFRange, NonNull::from(&mut range).cast::<c_void>()) };
        ok.then_some(range)
    }

    /// The text of `range` in a text element (kAXStringForRangeParameterizedAttribute).
    pub fn string_for_range(&self, range: CFRange) -> Option<String> {
        let mut range = range;
        let param = unsafe { AXValue::new(AXValueType::CFRange, NonNull::from(&mut range).cast::<c_void>()) }?;
        let name = CFString::from_static_str("AXStringForRange");
        let mut out: *const CFType = std::ptr::null();
        let err = unsafe { self.0.copy_parameterized_attribute_value(&name, &param, NonNull::from(&mut out)) };
        if err != AXError::Success || out.is_null() {
            return None;
        }
        let value = unsafe { CFRetained::from_raw(NonNull::new_unchecked(out as *mut CFType)) };
        value.downcast_ref::<CFString>().map(|s| s.to_string())
    }

    pub fn set_range(&self, name: &str, range: CFRange) -> bool {
        let mut range = range;
        let Some(value) = (unsafe { AXValue::new(AXValueType::CFRange, NonNull::from(&mut range).cast::<c_void>()) }) else {
            return false;
        };
        let name = CFString::from_str(name);
        unsafe { self.0.set_attribute_value(&name, &value) == AXError::Success }
    }

    pub fn set_bool(&self, name: &str, on: bool) -> bool {
        let name = CFString::from_str(name);
        unsafe { self.0.set_attribute_value(&name, CFBoolean::new(on)) == AXError::Success }
    }

    pub fn pid(&self) -> Option<i32> {
        let mut pid: i32 = 0;
        let err = unsafe { self.0.pid(NonNull::from(&mut pid)) };
        (err == AXError::Success && pid > 0).then_some(pid)
    }

    pub fn children(&self) -> Vec<Element> {
        let Some(value) = self.attr("AXChildren") else { return Vec::new() };
        let Ok(array) = value.downcast::<objc2_core_foundation::CFArray>() else { return Vec::new() };
        let array: CFRetained<objc2_core_foundation::CFArray<CFType>> = unsafe { CFRetained::cast_unchecked(array) };
        array
            .iter()
            .filter_map(|item| item.downcast::<AXUIElement>().ok())
            .map(|el| {
                unsafe { el.set_messaging_timeout(MESSAGING_TIMEOUT_S) };
                Element(el)
            })
            .collect()
    }

    /// The CGWindowID behind a window element (a private but long-stable call that window
    /// managers rely on).
    pub fn window_id(&self) -> Option<u32> {
        extern "C" {
            fn _AXUIElementGetWindow(element: *const AXUIElement, id: *mut u32) -> AXError;
        }
        let mut id: u32 = 0;
        let err = unsafe { _AXUIElementGetWindow(CFRetained::as_ptr(&self.0).as_ptr(), &mut id) };
        (err == AXError::Success && id != 0).then_some(id)
    }

    pub fn role(&self) -> String {
        self.string("AXRole").unwrap_or_default()
    }

    pub fn subrole(&self) -> String {
        self.string("AXSubrole").unwrap_or_default()
    }

    /// A password field (secure text field), which Navi never reads.
    pub fn is_secure(&self) -> bool {
        self.subrole() == "AXSecureTextField" || self.role() == "AXSecureTextField"
    }
}

/// The element that has the keyboard focus in the app in front, if the app says. Asked of the
/// front app itself: the system-wide element often gives nothing (it did for TextEdit), while the
/// app answers.
pub fn focused_element() -> Option<Element> {
    crate::mac::front::front_pid()
        .and_then(|pid| Element::application(pid).element("AXFocusedUIElement"))
        .or_else(|| Element::system_wide().element("AXFocusedUIElement"))
}

/// Chromium and Electron apps build their accessibility tree only once asked; this asks (once per
/// app is enough, and it costs nothing when already on).
pub fn wake_web_accessibility(pid: i32) {
    let app = Element::application(pid);
    let _ = app.set_bool("AXManualAccessibility", true);
}
