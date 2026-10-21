//! Voice commands (Settings > Voice commands): the microphone stays open and Navi listens for its
//! wake word ("Navi" and "Hey Navi" to start with, or any words you choose) and for a command said
//! straight after it, so you can talk to it without touching the keyboard. Off by default.
//!
//! * Its own microphone stream (WASAPI shared mode, alongside anything else using the mic) feeds
//!   10 seconds of 16 kHz audio into a ring.
//! * spoken::IdleListener spots where each utterance starts and sends its first moments to the
//!   speech engine as it goes: a check 0.4 s in, then every 0.3 s of new speech up to 3 s, one at
//!   a time, and a last one when the utterance ends. The words are matched by spoken::Matcher,
//!   with no punctuation needed after the name, so "Navi watch my screen" in one breath counts.
//!   Only the first check goes with the wake words as Whisper's prompt (see spoken::USE_PROMPT).
//!   While checks come back slowly (another app busy on the GPU), only the last one is sent.
//! * "Navi" alone, or "Navi, email Sam..." (dictation), starts a hands-free take with the audio
//!   from the start of the utterance in it, so every word is kept (controller::start_from_wake);
//!   the bubble shows a moment after you stop, or while you are still talking. "Navi, watch my
//!   screen" runs the command with no take at all (controller::run_spoken).
//! * During call notes it keeps listening in call mode: only the commands that stop the notes or
//!   watch the screen. "Navi, ..." on a call is still a job for the call's own transcript (jobs.rs).
//! * Paused while a take is running (the take's own listener takes over, see
//!   controller::spawn_take_listener), while the shortcut is paused from the tray, during a
//!   microphone test, and while Navi reads text aloud (hold_off), so it never wakes itself.
//! * A microphone that won't open (the one named in Settings unplugged, say) is never swapped for
//!   another. The bubble says why once, and it is tried again every 30 seconds, or at once when
//!   Settings name another.
//! * Each check is one short transcription on this computer. Nothing is kept or sent anywhere, and
//!   what was heard is never logged: only which command fired, and how quickly.

use crate::audio::{self, To16k};
use crate::engine::TranscribeOptions;
use crate::spoken::{IdleCheck, IdleDecision, IdleListener, ListenMode, Matcher, Ring};
use crate::state::{App, Phase};
use crate::types::{PillState, VoiceCommandSettings};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

const RATE: usize = audio::SAMPLE_RATE as usize;
const TICK: Duration = Duration::from_millis(40);
/// Checks slower than this, as a median, and only the last check of each utterance is sent.
const SLOW_CHECK_MS: u64 = 500;
/// How many of the latest checks the median is taken over.
const TIMED_CHECKS: usize = 7;
/// Quiet kept after Navi's own voice stops, before the listener hears the room again.
const SPEECH_TAIL: Duration = Duration::from_millis(300);

/// Until when the listener holds off: Navi is reading text aloud (the Speak page, "Hear it").
static HOLD_OFF_UNTIL: Mutex<Option<Instant>> = parking_lot::const_mutex(None);

/// Navi is about to play `secs` of its own voice: the listener stops hearing the room until it has
/// finished (and a moment after), so a sentence that starts with the wake word can't wake it.
pub fn hold_off(secs: f32) {
    let until = Instant::now() + Duration::from_secs_f32(secs.max(0.0)) + SPEECH_TAIL;
    let mut slot = HOLD_OFF_UNTIL.lock();
    if slot.is_none_or(|t| t < until) {
        *slot = Some(until);
    }
}

fn holding_off() -> bool {
    HOLD_OFF_UNTIL.lock().is_some_and(|t| Instant::now() < t)
}

/// The median of the latest engine times, once there are a few.
fn median_ms(times: &std::collections::VecDeque<u64>) -> Option<u64> {
    if times.len() < 3 {
        return None;
    }
    let mut v: Vec<u64> = times.iter().copied().collect();
    v.sort_unstable();
    Some(v[v.len() / 2])
}

/// Starts the listener thread. It opens and closes the microphone as Settings and Navi's state
/// allow.
pub fn spawn(app: Arc<App>) {
    let spawned = std::thread::Builder::new().name("navi-wake".into()).spawn(move || listen(app));
    if let Err(e) = spawned {
        log::warn!("couldn't start listening for voice commands: {e}");
    }
}

/// True when the listener should have the microphone open. Call notes no longer close it: it
/// listens in call mode.
fn wanted(app: &App) -> bool {
    let enabled = app.settings.read().voice_commands.enabled;
    enabled && !app.paused.load(Ordering::SeqCst) && !app.quitting.load(Ordering::SeqCst)
}

/// Call mode while Navi takes call notes: only the commands that stop them or watch the screen.
fn mode(app: &App) -> ListenMode {
    if app.meeting.lock().is_some() {
        ListenMode::Call
    } else {
        ListenMode::Normal
    }
}

/// True while audio heard now must not wake Navi (a take, or a microphone test, is running, or Navi
/// is reading text aloud).
fn busy(app: &App) -> bool {
    !matches!(app.session.lock().phase, Phase::Idle) || app.mic_test.load(Ordering::SeqCst) || app.audio.is_recording() || holding_off()
}

/// A check's words, back from the engine (None when it was missing or the check failed), and the
/// engine's time for it.
type CheckResult = (IdleCheck, Option<String>, Option<u64>);

fn listen(app: Arc<App>) {
    let ring = Arc::new(Mutex::new(Ring::new()));
    // A decision is being carried out (a take starting, a command running): no checks go out, and
    // results that come back meanwhile are stale.
    let acting = Arc::new(AtomicBool::new(false));
    let (results_tx, results): (Sender<CheckResult>, Receiver<CheckResult>) = std::sync::mpsc::channel();
    let mut idle = IdleListener::new(true);
    // Rebuilt whenever Settings > Voice commands changes.
    let mut matcher_for: Option<VoiceCommandSettings> = None;
    let mut matcher = Matcher::new(&VoiceCommandSettings::default());
    let mut was_acting = false;
    // Checks sent for the latest utterance (its id and how many), for the log.
    let mut sent: (u64, usize) = (0, 0);
    // The engine's time for the latest checks, and whether they have been slow (sliding checks off).
    let mut engine_ms: std::collections::VecDeque<u64> = std::collections::VecDeque::new();
    let mut slow = false;
    let mut mic: Option<Mic> = None;
    // The microphone the stream was last opened (or tried) on, so a change in Settings reopens
    // it, straight away even while waiting to retry one that wouldn't open.
    let mut opened_on: Option<String> = None;
    let mut retry_at = Instant::now();
    // The last reason the microphone wouldn't open, so a mic left unplugged is logged and shown
    // once, not every 30 seconds.
    let mut open_error: Option<String> = None;
    loop {
        std::thread::sleep(TICK);
        if app.quitting.load(Ordering::SeqCst) {
            break;
        }
        if !wanted(&app) {
            if mic.take().is_some() {
                log::info!("stopped listening for voice commands");
            }
            ring.lock().clear();
            idle.reset();
            // Back on (switched on again, or a pause over): try the mic at once, and say again if
            // it still won't open.
            retry_at = Instant::now();
            open_error = None;
            continue;
        }
        let device = app.settings.read().mic.device.clone();
        if mic.is_some() && opened_on != device {
            log::info!("the microphone changed in Settings; listening for voice commands on the new one");
            mic = None;
            ring.lock().clear();
            idle.reset();
        }
        if mic.is_none() {
            if opened_on != device {
                retry_at = Instant::now();
            }
            if Instant::now() < retry_at {
                continue;
            }
            let opened = Mic::open(device.as_deref());
            opened_on = device.clone();
            match opened {
                Ok(m) => {
                    log::info!("listening for {}", app.settings.read().voice_commands.wake_label());
                    // On a Mac, start the quick engine the checks use now, not on the first "Navi".
                    let warm_app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Some(engine) = warm_app.engine.read().await.clone() {
                            engine.warm_quick();
                        }
                    });
                    mic = Some(m);
                    open_error = None;
                }
                Err(e) => {
                    if open_error.as_deref() != Some(e.as_str()) {
                        log::warn!("couldn't open the microphone for voice commands: {e}");
                        let wake = app.settings.read().voice_commands.wake_label();
                        tell(&app, open_notice(&wake, device.as_deref(), &e));
                    }
                    open_error = Some(e);
                    retry_at = Instant::now() + Duration::from_secs(30);
                    continue;
                }
            }
        }
        let Some(m) = mic.as_mut() else { continue };
        let fresh = m.take_16k();
        if m.failed() {
            log::warn!("the microphone for voice commands stopped; opening it again shortly");
            mic = None;
            retry_at = Instant::now() + Duration::from_secs(3);
            continue;
        }
        {
            let s = app.settings.read();
            if matcher_for.as_ref() != Some(&s.voice_commands) {
                matcher = Matcher::new(&s.voice_commands);
                matcher_for = Some(s.voice_commands.clone());
            }
        }
        // The CPU engine is too slow for sliding checks: only the one at the end of each utterance.
        // So is a GPU another app is busy on (a render, a game): every decision would come from
        // stale results, so the same until checks are quick again.
        let on_cpu = app.engine_status.lock().device == "cpu";
        let now_slow = median_ms(&engine_ms).is_some_and(|ms| ms > SLOW_CHECK_MS);
        if now_slow != slow {
            slow = now_slow;
            log::info!("voice: checks are {}, so {}", if slow { "slow" } else { "quick again" }, if slow { "only the last one of each utterance is sent" } else { "they go as you talk again" });
        }
        idle.set_sliding(!on_cpu && !slow);
        // Read before the ring is locked, so the ring is never held while the session lock is taken.
        let (busy_now, acting_now) = (busy(&app), acting.load(Ordering::SeqCst));
        if was_acting && !acting_now {
            // The ring was emptied as the action ended; what the listener was following is gone.
            idle.reset();
        }
        was_acting = acting_now;
        on_tick(&mut ring.lock(), &mut idle, &fresh, busy_now, acting_now);
        while let Ok((check, text, ms)) = results.try_recv() {
            if let Some(ms) = ms {
                if engine_ms.len() >= TIMED_CHECKS {
                    engine_ms.pop_front();
                }
                engine_ms.push_back(ms);
            }
            if busy_now || acting.load(Ordering::SeqCst) {
                continue;
            }
            let Some(text) = text else {
                idle.on_failed(&check);
                continue;
            };
            let decision = idle.on_result(&check, &text, &matcher, mode(&app));
            let checks = if sent.0 == check.utt { sent.1 } else { 1 };
            act(&app, &ring, &acting, decision, checks);
        }
        // A wake word or command heard in full, waiting only for the quiet after it.
        if !busy_now && !acting.load(Ordering::SeqCst) {
            let decision = idle.poll();
            if decision != IdleDecision::Wait {
                act(&app, &ring, &acting, decision, sent.1);
            }
        }
        if !busy_now && !acting.load(Ordering::SeqCst) {
            let due = {
                let r = ring.lock();
                idle.next_check(&r).map(|c| (c, r.slice(c.start, c.end)))
            };
            if let Some((check, audio)) = due {
                sent = if sent.0 == check.utt { (check.utt, sent.1 + 1) } else { (check.utt, 1) };
                let language = app.settings.read().engine.language.clone();
                // The prompt only with the first sliding check: with a longer window Whisper drops
                // the name it was told about ("Watch my screen." for "Navi, watch my screen").
                let prompt = if check.first { matcher.prompt() } else { String::new() };
                send_check(&app, check, audio, TranscribeOptions { language, prompt }, results_tx.clone());
            }
        }
    }
}

/// One listener tick: new audio goes into the ring and the idle listener, unless Navi is busy. A
/// busy tick drops the new audio, empties the ring and forgets the utterance under way, so nothing
/// said during a take can wake Navi afterwards; except while a wake is starting a take (`acting`):
/// what the ring heard as the take's microphone opened is still to be joined onto it, so it is
/// kept as it is.
fn on_tick(ring: &mut Ring, idle: &mut IdleListener, fresh: &[f32], busy: bool, acting: bool) {
    if busy {
        if !acting {
            ring.clear();
        }
        idle.reset();
        return;
    }
    ring.push(fresh);
    idle.advance(ring);
}

/// Transcribes one check on the engine Navi already runs and sends the words back to the loop.
fn send_check(app: &Arc<App>, check: IdleCheck, audio: Vec<f32>, opts: TranscribeOptions, results: Sender<CheckResult>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let engine = app.engine.read().await.clone();
        let (text, ms) = match engine {
            None => (None, None),
            Some(engine) => match engine.transcribe_quick(&audio, &opts).await {
                Ok(t) => {
                    log::debug!("voice: checked {} ms of speech in {} ms", audio.len() * 1000 / RATE, t.ms);
                    (Some(t.text), Some(t.ms))
                }
                Err(e) => {
                    log::debug!("voice: a check failed: {e:#}");
                    (None, None)
                }
            },
        };
        let _ = results.send((check, text, ms));
    });
}

/// Carries out what the idle listener decided. `checks`: how many it took, for the log.
fn act(app: &Arc<App>, ring: &Arc<Mutex<Ring>>, acting: &Arc<AtomicBool>, decision: IdleDecision, checks: usize) {
    match decision {
        IdleDecision::Wait => {}
        // Every bit of speech that isn't "Navi" ends here, about once a second while anyone talks,
        // so it stays out of the everyday log.
        IdleDecision::Ignore => log::debug!("voice: ignored an utterance"),
        IdleDecision::Wake { start, heard_to, after_ms, .. } => {
            log::info!("voice: woke {after_ms} ms after you stopped ({checks} checks)");
            acting.store(true, Ordering::SeqCst);
            let (preroll, preroll_end, judged) = {
                let r = ring.lock();
                (r.since(start), r.end(), take_position(start, r.first(), heard_to))
            };
            let (app, ring, acting) = (app.clone(), ring.clone(), acting.clone());
            tauri::async_runtime::spawn(async move {
                match crate::controller::start_from_wake(&app, preroll).await {
                    Ok(Some(seq)) => {
                        // Only once the gap is in: the take's listener reads the take's audio by
                        // position, so nothing may be put in front of it after it starts.
                        let gap = ring.lock().since(preroll_end);
                        join_gap(&app, gap);
                        crate::controller::spawn_take_listener(app.clone(), seq, Some(judged));
                    }
                    // The shortcut won the microphone.
                    Ok(None) => {}
                    Err(e) => {
                        // The error sound has played; say why, or "Navi" just beeps.
                        log::warn!("voice: couldn't start a take: {e:#}");
                        show_error(&app, &e);
                    }
                }
                ring.lock().clear();
                acting.store(false, Ordering::SeqCst);
            });
        }
        IdleDecision::Command { action, start, end, heard, after_ms } => {
            log::info!("voice: {} {after_ms} ms after you stopped", action.id());
            acting.store(true, Ordering::SeqCst);
            let audio = ring.lock().slice(start, end);
            let (app, ring, acting) = (app.clone(), ring.clone(), acting.clone());
            tauri::async_runtime::spawn(async move {
                if let Err(e) = crate::controller::run_spoken(&app, action, &heard, audio, None).await {
                    log::warn!("voice: {} failed: {e:#}", action.id());
                    app.sounds.play(crate::sounds::Cue::Error, &app.settings().sounds);
                    show_error(&app, &e);
                }
                ring.lock().clear();
                acting.store(false, Ordering::SeqCst);
            });
        }
    }
}

/// Where absolute ring position `pos` falls in a wake take's audio, which opens with the ring's
/// audio from `start` (clamped to `ring_first`, the oldest sample the ring still held).
fn take_position(start: u64, ring_first: u64, pos: u64) -> u64 {
    pos.saturating_sub(start.max(ring_first))
}

/// The first line of `e` in the bubble.
fn show_error(app: &Arc<App>, e: &anyhow::Error) {
    let message = e.to_string().lines().next().unwrap_or("Navi couldn't do that").trim().to_string();
    app.show_pill_window(crate::context::foreground_hwnd());
    let gen = app.pill(PillState::Error { message });
    app.hide_pill_later(gen, 3000);
}

/// Shows a short note in the bubble, unless a take or a microphone test is using it.
fn tell(app: &Arc<App>, message: String) {
    if busy(app) {
        return;
    }
    app.show_pill_window(crate::context::foreground_hwnd());
    let gen = app.pill(PillState::Notice { message });
    app.hide_pill_later(gen, 4000);
}

/// What the bubble says when the listener can't open the microphone. `phrase` is the wake word as
/// people say it (VoiceCommandSettings::wake_label: "Navi" to start with).
fn open_notice(phrase: &str, device: Option<&str>, error: &str) -> String {
    match device.map(str::trim).filter(|n| !n.is_empty()) {
        // Listening starts by itself once it is plugged in.
        Some(name) if error.ends_with("is not connected") => format!("Listening for \u{201c}{phrase}\u{201d} is waiting for the microphone '{name}'"),
        _ => format!("Can't listen for \u{201c}{phrase}\u{201d}. {error}"),
    }
}

/// Joins `gap`, what the listener heard while the take's own microphone was opening, onto the
/// take "Navi" has just started, between the preroll and the take's audio. Also skips the start
/// cue trim for it: you are mid-sentence when the cue plays, so that stretch is speech.
fn join_gap(app: &App, gap: Vec<f32>) {
    let mut s = app.session.lock();
    if mark_gap(&mut s.phase, gap.len()) && !gap.is_empty() {
        app.audio.prepend(&gap);
    }
}

/// Notes a joined gap of `gap_len` samples on the take, if it is a live wake word take. False
/// (nothing to join) when it isn't: the shortcut won the microphone, or the take has already ended.
fn mark_gap(phase: &mut Phase, gap_len: usize) -> bool {
    let Phase::Recording(rec) = phase else { return false };
    if !rec.from_wake {
        return false;
    }
    rec.cue_ms = 0;
    rec.preroll_ms += (gap_len * 1000 / RATE) as u32;
    true
}

/// The listener's own microphone stream: mono at the device rate, converted to 16 kHz on demand.
struct Mic {
    _stream: cpal::Stream,
    raw: Arc<Mutex<Vec<f32>>>,
    failed: Arc<AtomicBool>,
    converter: To16k,
}

impl Mic {
    fn open(name: Option<&str>) -> Result<Self, String> {
        let host = cpal::default_host();
        // The same rule as a take (audio.rs): a named microphone that isn't there is not swapped
        // for the default, or "Navi" would be heard and then the take couldn't open its mic.
        let (device, label) = match name.map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) => {
                let found = host.input_devices().ok().and_then(|mut it| it.find(|d| d.name().map(|x| x == n).unwrap_or(false)));
                (found.ok_or_else(|| format!("The microphone '{n}' is not connected"))?, n.to_string())
            }
            None => {
                let device = host.default_input_device().ok_or_else(|| audio::NO_MICROPHONE.to_string())?;
                let label = device.name().unwrap_or_else(|_| "default microphone".into());
                (device, label)
            }
        };
        // Worded as a take's would be: the bubble may show it.
        let readable = |e: &dyn std::fmt::Display, unplugged: bool| audio::describe_open_error(e, unplugged, &label).to_string();
        let supported = device
            .default_input_config()
            .map_err(|e| readable(&e, matches!(e, cpal::DefaultStreamConfigError::DeviceNotAvailable)))?;
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.config();
        let channels = (config.channels as usize).max(1);
        let raw = Arc::new(Mutex::new(Vec::new()));
        let failed = Arc::new(AtomicBool::new(false));
        let stream = match format {
            cpal::SampleFormat::F32 => build::<f32>(&device, &config, channels, raw.clone(), failed.clone()),
            cpal::SampleFormat::I16 => build::<i16>(&device, &config, channels, raw.clone(), failed.clone()),
            cpal::SampleFormat::I32 => build::<i32>(&device, &config, channels, raw.clone(), failed.clone()),
            cpal::SampleFormat::U16 => build::<u16>(&device, &config, channels, raw.clone(), failed.clone()),
            other => return Err(format!("The microphone '{label}' uses an audio format Navi cannot read ({other})")),
        }
        .map_err(|e| readable(&e, matches!(e, cpal::BuildStreamError::DeviceNotAvailable)))?;
        stream.play().map_err(|e| readable(&e, matches!(e, cpal::PlayStreamError::DeviceNotAvailable)))?;
        let converter = To16k::new(config.sample_rate.0).map_err(|e| e.to_string())?;
        Ok(Self { _stream: stream, raw, failed, converter })
    }

    fn take_16k(&mut self) -> Vec<f32> {
        let raw = std::mem::take(&mut *self.raw.lock());
        self.converter.push(&raw).unwrap_or_default()
    }

    fn failed(&self) -> bool {
        self.failed.load(Ordering::SeqCst)
    }
}

fn build<T>(device: &cpal::Device, config: &cpal::StreamConfig, channels: usize, raw: Arc<Mutex<Vec<f32>>>, failed: Arc<AtomicBool>) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let inv = 1.0 / channels as f32;
    device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            let mut out = raw.lock();
            // Ten seconds at most, should the listener stall.
            if out.len() > 48_000 * 10 {
                out.clear();
            }
            out.extend(data.chunks_exact(channels).map(|f| f.iter().map(|&s| s.to_sample::<f32>()).sum::<f32>() * inv));
        },
        move |err| {
            log::warn!("voice commands microphone: {err}");
            failed.store(true, Ordering::SeqCst);
        },
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(secs: f32, amp: f32) -> Vec<f32> {
        (0..(secs * RATE as f32) as usize).map(|i| (i as f32 * 0.07).sin() * amp).collect()
    }

    #[test]
    fn the_gap_is_only_what_came_after_the_preroll() {
        let mut ring = Ring::new();
        let mut idle = IdleListener::new(true);
        on_tick(&mut ring, &mut idle, &vec![0.1; RATE], false, false);
        // The wake word was heard and the preroll taken; start_from_wake opens the take's mic.
        let (preroll, heard_to) = (ring.since(0), ring.end());
        on_tick(&mut ring, &mut idle, &vec![0.2; RATE / 10], false, true); // heard while it opened
        // The take's microphone is open (busy) and the wake is still being acted on: the ring is
        // held, and what the listener hears now is the take's own audio, so it isn't added.
        on_tick(&mut ring, &mut idle, &vec![0.3; RATE / 10], true, true);
        let gap = ring.since(heard_to);
        assert_eq!((preroll.len(), gap.len()), (RATE, RATE / 10));
        assert!(gap.iter().all(|v| *v == 0.2), "none of the preroll, nor the take's audio, is repeated");

        // Busy with nothing being acted on (a take from the shortcut): the ring is emptied.
        on_tick(&mut ring, &mut idle, &vec![0.3; RATE / 10], true, false);
        assert!(ring.since(0).is_empty() && ring.since(heard_to).is_empty());
        assert_eq!(ring.end(), heard_to + (RATE / 10) as u64, "positions still count on");
    }

    #[test]
    fn a_busy_tick_forgets_the_utterance_under_way() {
        let mut ring = Ring::new();
        let mut idle = IdleListener::new(false);
        let mut audio = tone(1.0, 0.0005);
        audio.extend(tone(0.3, 0.2)); // speech starts...
        for chunk in audio.chunks(RATE / 25) {
            on_tick(&mut ring, &mut idle, chunk, false, false);
        }
        on_tick(&mut ring, &mut idle, &tone(0.04, 0.2), true, true); // ...and a take starts
        // The take ends and the room is quiet: the utterance from before it is never checked.
        for chunk in tone(1.0, 0.0005).chunks(RATE / 25) {
            on_tick(&mut ring, &mut idle, chunk, false, false);
            assert_eq!(idle.next_check(&ring), None, "speech from before the take is never checked afterwards");
        }
        // Without the take, the same speech gets its check once it ends.
        let (mut ring, mut idle) = (Ring::new(), IdleListener::new(false));
        let mut audio = tone(1.0, 0.0005);
        audio.extend(tone(0.3, 0.2));
        audio.extend(tone(1.0, 0.0005));
        let mut checks = 0;
        for chunk in audio.chunks(RATE / 25) {
            on_tick(&mut ring, &mut idle, chunk, false, false);
            checks += usize::from(idle.next_check(&ring).is_some());
        }
        assert_eq!(checks, 1);
    }

    fn recording(from_wake: bool) -> crate::state::Recording {
        let now = Instant::now();
        crate::state::Recording {
            kind: crate::state::Kind::HandsFree,
            seq: 1,
            pressed: now,
            started: now,
            ctx: Default::default(),
            mode: crate::text::modes::builtin_modes().remove(0),
            announced: true,
            cue_at_ms: 0,
            cue_ms: 180,
            cue_word: None,
            pending_tap: None,
            tap_up_at: None,
            ramble_cue: None,
            ramble_started: None,
            ocr: None,
            from_wake,
            preroll_ms: 1_000,
        }
    }

    #[test]
    fn the_gap_joins_only_a_live_wake_take() {
        let mut phase = Phase::Recording(recording(true));
        assert!(mark_gap(&mut phase, RATE / 10));
        let Phase::Recording(rec) = &phase else { unreachable!() };
        assert_eq!((rec.cue_ms, rec.preroll_ms), (0, 1_100), "no cue trim; the gap counts as preroll");

        let mut phase = Phase::Recording(recording(false));
        assert!(!mark_gap(&mut phase, RATE / 10), "the shortcut won the microphone");
        let Phase::Recording(rec) = &phase else { unreachable!() };
        assert_eq!((rec.cue_ms, rec.preroll_ms), (180, 1_000), "a shortcut take is left alone");

        assert!(!mark_gap(&mut Phase::Idle, RATE / 10), "the take has already ended");
        assert!(!mark_gap(&mut Phase::Processing, RATE / 10));
    }

    #[test]
    fn a_missing_named_microphone_is_not_swapped_for_the_default() {
        let err = Mic::open(Some("Navi test: no such microphone")).err().expect("must not open another mic");
        assert!(err.contains("is not connected"), "{err}");
        assert_eq!(
            open_notice("Navi", Some("Navi test: no such microphone"), &err),
            "Listening for \u{201c}Navi\u{201d} is waiting for the microphone 'Navi test: no such microphone'"
        );
    }

    #[test]
    fn what_the_idle_listener_heard_is_found_in_the_take() {
        // The take opens with the ring's audio from the utterance's start.
        assert_eq!(take_position(16_000, 0, 24_000), 8_000);
        // A start the ring no longer held: the take opens with its oldest sample.
        assert_eq!(take_position(1_000, 4_000, 24_000), 20_000);
        assert_eq!(take_position(16_000, 0, 10_000), 0, "never before the take's start");
    }

    #[test]
    fn slow_checks_are_found_by_their_median() {
        let mut t: std::collections::VecDeque<u64> = [150, 160].into_iter().collect();
        assert_eq!(median_ms(&t), None, "two checks are too few to judge");
        t.push_back(9_000);
        assert_eq!(median_ms(&t), Some(160), "one slow check (the engine waking up) is not enough");
        t.extend([700, 800]);
        assert!(median_ms(&t).is_some_and(|ms| ms > SLOW_CHECK_MS));
    }

    #[test]
    fn navi_reading_aloud_holds_the_listener_off() {
        hold_off(0.2);
        assert!(holding_off());
        // A shorter clip never cuts a longer one short.
        hold_off(0.0);
        let until = HOLD_OFF_UNTIL.lock().expect("set");
        assert!(until >= Instant::now() + Duration::from_millis(150), "{until:?}");
        *HOLD_OFF_UNTIL.lock() = None;
        assert!(!holding_off());
    }

    #[test]
    fn says_why_it_cant_listen() {
        assert_eq!(open_notice("Navi", Some(" USB Mic "), "The microphone 'USB Mic' is not connected"), "Listening for \u{201c}Navi\u{201d} is waiting for the microphone 'USB Mic'");
        assert_eq!(open_notice("Navi", None, audio::NO_MICROPHONE), "Can't listen for \u{201c}Navi\u{201d}. No microphone found");
        let blocked = audio::describe_open_error(&"Access is denied. (0x80070005)", false, "USB Mic").to_string();
        assert_eq!(open_notice("Hey Navi", Some("USB Mic"), &blocked), format!("Can't listen for \u{201c}Hey Navi\u{201d}. {blocked}"));
        // A custom wake word is named as it was typed.
        assert_eq!(open_notice("Computer", None, audio::NO_MICROPHONE), "Can't listen for \u{201c}Computer\u{201d}. No microphone found");
    }
}
