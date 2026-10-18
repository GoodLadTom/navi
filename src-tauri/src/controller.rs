//! The take state machine: hotkey events in, text out.
//!
//! Idle --TalkDown--> Recording(Talk) --TalkUp (held)--> Processing --> Idle
//!                       |  TalkUp quickly, then TalkDown within 380 ms -> Recording(HandsFree)
//!                       |  Ramble key while held                        -> Recording(Ramble)
//!                       |  Cancel                                       -> Idle (discarded)
//! Recording(HandsFree | Ramble) --TalkDown--> Processing
//! Processing --TalkDown, held--> "Still finishing the last one" (nothing records until Idle)
//!
//! The microphone opens the moment the keys go down, but the start sound and the bubble wait a moment
//! (`grace`), so a quick tap, or a shortcut that merely starts with the talk keys (Ctrl + Win +
//! Right), ends silently instead of greeting you and then popping.

use crate::engine::TranscribeOptions;
use crate::insert::{self, InsertOutcome};
use crate::sounds::Cue;
use crate::meeting::PromptState;
use crate::spoken::{self, Action, Matcher, TakeAction, TakeListener};
use crate::state::{App, Kind, LastInsert, Phase, Recording};
use crate::text::{agent, cleanup, learn, llm::Llm, modes, pipeline, ramble, voice};
use crate::types::{AppContext, ContextSettings, EngineStatus, HotkeyEvent, Mode, ModeKind, PillState, Settings, Take, VoiceCommand};
use crate::{audio, context, hotkey, integrations, store::secrets};
use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedReceiver;

const DOUBLE_TAP_MS: u128 = 350;
const DOUBLE_TAP_WINDOW_MS: u64 = 380;
const MIN_SPEECH_SAMPLES: usize = audio::SAMPLE_RATE as usize / 4;
const RAMBLE_CHUNK_SECS: u64 = 12;
/// Wait before announcing a take when the shortcut is only modifiers (Ctrl + Win, Right Ctrl),
/// which also begin everyday shortcuts, and when it is an ordinary key.
const GRACE_MODIFIERS_MS: u64 = 180;
const GRACE_KEY_MS: u64 = 110;
/// Longest stretch of audio trimmed for the start cue.
const CUE_TRIM_MAX_MS: u32 = 160;
/// How long a spoken edit ("Navi, make that shorter") waits for the AI before giving up and saying
/// so. The same as dictation's AI step.
const COMMAND_AI_DEADLINE: Duration = Duration::from_secs(25);
/// Letters and digits at the end of the last insert that must still sit just before the caret
/// before a spoken edit selects back over it. context.rs reads 1,500 characters there, plenty.
const CARET_CHECK_CHARS: usize = 600;
/// How long "Still finishing the last one" shows before the bubble goes back to processing.
const BUSY_NOTE_MS: u64 = 1600;
/// A wake word take finishes after this much quiet, whether or not anything followed the name yet.
/// 1.6 s cut people off while they were thinking of the next word; 6 s leaves room to think, and
/// Enter or the finish phrase ("that's all") finishes it sooner.
const WAKE_QUIET: Duration = Duration::from_secs(6);
const WAKE_MAX: Duration = Duration::from_secs(120);
/// How often the take's own listener reads the take's audio (spawn_take_listener).
const TAKE_TICK: Duration = Duration::from_millis(50);
/// Below this much speech a voice-cancelled take is not worth keeping in History.
const CANCEL_KEEP_SPEECH_SECS: f32 = 1.5;
/// With History off, a take with this much speech (and any ramble) is cancelled by voice only
/// when the phrase is said a second time within CANCEL_CONFIRM: nothing could bring it back.
const CANCEL_CONFIRM_SPEECH_SECS: f32 = 15.0;
const CANCEL_CONFIRM: Duration = Duration::from_secs(4);
/// How long "what's wrong with this?" waits for the AI.
const CRITIQUE_DEADLINE: Duration = Duration::from_secs(120);
/// How long a ramble waits for the AI's notes before making its own offline notes. Shortcut
/// presses meanwhile get "Still finishing the last one", so it can't wait for ever.
const RAMBLE_AI_DEADLINE: Duration = Duration::from_secs(90);

pub fn spawn(app: Arc<App>, mut rx: UnboundedReceiver<hotkey::HotkeyMsg>) {
    tauri::async_runtime::spawn(async move {
        while let Some((ev, at)) = rx.recv().await {
            // The shortcut picker in the interface shows every press. The take itself happens
            // as normal everywhere, Navi's own window included, the way Superwhisper works.
            match ev {
                HotkeyEvent::TalkDown => app.emit("shortcut", "down"),
                HotkeyEvent::TalkUp => app.emit("shortcut", "up"),
                _ => {}
            }
            // Paused from the tray, nothing new starts, but a take already running can still be
            // finished (Enter) or cancelled (Esc): the hook sends only those while paused.
            if app.paused.load(Ordering::SeqCst) && !matches!(ev, HotkeyEvent::Finish | HotkeyEvent::Cancel) {
                log::info!("shortcut {ev:?} ignored: Navi is paused");
                continue;
            }
            log::info!("shortcut {ev:?}");
            if let Err(e) = handle(&app, ev, at).await {
                log::warn!("take failed: {e:#}");
                fail(&app, &short_error(&e));
            }
        }
    });
}

/// How long a press waits before Navi plays the start sound and shows the bubble.
fn grace(settings: &Settings) -> Duration {
    let keys = hotkey::parse_chord(&settings.hotkeys.talk).unwrap_or_default();
    let modifiers_only = !keys.is_empty() && keys.iter().all(|k| k.is_modifier());
    Duration::from_millis(if modifiers_only { GRACE_MODIFIERS_MS } else { GRACE_KEY_MS })
}

/// `at`: when the hook saw the key event.
async fn handle(app: &Arc<App>, ev: HotkeyEvent, at: Instant) -> Result<()> {
    let settings = app.settings();
    match ev {
        HotkeyEvent::TalkDown => {
            let action = {
                let mut guard = app.session.lock();
                let s = &mut *guard;
                match &mut s.phase {
                    // Claimed under the lock, so "Navi" can't start a take while this one opens
                    // the microphone. A take "Navi" is starting has the claim: this press joins it.
                    Phase::Idle if !s.starting => {
                        s.starting = true;
                        1
                    }
                    Phase::Recording(rec) if rec.pending_tap.is_some() => {
                        rec.pending_tap = None;
                        rec.kind = Kind::HandsFree;
                        2
                    }
                    Phase::Recording(rec) if rec.kind != Kind::Talk => 3,
                    Phase::Processing => {
                        s.busy_press = Some(at);
                        4
                    }
                    _ => 0,
                }
            };
            match action {
                1 => start(app, &settings, at).await?,
                2 => {
                    // Enter can finish it from now on.
                    if let Some(h) = app.hotkeys.lock().as_ref() {
                        h.set_hands_free(true);
                    }
                    // The second tap of a double tap: hands-free. Greet now if the first tap was
                    // too quick to have been announced; otherwise switch the bubble over.
                    let reveal = {
                        let mut s = app.session.lock();
                        match &mut s.phase {
                            Phase::Recording(rec) if rec.announced => {
                                app.pill(PillState::Listening { kind: Kind::HandsFree.as_str().into(), mode_name: rec.mode.name.clone(), app_label: rec.ctx.app_label.clone() });
                                None
                            }
                            Phase::Recording(rec) => Some(announce_locked(app, rec, &settings)),
                            _ => None,
                        }
                    };
                    if let Some(hwnd) = reveal {
                        app.reveal_pill_window(hwnd);
                    }
                }
                3 => stop(app).await?,
                4 => busy_note(app, &settings, at),
                _ => {}
            }
        }
        HotkeyEvent::TalkUp => {
            enum Next {
                Nothing,
                Stop,
                /// Released quickly: wait to see whether a second tap makes it hands-free.
                MaybeDoubleTap(u64),
                /// Released before the take was even announced: nothing was said.
                Drop,
            }
            let next = {
                let mut s = app.session.lock();
                s.busy_press = None;
                let seq = s.tap_seq + 1;
                let next = match &mut s.phase {
                    Phase::Recording(rec) if rec.kind == Kind::Talk && rec.pending_tap.is_none() => {
                        // Timed by the hook, so a slow start (a Bluetooth microphone waking up)
                        // does not turn a quick tap into a long press.
                        let held_for = at.saturating_duration_since(rec.pressed);
                        if settings.hotkeys.double_tap_hands_free && held_for.as_millis() < DOUBLE_TAP_MS {
                            rec.pending_tap = Some(seq);
                            Next::MaybeDoubleTap(seq)
                        } else if !rec.announced {
                            Next::Drop
                        } else {
                            Next::Stop
                        }
                    }
                    _ => Next::Nothing,
                };
                if let Next::MaybeDoubleTap(seq) = next {
                    s.tap_seq = seq;
                }
                next
            };
            match next {
                Next::MaybeDoubleTap(seq) => {
                    let app2 = app.clone();
                    tauri::async_runtime::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(DOUBLE_TAP_WINDOW_MS)).await;
                        // A single quick tap: nothing was said, drop it quietly. Checked under the
                        // same lock as the drop, so a second tap that just arrived is never lost.
                        cancel_if(&app2, false, "a single quick tap", |r| r.pending_tap == Some(seq));
                    });
                }
                Next::Drop => cancel(app, false, "released before it began"),
                Next::Stop => stop(app).await?,
                Next::Nothing => {}
            }
        }
        HotkeyEvent::Ramble => {
            let became = {
                let mut s = app.session.lock();
                match &mut s.phase {
                    Phase::Recording(rec) if rec.kind != Kind::Ramble => {
                        rec.kind = Kind::Ramble;
                        rec.pending_tap = None;
                        rec.announced = true;
                        rec.ramble_started = Some(Instant::now());
                        let now_samples = (rec.started.elapsed().as_millis() as usize) * (audio::SAMPLE_RATE as usize) / 1000;
                        let cue = app.sounds.duration_ms(Cue::Ramble, &settings.sounds) as usize * (audio::SAMPLE_RATE as usize) / 1000;
                        rec.ramble_cue = Some((now_samples, now_samples + cue));
                        // Under the lock, so a cancel can't slip in between; the window is shown
                        // once the lock is let go.
                        app.sounds.play(Cue::Ramble, &settings.sounds);
                        app.pill(PillState::Listening { kind: Kind::Ramble.as_str().into(), mode_name: "Notes".into(), app_label: rec.ctx.app_label.clone() });
                        Some(rec.ctx.hwnd)
                    }
                    _ => None,
                }
            };
            if let Some(hwnd) = became {
                app.reveal_pill_window(hwnd);
                if let Some(h) = app.hotkeys.lock().as_ref() {
                    h.set_hands_free(true);
                }
                spawn_ramble_preview(app.clone());
            }
        }
        HotkeyEvent::Finish => {
            // Enter during a hands-free take or a ramble: finish it as pressing the shortcut would.
            let hands_free = matches!(&app.session.lock().phase, Phase::Recording(rec) if rec.kind != Kind::Talk);
            if hands_free {
                stop(app).await?;
            }
        }
        HotkeyEvent::Cancel => {
            // Before the start sound this was another shortcut that starts with the talk keys, so
            // it ends silently; after it, the cancel pop confirms. The same goes for a press while
            // the last take is processing: no "still finishing" note for a shortcut.
            app.session.lock().busy_press = None;
            cancel(app, true, "the cancel key or another shortcut");
        }
        HotkeyEvent::ModeKey(n) => {
            if let Some(key) = settings.hotkeys.mode_keys.get(n as usize) {
                switch_mode(app, &key.mode);
            }
        }
    }
    Ok(())
}

/// Makes `target` the active mode ("auto", "next" or a mode id), saves it, ticks it in the tray
/// and says so in the bubble. A take being recorded hands-free picks up the new mode at once.
/// "next" goes Auto, then each mode in turn, then back to Auto.
pub fn switch_mode(app: &Arc<App>, target: &str) {
    let all = all_modes(app);
    let choosable: Vec<&Mode> = all.iter().filter(|m| m.kind != ModeKind::Notes).collect();
    let settings = app.settings();
    let current = settings.modes.active.trim().to_string();
    let next_id = match target.trim() {
        "" | "auto" => String::new(),
        "next" => match choosable.iter().position(|m| m.id == current) {
            None => choosable.first().map(|m| m.id.clone()).unwrap_or_default(),
            Some(i) if i + 1 < choosable.len() => choosable[i + 1].id.clone(),
            Some(_) => String::new(),
        },
        id if choosable.iter().any(|m| m.id == id) => id.to_string(),
        other => {
            log::info!("mode key for an unknown mode {other:?} ignored");
            // The tray may still tick a mode that has gone; put it right.
            crate::sync_tray_modes(app);
            return;
        }
    };
    let mut next = settings.clone();
    next.modes.active = next_id.clone();
    if let Err(e) = crate::settings::save(&app.paths, &next) {
        log::warn!("could not save the mode: {e:#}");
    }
    *app.settings.write() = next.clone();
    app.emit("settings-changed", ());
    crate::sync_tray_modes(app);
    let name = all.iter().find(|m| m.id == next_id).map(|m| m.name.clone()).unwrap_or_else(|| "Auto".into());
    log::info!("mode is now {name}");

    // A hands-free take carries on in the new mode, and its bubble says so.
    let live = {
        let mut s = app.session.lock();
        match &mut s.phase {
            Phase::Recording(rec) if rec.kind != Kind::Ramble => {
                rec.mode = all.iter().find(|m| m.id == next_id).cloned().unwrap_or_else(|| modes::pick_mode(&all, &rec.ctx, &next, None));
                if rec.announced {
                    app.pill(PillState::Listening { kind: rec.kind.as_str().into(), mode_name: rec.mode.name.clone(), app_label: rec.ctx.app_label.clone() });
                }
                true
            }
            Phase::Idle => false,
            _ => true,
        }
    };
    if !live {
        app.show_pill_window(context::foreground_hwnd());
        // With "Pick the mode from the app" off, Auto always uses the default mode (pick_mode).
        let message = if !next_id.is_empty() {
            format!("Mode: {name}")
        } else if next.modes.auto_by_app {
            "Mode: Auto, picked from the app".to_string()
        } else {
            format!("Mode: Auto (your default, {})", modes::pick_mode(&all, &AppContext::default(), &next, None).name)
        };
        let gen = app.pill(PillState::Notice { message });
        app.hide_pill_later(gen, 1500);
    }
}

/// `pressed`: when the hook saw the keys go down. The caller has claimed `Session::starting`.
async fn start(app: &Arc<App>, settings: &Settings, pressed: Instant) -> Result<()> {
    // A microphone test left running (setup or Settings) must never block a real take.
    if app.mic_test.swap(false, Ordering::SeqCst) {
        app.audio.cancel();
        app.emit("mic-test", false);
    }
    // Listen from the moment the keys go down, so no word is lost. On failure the caller's fail()
    // plays the error sound (once) and says why.
    if let Err(e) = app.audio.start(settings.mic.device.as_deref()) {
        app.session.lock().starting = false;
        return Err(e);
    }
    let started = Instant::now();
    if let Some(h) = app.hotkeys.lock().as_ref() {
        h.set_recording(true);
    }

    // Look at what happened to the last take (learning from corrections), alongside this one
    // rather than before it: reading another window can take a second.
    {
        let app = app.clone();
        tauri::async_runtime::spawn(async move { learning_check_now(&app).await });
    }

    let ctx_settings = settings.context.clone();
    let ctx = tokio::task::spawn_blocking(move || context::capture(&ctx_settings)).await.unwrap_or_default();
    let all = all_modes(app);
    let active = settings.modes.active.trim();
    let mode = modes::pick_mode(&all, &ctx, settings, (!active.is_empty()).then_some(active));

    let hwnd = ctx.hwnd;
    let excluded = settings.context.excluded_apps.iter().any(|a| a.eq_ignore_ascii_case(&ctx.process_name));
    let ocr = if settings.context.ocr && !excluded && hwnd != 0 {
        Some(tokio::task::spawn_blocking(move || context::ocr_window(hwnd)))
    } else {
        None
    };
    log::info!(
        "take started in {} ({} mode), microphone: {}",
        if ctx.app_label.is_empty() { "an unknown app" } else { ctx.app_label.as_str() },
        mode.name,
        settings.mic.device.as_deref().unwrap_or("Windows default")
    );
    let seq = {
        let mut s = app.session.lock();
        s.starting = false;
        // Nothing else starts a take while this one holds the claim, but never overwrite one.
        if !matches!(s.phase, Phase::Idle) {
            log::warn!("another take began while the shortcut's was starting; it carries on");
            return Ok(());
        }
        s.take_seq += 1;
        let seq = s.take_seq;
        s.phase = Phase::Recording(Recording {
            kind: Kind::Talk,
            seq,
            pressed,
            started,
            ctx,
            mode,
            announced: false,
            cue_at_ms: 0,
            cue_ms: 0,
            cue_word: None,
            pending_tap: None,
            ramble_cue: None,
            ramble_started: None,
            ocr,
            from_wake: false,
            preroll_ms: 0,
        });
        seq
    };
    spawn_release_watchdog(app.clone());
    spawn_mic_watch(app.clone(), seq);
    // Cancel and finish by voice once it turns hands-free (or a ramble).
    spawn_take_listener(app.clone(), seq, None);

    let wait = grace(settings).saturating_sub(pressed.elapsed());
    if wait.is_zero() {
        announce(app, seq, settings);
    } else {
        let app = app.clone();
        let settings = settings.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(wait).await;
            announce(&app, seq, &settings);
        });
    }
    Ok(())
}

/// The wake word was heard (wake.rs): starts a hands-free take with the words said while Navi woke
/// up already in it. The sound and the bubble come first, at once; the take's microphone opens
/// behind them, and what the listener heard meanwhile is joined on (wake::join_gap), so nothing
/// is lost. It finishes by itself after 6 s of quiet, with the finish phrase, or with Enter or the
/// shortcut. Some(seq) when a take started; None when another take has the microphone.
pub async fn start_from_wake(app: &Arc<App>, preroll: Vec<f32>) -> Result<Option<u64>> {
    {
        let mut s = app.session.lock();
        // A take under way, or one the shortcut is starting: that one has the microphone.
        if app.paused.load(Ordering::SeqCst) || s.starting || !matches!(s.phase, Phase::Idle) {
            return Ok(None);
        }
        s.starting = true;
    }
    let settings = app.settings();
    let all = all_modes(app);
    let active = settings.modes.active.trim();
    let active = (!active.is_empty()).then_some(active);
    // At once: the mode is a guess until the app in front has been read.
    app.sounds.play(Cue::Talk, &settings.sounds);
    let guess = modes::pick_mode(&all, &AppContext::default(), &settings, active).name;
    app.pill(PillState::Listening { kind: Kind::HandsFree.as_str().into(), mode_name: guess, app_label: String::new() });
    app.reveal_pill_window(context::foreground_hwnd());
    if let Err(e) = app.audio.start(settings.mic.device.as_deref()) {
        app.session.lock().starting = false;
        // wake.rs says why in the bubble.
        app.sounds.play(Cue::Error, &settings.sounds);
        return Err(e);
    }
    app.audio.prepend(&preroll);
    let started = Instant::now();
    let ctx_settings = settings.context.clone();
    let ctx = tokio::task::spawn_blocking(move || context::capture(&ctx_settings)).await.unwrap_or_default();
    let mode = modes::pick_mode(&all, &ctx, &settings, active);
    let hwnd = ctx.hwnd;
    let excluded = settings.context.excluded_apps.iter().any(|a| a.eq_ignore_ascii_case(&ctx.process_name));
    let ocr = if settings.context.ocr && !excluded && hwnd != 0 { Some(tokio::task::spawn_blocking(move || context::ocr_window(hwnd))) } else { None };
    let preroll_ms = (preroll.len() as u64 * 1000 / audio::SAMPLE_RATE as u64) as u32;
    log::info!("take started by the wake word in {} ({} mode)", if ctx.app_label.is_empty() { "an unknown app" } else { ctx.app_label.as_str() }, mode.name);
    let seq = {
        let mut s = app.session.lock();
        s.starting = false;
        // Nothing else starts a take while this one holds the claim, but never overwrite one. The
        // bubble is left as it is: the other take owns it.
        if !matches!(s.phase, Phase::Idle) {
            return Ok(None);
        }
        s.take_seq += 1;
        let seq = s.take_seq;
        // The bubble again, with the real mode and app; no second sound.
        app.pill(PillState::Listening { kind: Kind::HandsFree.as_str().into(), mode_name: mode.name.clone(), app_label: ctx.app_label.clone() });
        s.phase = Phase::Recording(Recording {
            kind: Kind::HandsFree,
            seq,
            pressed: started,
            started,
            ctx,
            mode,
            announced: true,
            cue_at_ms: 0,
            cue_ms: 0,
            cue_word: None,
            pending_tap: None,
            ramble_cue: None,
            ramble_started: None,
            ocr,
            from_wake: true,
            preroll_ms,
        });
        if let Some(h) = app.hotkeys.lock().as_ref() {
            h.set_recording(true);
            h.set_hands_free(true);
        }
        seq
    };
    app.reveal_pill_window(hwnd);
    spawn_wake_finish(app.clone(), seq, preroll.len());
    spawn_mic_watch(app.clone(), seq);
    Ok(Some(seq))
}

/// Finishes a wake word take once you have been quiet for WAKE_QUIET.
fn spawn_wake_finish(app: Arc<App>, seq: u64, from: usize) {
    tauri::async_runtime::spawn(async move {
        let begun = Instant::now();
        let mut looked = from;
        let mut last_voice: Option<Instant> = None;
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let live = matches!(&app.session.lock().phase, Phase::Recording(r) if r.seq == seq && r.kind == Kind::HandsFree);
            if !live {
                break;
            }
            let new = app.audio.snapshot_16k(looked);
            looked += new.len();
            if speech_seconds(&new) >= 0.06 {
                last_voice = Some(Instant::now());
            }
            let quiet = last_voice.unwrap_or(begun).elapsed();
            let too_long = begun.elapsed() >= WAKE_MAX;
            if quiet >= WAKE_QUIET || too_long {
                log::info!("wake word take finished: {}", if too_long { "two minutes is the most" } else { "you stopped talking" });
                if let Err(e) = stop(&app).await {
                    fail(&app, &short_error(&e));
                }
                break;
            }
        }
    });
}

/// Plays the start sound and shows the bubble for take `seq`, unless it has ended, was already announced, was
/// a quick tap that may yet become a double tap (the second tap announces it), or the keys were
/// let go while the take was still starting (its TalkUp, queued behind, drops it silently).
fn announce(app: &Arc<App>, seq: u64, settings: &Settings) {
    let held = app.hotkeys.lock().as_ref().map(|h| h.chord_is_down()).unwrap_or(true);
    let hwnd = {
        let mut s = app.session.lock();
        let Phase::Recording(rec) = &mut s.phase else { return };
        if rec.seq != seq || rec.announced || rec.pending_tap.is_some() || (rec.kind == Kind::Talk && !held) {
            return;
        }
        announce_locked(app, rec, settings)
    };
    app.reveal_pill_window(hwnd);
}

/// The cue and the bubble's state, with the session lock held so a cancel can't slip in between.
/// Returns the window to show the bubble near: the caller shows it with `reveal_pill_window` once
/// the lock is let go, which also covers a take dropped in that moment.
fn announce_locked(app: &Arc<App>, rec: &mut Recording, settings: &Settings) -> isize {
    // The cues are soft tones, not words, so only their first moment is trimmed (enough to drop
    // the attack picked up by a laptop mic) and nothing you say straight away is lost. In a "Navi"
    // take you are usually mid-sentence when the cue plays, so nothing is trimmed there.
    let cue_ms = if settings.sounds.enabled && !rec.from_wake { app.sounds.duration_ms(Cue::Talk, &settings.sounds).min(CUE_TRIM_MAX_MS) } else { 0 };
    rec.announced = true;
    // A wake word take's audio opens with what the listener heard before the microphone was
    // handed over.
    rec.cue_at_ms = rec.started.elapsed().as_millis() as u32 + rec.preroll_ms;
    rec.cue_ms = cue_ms;
    rec.cue_word = None;
    app.sounds.play(Cue::Talk, &settings.sounds);
    app.pill(PillState::Listening { kind: rec.kind.as_str().into(), mode_name: rec.mode.name.clone(), app_label: rec.ctx.app_label.clone() });
    rec.ctx.hwnd
}

/// The hook can miss a key release (lock screen, a UAC prompt). While a push-to-talk take is live,
/// check the keys are really still held, and finish the take if they are not.
fn spawn_release_watchdog(app: Arc<App>) {
    tauri::async_runtime::spawn(async move {
        let mut misses = 0;
        loop {
            tokio::time::sleep(Duration::from_millis(150)).await;
            let watching = matches!(&app.session.lock().phase, Phase::Recording(r) if r.kind == Kind::Talk && r.pending_tap.is_none());
            if !watching {
                // Hands-free, ramble, a pending double tap, or the take already ended.
                let alive = matches!(&app.session.lock().phase, Phase::Recording(r) if r.kind == Kind::Talk);
                if alive {
                    continue;
                }
                break;
            }
            let down = app.hotkeys.lock().as_ref().map(|h| h.chord_is_down()).unwrap_or(true);
            misses = if down { 0 } else { misses + 1 };
            // Two checks in a row, so a release the hook is about to report is not raced.
            if misses >= 2 {
                log::info!("talk keys no longer held but no release was seen; finishing the take");
                if let Err(e) = stop(&app).await {
                    fail(&app, &short_error(&e));
                }
                break;
            }
        }
    });
}

/// A microphone unplugged (or failing) part way through take `seq` ends the take there, keeping
/// what came before. Hands-free takes and rambles have nothing else that would notice, so the
/// bubble would say Listening for minutes while nothing was recorded.
fn spawn_mic_watch(app: Arc<App>, seq: u64) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let live = matches!(&app.session.lock().phase, Phase::Recording(r) if r.seq == seq);
            if !live {
                break;
            }
            if let Some(m) = app.audio.last_error() {
                log::warn!("{m}; finishing the take");
                if let Err(e) = stop(&app).await {
                    fail(&app, &short_error(&e));
                }
                break;
            }
        }
    });
}

/// What the bubble says after a take whose microphone went away part way (`error` from
/// `AudioService::last_error`). The microphone's name is left to the log, so it fits.
fn mic_lost_message(error: &str) -> String {
    // It doesn't claim anything was kept: when the mic dropped before you spoke, nothing was.
    let what = if error.contains("disconnected") { "The microphone was disconnected" } else { "The microphone stopped working" };
    format!("{what}, so Navi stopped listening.")
}

async fn stop(app: &Arc<App>) -> Result<()> {
    stop_take(app, None, None).await
}

/// stop() for take `only` (None: whichever is recording), keeping the audio before 16 kHz sample
/// `cut` when given (the finish phrase, heard by the take's own listener, is cut off that way).
pub async fn stop_take(app: &Arc<App>, only: Option<u64>, cut: Option<usize>) -> Result<()> {
    let (rec, mode_name) = {
        let mut s = app.session.lock();
        if let (Some(seq), Phase::Recording(r)) = (only, &s.phase) {
            if r.seq != seq {
                return Ok(());
            }
        }
        match std::mem::replace(&mut s.phase, Phase::Processing) {
            Phase::Recording(r) => {
                let mode_name = if r.kind == Kind::Ramble { "Notes".to_string() } else { r.mode.name.clone() };
                s.processing_mode = mode_name.clone();
                (r, mode_name)
            }
            other => {
                s.phase = other;
                return Ok(());
            }
        }
    };
    if let Some(h) = app.hotkeys.lock().as_ref() {
        h.set_recording(false);
    }
    let settings = app.settings();
    let recording = app.audio.stop();
    let released = Instant::now();
    let mut recording = match recording {
        Ok(r) => r,
        Err(e) => {
            app.session.lock().phase = Phase::Idle;
            return Err(e);
        }
    };
    // Before any trimming: the cut is a position in the audio as recorded, preroll included.
    if let Some(cut) = cut {
        recording.samples.truncate(cut);
        log::info!("take finished by voice");
    }
    // The microphone went away part way: what came before is used, and the bubble says so after.
    let mic_lost = app.audio.last_error();
    if let Some(m) = &mic_lost {
        log::warn!("{m}; keeping what came before");
    }
    log::info!(
        "take stopped ({}): {:.1} s of audio, {:.1} s of it speech, peak {:.0} dBFS",
        rec.kind.as_str(),
        recording.samples.len() as f32 / audio::SAMPLE_RATE as f32,
        speech_seconds(&recording.samples),
        peak_dbfs(&recording.samples)
    );
    app.sounds.play(Cue::Stop, &settings.sounds);
    app.pill(PillState::Processing { mode_name });

    let app2 = app.clone();
    let is_ramble = rec.kind == Kind::Ramble;
    tauri::async_runtime::spawn(async move {
        let result = if is_ramble {
            process_ramble(&app2, rec, recording.samples, released).await
        } else {
            process_take(&app2, rec, recording.samples, released).await
        };
        // The mic went away part way. Said while still processing, so a take begun straight after
        // never has its bubble covered. No error sound: nothing failed to go in. A ramble's
        // summary (where the notes went, whether tasks were sent) matters more, so it stays.
        if let (Ok(()), Some(m), false) = (&result, &mic_lost, is_ramble) {
            let message = mic_lost_message(m);
            app2.show_pill_window(context::foreground_hwnd());
            let hold = read_ms(&message, 3000);
            let gen = app2.pill(PillState::Error { message });
            app2.hide_pill_later(gen, hold);
        }
        app2.session.lock().phase = Phase::Idle;
        if let Err(e) = result {
            log::warn!("processing failed: {e:#}");
            fail(&app2, &short_error(&e));
        }
    });
    Ok(())
}

fn cancel(app: &Arc<App>, audible: bool, reason: &str) {
    cancel_if(app, audible, reason, |_| true);
}

/// Drops the take being recorded, if `still` agrees it is the one meant. The check, the state
/// change and switching the microphone off all happen under one lock, so a take that has just
/// changed (a second tap) is never dropped by mistake and a new take is never cut off. The cancel
/// pop plays only if `audible` and the take had been announced (before the start sound it ends silently).
fn cancel_if(app: &Arc<App>, audible: bool, reason: &str, still: impl Fn(&Recording) -> bool) {
    let (announced, gen) = {
        let mut s = app.session.lock();
        let announced = match &s.phase {
            Phase::Recording(rec) if still(rec) => rec.announced,
            _ => return,
        };
        s.phase = Phase::Idle;
        app.audio.cancel();
        if let Some(h) = app.hotkeys.lock().as_ref() {
            h.set_recording(false);
        }
        // The bubble is hidden once the lock is let go: hiding its window waits on the main thread.
        (announced, app.pill_gen.load(Ordering::SeqCst))
    };
    // Unless something new has shown in the bubble since.
    if app.pill_gen.load(Ordering::SeqCst) == gen {
        app.pill(PillState::Hidden);
    }
    log::info!("take dropped: {reason}");
    if audible && announced {
        app.sounds.play(Cue::Cancel, &app.settings().sounds);
    }
}

/// Listens to take `seq`'s own audio for "don't worry" (cancel), "that's all" (finish) and, in a
/// take the wake word started, a command said straight after it (spoken::TakeListener). Nothing
/// while Settings > Voice commands is off, or on the CPU engine (too slow to check as you go).
/// `heard`: in a wake take, how far into its audio the idle listener had judged when it woke Navi
/// (the wake word); what comes after that may be the command.
pub fn spawn_take_listener(app: Arc<App>, seq: u64, heard: Option<u64>) {
    if !app.settings.read().voice_commands.enabled || app.engine_status.lock().device == "cpu" {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let (from_wake, preroll_ms) = match &app.session.lock().phase {
            Phase::Recording(r) if r.seq == seq => (r.from_wake, r.preroll_ms),
            _ => return,
        };
        let mut listener = TakeListener::new(from_wake, preroll_ms as u64 * (audio::SAMPLE_RATE as u64 / 1000));
        if let Some(heard) = heard {
            listener.skip_to(heard);
        }
        let mut ring = spoken::Ring::new();
        let mut matcher_for = app.settings.read().voice_commands.clone();
        let mut matcher = Matcher::new(&matcher_for);
        let mut looked = 0usize;
        let mut armed = false;
        let mut waiting: Option<(spoken::TakeCheck, tokio::sync::oneshot::Receiver<Option<String>>)> = None;
        // Seconds of speech in the take so far, and until when a first cancel waits for a second.
        let mut speech_secs = 0f32;
        let mut confirm: Option<(Instant, u64)> = None;
        loop {
            tokio::time::sleep(TAKE_TICK).await;
            // Only while this take records: once it is being transcribed, nothing more is checked.
            let kind = match &app.session.lock().phase {
                Phase::Recording(r) if r.seq == seq => r.kind,
                _ => break,
            };
            // A cancel nobody said again: the take carries on, and so does its bubble.
            if let Some((_, gen)) = confirm.filter(|(until, _)| Instant::now() >= *until) {
                confirm = None;
                restore_listening(&app, seq, gen);
            }
            {
                let s = app.settings.read();
                if !s.voice_commands.enabled {
                    break;
                }
                if s.voice_commands != matcher_for {
                    matcher_for = s.voice_commands.clone();
                    matcher = Matcher::new(&matcher_for);
                }
            }
            // A copy of what is new; the recording itself is left alone.
            let new = app.audio.snapshot_16k(looked);
            looked += new.len();
            speech_secs += speech_seconds(&new);
            ring.push(&new);
            // Push-to-talk: read and segmented, but nothing acts until it turns hands-free.
            if !armed && kind != Kind::Talk {
                listener.arm(if from_wake { 0 } else { ring.end() });
                armed = true;
            }
            listener.advance(&ring);
            let finished = match waiting.as_mut() {
                Some((_, rx)) => match rx.try_recv() {
                    Ok(text) => Some(text),
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => None,
                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => Some(None),
                },
                None => None,
            };
            let mut action = None;
            if let Some(text) = finished {
                if let Some((check, _)) = waiting.take() {
                    match text {
                        Some(text) => action = listener.on_result(&check, &text, &matcher, ring.end()),
                        None => listener.on_failed(&check),
                    }
                }
            }
            if let Some(action) = action.or_else(|| listener.poll(ring.end())) {
                let name = match &action {
                    TakeAction::Cancel { .. } => "cancel",
                    TakeAction::Finish { .. } => "finish",
                    TakeAction::Command { action, .. } => action.id(),
                };
                // Nothing could bring this take back if the cancel was misheard: ask for it twice.
                let save_history = app.settings.read().privacy.save_history;
                if matches!(action, TakeAction::Cancel { .. }) && confirm.is_none() && cancel_needs_confirming(save_history, kind, speech_secs) {
                    log::info!("voice: cancel in a long take, {} ms after you stopped; waiting for it a second time", listener.fired_after_ms());
                    let say = matcher_for.first("cancel").unwrap_or_else(|| "the cancel phrase".into());
                    app.show_pill_window(context::foreground_hwnd());
                    let gen = app.pill(PillState::Notice { message: format!("Say \u{201c}{say}\u{201d} again, or press Esc, to throw this away") });
                    confirm = Some((Instant::now() + CANCEL_CONFIRM, gen));
                    listener.resume();
                    continue;
                }
                log::info!("voice: {name} in the take, {} ms after you stopped", listener.fired_after_ms());
                carry_out(&app, seq, action, &ring).await;
                break;
            }
            if armed && waiting.is_none() {
                if let Some(check) = listener.next_check() {
                    let clip = ring.slice(check.start, check.end);
                    // No prompt: these checks hold the wake word and more, and with the wake words
                    // as its prompt Whisper drops the name ("Watch my screen."). An echoed prompt
                    // can never sound like a command either.
                    let opts = TranscribeOptions { language: app.settings.read().engine.language.clone(), prompt: String::new() };
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let engine_app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        let engine = engine_app.engine.read().await.clone();
                        let text = match engine {
                            Some(engine) => match engine.transcribe(&clip, &opts).await {
                                Ok(t) => {
                                    log::debug!("voice: checked {} ms of the take in {} ms", clip.len() * 1000 / audio::SAMPLE_RATE as usize, t.ms);
                                    Some(t.text)
                                }
                                Err(e) => {
                                    log::debug!("voice: a check in the take failed: {e:#}");
                                    None
                                }
                            },
                            None => None,
                        };
                        let _ = tx.send(text);
                    });
                    waiting = Some((check, rx));
                }
            }
        }
        // Dropping `waiting` abandons a check still in flight.
    });
}

/// What the take's listener heard, carried out on take `seq`. `ring` holds the take's audio.
async fn carry_out(app: &Arc<App>, seq: u64, action: TakeAction, ring: &spoken::Ring) {
    match action {
        TakeAction::Cancel { .. } => cancel_by_voice(app, seq).await,
        TakeAction::Finish { cut, .. } => {
            if let Err(e) = stop_take(app, Some(seq), Some(cut as usize)).await {
                fail(app, &short_error(&e));
            }
        }
        TakeAction::Command { action, heard } => {
            let Some(rec) = take_for_command(app, seq) else { return };
            if let Err(e) = run_spoken(app, action, &heard, ring.slice(0, ring.end()), Some(rec)).await {
                log::warn!("voice: {} failed: {e:#}", action.id());
                fail(app, &short_error(&e));
            }
        }
    }
}

/// Whether a take cancelled by voice goes to History: with History on, and only when it held
/// enough speech to be worth re-running (a misheard "don't worry" can be re-run there).
fn keep_cancelled(save_history: bool, speech_secs: f32) -> bool {
    save_history && speech_secs >= CANCEL_KEEP_SPEECH_SECS
}

/// Whether "don't worry" must be said twice before take `kind` goes: History is off, so a
/// misheard cancel could not be re-run, and there is a lot to lose (a ramble, or a long take).
fn cancel_needs_confirming(save_history: bool, kind: Kind, speech_secs: f32) -> bool {
    !save_history && (kind == Kind::Ramble || speech_secs >= CANCEL_CONFIRM_SPEECH_SECS)
}

/// The Listening bubble again for take `seq` after a note (`gen`) asked for the cancel twice,
/// unless the take has ended or something newer is in the bubble.
fn restore_listening(app: &Arc<App>, seq: u64, gen: u64) {
    let s = app.session.lock();
    let Phase::Recording(rec) = &s.phase else { return };
    if rec.seq != seq || app.pill_gen.load(Ordering::SeqCst) != gen {
        return;
    }
    let mode_name = if rec.kind == Kind::Ramble { "Notes".to_string() } else { rec.mode.name.clone() };
    app.pill(PillState::Listening { kind: rec.kind.as_str().into(), mode_name, app_label: rec.ctx.app_label.clone() });
}

/// "Don't worry": drops take `seq` like Esc, with the cancel sound and a short "Cancelled" note.
/// Its recording goes to History as "cancelled" (when History is on and it held at least
/// CANCEL_KEEP_SPEECH_SECS of speech), so a misheard cancel can be re-run.
pub async fn cancel_by_voice(app: &Arc<App>, seq: u64) {
    let (rec, samples, gen) = {
        let mut s = app.session.lock();
        if !matches!(&s.phase, Phase::Recording(r) if r.seq == seq) {
            return;
        }
        let Phase::Recording(rec) = std::mem::replace(&mut s.phase, Phase::Idle) else { return };
        // Under the lock, as cancel_if does, so a new take is never cut off.
        let samples = app.audio.stop().ok().map(|r| r.samples);
        if let Some(h) = app.hotkeys.lock().as_ref() {
            h.set_recording(false);
        }
        (rec, samples, app.pill_gen.load(Ordering::SeqCst))
    };
    log::info!("take dropped: you said the cancel phrase");
    let settings = app.settings();
    app.sounds.play(Cue::Cancel, &settings.sounds);
    // Unless something new has shown in the bubble since.
    if app.pill_gen.load(Ordering::SeqCst) == gen {
        let gen = app.pill(PillState::Notice { message: "Cancelled".into() });
        app.hide_pill_later(gen, 900);
    }
    if let Some(samples) = samples {
        if keep_cancelled(settings.privacy.save_history, speech_seconds(&samples)) {
            save_take(app, &settings, &rec, "", "", &samples, 0, "cancelled", rec.kind.as_str(), serde_json::json!({ "cancelledBy": "voice" }));
        }
    }
}

/// Ends take `seq` silently for a command said straight after the wake word, returning it so the
/// command can use its window. None when it has already ended.
fn take_for_command(app: &Arc<App>, seq: u64) -> Option<Recording> {
    let rec = {
        let mut s = app.session.lock();
        if !matches!(&s.phase, Phase::Recording(r) if r.seq == seq) {
            return None;
        }
        let Phase::Recording(rec) = std::mem::replace(&mut s.phase, Phase::Idle) else { return None };
        app.audio.cancel();
        if let Some(h) = app.hotkeys.lock().as_ref() {
            h.set_recording(false);
        }
        rec
    };
    // No sound: the command makes its own, and its note replaces the bubble.
    log::info!("take dropped: a command straight after the wake word");
    Some(rec)
}

/// A short note in the bubble for a spoken command.
fn spoken_note(app: &Arc<App>, message: String) {
    app.show_pill_window(context::foreground_hwnd());
    let hold = read_ms(&message, 2500);
    let gen = app.pill(PillState::Notice { message });
    app.hide_pill_later(gen, hold);
}

/// A take-shaped record for a command heard with no take (scratch that from the idle listener):
/// the app in front now and the mode it would get.
async fn bare_recording(app: &Arc<App>) -> Recording {
    let settings = app.settings();
    let ctx_settings = settings.context.clone();
    let ctx = tokio::task::spawn_blocking(move || context::capture(&ctx_settings)).await.unwrap_or_default();
    let all = all_modes(app);
    let active = settings.modes.active.trim();
    let mode = modes::pick_mode(&all, &ctx, &settings, (!active.is_empty()).then_some(active));
    let now = Instant::now();
    Recording {
        kind: Kind::HandsFree,
        seq: 0,
        pressed: now,
        started: now,
        ctx,
        mode,
        announced: true,
        cue_at_ms: 0,
        cue_ms: 0,
        cue_word: None,
        pending_tap: None,
        ramble_cue: None,
        ramble_started: None,
        ocr: None,
        from_wake: true,
        preroll_ms: 0,
    }
}

/// Runs a command heard by a listener, with no take (idle) or straight after the wake word in a
/// take (`rec`, already ended). `heard` and `audio` are only for History (scratch that).
pub async fn run_spoken(app: &Arc<App>, action: Action, heard: &str, audio: Vec<f32>, rec: Option<Recording>) -> Result<()> {
    let settings = app.settings();
    // watch.rs shows its own notes only while nothing else is using the bubble; from a finished
    // take (process_take) it is still processing, so the note is shown here instead.
    let idle = matches!(app.session.lock().phase, Phase::Idle);
    match action {
        Action::WatchOn => {
            if crate::watch::is_watching() {
                spoken_note(app, "Already watching your screen".into());
                return Ok(());
            }
            crate::watch::start(app)?;
            if !idle {
                spoken_note(app, "Watching your screen".into());
            }
            app.sounds.play(Cue::Done, &settings.sounds);
        }
        Action::WatchOff => {
            if !crate::watch::is_watching() {
                spoken_note(app, "Navi wasn\u{2019}t watching your screen".into());
                return Ok(());
            }
            crate::watch::stop(app)?;
            if !idle {
                spoken_note(app, "Writing up what you did\u{2026}".into());
            }
            app.sounds.play(Cue::Done, &settings.sounds);
        }
        Action::NotesOn => {
            if app.meeting.lock().is_some() {
                spoken_note(app, "Already taking notes".into());
                return Ok(());
            }
            // Opening the call audio can take a moment, so not on the async threads.
            let app2 = app.clone();
            tokio::task::spawn_blocking(move || crate::meeting::start(&app2, None)).await.map_err(|e| anyhow!("Navi couldn't start the notes: {e}"))??;
            let message = match settings.voice_commands.say("notes_off") {
                Some(say) => format!("Taking notes. Say \u{201c}{say}\u{201d} when you\u{2019}re done."),
                None => "Taking notes. Stop them from the card in the corner.".to_string(),
            };
            spoken_note(app, message);
            app.sounds.play(Cue::Done, &settings.sounds);
        }
        Action::NotesOff => {
            if app.meeting.lock().is_none() {
                spoken_note(app, "Navi wasn\u{2019}t taking notes".into());
                return Ok(());
            }
            crate::meeting::stop(app)?;
            spoken_note(app, "Finishing your notes".into());
            app.sounds.play(Cue::Done, &settings.sounds);
        }
        Action::ScratchThat => {
            let rec = match rec {
                Some(rec) => rec,
                None => {
                    // run_command's own bubble needs the window up.
                    app.show_pill_window(context::foreground_hwnd());
                    bare_recording(app).await
                }
            };
            run_command(app, &rec, VoiceCommand::Undo, heard, &audio, Instant::now()).await?;
        }
        Action::Wake | Action::Cancel | Action::Finish => {}
    }
    Ok(())
}

fn fail(app: &Arc<App>, message: &str) {
    let recording = {
        let mut s = app.session.lock();
        let recording = matches!(s.phase, Phase::Recording(_));
        // A take begun since (the shortcut pressed again straight away) carries on undisturbed.
        if !recording {
            s.phase = Phase::Idle;
        }
        recording
    };
    if recording {
        return;
    }
    if let Some(h) = app.hotkeys.lock().as_ref() {
        h.set_recording(false);
    }
    app.sounds.play(Cue::Error, &app.settings().sounds);
    // The window may be hidden from the last take, or never shown (a microphone that won't open).
    app.show_pill_window(context::foreground_hwnd());
    let gen = app.pill(PillState::Error { message: message.to_string() });
    app.hide_pill_later(gen, read_ms(message, 3000));
}

/// How long a message stays in the bubble: at least `at_least` ms, longer for a long one so it can
/// be read, as it may run to two lines. ui/js/bubble.js holds them a little longer still.
fn read_ms(message: &str, at_least: u64) -> u64 {
    (1000 + 50 * message.chars().count() as u64).max(at_least).min(6000)
}

/// The talk keys went down (`pressed`) while the last take is still being written up. Nothing is
/// recording, so if they are still held once the grace is over (not a tap, not another shortcut)
/// say so, or you would talk to nobody. Then the bubble goes back to working on the last one.
fn busy_note(app: &Arc<App>, settings: &Settings, pressed: Instant) {
    let wait = grace(settings).saturating_sub(pressed.elapsed());
    let (app, settings) = (app.clone(), settings.clone());
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(wait).await;
        let mode_name = {
            let mut s = app.session.lock();
            if s.busy_press != Some(pressed) || !matches!(s.phase, Phase::Processing) {
                return;
            }
            s.busy_press = None;
            s.processing_mode.clone()
        };
        log::info!("talk keys held while the last take is still processing");
        app.sounds.play(Cue::Error, &settings.sounds);
        let gen = app.pill(PillState::Error { message: "Still finishing the last one. Try again in a moment.".into() });
        tokio::time::sleep(Duration::from_millis(BUSY_NOTE_MS)).await;
        let still = matches!(app.session.lock().phase, Phase::Processing);
        if still && app.pill_gen.load(Ordering::SeqCst) == gen {
            app.pill(PillState::Processing { mode_name });
        }
    });
}

/// Drops the stretch of audio while the start cue played (less its last 60 ms), so speaker bleed
/// never becomes text. Anything said before the cue (a quick talker, during the grace) is kept.
fn trim_cue(samples: &mut Vec<f32>, rec: &Recording, settings: &Settings) {
    if settings.mic.trim_cue && rec.cue_ms > 0 {
        let sample_at = |ms: u32| ((ms as usize) * (audio::SAMPLE_RATE as usize) / 1000).min(samples.len());
        let start = sample_at(rec.cue_at_ms);
        let end = sample_at(rec.cue_at_ms + rec.cue_ms.saturating_sub(60));
        samples.drain(start..end);
    }
}

/// Loudest sample in dBFS (-120 for silence), for the log.
fn peak_dbfs(samples: &[f32]) -> f32 {
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak <= 1e-6 {
        -120.0
    } else {
        20.0 * peak.log10()
    }
}

/// The speech engine isn't there for a take. `reason` is short enough to go beside "Saved in
/// History." when the take is kept; `hint` says what to do otherwise.
#[derive(Debug)]
struct NoEngine {
    reason: &'static str,
    hint: &'static str,
}

impl std::fmt::Display for NoEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.reason, self.hint)
    }
}

impl std::error::Error for NoEngine {}

/// Why there is no engine, from its status: still starting, or failed, so nobody keeps trying
/// again for an engine that is never coming.
fn no_engine(status: &EngineStatus) -> NoEngine {
    const SETTINGS: &str = "See Settings > Speech engine.";
    match status.state.as_str() {
        "missing-model" => NoEngine { reason: "The speech model isn't downloaded yet.", hint: SETTINGS },
        "missing-engine" => NoEngine { reason: "The speech engine isn't installed yet.", hint: SETTINGS },
        "error" => NoEngine { reason: "The speech engine couldn't start.", hint: SETTINGS },
        // Starting, or restarting (stopped, or still ready a moment before the new one is in).
        _ => NoEngine { reason: "The speech engine is still starting.", hint: "Try again in a moment." },
    }
}

async fn transcribe(app: &Arc<App>, samples: &[f32], settings: &Settings, ctx: &AppContext, screen: Option<&str>) -> Result<String> {
    let engine = app.engine.read().await.clone();
    let Some(engine) = engine else {
        let status = app.engine_status.lock().clone();
        return Err(no_engine(&status).into());
    };
    let mut prompt = app.db.vocabulary_prompt(600).unwrap_or_default();
    let mut names = context_terms(ctx.field_text.as_deref());
    for n in context_terms(screen) {
        if names.len() >= 30 {
            break;
        }
        if !names.contains(&n) {
            names.push(n);
        }
    }
    if !names.is_empty() {
        if !prompt.is_empty() {
            prompt.push_str(", ");
        }
        prompt.push_str(&names.join(", "));
    }
    let opts = TranscribeOptions { language: settings.engine.language.clone(), prompt };
    let tr = engine.transcribe(samples, &opts).await?;
    Ok(tr.text)
}

/// Capitalised words near the cursor (names in the email you are replying to) so the engine spells them right.
fn context_terms(text: Option<&str>) -> Vec<String> {
    let Some(text) = text else { return Vec::new() };
    let common = ["I", "The", "A", "An", "And", "But", "So", "Hi", "Hello", "Thanks", "Cheers", "Best", "Regards", "Dear", "This", "That", "It", "We", "You", "He", "She", "They", "If", "In", "On", "At", "For", "To", "Of", "Yes", "No", "OK", "Okay"];
    let mut out: Vec<String> = Vec::new();
    for raw in text.split(|c: char| c.is_whitespace() || ",.;:!?()[]\"'".contains(c)) {
        let w = raw.trim();
        if w.len() < 3 || w.len() > 24 || !w.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
            continue;
        }
        if common.contains(&w) || w.chars().any(|c| c.is_ascii_digit()) || out.iter().any(|x| x == w) {
            continue;
        }
        out.push(w.to_string());
        if out.len() >= 20 {
            break;
        }
    }
    out
}

/// Seconds of audio louder than about -42 dBFS, counted in 20 ms frames.
fn speech_seconds(samples: &[f32]) -> f32 {
    let frame = (audio::SAMPLE_RATE as usize / 50).max(1);
    let threshold = 10f32.powf(-42.0 / 20.0);
    let loud = samples
        .chunks(frame)
        .filter(|c| (c.iter().map(|x| x * x).sum::<f32>() / c.len().max(1) as f32).sqrt() > threshold)
        .count();
    loud as f32 / 50.0
}

/// True when the engine heard nothing worth keeping. With clear speech in the audio, only
/// bracketed noise tags count as empty, so a real "Thank you." is never thrown away.
fn heard_nothing(text: &str, samples: &[f32]) -> bool {
    if text.trim().is_empty() {
        return true;
    }
    if speech_seconds(samples) >= 0.4 {
        cleanup::is_noise_marker(text)
    } else {
        cleanup::is_hallucination(text)
    }
}

pub fn all_modes(app: &App) -> Vec<Mode> {
    modes::merge(modes::builtin_modes(), app.db.list_stored_modes().unwrap_or_default())
}

pub fn make_llm(settings: &Settings) -> Option<Llm> {
    let key = match settings.ai.provider.as_str() {
        "anthropic" | "openai" | "custom" => secrets::get(&settings.ai.provider),
        _ => None,
    };
    Llm::from_settings(&settings.ai, key, settings.privacy.local_only)
}

/// The AI for jobs nobody sits waiting on: call notes, Short ideas, watch notes, call jobs and
/// "what's wrong with this?". The model named in Settings > AI for longer jobs, or Sonnet through
/// Claude Code (quality matters more than speed here), or the dictation model.
pub fn make_llm_long(settings: &Settings) -> Option<Llm> {
    let mut s = settings.clone();
    let wanted = settings.ai.long_model.trim();
    if !wanted.is_empty() && long_model_fits(&settings.ai.provider, wanted) {
        s.ai.model = wanted.to_string();
    } else {
        if !wanted.is_empty() {
            log::info!("the model for longer jobs ({wanted}) isn't one {} has, so using the usual one", settings.ai.provider);
        }
        if settings.ai.provider == "claude-code" {
            s.ai.model = "sonnet".into();
        }
    }
    make_llm(&s)
}

/// Whether `model` (Settings > AI > Model for longer jobs) can belong to `provider`. It is kept
/// when the provider changes, and "opus" picked under Claude Code means nothing to OpenAI.
fn long_model_fits(provider: &str, model: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    let claude_names = ["haiku", "sonnet", "opus"];
    let claude = m.starts_with("claude") || claude_names.contains(&m.as_str());
    match provider {
        "claude-code" => claude_names.iter().any(|n| m.contains(n)),
        "anthropic" => m.starts_with("claude"),
        // Ollama takes local model names; a "gpt-" one came from OpenAI.
        "ollama" => !claude && !m.starts_with("gpt-"),
        // A custom server may serve Claude under its full names (a proxy, a company gateway); only
        // Claude Code's bare names are left over from another provider.
        "custom" => !claude_names.contains(&m.as_str()),
        _ => !claude,
    }
}

async fn project_files(app: &Arc<App>, ctx: &AppContext, settings: &Settings, utterance: &str) -> Vec<String> {
    let Some(project) = agent::detect_project(ctx, &settings.agent) else { return Vec::new() };
    let cached = app.project_index.lock().get(&project.path).filter(|(at, _)| at.elapsed() < Duration::from_secs(300)).map(|(_, f)| f.clone());
    let files = match cached {
        Some(f) => f,
        None => {
            let root = std::path::PathBuf::from(&project.path);
            let list = tokio::task::spawn_blocking(move || agent::index_files(&root, 6000)).await.unwrap_or_default();
            let list = Arc::new(list);
            app.project_index.lock().insert(project.path.clone(), (Instant::now(), list.clone()));
            list
        }
    };
    agent::candidates(utterance, &files, 30)
}

/// The OCR text if it finished, waiting at most a moment more for it.
async fn screen_text(rec: &mut Recording) -> Option<String> {
    let handle = rec.ocr.take()?;
    match tokio::time::timeout(Duration::from_millis(700), handle).await {
        Ok(Ok(text)) => text,
        _ => None,
    }
}

/// What a finished take turns out to be, before any dictation.
#[derive(Debug, PartialEq)]
enum AfterTake {
    /// The cancel phrase on its own: nothing is typed or saved.
    Cancel,
    /// A command straight after the wake word.
    Spoken(Action),
    /// Dictation, or a spoken edit (voice::parse decides). `rest_after_wake`: the words after a
    /// wake word, when the take opens with one.
    Dictate { raw: String, rest_after_wake: Option<String> },
}

/// `listening`: Settings > Voice commands is on. `hands_free`: not a push-to-talk take.
/// `from_wake`: the wake word started the take.
fn after_take(raw: &str, m: &Matcher, listening: bool, hands_free: bool, from_wake: bool) -> AfterTake {
    // Only the wake words in Settings decide what the take is ("Nav, take notes" said to a
    // colleague is dictation). In a take the wake word didn't start, the name needs its strong
    // form or a pause after it, as the idle listener asks ("Navy, take notes" is dictation).
    let wake_rest = |text: &str| m.match_wake(text).filter(|hit| from_wake || !hit.weak || hit.paused).map(|hit| hit.rest);
    let rest = wake_rest(raw);
    // The cancel phrase word for word, after the wake word or (except while holding push-to-talk,
    // which is deliberate: "Don't worry." is then a reply to type) as the whole take.
    let said_after_wake = rest.as_deref().is_some_and(|r| m.match_exact(r, &[Action::Cancel]).is_some());
    let said_alone = hands_free && rest.is_none() && m.match_in_take(raw) == Some(Action::Cancel);
    if listening && (said_after_wake || said_alone) {
        return AfterTake::Cancel;
    }
    if let Some(action) = rest.as_deref().and_then(|r| m.match_command(r, spoken::AFTER_WAKE)) {
        return AfterTake::Spoken(action);
    }
    // Dictation after the wake word: Whisper may spell the name in a way only voice.rs knows.
    let rest_of = |text: &str| wake_rest(text).or_else(|| if from_wake { voice::strip_wake_phrase(text) } else { None });
    if listening && hands_free {
        // The finish phrase is cut off the audio by the take's listener; this is the backstop.
        let raw = m.strip_trailing(raw, Action::Finish);
        let rest_after_wake = rest_of(&raw);
        return AfterTake::Dictate { raw, rest_after_wake };
    }
    AfterTake::Dictate { raw: raw.to_string(), rest_after_wake: rest_of(raw) }
}

async fn process_take(app: &Arc<App>, mut rec: Recording, mut samples: Vec<f32>, released: Instant) -> Result<()> {
    let settings = app.settings();
    let full_audio = samples.clone();
    trim_cue(&mut samples, &rec, &settings);
    if samples.len() < MIN_SPEECH_SAMPLES {
        log::info!("take too short to transcribe");
        app.pill(PillState::Hidden);
        return Ok(());
    }
    let screen = screen_text(&mut rec).await;
    let raw = match transcribe(app, &samples, &settings, &rec.ctx, screen.as_deref()).await {
        Ok(raw) => raw,
        Err(e) => return Err(keep_failed_take(app, &settings, &rec, "", &full_audio, released, rec.kind.as_str(), e)),
    };
    let raw = cleanup::strip_cue_echo(&raw, rec.cue_word);
    log::info!("heard {} words in {} ms", raw.split_whitespace().count(), released.elapsed().as_millis());
    if heard_nothing(&raw, &samples) {
        log::info!("nothing worth typing (speech {:.1} s, peak {:.0} dBFS)", speech_seconds(&samples), peak_dbfs(&samples));
        app.sounds.play(Cue::Cancel, &settings.sounds);
        let gen = app.pill(PillState::Error { message: "Didn't catch that".into() });
        app.hide_pill_later(gen, 2200);
        return Ok(());
    }

    // Voice commands: the cancel phrase, a command after the wake word (the backstop when the
    // take's listener missed it, and how "Navi, take notes" works from the shortcut), and the
    // finish phrase left at the end.
    let matcher = Matcher::new(&settings.voice_commands);
    let (raw, rest) = match after_take(&raw, &matcher, settings.voice_commands.enabled, rec.kind != Kind::Talk, rec.from_wake) {
        AfterTake::Cancel => {
            log::info!("take dropped: it was the cancel phrase");
            app.sounds.play(Cue::Cancel, &settings.sounds);
            let gen = app.pill(PillState::Notice { message: "Cancelled".into() });
            app.hide_pill_later(gen, 900);
            return Ok(());
        }
        AfterTake::Spoken(action) => {
            log::info!("voice: {} after the take", action.id());
            return run_spoken(app, action, &raw, full_audio, Some(rec)).await;
        }
        AfterTake::Dictate { raw, rest_after_wake } => (raw, rest_after_wake),
    };
    if !raw.chars().any(char::is_alphanumeric) {
        // Only the finish phrase was said.
        log::info!("nothing to type once the finish phrase was taken off");
        app.pill(PillState::Hidden);
        return Ok(());
    }

    let has_selection = rec.ctx.selection.is_some();
    let command = match rest.as_deref().filter(|r| rec.from_wake && r.chars().any(char::is_alphabetic)) {
        // After any wake word, custom ones too ("Hey, make that shorter").
        Some(r) => voice::parse_after_wake(&format!("Navi, {r}"), has_selection),
        None if rec.from_wake => voice::parse_after_wake(&raw, has_selection),
        None => voice::parse(&raw, has_selection),
    };
    if let Some(cmd) = command {
        if command_has_target(app, &rec, &cmd) {
            log::info!("voice command: {}", command_name(&cmd));
            let meta = serde_json::json!({ "command": cmd });
            // After the wake word a rewrite may really have been the next line of dictation, so
            // if it fails the words go on the clipboard rather than only into History.
            let words_matter = rec.from_wake && matches!(cmd, VoiceCommand::Rewrite { .. } | VoiceCommand::Replace { .. });
            match run_command(app, &rec, cmd, &raw, &full_audio, released).await {
                Ok(()) => return Ok(()),
                // A command that fails (no AI, nothing to scratch, the AI didn't answer) says why
                // and types nothing: the command words would land in the text, or over the
                // selection. What was said is kept in History.
                Err(e) => {
                    let mut meta = meta;
                    meta["error"] = serde_json::json!(short_error(&e));
                    save_take(app, &settings, &rec, &raw, "", &full_audio, released.elapsed().as_millis() as u64, "error", "command", meta);
                    if words_matter {
                        let words = rest.clone().filter(|w| w.chars().any(char::is_alphanumeric)).map(|w| capitalise_first(&w));
                        if let Some(words) = words {
                            if insert::set_clipboard_text(&words).is_ok() {
                                return Err(anyhow!(with_note(&short_error(&e), "Your words are on the clipboard.")));
                            }
                        }
                    }
                    return Err(e);
                }
            }
        } else {
            // "Hello Navi, this is my first note": nothing here to edit, so it is dictation.
            log::info!("heard Navi's name but there is nothing here to edit, so typing it as dictation");
        }
    }

    // The wake word began the take: what came after it is the dictation.
    let raw = if rec.from_wake {
        match rest {
            Some(rest) if rest.chars().any(char::is_alphanumeric) => capitalise_first(&rest),
            Some(_) => {
                log::info!("the wake word, then nothing");
                app.sounds.play(Cue::Cancel, &settings.sounds);
                let gen = app.pill(PillState::Error { message: format!("Didn't catch anything after \u{201c}{}\u{201d}", settings.voice_commands.wake_label()) });
                app.hide_pill_later(gen, 2200);
                return Ok(());
            }
            None => raw,
        }
    } else {
        raw
    };

    let words = app.db.list_words().unwrap_or_default();
    let snippets = app.db.list_snippets().unwrap_or_default();
    let llm = if rec.mode.use_ai { make_llm(&settings) } else { None };
    let files = if rec.mode.kind == ModeKind::Prompt { project_files(app, &rec.ctx, &settings, &raw).await } else { Vec::new() };
    let out = match pipeline::run(pipeline::PipelineInput {
        raw: &raw,
        mode: &rec.mode,
        ctx: &rec.ctx,
        settings: &settings,
        words: &words,
        snippets: &snippets,
        llm: llm.as_ref(),
        project_files: &files,
    })
    .await
    {
        Ok(out) => out,
        Err(e) => return Err(keep_failed_take(app, &settings, &rec, &raw, &full_audio, released, rec.kind.as_str(), e)),
    };

    // Speak mode: read it aloud in the ElevenLabs voice instead of typing it.
    if rec.mode.id == "speak" {
        let spoken = speak_aloud(app, &settings, &out.text).await;
        let latency = released.elapsed().as_millis() as u64;
        let mut meta = serde_json::json!({ "voice": settings.speak.voice_name });
        match spoken {
            Ok(()) => {
                log::info!("spoke {} words in {latency} ms", out.text.split_whitespace().count());
                save_take(app, &settings, &rec, &raw, &out.text, &full_audio, latency, "spoken", rec.kind.as_str(), meta);
                // Past Navi's 5,000-character cap (speak::MAX_CHARS) only the start is read: say so,
                // or the rest just goes missing.
                if crate::speak::fit(&out.text).1 {
                    let message = "Read the first part only. Navi reads up to 5,000 characters.".to_string();
                    let hold = read_ms(&message, 2600);
                    let gen = app.pill(PillState::Notice { message });
                    app.hide_pill_later(gen, hold);
                } else {
                    let gen = app.pill(PillState::Done { preview: preview(&out.text), outcome: "spoken".into() });
                    app.hide_pill_later(gen, 1300);
                }
            }
            Err(e) => {
                // Never lose what was said: it goes on the clipboard and into History instead.
                log::warn!("speak mode failed: {e:#}");
                let reason = short_error(&e);
                let t = out.text.clone();
                let copied = matches!(tokio::task::spawn_blocking(move || insert::set_clipboard_text(&t)).await, Ok(Ok(())));
                meta["error"] = serde_json::json!(reason);
                save_take(app, &settings, &rec, &raw, &out.text, &full_audio, latency, if copied { "clipboard" } else { "error" }, rec.kind.as_str(), meta);
                app.sounds.play(Cue::Error, &settings.sounds);
                // The reason is shortened if need be so the clipboard note always shows.
                let message = if copied { with_note(&reason, "Your words are on the clipboard.") } else { reason };
                let hold = read_ms(&message, 3500);
                let gen = app.pill(PillState::Error { message });
                app.hide_pill_later(gen, hold);
            }
        }
        return Ok(());
    }

    let text = out.text.clone();
    let method = settings.general.paste_method.clone();
    let restore = settings.general.restore_clipboard;
    let outcome = tokio::task::spawn_blocking(move || insert::insert_text(&text, &method, restore))
        .await
        .map_err(|e| anyhow!("pasting stopped unexpectedly: {e}"))
        .and_then(|inserted| inserted)
        .unwrap_or_else(|e| {
            log::warn!("insert failed: {e:#}");
            let _ = insert::set_clipboard_text(&out.text);
            InsertOutcome::ClipboardOnly("Navi could not type into that window, so the text is on your clipboard.".into())
        });
    let latency = released.elapsed().as_millis() as u64;
    log::info!("{} {} words into {} in {latency} ms", outcome.status(), out.text.split_whitespace().count(), rec.ctx.app_label);

    let mut meta = serde_json::json!({});
    if !out.warnings.is_empty() {
        meta["warnings"] = serde_json::json!(out.warnings);
    }
    if out.used_ai {
        meta["ai"] = serde_json::json!(true);
    }
    let take = save_take(app, &settings, &rec, &raw, &out.text, &full_audio, latency, outcome.status(), rec.kind.as_str(), meta);
    let _ = app.db.bump_word_uses(&out.terms_used);

    // Only text that really went in can be scratched or changed later. After a clipboard-only or
    // part-typed result the window holds something else, so neither this nor an older insert counts.
    *app.last_insert.lock() = match outcome {
        InsertOutcome::Pasted | InsertOutcome::Typed => Some(LastInsert {
            hwnd: rec.ctx.hwnd,
            text: out.text.clone(),
            take_id: take.id.clone(),
            app_label: rec.ctx.app_label.clone(),
            at: Instant::now(),
            checked: false,
            replaced: None,
        }),
        InsertOutcome::ClipboardOnly(_) => None,
    };
    schedule_learning_check(app.clone(), &settings);

    match &outcome {
        InsertOutcome::ClipboardOnly(reason) => {
            app.sounds.play(Cue::Done, &settings.sounds);
            let gen = app.pill(PillState::Error { message: reason.clone() });
            app.hide_pill_later(gen, read_ms(reason, 3500));
        }
        _ => {
            app.sounds.play(Cue::Done, &settings.sounds);
            // The mode wanted the AI but its answer didn't come or wasn't usable: say so, or the
            // text looks like Clean's and the mode seems to do nothing.
            if let Some(why) = out.warnings.first().filter(|_| !out.used_ai) {
                let message = ai_fallback_note(why);
                let hold = read_ms(&message, 3500);
                let gen = app.pill(PillState::Notice { message });
                app.hide_pill_later(gen, hold);
            } else {
                let gen = app.pill(PillState::Done { preview: preview(&out.text), outcome: outcome.status().into() });
                app.hide_pill_later(gen, 1300);
            }
        }
    }
    Ok(())
}

/// The bubble's short version of a pipeline warning ("AI step skipped: no reply in time. ...").
fn ai_fallback_note(warning: &str) -> String {
    let w = warning.to_lowercase();
    let why = if w.contains("no reply in time") {
        "The AI didn't answer in time"
    } else if w.contains("replied to your words") || w.contains("answered your question") {
        "The AI answered instead of rewriting"
    } else if w.contains("far more") || w.contains("wrote code") {
        "The AI added things you didn't say"
    } else if w.contains("returned nothing") {
        "The AI sent back nothing"
    } else {
        "The AI step didn't work"
    };
    format!("{why}, so Navi typed a plain tidy instead.")
}

/// A spoken edit needs something to act on: the text Navi just typed into this window (rewrite,
/// replace) or a selection (transform). Without one it is ordinary dictation that happens to start
/// with the name. Undo always runs, so "Navi, scratch that" with nothing to scratch says so (and
/// types nothing).
fn command_has_target(app: &App, rec: &Recording, cmd: &VoiceCommand) -> bool {
    let fresh_insert = || app.last_insert.lock().as_ref().is_some_and(|l| l.at.elapsed() < Duration::from_secs(600) && l.hwnd == rec.ctx.hwnd);
    match cmd {
        VoiceCommand::Undo | VoiceCommand::Watch { .. } => true,
        VoiceCommand::Transform { .. } => rec.ctx.selection.is_some(),
        VoiceCommand::Critique { .. } => rec.ctx.selection.is_some() || fresh_insert(),
        VoiceCommand::Rewrite { .. } | VoiceCommand::Replace { .. } => fresh_insert(),
    }
}

fn command_name(cmd: &VoiceCommand) -> &'static str {
    match cmd {
        VoiceCommand::Undo => "undo",
        VoiceCommand::Rewrite { .. } => "rewrite",
        VoiceCommand::Replace { .. } => "replace",
        VoiceCommand::Transform { .. } => "transform",
        VoiceCommand::Critique { .. } => "critique",
        VoiceCommand::Watch { .. } => "watch",
    }
}

/// "hello" -> "Hello", "write to Sam" -> "Write to Sam".
fn capitalise_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Counts "what's wrong with this?" questions, so only the answer to the latest one is shown.
static CRITIQUE_SEQ: AtomicU64 = AtomicU64::new(0);

/// "What's wrong with this?": the AI looks in the background and the answer appears in the card in
/// the corner, so the next take is never held up.
fn spawn_critique(app: Arc<App>, llm: Llm, instruction: String, text: String, settings: Settings) {
    let seq = CRITIQUE_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    crate::meeting::show_prompt(&app, PromptState::Thinking { title: "Looking for problems".into() });
    tauri::async_runtime::spawn(async move {
        let (system, user) = voice::critique_messages(&instruction, &text, &settings);
        let failed = |why: String| PromptState::Answer { title: "Navi couldn't check it".into(), body: why };
        let state = match tokio::time::timeout(CRITIQUE_DEADLINE, llm.complete(&system, &user, 1500, 0.3)).await {
            Ok(Ok(reply)) => {
                let mut body = reply.trim().to_string();
                if settings.writing.no_em_dashes {
                    body = cleanup::no_em_dashes(&body);
                }
                if settings.writing.british {
                    body = crate::text::britishise::britishise(&body);
                }
                PromptState::Answer { title: "What's wrong with it".into(), body }
            }
            Ok(Err(e)) => failed(e.to_string()),
            Err(_) => failed(format!("{} took too long to answer.", llm.display_name())),
        };
        // Asked again about something else meanwhile: that answer is the one to show.
        if CRITIQUE_SEQ.load(Ordering::SeqCst) != seq {
            log::info!("a newer \"what's wrong\" was asked, so this answer was dropped");
            return;
        }
        // Closed while it was looking: leave it closed.
        if matches!(crate::meeting::prompt_state(), PromptState::Thinking { .. }) {
            crate::meeting::show_prompt(&app, state);
        }
    });
}

async fn run_command(app: &Arc<App>, rec: &Recording, cmd: VoiceCommand, raw: &str, audio: &[f32], released: Instant) -> Result<()> {
    let settings = app.settings();
    let last = app.last_insert.lock().clone();
    let fresh = last.as_ref().filter(|l| l.at.elapsed() < Duration::from_secs(600) && l.hwnd == rec.ctx.hwnd);
    let summary: String;
    // What the command left in the text, for History: nothing for those that only delete or look.
    let mut final_text = String::new();
    // A change to a selection becomes the last insert, so "scratch that" can put the selection back.
    let mut new_insert: Option<LastInsert> = None;
    match &cmd {
        VoiceCommand::Undo => {
            let l = fresh.ok_or_else(|| anyhow!("There's nothing recent to scratch here."))?;
            insert_at_caret(rec, l)?;
            // Delete exactly what Navi typed, or put back what a change to a selection replaced.
            // Safe in terminals too, where Ctrl+Z would pause the program.
            let back = l.replaced.clone().unwrap_or_default();
            let (last, method, restore, reading) = (l.clone(), settings.general.paste_method.clone(), settings.general.restore_clipboard, settings.context.clone());
            let outcome = tokio::task::spawn_blocking(move || select_back_in(&last, &back, &method, restore, &reading)).await.map_err(|e| anyhow!("{e}"))??;
            *app.last_insert.lock() = None;
            if let InsertOutcome::ClipboardOnly(reason) = outcome {
                return Err(anyhow!(reason));
            }
            summary = if l.replaced.is_some() { "Put back".into() } else { "Scratched".into() };
        }
        VoiceCommand::Replace { from, to } => {
            let l = fresh.ok_or_else(|| anyhow!("There's nothing recent to change here."))?;
            insert_at_caret(rec, l)?;
            let mut new_text = voice::apply_replace(&l.text, from, to);
            if new_text == l.text {
                // The words were not found as spoken (a different spelling, say): let the AI do it.
                let llm = make_llm(&settings).ok_or_else(|| anyhow!("Couldn't find \"{from}\" in what you just said."))?;
                let VoiceCommand::Rewrite { instruction } = voice::replace_as_rewrite(from, to) else { unreachable!() };
                let (sys, user) = voice::rewrite_messages(&instruction, &l.text, &settings);
                new_text = finish_text(&ai_edit(&llm, &sys, &user, 1200, 0.2).await?, &l.text, &settings)?;
            }
            replace_last(app, l, new_text.clone(), &settings).await?;
            final_text = new_text;
            summary = format!("Changed to {to}");
        }
        VoiceCommand::Rewrite { instruction } => {
            let l = fresh.ok_or_else(|| anyhow!("There's nothing recent to rewrite here."))?;
            insert_at_caret(rec, l)?;
            let llm = make_llm(&settings).ok_or_else(|| anyhow!("Rewrites need an AI provider. Turn one on in Settings."))?;
            let (sys, user) = voice::rewrite_messages(instruction, &l.text, &settings);
            let new_text = finish_text(&ai_edit(&llm, &sys, &user, 1200, 0.3).await?, &l.text, &settings)?;
            replace_last(app, l, new_text.clone(), &settings).await?;
            final_text = new_text;
            summary = "Rewritten".into();
        }
        VoiceCommand::Watch { on } => {
            let watching = crate::watch::is_watching();
            summary = match (*on, watching) {
                (true, false) => {
                    crate::watch::start(app)?;
                    "Watching your screen".into()
                }
                (true, true) => "Already watching".into(),
                (false, true) => {
                    crate::watch::stop(app)?;
                    "Stopped watching".into()
                }
                (false, false) => "Navi wasn't watching".into(),
            };
        }
        VoiceCommand::Critique { instruction } => {
            let text = rec.ctx.selection.clone().filter(|s| !s.trim().is_empty()).or_else(|| fresh.map(|l| l.text.clone())).ok_or_else(|| anyhow!("Select some text first, then ask what's wrong with it."))?;
            let llm = make_llm_long(&settings).ok_or_else(|| anyhow!("That needs an AI. Turn one on in Settings > AI."))?;
            spawn_critique(app.clone(), llm, instruction.clone(), text, settings.clone());
            summary = "Looking for problems".into();
        }
        VoiceCommand::Transform { instruction } => {
            let selection = rec.ctx.selection.clone().ok_or_else(|| anyhow!("Select some text first."))?;
            let llm = make_llm(&settings).ok_or_else(|| anyhow!("That needs an AI provider. Turn one on in Settings."))?;
            let (sys, user) = voice::rewrite_messages(instruction, &selection, &settings);
            let new_text = finish_text(&ai_edit(&llm, &sys, &user, 1600, 0.3).await?, &selection, &settings)?;
            let t = new_text.clone();
            let method = settings.general.paste_method.clone();
            let restore = settings.general.restore_clipboard;
            let outcome = tokio::task::spawn_blocking(move || insert::insert_text(&t, &method, restore)).await.map_err(|e| anyhow!("{e}"))??;
            if let InsertOutcome::ClipboardOnly(reason) = outcome {
                // Nothing (or only part) went in, so the window no longer matches any last insert.
                *app.last_insert.lock() = None;
                return Err(anyhow!(reason));
            }
            new_insert = Some(LastInsert {
                hwnd: rec.ctx.hwnd,
                text: new_text.clone(),
                take_id: String::new(),
                app_label: rec.ctx.app_label.clone(),
                at: Instant::now(),
                // The AI wrote this, nobody said it: no misheard words to learn from.
                checked: true,
                replaced: Some(selection),
            });
            final_text = new_text;
            summary = "Done".into();
        }
    }
    let latency = released.elapsed().as_millis() as u64;
    let take = save_take(app, &settings, rec, raw, &final_text, audio, latency, "command", "command", serde_json::json!({ "command": cmd }));
    if let Some(mut l) = new_insert {
        l.take_id = take.id;
        *app.last_insert.lock() = Some(l);
    }
    app.sounds.play(Cue::Done, &settings.sounds);
    let gen = app.pill(PillState::Done { preview: summary, outcome: "command".into() });
    app.hide_pill_later(gen, 1300);
    Ok(())
}

/// A spoken edit's AI call, given as long as dictation gives the AI and no longer: when it runs
/// out, the command fails and says so, and what was said stays in History.
async fn ai_edit(llm: &Llm, system: &str, user: &str, max_tokens: u32, temperature: f32) -> Result<String> {
    tokio::time::timeout(COMMAND_AI_DEADLINE, llm.complete(system, user, max_tokens, temperature))
        .await
        .map_err(|_| anyhow!("{} took too long", llm.display_name()))?
}

async fn replace_last(app: &Arc<App>, last: &LastInsert, new_text: String, settings: &Settings) -> Result<()> {
    let (l, t) = (last.clone(), new_text.clone());
    let (method, restore, reading) = (settings.general.paste_method.clone(), settings.general.restore_clipboard, settings.context.clone());
    let outcome = tokio::task::spawn_blocking(move || select_back_in(&l, &t, &method, restore, &reading)).await.map_err(|e| anyhow!("{e}"))??;
    if let InsertOutcome::ClipboardOnly(reason) = outcome {
        // The window no longer holds what Navi knows it typed.
        *app.last_insert.lock() = None;
        return Err(anyhow!(reason));
    }
    if let Some(l) = app.last_insert.lock().as_mut() {
        l.text = new_text;
        l.at = Instant::now();
        l.checked = false;
    }
    Ok(())
}

/// Selects back over what Navi typed (`last`) and replaces it with `text`, put in the way Settings
/// says (`method`), but only if that window is still in front and Navi's text still sits just
/// before the caret. Checked again here, just before the keys, because the AI can take a while:
/// you may have moved on, typed more, or clicked into another field of the same window. Reads the
/// field only as Settings allow (`reading`). Runs on a blocking thread.
fn select_back_in(last: &LastInsert, text: &str, method: &str, restore: bool, reading: &ContextSettings) -> Result<InsertOutcome> {
    if context::foreground_hwnd() != last.hwnd {
        return Err(anyhow!("You've moved to another window, so Navi left the text alone."));
    }
    let now = context::capture(reading);
    if now.category != "terminal" && !ends_with_insert(now.field_text.as_deref(), &last.text) {
        return Err(anyhow!(TEXT_CHANGED));
    }
    insert::select_back_and_replace_with(last.text.encode_utf16().count(), text, method, restore)
}

const TEXT_CHANGED: &str = "The text has changed since Navi typed it, so Navi left it alone.";

/// Refuses a spoken edit when what Navi typed is no longer just before the caret (you typed after
/// it, changed it or clicked elsewhere): selecting back from there would delete your own words.
fn insert_at_caret(rec: &Recording, last: &LastInsert) -> Result<()> {
    // Terminals delete with Backspace, which can't reach past the line being typed, and their
    // screens rarely read back as typed.
    if rec.ctx.category == "terminal" || ends_with_insert(rec.ctx.field_text.as_deref(), &last.text) {
        Ok(())
    } else {
        Err(anyhow!(TEXT_CHANGED))
    }
}

/// True when `field` (the text before the caret) still ends with what Navi typed. Only letters
/// and digits are compared, as apps change spacing, quotes and line breaks as text goes in. With
/// nothing read (the app doesn't say, reading is off, or the read ran out of time) there is
/// nothing to go on, so it counts. An empty field read refuses, as nothing is before the caret,
/// but context.rs reports an empty field as nothing read (None) for now, so that case counts too.
fn ends_with_insert(field: Option<&str>, inserted: &str) -> bool {
    let Some(field) = field else { return true };
    let squash = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect::<Vec<char>>();
    let (field, inserted) = (squash(field), squash(inserted));
    let tail = &inserted[inserted.len().saturating_sub(CARET_CHECK_CHARS)..];
    // Or all that was read is the end of Navi's text: it was read only so far back, or the app
    // keeps each paragraph apart (Notion) and gave just the last one.
    field.ends_with(tail) || (!field.is_empty() && inserted.ends_with(&field))
}

/// Tidies an AI rewrite. Errors when the model chatted back instead of editing the text.
fn finish_text(reply: &str, original: &str, settings: &Settings) -> Result<String> {
    let mut t = pipeline::clean_ai_reply(reply, original).ok_or_else(|| anyhow!("The AI replied instead of editing, so nothing was changed."))?;
    if settings.writing.no_em_dashes {
        t = cleanup::no_em_dashes(&t);
    }
    if settings.writing.british {
        t = crate::text::britishise::britishise(&t);
    }
    Ok(t)
}

async fn process_ramble(app: &Arc<App>, mut rec: Recording, mut samples: Vec<f32>, released: Instant) -> Result<()> {
    let settings = app.settings();
    let full_audio = samples.clone();
    // Cut the ramble cue out, then the start cue.
    if let Some((a, b)) = rec.ramble_cue {
        let (a, b) = (a.min(samples.len()), b.min(samples.len()));
        samples.drain(a..b);
    }
    trim_cue(&mut samples, &rec, &settings);
    if samples.len() < MIN_SPEECH_SAMPLES {
        log::info!("ramble too short to transcribe");
        app.pill(PillState::Hidden);
        return Ok(());
    }
    let screen = screen_text(&mut rec).await;
    let raw = match transcribe(app, &samples, &settings, &rec.ctx, screen.as_deref()).await {
        Ok(raw) => raw,
        Err(e) => return Err(keep_failed_take(app, &settings, &rec, "", &full_audio, released, "ramble", e)),
    };
    let raw = cleanup::strip_cue_echo(&cleanup::strip_cue_echo(&raw, rec.cue_word), Some("listen"));
    // The finish phrase is cut off the audio by the take's listener; this is the backstop.
    let raw = if settings.voice_commands.enabled { Matcher::new(&settings.voice_commands).strip_trailing(&raw, Action::Finish) } else { raw };
    log::info!("ramble: heard {} words", raw.split_whitespace().count());
    if heard_nothing(&raw, &samples) {
        log::info!("nothing worth keeping (speech {:.1} s, peak {:.0} dBFS)", speech_seconds(&samples), peak_dbfs(&samples));
        let gen = app.pill(PillState::Error { message: "Didn't catch that".into() });
        app.hide_pill_later(gen, 2200);
        return Ok(());
    }
    let words = app.db.list_words().unwrap_or_default();
    let (tidy, _) = cleanup::apply_dictionary(&raw, &words);
    let llm = make_llm(&settings);
    let mut notes = match &llm {
        Some(llm) => {
            let (sys, user) = ramble::notes_messages(&tidy, &rec.ctx, &settings);
            // Its own deadline: no take can start until this one is written up.
            let reply = tokio::time::timeout(RAMBLE_AI_DEADLINE, llm.complete(&sys, &user, 2500, 0.2))
                .await
                .unwrap_or_else(|_| Err(anyhow!("{} took too long", llm.display_name())));
            match reply.and_then(|o| ramble::parse_notes(&o)) {
                Ok(n) => n,
                Err(e) => {
                    log::warn!("notes from AI failed, using offline notes: {e:#}");
                    ramble::offline_notes(&tidy)
                }
            }
        }
        None => ramble::offline_notes(&tidy),
    };
    if settings.writing.british {
        notes.summary = crate::text::britishise::britishise(&notes.summary);
    }
    let when = chrono::Local::now().format("%A %-d %B %Y, %H:%M").to_string();
    let markdown = ramble::to_markdown(&notes, &tidy, &when);
    let dir = settings.ramble.notes_dir.clone().map(std::path::PathBuf::from).unwrap_or_else(|| app.paths.notes_default.clone());
    let saved = integrations::save_markdown(&dir, &notes.title, &markdown);

    // What happened to the tasks, for the bubble: a failure is said there, never only logged, so
    // nobody thinks they went when they didn't.
    let mut sent = String::new();
    if let Some(why) = ramble_send_blocked(&settings, !notes.tasks.is_empty(), |name| secrets::get(name).is_some()) {
        log::info!("ramble not sent to {}: {why}", settings.ramble.tasks_to);
        sent = format!(" · not sent ({why})");
    } else {
        match settings.ramble.tasks_to.as_str() {
            "clickup" if !notes.tasks.is_empty() => {
                if let (Some(token), Some(list)) = (secrets::get("clickup"), settings.ramble.clickup_list_id.clone()) {
                    match integrations::clickup_create_tasks(&token, &list, &notes.title, &notes.tasks).await {
                        Ok(n) => sent = format!(" · {n} tasks to ClickUp"),
                        Err(e) => {
                            log::warn!("ClickUp: {e:#}");
                            sent = format!(" · not sent: {}", first_sentence(&short_error(&e)));
                        }
                    }
                }
            }
            "notion" => {
                if let (Some(token), Some(db)) = (secrets::get("notion"), settings.ramble.notion_database_id.clone()) {
                    match integrations::notion_create_page(&token, &db, &notes).await {
                        Ok(_) => sent = " · sent to Notion".into(),
                        Err(e) => {
                            log::warn!("Notion: {e:#}");
                            sent = format!(" · not sent: {}", first_sentence(&short_error(&e)));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let mut status = "notes";
    if settings.ramble.paste_notes {
        let text = ramble::to_plain(&notes);
        let method = settings.general.paste_method.clone();
        let restore = settings.general.restore_clipboard;
        if let Ok(Ok(o)) = tokio::task::spawn_blocking(move || insert::insert_text(&text, &method, restore)).await {
            if o == InsertOutcome::Pasted {
                status = "notes";
            }
        }
    }
    let latency = released.elapsed().as_millis() as u64;
    let notes_path = saved.as_ref().ok().map(|p| p.display().to_string());
    let final_text = ramble::to_plain(&notes);
    save_take(app, &settings, &rec, &raw, &final_text, &full_audio, latency, status, "ramble", serde_json::json!({ "notes": notes, "notesPath": notes_path }));
    // On the Notes page too, beside your calls.
    let spoken_ms = (samples.len() as u64) * 1000 / audio::SAMPLE_RATE as u64;
    crate::meeting::save_ramble(app, &notes.title, ramble::to_markdown(&notes, "", ""), &tidy, spoken_ms, notes_path);

    app.sounds.play(Cue::Done, &settings.sounds);
    let tasks = notes.tasks.len();
    // Said in words: a tick alone left it unclear where the ramble went.
    let summary = format!("Saved to Notes{}{}", if tasks > 0 { format!(" · {tasks} task{}", if tasks == 1 { "" } else { "s" }) } else { String::new() }, sent);
    let hold = read_ms(&summary, 2600);
    let gen = app.pill(PillState::Notice { message: summary });
    app.hide_pill_later(gen, hold);
    Ok(())
}

/// Why a ramble's tasks can't go where Settings > Notes and tasks sends them, before trying, as a
/// short note for the bubble: no token or ID saved, or Local only (which blocks ClickUp and Notion,
/// as it does for call notes). None when they can be sent, or there is nothing to send: ClickUp
/// takes only the tasks, Notion the whole note. `has_token` asks Windows Credential Manager.
fn ramble_send_blocked(settings: &Settings, has_tasks: bool, has_token: impl Fn(&str) -> bool) -> Option<String> {
    let (service, token, id, id_name) = match settings.ramble.tasks_to.as_str() {
        "clickup" if has_tasks => ("ClickUp", "clickup", settings.ramble.clickup_list_id.as_deref(), "list ID"),
        "notion" => ("Notion", "notion", settings.ramble.notion_database_id.as_deref(), "database ID"),
        _ => return None,
    };
    if !has_token(token) {
        Some(format!("{service} isn't connected"))
    } else if id.is_none_or(|id| id.trim().is_empty()) {
        Some(format!("no {service} {id_name}"))
    } else if settings.privacy.local_only {
        // Only when something really would have gone.
        Some("Local only".into())
    } else {
        None
    }
}

/// The first sentence of `message`: a service's error in full can push the rest of the bubble out.
fn first_sentence(message: &str) -> &str {
    match message.find(". ") {
        Some(i) => &message[..=i],
        None => message,
    }
}

/// While rambling, transcribe what has been said so far every few seconds and show the tail in the bubble.
fn spawn_ramble_preview(app: Arc<App>) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(RAMBLE_CHUNK_SECS)).await;
            let still = matches!(&app.session.lock().phase, Phase::Recording(r) if r.kind == Kind::Ramble);
            if !still {
                break;
            }
            let settings = app.settings();
            let samples = app.audio.snapshot_16k(0);
            let start = samples.len().saturating_sub(audio::SAMPLE_RATE as usize * 30);
            let Some(engine) = app.engine.read().await.clone() else { continue };
            let opts = TranscribeOptions { language: settings.engine.language.clone(), prompt: String::new() };
            if let Ok(tr) = engine.transcribe(&samples[start..], &opts).await {
                let still = matches!(&app.session.lock().phase, Phase::Recording(r) if r.kind == Kind::Ramble);
                if still && !heard_nothing(&tr.text, &samples[start..]) {
                    app.emit("ramble-partial", tr.text.trim().to_string());
                }
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn save_take(app: &Arc<App>, settings: &Settings, rec: &Recording, raw: &str, final_text: &str, audio_samples: &[f32], latency_ms: u64, status: &str, kind: &str, meta: serde_json::Value) -> Take {
    let id = uuid::Uuid::new_v4().to_string();
    let duration_ms = (audio_samples.len() as u64) * 1000 / (audio::SAMPLE_RATE as u64);
    let mut take = Take {
        id: id.clone(),
        created_at: chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, false),
        duration_ms,
        app_name: rec.ctx.app_label.clone(),
        window_title: rec.ctx.window_title.clone(),
        mode_id: if kind == "ramble" { "notes".into() } else { rec.mode.id.clone() },
        raw_text: raw.to_string(),
        final_text: final_text.to_string(),
        // A voice edit changes words already counted when they were dictated.
        words: if kind == "command" { 0 } else { final_text.split_whitespace().count() as u32 },
        audio_path: None,
        model: settings.engine.model.clone(),
        latency_ms,
        status: status.to_string(),
        kind: kind.to_string(),
        meta,
    };
    if !settings.privacy.save_history {
        return take;
    }
    let path = app.paths.audio.join(format!("{id}.wav"));
    match audio::write_wav(&path, audio_samples) {
        Ok(()) => take.audio_path = Some(path.display().to_string()),
        Err(e) => log::warn!("could not save the recording: {e:#}"),
    }
    if let Err(e) = app.db.insert_take(&take) {
        log::warn!("could not save the take: {e:#}");
    }
    app.emit("take", take.clone());
    take
}

/// Never lose what was said: a take that failed after it was recorded (no speech engine, say) is
/// kept in History with its recording, as "Failed", so it can be re-run there. Returns the error
/// to show, saying so when it was kept.
#[allow(clippy::too_many_arguments)]
fn keep_failed_take(app: &Arc<App>, settings: &Settings, rec: &Recording, raw: &str, audio_samples: &[f32], released: Instant, kind: &str, e: anyhow::Error) -> anyhow::Error {
    log::warn!("take failed: {e:#}");
    let latency = released.elapsed().as_millis() as u64;
    let take = save_take(app, settings, rec, raw, "", audio_samples, latency, "error", kind, serde_json::json!({ "error": short_error(&e) }));
    if take.audio_path.is_none() {
        // History is off, or the recording couldn't be written: nothing to re-run.
        return e;
    }
    let reason = match e.downcast_ref::<NoEngine>() {
        Some(no) => no.reason.to_string(),
        None => short_error(&e),
    };
    anyhow!(with_note(&reason, "Saved in History."))
}

/// `reason` and then `note`, with the reason shortened so the note survives `short_error`.
fn with_note(reason: &str, note: &str) -> String {
    let room = 80usize.saturating_sub(note.chars().count() + 2);
    let reason = reason.trim();
    if reason.chars().count() > room {
        format!("{}… {note}", reason.chars().take(room).collect::<String>().trim_end())
    } else {
        format!("{reason} {note}")
    }
}

/// Speak mode: reads `text` aloud in the ElevenLabs voice. Its own reasons are short enough to go
/// beside "Your words are on the clipboard." in the bubble.
async fn speak_aloud(app: &Arc<App>, settings: &Settings, text: &str) -> Result<()> {
    if settings.privacy.local_only {
        return Err(anyhow!(SPEAK_LOCAL_ONLY));
    }
    let key = secrets::get("elevenlabs").ok_or_else(|| anyhow!(SPEAK_NO_KEY))?;
    let samples = crate::speak::speak(&key, &settings.speak.voice_id, &settings.speak.model, text).await?;
    // Voice commands stop listening while Navi talks, or a sentence starting "Navi, ..." would wake it.
    crate::wake::hold_off(samples.len() as f32 / crate::speak::SAMPLE_RATE as f32);
    app.sounds.play_samples(samples, crate::speak::SAMPLE_RATE);
    Ok(())
}

const SPEAK_LOCAL_ONLY: &str = "Local only is on, so ElevenLabs is blocked.";
const SPEAK_NO_KEY: &str = "Add your ElevenLabs key in Settings > Voice.";

fn preview(text: &str) -> String {
    let t: String = text.chars().take(60).collect();
    if text.chars().count() > 60 { format!("{t}…") } else { t }
}

// ------------------------------------------------------------------------------------------
// Learning from corrections
// ------------------------------------------------------------------------------------------

fn schedule_learning_check(app: Arc<App>, settings: &Settings) {
    if !settings.learning.enabled || !settings.context.read_field {
        return;
    }
    let delay = settings.learning.check_after_secs.max(5) as u64;
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(delay)).await;
        learning_check_now(&app).await;
    });
}

pub async fn learning_check_now(app: &Arc<App>) {
    let settings = app.settings();
    if !settings.learning.enabled || !settings.context.read_field {
        return;
    }
    let last = {
        let mut guard = app.last_insert.lock();
        match guard.as_mut() {
            Some(l) if !l.checked && l.at.elapsed() >= Duration::from_secs(3) => {
                l.checked = true;
                l.clone()
            }
            _ => return,
        }
    };
    let hwnd = last.hwnd;
    let Ok(Some(now)) = tokio::task::spawn_blocking(move || context::read_focused_text(hwnd)).await else { return };
    for (from, to) in learn::find_corrections(&last.text, &now) {
        let Ok(Some(sugg)) = app.db.record_suggestion(&from, &to, &last.app_label) else { continue };
        // An everyday word ("quickly" -> "Quikly") is only ever suggested: added by itself it
        // would rewrite that word in everything said from then on.
        if sugg.count >= settings.learning.auto_add_after.max(1) && learn::can_auto_add(&from) {
            if let Ok(Some(word)) = app.db.accept_suggestion(&sugg.id) {
                // Never cover the bubble of a take in progress; the dictionary still updates.
                let idle = matches!(app.session.lock().phase, Phase::Idle);
                if idle {
                    app.sounds.play(Cue::Learn, &settings.sounds);
                    app.show_pill_window(context::foreground_hwnd());
                    let gen = app.pill(PillState::Learned { from: from.clone(), to: word.term.clone() });
                    app.hide_pill_later(gen, 2900);
                } else {
                    log::info!("learned a word while a take was running");
                }
            }
        } else {
            app.emit("suggestion", sugg);
        }
    }
}

fn short_error(e: &anyhow::Error) -> String {
    let s = e.to_string();
    let first = s.lines().next().unwrap_or("Something went wrong").trim().to_string();
    if first.chars().count() > 80 { first.chars().take(78).collect::<String>() + "…" } else { first }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_just_before_the_caret_counts() {
        assert!(ends_with_insert(Some("Dear Sam,\nThanks for the notes. See you on Friday."), "See you on Friday."));
        // Apps change spacing, quotes, line breaks and capitals as the text goes in.
        assert!(ends_with_insert(Some("Hi \u{2018}team\u{2019}\n\nit\u{2019}s  done"), " Hi 'team'\r\nIt's done"));
        assert!(ends_with_insert(Some("ends here "), "ends here"));
        // Notion reads back only the paragraph the caret is in.
        assert!(ends_with_insert(Some("And the second paragraph."), "The first paragraph.\n\nAnd the second paragraph."));
    }

    #[test]
    fn typing_after_the_insert_or_moving_the_caret_refuses() {
        // Typed two words by hand after Navi's text.
        assert!(!ends_with_insert(Some("See you on Friday. Cheers Alex"), "See you on Friday."));
        // Clicked back into the greeting.
        assert!(!ends_with_insert(Some("Dear Sam,"), "See you on Friday."));
        // Deleted the start of Navi's text by hand: the count would reach into the text before.
        assert!(!ends_with_insert(Some("Notes: on Friday."), "See you on Friday."));
        // An empty field read: nothing is before the caret. context.rs gives None for an empty
        // field today (counted as nothing read), so this guards a read that keeps the empty text.
        assert!(!ends_with_insert(Some(""), "See you on Friday."));
    }

    #[test]
    fn nothing_read_or_nothing_to_compare_counts() {
        assert!(ends_with_insert(None, "anything at all"));
        assert!(ends_with_insert(Some("whatever"), "?! "));
    }

    #[test]
    fn a_long_insert_is_checked_by_its_end() {
        let long: String = (0..400).map(|i| format!("word{i} ")).collect();
        // The field read stops 1,500 characters back, partway into the insert.
        let field: String = long.chars().skip(long.chars().count() - 1500).collect();
        assert!(ends_with_insert(Some(&field), &long));
        assert!(!ends_with_insert(Some(&format!("{field} extra")), &long));
    }

    #[test]
    fn long_model_must_suit_the_provider() {
        assert!(long_model_fits("claude-code", "opus"));
        assert!(long_model_fits("claude-code", "claude-sonnet-4-5"));
        assert!(!long_model_fits("claude-code", "gpt-4.1"));
        assert!(long_model_fits("anthropic", "claude-opus-4-1"));
        assert!(!long_model_fits("anthropic", "opus"));
        assert!(long_model_fits("openai", "gpt-4.1"));
        assert!(!long_model_fits("openai", "opus"));
        assert!(!long_model_fits("openai", "claude-sonnet-4-5"));
        assert!(long_model_fits("ollama", "qwen2.5:14b"));
        assert!(!long_model_fits("ollama", "Sonnet"));
        assert!(!long_model_fits("ollama", "gpt-4.1"));
        assert!(long_model_fits("custom", "mistral-large"));
        assert!(!long_model_fits("custom", "haiku"));
        assert!(!long_model_fits("custom", " Opus "));
        // A gateway serving Claude by its full names keeps them.
        assert!(long_model_fits("custom", "claude-sonnet-4-5"));
        assert!(!long_model_fits("codex", "opus"));
        assert!(!long_model_fits("codex", "claude-sonnet-4-5"));
    }

    fn ai(provider: &str, model: &str, long_model: &str) -> Settings {
        let mut s = Settings::default();
        s.privacy.local_only = false;
        s.ai.provider = provider.into();
        s.ai.model = model.into();
        s.ai.long_model = long_model.into();
        s
    }

    #[test]
    fn a_long_model_left_from_another_provider_is_ignored() {
        assert_eq!(make_llm_long(&ai("claude-code", "haiku", "gpt-4.1")).unwrap().model, "sonnet");
        assert_eq!(make_llm_long(&ai("claude-code", "haiku", "opus")).unwrap().model, "opus");
        assert_eq!(make_llm_long(&ai("claude-code", "haiku", "")).unwrap().model, "sonnet");
        assert_eq!(make_llm_long(&ai("ollama", "llama3.1", "opus")).unwrap().model, "llama3.1");
        assert_eq!(make_llm_long(&ai("ollama", "llama3.1", "qwen2.5:14b")).unwrap().model, "qwen2.5:14b");
    }

    fn status(state: &str) -> EngineStatus {
        EngineStatus { state: state.into(), message: "details".into(), ..Default::default() }
    }

    #[test]
    fn no_engine_says_why_instead_of_try_again() {
        assert_eq!(no_engine(&status("starting")).to_string(), "The speech engine is still starting. Try again in a moment.");
        assert_eq!(no_engine(&status("stopped")).reason, "The speech engine is still starting.");
        assert_eq!(no_engine(&status("ready")).reason, "The speech engine is still starting.");
        assert_eq!(no_engine(&status("missing-model")).to_string(), "The speech model isn't downloaded yet. See Settings > Speech engine.");
        assert_eq!(no_engine(&status("missing-engine")).reason, "The speech engine isn't installed yet.");
        assert_eq!(no_engine(&status("error")).to_string(), "The speech engine couldn't start. See Settings > Speech engine.");
        // Every message stays within what short_error keeps.
        for state in ["starting", "missing-model", "missing-engine", "error"] {
            assert!(no_engine(&status(state)).to_string().chars().count() <= 80, "{state}");
        }
    }

    #[test]
    fn the_history_note_survives_a_long_reason() {
        assert_eq!(with_note("The speech engine is still starting.", "Saved in History."), "The speech engine is still starting. Saved in History.");
        let long = "The speech engine stopped unexpectedly while it was working on your words, twice";
        let noted = with_note(long, "Saved in History.");
        assert!(noted.ends_with("… Saved in History."), "{noted}");
        assert!(noted.chars().count() <= 80, "{noted}");
        assert_eq!(short_error(&anyhow!(noted.clone())), noted);
    }

    #[test]
    fn the_clipboard_note_survives_every_speak_reason() {
        for reason in [SPEAK_LOCAL_ONLY, SPEAK_NO_KEY] {
            assert_eq!(with_note(reason, "Your words are on the clipboard."), format!("{reason} Your words are on the clipboard."));
        }
        let long = "ElevenLabs refused the request because the voice you picked is not in this account any more";
        let noted = with_note(long, "Your words are on the clipboard.");
        assert!(noted.ends_with("… Your words are on the clipboard."), "{noted}");
        assert!(noted.chars().count() <= 80, "{noted}");
    }

    #[test]
    fn a_lost_microphone_is_said_plainly() {
        assert_eq!(mic_lost_message("The microphone 'Microphone (Realtek(R) Audio)' was disconnected"), "The microphone was disconnected, so Navi stopped listening.");
        assert_eq!(mic_lost_message("The microphone 'USB Mic' stopped working: device busy"), "The microphone stopped working, so Navi stopped listening.");
    }

    #[test]
    fn long_messages_stay_up_long_enough_to_read() {
        assert_eq!(read_ms("Didn't catch that", 2200), 2200);
        assert_eq!(read_ms("Still finishing the last one.", 3000), 3000);
        let long = "x".repeat(80);
        assert_eq!(read_ms(&long, 3000), 5000);
        assert_eq!(read_ms(&"x".repeat(400), 3000), 6000);
    }

    fn ramble_to(to: &str, list: Option<&str>, db: Option<&str>, local_only: bool) -> Settings {
        let mut s = Settings::default();
        s.ramble.tasks_to = to.into();
        s.ramble.clickup_list_id = list.map(str::to_string);
        s.ramble.notion_database_id = db.map(str::to_string);
        s.privacy.local_only = local_only;
        s
    }

    #[test]
    fn a_ramble_says_why_its_tasks_were_not_sent() {
        let all = |_: &str| true;
        let none = |_: &str| false;
        // Ready to send.
        assert_eq!(ramble_send_blocked(&ramble_to("clickup", Some("901"), None, false), true, all), None);
        assert_eq!(ramble_send_blocked(&ramble_to("notion", None, Some("abc"), false), false, all), None);
        // Nothing to send, or nowhere chosen.
        assert_eq!(ramble_send_blocked(&ramble_to("clickup", None, None, true), false, none), None);
        assert_eq!(ramble_send_blocked(&ramble_to("none", None, None, true), true, none), None);
        // Not set up: said, rather than skipped quietly.
        assert_eq!(ramble_send_blocked(&ramble_to("clickup", Some("901"), None, false), true, none).as_deref(), Some("ClickUp isn't connected"));
        assert_eq!(ramble_send_blocked(&ramble_to("clickup", Some("  "), None, false), true, all).as_deref(), Some("no ClickUp list ID"));
        assert_eq!(ramble_send_blocked(&ramble_to("notion", None, None, false), false, all).as_deref(), Some("no Notion database ID"));
        // Local only is named only when something really would have gone.
        assert_eq!(ramble_send_blocked(&ramble_to("clickup", Some("901"), None, true), true, all).as_deref(), Some("Local only"));
        assert_eq!(ramble_send_blocked(&ramble_to("notion", None, None, true), false, none).as_deref(), Some("Notion isn't connected"));
    }

    #[test]
    fn a_service_error_is_cut_to_its_first_sentence() {
        assert_eq!(first_sentence("ClickUp rejected the token. Check it in Settings."), "ClickUp rejected the token.");
        assert_eq!(first_sentence("No connection to Notion"), "No connection to Notion");
    }

    fn matcher_with_wake(wake: &[&str]) -> Matcher {
        let mut vc = crate::types::VoiceCommandSettings::default();
        for e in vc.commands.iter_mut().filter(|e| e.action == "wake") {
            e.phrases = wake.iter().map(|s| s.to_string()).collect();
        }
        Matcher::new(&vc)
    }

    fn dictate(raw: &str, rest: Option<&str>) -> AfterTake {
        AfterTake::Dictate { raw: raw.into(), rest_after_wake: rest.map(str::to_string) }
    }

    #[test]
    fn what_a_take_turns_out_to_be() {
        let m = matcher_with_wake(&["Navi", "Hey Navi"]);
        // The cancel phrase alone, or after the wake word, throws the take away while listening.
        assert_eq!(after_take("Don't worry.", &m, true, true, false), AfterTake::Cancel);
        assert_eq!(after_take("Navi, don't worry", &m, true, true, false), AfterTake::Cancel);
        assert_eq!(after_take("Don't worry.", &m, false, true, false), dictate("Don't worry.", None), "voice commands off: it is typed");
        assert_eq!(after_take("Don't worry about the deadline.", &m, true, true, false), dictate("Don't worry about the deadline.", None));
        // Holding push-to-talk is deliberate: a bare "Don't worry." is a reply to type. After the
        // wake word it still cancels.
        assert_eq!(after_take("Don't worry.", &m, true, false, false), dictate("Don't worry.", None));
        assert_eq!(after_take("Navi, don't worry.", &m, true, false, false), AfterTake::Cancel);
        // Word for word: a short reply with a word either side is typed.
        for said in ["Okay, don't worry.", "Don't worry, thanks.", "So don't worry."] {
            assert_eq!(after_take(said, &m, true, true, false), dictate(said, None), "{said}");
        }
        // A command after the wake word runs whether or not the listener is on.
        assert_eq!(after_take("Navi, take notes", &m, true, true, false), AfterTake::Spoken(Action::NotesOn));
        assert_eq!(after_take("Navi, take notes", &m, false, false, false), AfterTake::Spoken(Action::NotesOn));
        assert_eq!(after_take("Hey Navi, watch my screen.", &m, false, false, false), AfterTake::Spoken(Action::WatchOn));
        // Only the wake words in Settings, and "Nav" never: said to a colleague, it is dictation.
        for said in ["Nav, take notes.", "Hi Nav, take notes please.", "Nav, stop watching.", "Nav, don't worry."] {
            assert!(matches!(after_take(said, &m, true, true, false), AfterTake::Dictate { .. }), "{said}");
        }
        let hey_navi = matcher_with_wake(&["Hey Navi"]);
        assert!(matches!(after_take("Navi, take notes.", &hey_navi, true, true, false), AfterTake::Dictate { .. }), "a removed wake word no longer counts");
        // Outside a wake take, "Navy" needs a pause after it, as the idle listener asks.
        assert!(matches!(after_take("Navy take notes", &m, true, true, false), AfterTake::Dictate { .. }));
        assert_eq!(after_take("Navy, take notes", &m, true, true, false), AfterTake::Spoken(Action::NotesOn));
        assert_eq!(after_take("Navy take notes", &m, true, true, true), AfterTake::Spoken(Action::NotesOn), "the wake word started this take");
        // The finish phrase at the end goes, only in a hands-free take while listening.
        assert_eq!(after_take("See you Friday. That's all.", &m, true, true, false), dictate("See you Friday.", None));
        assert_eq!(after_take("See you Friday. That's all.", &m, true, false, false), dictate("See you Friday. That's all.", None));
        assert_eq!(after_take("See you Friday. That's all.", &m, false, true, false), dictate("See you Friday. That's all.", None));
        assert_eq!(after_take("See you Friday. That's all, thanks.", &m, true, true, false), dictate("See you Friday. That's all, thanks.", None), "word for word");
        assert_eq!(after_take("Navi, email Sam. That's all.", &m, true, true, true), dictate("Navi, email Sam.", Some("email Sam.")));
        // A custom wake word leaves what came after it for voice edits and dictation.
        let hey = matcher_with_wake(&["Hey"]);
        assert_eq!(after_take("Hey, make that shorter", &hey, true, true, true), dictate("Hey, make that shorter", Some("make that shorter")));
    }

    #[test]
    fn a_voice_cancelled_take_is_kept_only_with_enough_speech() {
        assert!(!keep_cancelled(true, 1.4));
        assert!(keep_cancelled(true, 1.6));
        assert!(!keep_cancelled(false, 3.0), "History off keeps nothing");
    }

    #[test]
    fn a_cancel_nothing_could_undo_is_asked_for_twice() {
        // History off: a ramble, or a long take, is too much to lose to a misheard phrase.
        assert!(cancel_needs_confirming(false, Kind::Ramble, 2.0));
        assert!(cancel_needs_confirming(false, Kind::HandsFree, 16.0));
        assert!(!cancel_needs_confirming(false, Kind::HandsFree, 5.0), "a short take goes at once");
        // History on: it is kept there as Cancelled, with Re-run.
        assert!(!cancel_needs_confirming(true, Kind::Ramble, 100.0));
        assert!(!cancel_needs_confirming(true, Kind::HandsFree, 100.0));
    }

    #[test]
    fn a_failed_take_keeps_the_engine_reason_from_the_error() {
        let e: anyhow::Error = no_engine(&status("missing-model")).into();
        assert_eq!(e.downcast_ref::<NoEngine>().map(|n| n.reason), Some("The speech model isn't downloaded yet."));
        assert_eq!(short_error(&e), "The speech model isn't downloaded yet. See Settings > Speech engine.");
    }
}
