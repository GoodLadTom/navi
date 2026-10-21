//! Core Audio for call notes (macOS 14.2 or later): which apps are using the microphone (process
//! objects, the Mac's counterpart of the registry entries behind Windows' privacy light), and what
//! the computer plays (a process tap in a private aggregate device, the counterpart of WASAPI
//! loopback).
//!
//! * `mic_users` lists every process Core Audio knows, keyed by the name Navi gives the app
//!   (`front::process_name_for`, so Zoom is "zoom.exe" as on Windows) and true while it records.
//!   Helper processes share their app's name ("com.google.Chrome.helper" is Chrome), so an app is
//!   on the microphone when any of its processes is.
//! * `Tap` records a stereo mix of every app's output except Navi's own, without muting it, and
//!   hands it on as mono. Unlike loopback it runs on and on, delivering zeros while nothing plays.
//!   It needs the System Audio Recording permission (NSAudioCaptureUsageDescription in Info.plist);
//!   without it macOS gives silence rather than an error.
//!
//! Navi for Mac. Copyright (c) 2026 GoodLad. GNU GPL v3 only (see LICENSE and NOTICE).

use super::front::process_name_for;
use objc2::AnyThread;
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey, kAudioAggregateDeviceNameKey, kAudioAggregateDeviceTapAutoStartKey, kAudioAggregateDeviceTapListKey,
    kAudioAggregateDeviceUIDKey, kAudioHardwarePropertyProcessObjectList, kAudioHardwarePropertyTranslatePIDToProcessObject, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, kAudioObjectUnknown, kAudioProcessPropertyBundleID, kAudioProcessPropertyIsRunningInput, kAudioProcessPropertyPID,
    kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey, kAudioTapPropertyFormat, AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart,
    AudioDeviceStop, AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap, AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap, AudioObjectGetPropertyData,
    AudioObjectGetPropertyDataSize, AudioObjectID, AudioObjectPropertyAddress, AudioObjectPropertySelector, CATapDescription, CATapMuteBehavior,
};
use objc2_core_audio_types::{kAudioFormatFlagIsFloat, kAudioFormatLinearPCM, AudioBuffer, AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp};
use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFRetained, CFString, CFType};
use objc2_foundation::{NSArray, NSNumber, NSString};
use std::collections::HashMap;
use std::ffi::{c_void, CStr};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// A tap whose callback hasn't run for this long has stopped (its device went away).
const STALL: Duration = Duration::from_secs(5);

fn address(selector: AudioObjectPropertySelector) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress { mSelector: selector, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain }
}

/// Reads a fixed-size property (`T` plain data), with optional qualifier data.
fn get<T: Copy>(object: AudioObjectID, selector: AudioObjectPropertySelector, qualifier: Option<&[u8]>) -> Result<T, i32> {
    let addr = address(selector);
    let mut value = std::mem::MaybeUninit::<T>::zeroed();
    let mut size = std::mem::size_of::<T>() as u32;
    let (q_size, q_ptr) = qualifier.map_or((0, std::ptr::null()), |q| (q.len() as u32, q.as_ptr().cast::<c_void>()));
    let status = unsafe { AudioObjectGetPropertyData(object, NonNull::from(&addr), q_size, q_ptr, NonNull::from(&mut size), NonNull::new_unchecked(value.as_mut_ptr().cast())) };
    if status != 0 {
        return Err(status);
    }
    Ok(unsafe { value.assume_init() })
}

/// Reads a property that is a list of object ids.
fn get_ids(object: AudioObjectID, selector: AudioObjectPropertySelector) -> Result<Vec<AudioObjectID>, i32> {
    let addr = address(selector);
    let mut size = 0u32;
    let status = unsafe { AudioObjectGetPropertyDataSize(object, NonNull::from(&addr), 0, std::ptr::null(), NonNull::from(&mut size)) };
    if status != 0 {
        return Err(status);
    }
    let mut ids = vec![0 as AudioObjectID; size as usize / std::mem::size_of::<AudioObjectID>()];
    if ids.is_empty() {
        return Ok(ids);
    }
    let status = unsafe { AudioObjectGetPropertyData(object, NonNull::from(&addr), 0, std::ptr::null(), NonNull::from(&mut size), NonNull::new_unchecked(ids.as_mut_ptr().cast())) };
    if status != 0 {
        return Err(status);
    }
    ids.truncate(size as usize / std::mem::size_of::<AudioObjectID>());
    Ok(ids)
}

/// Reads a property that is a CFString (the caller owns it).
fn get_string(object: AudioObjectID, selector: AudioObjectPropertySelector) -> Option<String> {
    let raw = get::<*const CFString>(object, selector, None).ok()?;
    let s = unsafe { CFRetained::from_raw(NonNull::new(raw.cast_mut())?) };
    Some(s.to_string())
}

const SYSTEM: AudioObjectID = kAudioObjectSystemObject as AudioObjectID;

extern "C" {
    /// libproc: the executable's name (up to 2 * MAXCOMLEN bytes).
    fn proc_name(pid: i32, buffer: *mut c_void, size: u32) -> i32;
}

/// A process's executable name ("Google Chrome Helper"), or empty.
fn executable(pid: i32) -> String {
    let mut buf = [0u8; 256];
    let len = unsafe { proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if len <= 0 {
        return String::new();
    }
    String::from_utf8_lossy(&buf[..len as usize]).into_owned()
}

/// Core Audio's process object for `pid`, if that process has used audio.
fn process_object(pid: i32) -> Option<AudioObjectID> {
    get::<AudioObjectID>(SYSTEM, kAudioHardwarePropertyTranslatePIDToProcessObject, Some(&pid.to_ne_bytes())).ok().filter(|&id| id != kAudioObjectUnknown)
}

/// Every app Core Audio knows except Navi, by Navi's name for it, and whether it is recording
/// from an input device now (see the module notes). Empty before macOS 14.2.
pub fn mic_users() -> HashMap<String, bool> {
    let me = std::process::id() as i32;
    let mut out: HashMap<String, bool> = HashMap::new();
    let Ok(processes) = get_ids(SYSTEM, kAudioHardwarePropertyProcessObjectList) else { return out };
    for process in processes {
        let Ok(pid) = get::<i32>(process, kAudioProcessPropertyPID, None) else { continue };
        if pid == me || pid <= 0 {
            continue;
        }
        let bundle_id = get_string(process, kAudioProcessPropertyBundleID).unwrap_or_default();
        let exec = executable(pid);
        let name = process_name_for(&bundle_id, &exec, &exec);
        if name.is_empty() {
            continue;
        }
        let input = get::<u32>(process, kAudioProcessPropertyIsRunningInput, None).is_ok_and(|v| v != 0);
        *out.entry(name).or_insert(false) |= input;
    }
    out
}

/// What the IOProc needs, owned by the Tap and freed after the IOProc is gone.
struct Feed {
    on_audio: Box<dyn Fn(&[f32]) + Send + Sync>,
    /// Callbacks so far: the watchdog's sign of life.
    calls: Arc<AtomicU64>,
}

/// A running process tap (see the module notes). Recording stops, and the tap and its aggregate
/// device are destroyed, when it is dropped.
pub struct Tap {
    tap: AudioObjectID,
    device: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    feed: *mut Feed,
    calls: Arc<AtomicU64>,
    /// The tap's sample rate.
    pub rate: u32,
    stop_watch: Arc<AtomicBool>,
}

// The ids are plain numbers, and `feed` is only read by the IOProc until drop stops it.
unsafe impl Send for Tap {}

impl Tap {
    /// Starts recording everything the computer plays except Navi. `on_audio` gets mono samples at
    /// `rate` on Core Audio's thread; `failed` is raised if the tap stops delivering.
    pub fn start(on_audio: impl Fn(&[f32]) + Send + Sync + 'static, failed: Arc<AtomicBool>) -> Result<Tap, String> {
        let mut tap = Tap { tap: kAudioObjectUnknown, device: kAudioObjectUnknown, proc_id: None, feed: std::ptr::null_mut(), calls: Arc::new(AtomicU64::new(0)), rate: 0, stop_watch: Arc::new(AtomicBool::new(false)) };
        // Navi's own process object, if it has one yet, is left out so its cues aren't recorded.
        let exclude: Vec<objc2::rc::Retained<NSNumber>> = process_object(std::process::id() as i32).map(|id| NSNumber::new_u32(id)).into_iter().collect();
        let exclude = NSArray::from_retained_slice(&exclude);
        let description = unsafe { CATapDescription::initStereoGlobalTapButExcludeProcesses(CATapDescription::alloc(), &exclude) };
        unsafe {
            description.setName(&NSString::from_str("Navi call notes"));
            description.setPrivate(true);
            description.setMuteBehavior(CATapMuteBehavior::Unmuted);
        }
        let tap_uid = unsafe { description.UUID().UUIDString() }.to_string();
        let status = unsafe { AudioHardwareCreateProcessTap(Some(&description), &mut tap.tap) };
        if status != 0 || tap.tap == kAudioObjectUnknown {
            return Err(format!("couldn't create the process tap ({})", fourcc(status)));
        }
        let format = get::<AudioStreamBasicDescription>(tap.tap, kAudioTapPropertyFormat, None).map_err(|s| format!("couldn't read the tap's format ({})", fourcc(s)))?;
        if format.mFormatID != kAudioFormatLinearPCM || format.mFormatFlags & kAudioFormatFlagIsFloat == 0 || format.mBitsPerChannel != 32 {
            return Err(format!("unsupported tap format (id {}, flags {:#x}, {} bits)", fourcc(format.mFormatID as i32), format.mFormatFlags, format.mBitsPerChannel));
        }
        tap.rate = format.mSampleRate.round() as u32;

        // A private aggregate device whose only input is the tap. No sub-device: with the output
        // device in it, a headset's microphone would be opened too (and AirPods drop to call
        // quality).
        let key = |k: &CStr| CFString::from_str(k.to_str().unwrap_or_default());
        let sub_tap = CFDictionary::<CFString, CFType>::from_slices(
            &[&*key(kAudioSubTapUIDKey), &*key(kAudioSubTapDriftCompensationKey)],
            &[&*CFString::from_str(&tap_uid), CFBoolean::new(true)],
        );
        let taps = CFArray::<CFDictionary<CFString, CFType>>::from_retained_objects(&[sub_tap]);
        let device_uid = CFString::from_str(&format!("com.goodlad.navi.calltap.{tap_uid}"));
        let name = CFString::from_static_str("Navi call notes");
        let keys = [key(kAudioAggregateDeviceNameKey), key(kAudioAggregateDeviceUIDKey), key(kAudioAggregateDeviceIsPrivateKey), key(kAudioAggregateDeviceIsStackedKey), key(kAudioAggregateDeviceTapAutoStartKey), key(kAudioAggregateDeviceTapListKey)];
        let values: [&CFType; 6] = [&name, &device_uid, CFBoolean::new(true), CFBoolean::new(false), CFBoolean::new(true), &taps];
        let keys: Vec<&CFString> = keys.iter().map(|k| &**k).collect();
        let dict = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
        let status = unsafe { AudioHardwareCreateAggregateDevice(dict.as_opaque(), NonNull::from(&mut tap.device)) };
        if status != 0 || tap.device == kAudioObjectUnknown {
            return Err(format!("couldn't create the aggregate device ({})", fourcc(status)));
        }

        tap.feed = Box::into_raw(Box::new(Feed { on_audio: Box::new(on_audio), calls: tap.calls.clone() }));
        let status = unsafe { AudioDeviceCreateIOProcID(tap.device, Some(io_proc), tap.feed.cast(), NonNull::from(&mut tap.proc_id)) };
        if status != 0 || tap.proc_id.is_none() {
            return Err(format!("couldn't add the IOProc ({})", fourcc(status)));
        }
        let status = unsafe { AudioDeviceStart(tap.device, tap.proc_id) };
        if status != 0 {
            return Err(format!("couldn't start the tap ({})", fourcc(status)));
        }

        // The watchdog: a tap runs on through silence, so callbacks that stop mean it has gone.
        let calls = tap.calls.clone();
        let stop = tap.stop_watch.clone();
        let _ = std::thread::Builder::new().name("navi-tap-watch".into()).spawn(move || {
            let mut last = (0u64, std::time::Instant::now());
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(500));
                let now = calls.load(Ordering::SeqCst);
                if now != last.0 {
                    last = (now, std::time::Instant::now());
                } else if last.1.elapsed() >= STALL && !stop.load(Ordering::SeqCst) {
                    log::warn!("call notes: the computer's sound stopped arriving");
                    failed.store(true, Ordering::SeqCst);
                    break;
                }
            }
        });
        Ok(tap)
    }

    /// Callbacks so far (for tests).
    pub fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        unsafe {
            if self.device != kAudioObjectUnknown {
                if self.proc_id.is_some() {
                    let _ = AudioDeviceStop(self.device, self.proc_id);
                    let _ = AudioDeviceDestroyIOProcID(self.device, self.proc_id);
                }
                let _ = AudioHardwareDestroyAggregateDevice(self.device);
            }
            if self.tap != kAudioObjectUnknown {
                let _ = AudioHardwareDestroyProcessTap(self.tap);
            }
        }
        self.stop_watch.store(true, Ordering::SeqCst);
        // The IOProc is gone by now (AudioDeviceStop waits for a callback in progress).
        if !self.feed.is_null() {
            drop(unsafe { Box::from_raw(self.feed) });
            self.feed = std::ptr::null_mut();
        }
    }
}

/// Core Audio's callback: mixes the tap's buffers (interleaved or not) down to mono.
unsafe extern "C-unwind" fn io_proc(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    _output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client: *mut c_void,
) -> i32 {
    let Some(feed) = (client as *const Feed).as_ref() else { return 0 };
    feed.calls.fetch_add(1, Ordering::Relaxed);
    let list = input.as_ptr();
    let count = (*list).mNumberBuffers as usize;
    let buffers: &[AudioBuffer] = std::slice::from_raw_parts((*list).mBuffers.as_ptr(), count);
    let mut channels_total = 0usize;
    let mut mono: Vec<f32> = Vec::new();
    for buf in buffers {
        let channels = (buf.mNumberChannels as usize).max(1);
        if buf.mData.is_null() || buf.mDataByteSize == 0 {
            continue;
        }
        let data = std::slice::from_raw_parts(buf.mData as *const f32, buf.mDataByteSize as usize / 4);
        let frames = data.len() / channels;
        if mono.len() < frames {
            mono.resize(frames, 0.0);
        }
        for (i, frame) in data.chunks_exact(channels).enumerate() {
            mono[i] += frame.iter().sum::<f32>();
        }
        channels_total += channels;
    }
    if mono.is_empty() {
        return 0;
    }
    let inv = 1.0 / channels_total.max(1) as f32;
    for s in &mut mono {
        *s *= inv;
    }
    // Never let a panic cross into Core Audio.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (feed.on_audio)(&mono)));
    0
}

/// An OSStatus as Core Audio's four letters ("!obj") when it is one, otherwise the number.
fn fourcc(status: i32) -> String {
    let bytes = status.to_be_bytes();
    if bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        format!("'{}'", String::from_utf8_lossy(&bytes))
    } else {
        status.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_read_as_four_letters() {
        assert_eq!(fourcc(0x216f626a), "'!obj'");
        assert_eq!(fourcc(-50), "-50");
    }

    /// Lists the apps on the microphone, then records three seconds from the process tap and
    /// reports what arrived: counts and the peak level only, nothing is kept. Run by hand (play
    /// something to hear more than silence):
    /// cargo test --lib mac::audio::tests::live_tap -- --ignored --nocapture
    #[test]
    #[ignore]
    fn live_tap() {
        let users = mic_users();
        println!("{} apps known to Core Audio", users.len());
        for (name, on) in &users {
            if *on {
                println!("on the microphone: {name} -> {:?}", crate::meeting::call_app_name(name));
            }
        }
        let samples = Arc::new(AtomicU64::new(0));
        // A positive f32's bits order the same way as the number, so fetch_max works on them.
        let peak = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let (s, p) = (samples.clone(), peak.clone());
        let failed = Arc::new(AtomicBool::new(false));
        let tap = Tap::start(
            move |mono: &[f32]| {
                s.fetch_add(mono.len() as u64, Ordering::Relaxed);
                let max = mono.iter().fold(0f32, |m, x| m.max(x.abs()));
                p.fetch_max(max.to_bits(), Ordering::Relaxed);
            },
            failed.clone(),
        )
        .expect("start the tap");
        std::thread::sleep(Duration::from_secs(3));
        let calls = tap.calls();
        let rate = tap.rate;
        drop(tap);
        let got = samples.load(Ordering::SeqCst);
        let peak = f32::from_bits(peak.load(Ordering::SeqCst));
        let db = if peak > 0.0 { format!("{:.1} dBFS", 20.0 * peak.log10()) } else { "silence".into() };
        println!("tap: {rate} Hz, {calls} callbacks, {got} samples ({:.2} s), peak {peak:.4} ({db}), failed {}", got as f32 / rate.max(1) as f32, failed.load(Ordering::SeqCst));
        assert!(got as f32 / rate.max(1) as f32 > 2.5, "the tap delivered too little");
    }
}
