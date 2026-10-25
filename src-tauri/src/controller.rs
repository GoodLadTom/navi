//! The take state machine: hotkey events in, text out.
//!
//! Idle --TalkDown--> Recording(Talk) --TalkUp (held)--> Processing --> Idle
//!                       |  TalkUp quickly, then TalkDown within 380 ms -> Recording(HandsFree)
//!                       |  Ramble key while held                        -> Recording(Ramble)
//!                       |  Cancel                                       -> Idle (discarded)
//! Recording(HandsFree | Ramble) --TalkDown--> Processing
//!   (a TalkDown less than 45 ms after a TalkUp is a key bounce, ignored, here as in a double tap)
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
use crate::types::{ActionKey, AppContext, ContextSettings, EngineStatus, HotkeyEvent, Mode, ModeKind, PillState, Settings, Take, VoiceCommand};
use crate::{audio, context, hotkey, integrations, store::secrets};
use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedReceiver;

const DOUBLE_TAP_MS: u128 = 350;
const DOUBLE_TAP_WINDOW_MS: u64 = 380;
/// Keys up for less than this between a quick release and the next press is a bounce while the
/// fingers settle on the chord (or the hook's catch-up of a release it missed, which comes with no
/// gap at all), not a second tap: a deliberate double tap leaves the keys up for longer.
const KEY_BOUNCE_MS: u128 = 45;

fn is_key_bounce(gap_ms: u128) -> bool {
    gap_ms < KEY_BOUNCE_MS
}
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
/// In a field Navi can't read (reading is off, the app doesn't say, or a terminal), a spoken edit
/// selects back over what Navi typed only if it typed it this recently and no key has been pressed
/// since: with nothing to compare, anything older may have had words added after it or the caret
/// moved away, and selecting back would take the user's own words.
const BLIND_EDIT_WINDOW: Duration = Duration::from_secs(15);
/// How long "Still finishing the last one" shows before the bubble goes back to processing.
const BUSY_NOTE_MS: u64 = 1600;
/// A wake word take finishes after this much quiet, whether or not anything followed the name yet.
/// 1.6 s cut people off while they were thinking of the next word; 6 s leaves room to think, and
/// the finish phrase ("that's all"), a tap of the shortcut or (if Settings allows) Enter finishes it sooner.
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
/// An Ask AI take finishes this long after you stop talking (a tap of the key finishes it
/// sooner), or after ASK_FIRST_WORDS if nothing is said at all.
const ASK_QUIET: Duration = Duration::from_millis(2000);
const ASK_FIRST_WORDS: Duration = Duration::from_secs(8);
/// How long an Ask AI request waits for the AI (the model for longer jobs answers it).
const ASK_DEADLINE: Duration = Duration::from_secs(120);
/// What the bubble says while the AI works.
const AI_REWRITING: &str = "AI is rewriting";
const AI_ASKED: &str = "AI is working on it";
const AI_WRITING: &str = "AI is writing";

pub fn spawn(app: Arc<App>, mut rx: UnboundedReceiver<hotkey::HotkeyMsg>) {
    tauri::async_runtime::spawn(async move {
        loop {
            // A quick release waiting for a second tap: its window closes DOUBLE_TAP_WINDOW_MS after
            // the hook saw the release, however late this loop got to it (a slow start), and every
            // key event the hook saw before then is handled first (`biased`).
            let window = tap_window(&app);
            let msg = match window {
                Some((seq, closes)) => tokio::select! {
                    biased;
                    msg = rx.recv() => msg,
                    _ = tokio::time::sleep_until(closes.into()) => {
                        single_tap(&app, seq);
                        continue;
                    }
                },
                None => rx.recv().await,
            };
            let Some((ev, at)) = msg else { break };
            // Seen by the hook after the window closed (it waited behind a slow start): the quick
            // tap was a single one, so it goes before this event counts.
            if let Some((seq, closes)) = window {
                if window_closed(closes, at) {
                    single_tap(&app, seq);
                }
            }
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
                log::warn!("take failed: {}", for_log(&e));
                fail(&app, &short_error(&e));
            }
        }
    });
}

/// The take waiting to see whether its quick release becomes a double tap, and when the hook's
/// window for the second tap closes.
fn tap_window(app: &App) -> Option<(u64, Instant)> {
    match &app.session.lock().phase {
        Phase::Recording(rec) => Some((rec.pending_tap?, double_tap_closes(rec.tap_up_at?))),
        _ => None,
    }
}

/// When the window for a second tap closes: counted from the hook's time of the release, never
/// from when the controller got round to it.
fn double_tap_closes(released: Instant) -> Instant {
    released + Duration::from_millis(DOUBLE_TAP_WINDOW_MS)
}

/// The hook saw an event (`at`) after the double-tap window closed.
fn window_closed(closes: Instant, at: Instant) -> bool {
    at > closes
}

/// A single quick tap: nothing was said, so it is dropped quietly. Checked under the same lock as
/// the drop, so a second tap that has just been handled is never lost.
fn single_tap(app: &Arc<App>, seq: u64) {
    cancel_if(app, false, "a single quick tap", |r| r.pending_tap == Some(seq));
}

/// What a press of the talk keys does to a take being recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Press {
    /// The second tap of a double tap: the take goes hands-free.
    HandsFree,
    /// Let go and pressed again too fast for a finger (KEY_BOUNCE_MS): the keys bounced while the
    /// fingers settled on them or came off them. The take carries on as it was.
    Bounce,
    /// A hands-free take or a ramble: this press finishes it.
    Finish,
    /// Held to talk already: nothing to do.
    Held,
}

/// The decision for a press (`at`, the hook's time) during a take of `kind`. `pending_tap`: a quick
/// release is waiting for a second tap. `released`: when the hook last saw the keys let go in this
/// take, if it has. A bounce on the second tap of a double tap, or while letting go after starting
/// a ramble, would otherwise finish the take a moment after it began ("Didn't catch that").
fn press_while_recording(kind: Kind, pending_tap: bool, released: Option<Instant>, at: Instant) -> Press {
    let bounced = |up: Option<Instant>| is_key_bounce(up.map_or(0, |up| at.saturating_duration_since(up).as_millis()));
    if pending_tap {
        // The release is always kept with the pending tap; without one, no gap at all.
        return if bounced(released) { Press::Bounce } else { Press::HandsFree };
    }
    match kind {
        Kind::Talk => Press::Held,
        _ if released.is_some() && bounced(released) => Press::Bounce,
        _ => Press::Finish,
    }
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
            // How long the keys were up between the last release and this press, and how long the
            // first press lasted, for the log (timings only).
            let (mut gap_ms, mut held_ms, mut kind) = (0u128, 0u128, Kind::Talk);
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
                    Phase::Recording(rec) => {
                        let released = rec.tap_up_at.take();
                        if let Some(up) = released {
                            gap_ms = at.saturating_duration_since(up).as_millis();
                            held_ms = up.saturating_duration_since(rec.pressed).as_millis();
                        }
                        kind = rec.kind;
                        let pending = rec.pending_tap.take().is_some();
                        match press_while_recording(rec.kind, pending, released, at) {
                            Press::HandsFree => {
                                rec.kind = Kind::HandsFree;
                                2
                            }
                            // Still holding to talk: the quick release was the keys settling.
                            Press::Bounce if pending => 5,
                            // Hands-free or a ramble carries on; the bounce's own release is kept
                            // in turn, so the press after it is timed from that.
                            Press::Bounce => 6,
                            Press::Finish => 3,
                            Press::Held => 0,
                        }
                    }
                    Phase::Processing => {
                        s.busy_press = Some(at);
                        4
                    }
                    _ => 0,
                }
            };
            match action {
                1 => start(app, &settings, at).await?,
                6 => log::info!("ignored a key bounce in a {} take (keys up for {gap_ms} ms); it carries on", kind.as_str()),
                5 => {
                    log::info!("ignored a key bounce {held_ms} ms into the take (up for {gap_ms} ms); still holding to talk");
                    // The quick release held the greeting back; give it now.
                    let reveal = {
                        let mut s = app.session.lock();
                        match &mut s.phase {
                            Phase::Recording(rec) if !rec.announced && rec.pending_tap.is_none() => Some(announce_locked(app, rec, &settings)),
                            _ => None,
                        }
                    };
                    if let Some(hwnd) = reveal {
                        app.reveal_pill_window(hwnd);
                    }
                }
                2 => {
                    log::info!("double tap: hands-free (first press {held_ms} ms, keys up for {gap_ms} ms)");
                    // A tap of the shortcut finishes it; Enter too, if Settings allows.
                    if let Some(h) = app.hotkeys.lock().as_ref() {
                        h.set_enter_finishes(settings.hotkeys.enter_finishes);
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
                            rec.tap_up_at = Some(at);
                            Next::MaybeDoubleTap(seq)
                        } else if !rec.announced {
                            Next::Drop
                        } else {
                            Next::Stop
                        }
                    }
                    // Hands-free or a ramble: letting go changes nothing, but when it happened is
                    // kept, so a bounce straight after it is not taken for the press that finishes.
                    Phase::Recording(rec) if rec.kind != Kind::Talk => {
                        rec.tap_up_at = Some(at);
                        Next::Nothing
                    }
                    _ => Next::Nothing,
                };
                if let Next::MaybeDoubleTap(seq) = next {
                    s.tap_seq = seq;
                }
                next
            };
            match next {
                // The loop in `spawn` drops it as a single tap once the hook's window closes with
                // no second tap (`tap_window`).
                Next::MaybeDoubleTap(_) => {}
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
                    h.set_enter_finishes(settings.hotkeys.enter_finishes);
                }
                spawn_ramble_preview(app.clone());
            }
        }
        HotkeyEvent::Finish => {
            // Enter during a hands-free take or a ramble (when Settings lets Enter finish): finish it
            // as pressing the shortcut would.
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
        HotkeyEvent::ExtraKey(n) => {
            // Mode keys first, then action keys (lib.rs extra_key_chords).
            let n = n as usize;
            if let Some(key) = settings.hotkeys.mode_keys.get(n) {
                switch_mode(app, &key.mode);
            } else if let Some(key) = n.checked_sub(settings.hotkeys.mode_keys.len()).and_then(|i| settings.hotkeys.action_keys.get(i)) {
                action_key(app, key.clone(), &settings).await?;
            }
        }
    }
    Ok(())
}

/// An action key (Settings > Shortcuts): what a voice command does, without saying it.
async fn action_key(app: &Arc<App>, key: ActionKey, settings: &Settings) -> Result<()> {
    let recording = matches!(&app.session.lock().phase, Phase::Recording(_));
    match key.action.as_str() {
        "ask" => {
            // Pressed again while its take listens: the request is finished.
            let asking = matches!(&app.session.lock().phase, Phase::Recording(r) if r.ask);
            if asking {
                return stop(app).await;
            }
            start_ask(app, settings).await
        }
        // Not over a take being recorded, whose bubble they would take over.
        "watch" | "notes" if recording => {
            log::info!("action key {} ignored: a take is recording", key.action);
            Ok(())
        }
        "watch" | "notes" => {
            let action = match key.action.as_str() {
                "watch" if crate::watch::is_watching() => Action::WatchOff,
                "watch" => Action::WatchOn,
                _ if app.meeting.lock().is_some() => Action::NotesOff,
                _ => Action::NotesOn,
            };
            log::info!("action key: {}", action.id());
            // Starting call notes opens the call audio, which can take a moment: not here, where
            // it would hold up the next press of the talk keys.
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = run_spoken(&app, action, "", Vec::new(), None).await {
                    log::warn!("action key failed: {}", for_log(&e));
                    show_error(&app, &short_error(&e));
                }
            });
            Ok(())
        }
        "rewrite" | "scratch_that" | "critique" => {
            if !claim_for_key(app) {
                return Ok(());
            }
            app.show_pill_window(context::foreground_hwnd());
            if key.action == "rewrite" {
                show_working(app, AI_REWRITING);
            }
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let result = run_key_edit(&app, &key).await;
                {
                    let mut s = app.session.lock();
                    s.phase = Phase::Idle;
                    s.working = None;
                }
                if let Err(e) = result {
                    log::warn!("action key failed: {}", for_log(&e));
                    fail(&app, &short_error(&e));
                }
            });
            Ok(())
        }
        other => {
            log::info!("an action key for {other:?}, which this Navi doesn't know, was ignored");
            Ok(())
        }
    }
}

/// Claims the session for an action key that changes text, as a take does, so the talk keys say
/// "Still finishing the last one" meanwhile rather than start a take that types into the middle
/// of it. False, and nothing happens, while a take records or the last one is still being
/// written up (which says so).
fn claim_for_key(app: &Arc<App>) -> bool {
    let mut s = app.session.lock();
    match s.phase {
        Phase::Idle if !s.starting => {
            s.phase = Phase::Processing;
            s.processing_mode = String::new();
            s.working = None;
            true
        }
        Phase::Processing => {
            drop(s);
            busy_key(app);
            false
        }
        _ => {
            log::info!("action key ignored: a take is recording");
            false
        }
    }
}

/// An action key pressed while the last take or key is still being worked on: says so, then the
/// bubble goes back to what it was showing.
fn busy_key(app: &Arc<App>) {
    log::info!("action key pressed while the last one is still processing");
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        app.sounds.play(Cue::Error, &app.settings().sounds);
        let gen = app.pill(PillState::Error { message: "Still finishing the last one. Try again in a moment.".into() });
        tokio::time::sleep(Duration::from_millis(BUSY_NOTE_MS)).await;
        back_to_processing(&app, gen);
    });
}

/// The rewrite, scratch that and "what's wrong" keys: on the selection, or what Navi just typed.
async fn run_key_edit(app: &Arc<App>, key: &ActionKey) -> Result<()> {
    let settings = app.settings();
    // Said before anything is read or copied, so a key with no AI fails at once.
    match key.action.as_str() {
        "rewrite" if make_llm(&settings).is_none() => return Err(anyhow!("Rewrites need an AI. Set one up in Settings > AI.")),
        "critique" if make_llm_long(&settings).is_none() => return Err(anyhow!("That needs an AI. Set one up in Settings > AI.")),
        _ => {}
    }
    let mut rec = bare_recording(app).await;
    rec.from_wake = false;
    if key.action != "scratch_that" && rec.ctx.selection.is_none() {
        rec.ctx.selection = copied_selection(&rec.ctx, &settings).await;
    }
    let cmd = match key.action.as_str() {
        "scratch_that" => VoiceCommand::Undo,
        "critique" => VoiceCommand::Critique { instruction: "What's wrong with this?".into() },
        _ => {
            let instruction = key.rewrite_instruction().to_string();
            if rec.ctx.selection.is_some() {
                VoiceCommand::Transform { instruction }
            } else {
                VoiceCommand::Rewrite { instruction }
            }
        }
    };
    if !command_has_target(app, &rec, &cmd) {
        return Err(anyhow!(if key.action == "scratch_that" { "There's nothing recent to scratch here." } else { "Select some text first, then press the key." }));
    }
    log::info!("action key: {}", command_name(&cmd));
    run_command(app, &rec, cmd, None, Instant::now()).await
}

/// The selection, copied to the clipboard and read (the clipboard is then put back), for a key
/// pressed where Navi couldn't read the field (`ctx.field_text` is None; read, it would have had
/// the selection). Never in an app under "Never read these apps", a terminal (Ctrl + C stops the
/// program) or a code editor (there Ctrl + C with nothing selected copies the whole line, which
/// would be taken for a selection).
async fn copied_selection(ctx: &AppContext, settings: &Settings) -> Option<String> {
    if !may_copy_selection(ctx, settings) {
        return None;
    }
    let copied = tokio::task::spawn_blocking(insert::copy_selection).await.ok()?;
    copied
        .unwrap_or_else(|e| {
            log::warn!("couldn't copy the selection: {e:#}");
            None
        })
        .filter(|t| !t.trim().is_empty())
}

/// See `copied_selection`.
fn may_copy_selection(ctx: &AppContext, settings: &Settings) -> bool {
    ctx.field_text.is_none()
        && ctx.hwnd != 0
        && !ctx.process_name.trim().is_empty()
        && !matches!(ctx.category.as_str(), "terminal" | "editor")
        && !context::is_excluded(&ctx.process_name, &settings.context.excluded_apps)
}

/// Says in the bubble that the AI is working ("AI is rewriting"), with a clock, so a wait of
/// several seconds reads as work going on rather than nothing happening.
fn show_working(app: &Arc<App>, message: &str) {
    app.session.lock().working = Some(message.to_string());
    app.pill(PillState::Working { message: message.to_string() });
}

/// Puts the bubble back to what it showed while the last take or key was being worked on, after a
/// note (`gen`), unless that has finished or something newer is in the bubble.
fn back_to_processing(app: &Arc<App>, gen: u64) {
    let (still, working, mode_name) = {
        let s = app.session.lock();
        (matches!(s.phase, Phase::Processing), s.working.clone(), s.processing_mode.clone())
    };
    if still && app.pill_gen.load(Ordering::SeqCst) == gen {
        app.pill(match working {
            Some(message) => PillState::Working { message },
            None => PillState::Processing { mode_name },
        });
    }
}

/// An error in the bubble that leaves any take alone (`fail` ends the processing phase).
fn show_error(app: &Arc<App>, message: &str) {
    app.sounds.play(Cue::Error, &app.settings().sounds);
    app.show_pill_window(context::foreground_hwnd());
    let gen = app.pill(PillState::Error { message: message.to_string() });
    app.hide_pill_later(gen, read_ms(message, 3000));
}

/// The bubble while take `rec` records. An Ask AI take says so where the mode would be.
fn listening(rec: &Recording) -> PillState {
    if rec.ask {
        return PillState::Listening { kind: "ask".into(), mode_name: "Ask AI".into(), app_label: rec.ctx.app_label.clone() };
    }
    let mode_name = if rec.kind == Kind::Ramble { "Notes".to_string() } else { rec.mode.name.clone() };
    PillState::Listening { kind: rec.kind.as_str().into(), mode_name, app_label: rec.ctx.app_label.clone() }
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
                    app.pill(listening(rec));
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
    let excluded = context::is_excluded(&ctx.process_name, &settings.context.excluded_apps);
    let ocr = if settings.context.ocr && !excluded && hwnd != 0 {
        Some(tokio::task::spawn_blocking(move || context::ocr_window(hwnd)))
    } else {
        None
    };
    log::info!(
        "take started in {} ({} mode), microphone: {}",
        if ctx.app_label.is_empty() { "an unknown app" } else { ctx.app_label.as_str() },
        mode.name,
        settings.mic.device.as_deref().unwrap_or(if cfg!(target_os = "macos") { "system default" } else { "Windows default" })
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
            tap_up_at: None,
            ramble_cue: None,
            ramble_started: None,
            ocr,
            from_wake: false,
            preroll_ms: 0,
            ask: false,
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
/// is lost. It finishes by itself after 6 s of quiet, with the finish phrase, or with the shortcut
/// (or Enter, if Settings > Shortcuts allows it). Some(seq) when a take started; None when another take has the microphone.
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
    let excluded = context::is_excluded(&ctx.process_name, &settings.context.excluded_apps);
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
            tap_up_at: None,
            ramble_cue: None,
            ramble_started: None,
            ocr,
            from_wake: true,
            preroll_ms,
            ask: false,
        });
        if let Some(h) = app.hotkeys.lock().as_ref() {
            h.set_recording(true);
            h.set_enter_finishes(settings.hotkeys.enter_finishes);
        }
        seq
    };
    app.reveal_pill_window(hwnd);
    crate::media::hold(app, seq);
    spawn_quiet_finish(app.clone(), seq, preroll.len(), Quiet { before_speech: WAKE_QUIET, after_speech: WAKE_QUIET, what: "wake word take" });
    spawn_mic_watch(app.clone(), seq);
    Ok(Some(seq))
}

/// The Ask AI key (Settings > Shortcuts): listens for what you want done to the selection, or a
/// question when nothing is selected. Like a "Navi" take, the sound and the bubble come at once
/// and it finishes by itself when you stop talking, or with a tap of the key or the talk keys;
/// Esc cancels it. The caller has checked no take is recording.
async fn start_ask(app: &Arc<App>, settings: &Settings) -> Result<()> {
    {
        let mut s = app.session.lock();
        match s.phase {
            Phase::Idle if !s.starting => s.starting = true,
            Phase::Processing => {
                drop(s);
                busy_key(app);
                return Ok(());
            }
            _ => return Ok(()),
        }
    }
    if make_llm_long(settings).is_none() {
        app.session.lock().starting = false;
        return Err(anyhow!("Ask AI needs an AI. Set one up in Settings > AI."));
    }
    if app.mic_test.swap(false, Ordering::SeqCst) {
        app.audio.cancel();
        app.emit("mic-test", false);
    }
    app.sounds.play(Cue::Talk, &settings.sounds);
    app.pill(PillState::Listening { kind: "ask".into(), mode_name: "Ask AI".into(), app_label: String::new() });
    app.reveal_pill_window(context::foreground_hwnd());
    if let Err(e) = app.audio.start(settings.mic.device.as_deref()) {
        app.session.lock().starting = false;
        return Err(e);
    }
    let started = Instant::now();
    let ctx_settings = settings.context.clone();
    let mut ctx = tokio::task::spawn_blocking(move || context::capture(&ctx_settings)).await.unwrap_or_default();
    if ctx.selection.is_none() {
        ctx.selection = copied_selection(&ctx, settings).await;
    }
    let all = all_modes(app);
    let active = settings.modes.active.trim();
    let mode = modes::pick_mode(&all, &ctx, settings, (!active.is_empty()).then_some(active));
    let hwnd = ctx.hwnd;
    log::info!(
        "Ask AI take started in {} ({})",
        if ctx.app_label.is_empty() { "an unknown app" } else { ctx.app_label.as_str() },
        if ctx.selection.is_some() { "with a selection" } else { "nothing selected" }
    );
    // The start sound played before the microphone opened; trim what of it the microphone caught.
    let cue_ms = if settings.sounds.enabled { app.sounds.duration_ms(Cue::Talk, &settings.sounds).min(CUE_TRIM_MAX_MS) } else { 0 };
    let seq = {
        let mut s = app.session.lock();
        s.starting = false;
        if !matches!(s.phase, Phase::Idle) {
            return Ok(());
        }
        s.take_seq += 1;
        let seq = s.take_seq;
        let rec = Recording {
            kind: Kind::HandsFree,
            seq,
            pressed: started,
            started,
            ctx,
            mode,
            announced: true,
            cue_at_ms: 0,
            cue_ms,
            cue_word: None,
            pending_tap: None,
            tap_up_at: None,
            ramble_cue: None,
            ramble_started: None,
            ocr: None,
            from_wake: false,
            preroll_ms: 0,
            ask: true,
        };
        app.pill(listening(&rec));
        s.phase = Phase::Recording(rec);
        if let Some(h) = app.hotkeys.lock().as_ref() {
            h.set_recording(true);
            h.set_enter_finishes(settings.hotkeys.enter_finishes);
        }
        seq
    };
    app.reveal_pill_window(hwnd);
    crate::media::hold(app, seq);
    // Listened to from after the start sound, which the microphone may pick up as speech.
    let from = (app.sounds.duration_ms(Cue::Talk, &settings.sounds) as usize).max(300) * (audio::SAMPLE_RATE as usize) / 1000;
    spawn_quiet_finish(app.clone(), seq, from, Quiet { before_speech: ASK_FIRST_WORDS, after_speech: ASK_QUIET, what: "Ask AI take" });
    spawn_mic_watch(app.clone(), seq);
    spawn_take_listener(app.clone(), seq, None);
    Ok(())
}

/// How long a take that finishes by itself waits: `before_speech` while nothing has been said,
/// `after_speech` once something has. `what` names the take for the log.
struct Quiet {
    before_speech: Duration,
    after_speech: Duration,
    what: &'static str,
}

/// Finishes hands-free take `seq` once you have been quiet long enough (`quiet`), listening from
/// 16 kHz sample `from` on: a wake word take, or an Ask AI take.
fn spawn_quiet_finish(app: Arc<App>, seq: u64, from: usize, quiet: Quiet) {
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
            let limit = if last_voice.is_some() { quiet.after_speech } else { quiet.before_speech };
            let too_long = begun.elapsed() >= WAKE_MAX;
            if last_voice.unwrap_or(begun).elapsed() >= limit || too_long {
                log::info!("{} finished: {}", quiet.what, if too_long { "two minutes is the most" } else { "you stopped talking" });
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
    app.pill(listening(rec));
    // Pause or mute other apps' sound until this take stops recording (Settings > Sound).
    crate::media::hold(app, rec.seq);
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
                let mode_name = if r.kind == Kind::Ramble {
                    "Notes".to_string()
                } else if r.ask {
                    "Ask AI".to_string()
                } else {
                    r.mode.name.clone()
                };
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
        {
            let mut s = app2.session.lock();
            s.phase = Phase::Idle;
            s.working = None;
        }
        if let Err(e) = result {
            log::warn!("processing failed: {}", for_log(&e));
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
                            Some(engine) => match engine.transcribe_quick(&clip, &opts).await {
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
    app.pill(listening(rec));
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
        tap_up_at: None,
        ramble_cue: None,
        ramble_started: None,
        ocr: None,
        from_wake: true,
        preroll_ms: 0,
        ask: false,
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
            run_command(app, &rec, VoiceCommand::Undo, Some((heard, &audio)), Instant::now()).await?;
        }
        Action::Colour(id) => {
            // Saved, and every window and the tray icon change at once. Nothing is typed.
            let colour = crate::colours::switch(app, id);
            spoken_note(app, format!("Colour: {}", colour.name));
            app.sounds.play(Cue::Done, &settings.sounds);
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
        {
            let mut s = app.session.lock();
            if s.busy_press != Some(pressed) || !matches!(s.phase, Phase::Processing) {
                return;
            }
            s.busy_press = None;
        }
        log::info!("talk keys held while the last take is still processing");
        app.sounds.play(Cue::Error, &settings.sounds);
        let gen = app.pill(PillState::Error { message: "Still finishing the last one. Try again in a moment.".into() });
        tokio::time::sleep(Duration::from_millis(BUSY_NOTE_MS)).await;
        back_to_processing(&app, gen);
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

/// The longest a take waits for a speech engine that is starting or restarting (a new model, the
/// graphics card switched, the silence model just fetched). Warming up takes about 10 seconds on
/// the processor and 2 to 3 on the graphics card.
const ENGINE_WAIT: Duration = Duration::from_secs(30);
/// How often the bubble says again that the take is waiting, so it never fades meanwhile.
const ENGINE_WAIT_NOTE_EVERY: Duration = Duration::from_secs(3);
const ENGINE_WAIT_NOTE: &str = "Waiting for the speech engine to start\u{2026}";

/// Whether a take should wait for the engine rather than fail: a start is under way, or the
/// status doesn't say it can't start (no model, no engine, an error).
fn engine_on_its_way(state: &str, start_under_way: bool) -> bool {
    start_under_way || !matches!(state, "missing-model" | "missing-engine" | "error")
}

/// The speech engine for a take. A restart takes the old one away at once and the new one is only
/// there once it has warmed up, so a take in between waits for it (up to ENGINE_WAIT, the bubble
/// saying so), as call notes hold their audio, rather than failing with "still starting".
async fn engine_for_take(app: &Arc<App>) -> Result<Arc<crate::engine::Engine>> {
    let since = Instant::now();
    let mut noted: Option<Instant> = None;
    loop {
        if let Some(engine) = app.engine.read().await.clone() {
            if noted.is_some() {
                log::info!("speech engine ready after the take waited {} ms", since.elapsed().as_millis());
                let mode_name = app.session.lock().processing_mode.clone();
                app.pill(PillState::Processing { mode_name });
            }
            return Ok(engine);
        }
        let status = app.engine_status.lock().clone();
        let under_way = app.engine_start.try_lock().is_err();
        if !engine_on_its_way(&status.state, under_way) || since.elapsed() >= ENGINE_WAIT {
            return Err(no_engine(&status).into());
        }
        if noted.is_none_or(|at| at.elapsed() >= ENGINE_WAIT_NOTE_EVERY) {
            if noted.is_none() {
                log::info!("the speech engine is starting ({}); the take waits for it", status.state);
            }
            app.pill(PillState::Notice { message: ENGINE_WAIT_NOTE.into() });
            noted = Some(Instant::now());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn transcribe(app: &Arc<App>, samples: &[f32], settings: &Settings, ctx: &AppContext, screen: Option<&str>) -> Result<String> {
    let engine = engine_for_take(app).await?;
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

/// The AI for jobs nobody sits waiting on: call notes, watch notes, call jobs and
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
    make_llm(&s).map(Llm::for_long_jobs)
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
    if rec.ask {
        return run_ask(app, &rec, &raw, &full_audio, released).await;
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
    // A command whose phrases were all removed in Settings is off here too.
    let vc = &settings.voice_commands;
    let command = match rest.as_deref().filter(|r| rec.from_wake && r.chars().any(char::is_alphabetic)) {
        // After any wake word, custom ones too ("Hey, make that shorter").
        Some(r) => voice::parse_after_wake(&format!("Navi, {r}"), has_selection, vc),
        None if rec.from_wake => voice::parse_after_wake(&raw, has_selection, vc),
        None => voice::parse(&raw, has_selection, vc),
    };
    if let Some(cmd) = command {
        if command_has_target(app, &rec, &cmd) {
            log::info!("voice command: {}", command_name(&cmd));
            let meta = serde_json::json!({ "command": cmd });
            // After the wake word a rewrite may really have been the next line of dictation, so
            // if it fails the words go on the clipboard rather than only into History.
            let words_matter = rec.from_wake && matches!(cmd, VoiceCommand::Rewrite { .. } | VoiceCommand::Replace { .. });
            match run_command(app, &rec, cmd, Some((&raw, &full_audio)), released).await {
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
                                return Err(reworded(&e, with_note(&short_error(&e), "Your words are on the clipboard.")));
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
    if pipeline::uses_ai(&rec.mode, llm.as_ref()) {
        show_working(app, AI_WRITING);
    }
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
    schedule_learning_check(app.clone(), &settings, take.id.clone());

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
    let why = if w.contains("no reply in time") || w.contains("no reply within") || w.contains("took longer than") {
        "The AI didn't answer in time".to_string()
    } else if w.contains("replied to your words") || w.contains("answered your question") {
        "The AI answered instead of rewriting".to_string()
    } else if w.contains("far more") || w.contains("wrote code") {
        "The AI added things you didn't say".to_string()
    } else if w.contains("returned nothing") {
        "The AI sent back nothing".to_string()
    } else {
        // The AI's own error, named ("Ollama isn't running", "Claude rejected the API key").
        warning.strip_prefix("AI step skipped: ").and_then(ai_error_reason).unwrap_or_else(|| "The AI step didn't work".to_string())
    };
    format!("{why}, so Navi typed a plain tidy instead.")
}

/// An AI error from llm.rs or cli_ai.rs in a few words for the bubble: its first sentence, less any
/// detail in brackets, when that is short and plain ("Claude rejected the API key", "No connection
/// to OpenAI", "Claude Code isn't signed in"). None for raw detail (an address, a tool's output).
fn ai_error_reason(error: &str) -> Option<String> {
    if error.starts_with("Ollama isn't installed or isn't running") {
        return Some("Ollama isn't running".into());
    }
    let first = error.split(". ").next()?.trim().trim_end_matches(['.', '?']);
    let first = match first.rfind(" (") {
        Some(i) if first.ends_with(')') => &first[..i],
        _ => first,
    };
    let plain = !first.is_empty() && first.chars().count() <= 70 && !first.contains(": ") && !first.contains("://");
    plain.then(|| first.to_string())
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

/// Carries out `cmd` on take `rec`. `heard`: what was said and its recording, for History; None
/// for an action key, where nothing was said and nothing goes in History.
async fn run_command(app: &Arc<App>, rec: &Recording, cmd: VoiceCommand, heard: Option<(&str, &[f32])>, released: Instant) -> Result<()> {
    let settings = app.settings();
    let last = app.last_insert.lock().clone();
    let fresh = last.as_ref().filter(|l| l.at.elapsed() < Duration::from_secs(600) && l.hwnd == rec.ctx.hwnd);
    let summary: String;
    // What the command left in the text, for History: nothing for those that only delete or look.
    let mut final_text = String::new();
    // A change to a selection becomes the last insert, so "scratch that" can put the selection back.
    let mut new_insert: Option<LastInsert> = None;
    match &cmd {
        // Text you selected yourself: "scratch that" deletes it, whoever typed it. (Selecting moves
        // the caret, so the check below that Navi's words sit just before it would refuse.)
        VoiceCommand::Undo if scratches_selection(rec) => {
            tokio::task::spawn_blocking(insert::delete_selection).await.map_err(|e| anyhow!("{e}"))??;
            // If the selection was what Navi last typed, that text is gone now.
            if fresh.is_some_and(|l| same_words(rec.ctx.selection.as_deref().unwrap_or_default(), &l.text)) {
                *app.last_insert.lock() = None;
            }
            summary = "Deleted".into();
        }
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
                let llm = make_llm(&settings).ok_or_else(|| said_error(format!("Couldn't find \"{from}\" in what you just said."), "the words to change weren't found and there is no AI to look"))?;
                let VoiceCommand::Rewrite { instruction } = voice::replace_as_rewrite(from, to) else { unreachable!() };
                let (sys, user) = voice::rewrite_messages(&instruction, &l.text, &settings);
                show_working(app, AI_REWRITING);
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
            show_working(app, AI_REWRITING);
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
            show_working(app, AI_REWRITING);
            let new_text = finish_text(&ai_edit(&llm, &sys, &user, 1600, 0.3).await?, &selection, &settings)?;
            new_insert = Some(paste_over_selection(app, rec, &new_text, selection, &settings).await?);
            final_text = new_text;
            summary = "Done".into();
        }
    }
    let latency = released.elapsed().as_millis() as u64;
    let take_id = match heard {
        Some((raw, audio)) => save_take(app, &settings, rec, raw, &final_text, audio, latency, "command", "command", serde_json::json!({ "command": cmd })).id,
        None => String::new(),
    };
    if let Some(mut l) = new_insert {
        l.take_id = take_id;
        *app.last_insert.lock() = Some(l);
    }
    app.sounds.play(Cue::Done, &settings.sounds);
    let gen = app.pill(PillState::Done { preview: summary, outcome: "command".into() });
    app.hide_pill_later(gen, 1300);
    Ok(())
}

/// Pastes `new_text` over the selection (`selection`, read when take `rec` began) and returns the
/// last insert it makes, so "scratch that" can put the selection back. If you have moved to
/// another window while the AI worked, nothing is pasted: the text goes on the clipboard instead.
async fn paste_over_selection(app: &Arc<App>, rec: &Recording, new_text: &str, selection: String, settings: &Settings) -> Result<LastInsert> {
    if context::foreground_hwnd() != rec.ctx.hwnd {
        *app.last_insert.lock() = None;
        insert::set_clipboard_text(new_text)?;
        return Err(anyhow!("You've moved to another window, so the new text is on your clipboard."));
    }
    let t = new_text.to_string();
    let method = settings.general.paste_method.clone();
    let restore = settings.general.restore_clipboard;
    let outcome = tokio::task::spawn_blocking(move || insert::insert_text(&t, &method, restore)).await.map_err(|e| anyhow!("{e}"))??;
    if let InsertOutcome::ClipboardOnly(reason) = outcome {
        // Nothing (or only part) went in, so the window no longer matches any last insert.
        *app.last_insert.lock() = None;
        return Err(anyhow!(reason));
    }
    Ok(LastInsert {
        hwnd: rec.ctx.hwnd,
        text: new_text.to_string(),
        take_id: String::new(),
        app_label: rec.ctx.app_label.clone(),
        at: Instant::now(),
        // The AI wrote this, nobody said it: no misheard words to learn from.
        checked: true,
        replaced: Some(selection),
    })
}

/// What was said into an Ask AI take (`request`): the AI either changes the selection, pasted over
/// it (so "scratch that" puts it back), or answers, in a card in the middle of the screen. With
/// nothing selected it always answers. Both go in History with the recording.
async fn run_ask(app: &Arc<App>, rec: &Recording, request: &str, audio: &[f32], released: Instant) -> Result<()> {
    let settings = app.settings();
    let request = request.trim();
    let llm = make_llm_long(&settings).ok_or_else(|| anyhow!("Ask AI needs an AI. Set one up in Settings > AI."))?;
    let selection = rec.ctx.selection.clone().filter(|s| !s.trim().is_empty());
    show_working(app, AI_ASKED);
    let (system, user) = voice::ask_messages(request, selection.as_deref(), &settings);
    let reply = tokio::time::timeout(ASK_DEADLINE, llm.complete(&system, &user, 3000, 0.3)).await.map_err(|_| anyhow!("{} took too long to answer.", llm.display_name()))??;
    let latency = released.elapsed().as_millis() as u64;
    match (voice::read_ask_reply(&reply), selection) {
        (voice::AskReply::Edit(text), Some(selection)) => {
            let new_text = finish_text(&text, &selection, &settings)?;
            let mut last = paste_over_selection(app, rec, &new_text, selection, &settings).await?;
            log::info!("Ask AI: changed the selection in {latency} ms");
            last.take_id = save_take(app, &settings, rec, request, &new_text, audio, latency, "command", "command", serde_json::json!({ "ask": "edit" })).id;
            *app.last_insert.lock() = Some(last);
            app.sounds.play(Cue::Done, &settings.sounds);
            let gen = app.pill(PillState::Done { preview: "Done".into(), outcome: "command".into() });
            app.hide_pill_later(gen, 1300);
        }
        // An answer, or an edit with nothing to edit (the AI was told it can only answer then).
        (voice::AskReply::Answer(body) | voice::AskReply::Edit(body), _) => {
            let mut body = body.trim().to_string();
            if body.is_empty() {
                return Err(anyhow!("{} sent back an empty answer.", llm.display_name()));
            }
            if settings.writing.no_em_dashes {
                body = cleanup::no_em_dashes(&body);
            }
            if settings.writing.british {
                body = crate::text::britishise::britishise(&body);
            }
            log::info!("Ask AI: answered in {latency} ms");
            save_take(app, &settings, rec, request, &body, audio, latency, "answer", "command", serde_json::json!({ "ask": "answer" }));
            app.sounds.play(Cue::Done, &settings.sounds);
            app.pill(PillState::Hidden);
            crate::meeting::show_prompt(app, crate::meeting::PromptState::Reply { question: capitalise_first(request), body, expanded: false });
        }
    }
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
/// before the caret (in a field that can't be read: no key pressed since Navi typed). Checked again
/// here, just before the keys, because the AI can take a while: you may have moved on, typed more,
/// or clicked into another field of the same window. Reads the field only as Settings allow
/// (`reading`). Runs on a blocking thread.
fn select_back_in(last: &LastInsert, text: &str, method: &str, restore: bool, reading: &ContextSettings) -> Result<InsertOutcome> {
    if context::foreground_hwnd() != last.hwnd {
        return Err(anyhow!("You've moved to another window, so Navi left the text alone."));
    }
    let now = context::capture(reading);
    // The second look: the time limit was checked before the AI was asked, so only keys count now.
    select_back_check(now.field_text.as_deref(), now.category == "terminal", &last.text, None, hotkey::keys_pressed_since(last.at)).map_err(|why| anyhow!(why))?;
    insert::select_back_and_replace_with(last.text.encode_utf16().count(), text, method, restore)
}

/// "Scratch that" said over a selection deletes the selection (see `run_command`).
fn scratches_selection(rec: &Recording) -> bool {
    deletes_selection(&rec.ctx.category, rec.ctx.selection.as_deref())
}

/// There is a selection that isn't only spaces (read when the take began), and this isn't a terminal,
/// where Delete doesn't remove selected text (there "scratch that" deletes what Navi typed, as before).
fn deletes_selection(category: &str, selection: Option<&str>) -> bool {
    category != "terminal" && selection.is_some_and(|s| !s.trim().is_empty())
}

/// The same words, ignoring spacing, punctuation and case (as `ends_with_insert` compares them).
fn same_words(a: &str, b: &str) -> bool {
    let squash = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect::<String>();
    let (a, b) = (squash(a), squash(b));
    !a.is_empty() && a == b
}

const TEXT_CHANGED: &str = "The text has changed since Navi typed it, so Navi left it alone.";
const KEYS_SINCE: &str = "You've pressed keys since Navi typed, and it can't read this field to check.";

fn too_old_to_edit_blind() -> String {
    format!("Navi can't read this field, so it only edits text from the last {} seconds.", BLIND_EDIT_WINDOW.as_secs())
}

/// Refuses a spoken edit when what Navi typed may no longer be just before the caret (you typed
/// after it, changed it or clicked elsewhere): selecting back from there would delete your own words.
fn insert_at_caret(rec: &Recording, last: &LastInsert) -> Result<()> {
    select_back_check(rec.ctx.field_text.as_deref(), rec.ctx.category == "terminal", &last.text, Some(last.at.elapsed()), hotkey::keys_pressed_since(last.at)).map_err(|why| anyhow!(why))
}

/// Whether a spoken edit may select back over `inserted` (what Navi typed), and if not, why.
/// `field`: the text before the caret, None when nothing could be read (reading is off, the app
/// doesn't say, or the read ran out of time). A terminal counts as unread: its screen rarely reads
/// back as typed. With the field read, Navi's text must still end just before the caret. Unread,
/// there is nothing to compare, so only an insert younger than BLIND_EDIT_WINDOW (`age`; None
/// skips that, for the second look after the AI) with no key pressed since (`keys_since`, from the
/// hook) is trusted.
fn select_back_check(field: Option<&str>, terminal: bool, inserted: &str, age: Option<Duration>, keys_since: bool) -> std::result::Result<(), String> {
    match field.filter(|_| !terminal) {
        Some(_) if ends_with_insert(field, inserted) => Ok(()),
        Some(_) => Err(TEXT_CHANGED.into()),
        None if keys_since => Err(KEYS_SINCE.into()),
        None if age.is_some_and(|a| a > BLIND_EDIT_WINDOW) => Err(too_old_to_edit_blind()),
        None => Ok(()),
    }
}

/// True when `field` (the text before the caret) still ends with what Navi typed. Only letters
/// and digits are compared, as apps change spacing, quotes and line breaks as text goes in. With
/// nothing read (None) there is nothing to go on, so it does not count; `select_back_check`
/// decides those. An empty field read refuses, as nothing is before the caret (context.rs reports
/// an empty field as nothing read for now, which goes the same way).
fn ends_with_insert(field: Option<&str>, inserted: &str) -> bool {
    let Some(field) = field else { return false };
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
    // Why the AI's notes didn't come, said on the note and in the bubble, never only in the log.
    let mut ai_failed: Option<anyhow::Error> = None;
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
                    ai_failed = Some(e);
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
    let notice = ai_failed.as_ref().map(|e| ramble::rough_notes_notice(&e.to_string())).unwrap_or_default();
    let markdown = ramble::to_markdown(&notes, &tidy, &when, &notice);
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
    crate::meeting::save_ramble(app, &notes.title, ramble::to_markdown(&notes, "", "", &notice), &tidy, spoken_ms, notes_path);

    app.sounds.play(Cue::Done, &settings.sounds);
    // Said in words: a tick alone left it unclear where the ramble went.
    let summary = ramble_summary(notes.tasks.len(), &sent, ai_failed.as_ref().map(short_error).as_deref());
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

/// The bubble once a ramble is saved: where it went, its tasks and where they went (`sent`). With
/// `why` (the AI's error, shortened) it says these are rough notes and why: the note itself says
/// more (`ramble::rough_notes_notice`).
fn ramble_summary(tasks: usize, sent: &str, why: Option<&str>) -> String {
    let tasks = if tasks > 0 { format!(" · {tasks} task{}", if tasks == 1 { "" } else { "s" }) } else { String::new() };
    match why {
        None => format!("Saved to Notes{tasks}{sent}"),
        Some(why) => format!("Saved rough notes: the AI couldn't write them ({}){tasks}{sent}", first_sentence(why).trim().trim_end_matches('.')),
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
            // Only the last 30 seconds are copied: the whole of a long ramble is megabytes a time.
            let samples = app.audio.tail_16k(audio::SAMPLE_RATE as usize * 30);
            let Some(engine) = app.engine.read().await.clone() else { continue };
            let opts = TranscribeOptions { language: settings.engine.language.clone(), prompt: String::new() };
            if let Ok(tr) = engine.transcribe(&samples, &opts).await {
                let still = matches!(&app.session.lock().phase, Phase::Recording(r) if r.kind == Kind::Ramble);
                if still && !heard_nothing(&tr.text, &samples) {
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
    log::warn!("take failed: {}", for_log(&e));
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

fn preview(text: &str) -> String {
    let t: String = text.chars().take(60).collect();
    if text.chars().count() > 60 { format!("{t}…") } else { t }
}

// ------------------------------------------------------------------------------------------
// Learning from corrections
// ------------------------------------------------------------------------------------------

/// Looks for corrections to take `take_id` once it has had time to be fixed. Only that take: a
/// newer one pasted meanwhile waits for its own timer, so it is never looked at too early.
fn schedule_learning_check(app: Arc<App>, settings: &Settings, take_id: String) {
    if !settings.learning.enabled || !settings.context.read_field {
        return;
    }
    let delay = settings.learning.check_after_secs.max(5) as u64;
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(delay)).await;
        learning_check(&app, Some(&take_id)).await;
    });
}

/// Looks for corrections to whatever Navi typed last (at the next take, or from the interface).
pub async fn learning_check_now(app: &Arc<App>) {
    learning_check(app, None).await;
}

/// The least time a paste is given to be corrected before it is looked at.
const LEARN_MIN_WAIT: Duration = Duration::from_secs(3);

/// Whether `last` is due a look: not looked at yet, given LEARN_MIN_WAIT, and, for a take's own
/// timer (`only`), that take rather than a newer one.
fn learning_due(last: &LastInsert, only: Option<&str>) -> bool {
    !last.checked && last.at.elapsed() >= LEARN_MIN_WAIT && only.is_none_or(|id| id == last.take_id)
}

async fn learning_check(app: &Arc<App>, only: Option<&str>) {
    let settings = app.settings();
    if !settings.learning.enabled || !settings.context.read_field {
        return;
    }
    let last = {
        let mut guard = app.last_insert.lock();
        match guard.as_mut() {
            Some(l) if learning_due(l, only) => {
                l.checked = true;
                l.clone()
            }
            _ => return,
        }
    };
    let (hwnd, reading) = (last.hwnd, settings.context.clone());
    // Never an app under "Never read these apps" (read_focused_text checks the window's app).
    let Ok(Some(now)) = tokio::task::spawn_blocking(move || context::read_focused_text(hwnd, &reading)).await else { return };
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

/// An error whose message holds words the user said ("Couldn't find "Tuesday" in what you just
/// said."). The bubble shows the message; the log only ever gets `log`, which holds none of them.
#[derive(Debug)]
struct SaidError {
    message: String,
    log: &'static str,
}

impl std::fmt::Display for SaidError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SaidError {}

fn said_error(message: String, log: &'static str) -> anyhow::Error {
    anyhow::Error::new(SaidError { message, log })
}

fn said_in(e: &anyhow::Error) -> Option<&SaidError> {
    e.chain().find_map(|c| c.downcast_ref::<SaidError>())
}

/// `e` as the log may say it: never the words in a SaidError.
fn for_log(e: &anyhow::Error) -> String {
    match said_in(e) {
        Some(said) => said.log.to_string(),
        None => format!("{e:#}"),
    }
}

/// A new message for the bubble in place of `e`'s, kept out of the log just as `e`'s was.
fn reworded(e: &anyhow::Error, message: String) -> anyhow::Error {
    match said_in(e) {
        Some(said) => said_error(message, said.log),
        None => anyhow!(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An action key copies the selection only where Navi couldn't read the field, and where
    /// copying is safe: never a terminal (Ctrl + C stops the program) or a code editor (with
    /// nothing selected it copies the whole line, which would be taken for a selection).
    #[test]
    fn a_selection_is_copied_only_where_it_could_not_be_read_and_copying_is_safe() {
        let s = Settings::default();
        let ctx = |category: &str, process: &str, field: Option<&str>| AppContext { hwnd: 1, category: category.into(), process_name: process.into(), field_text: field.map(Into::into), ..Default::default() };
        assert!(may_copy_selection(&ctx("chat", "WhatsApp.exe", None), &s));
        assert!(!may_copy_selection(&ctx("chat", "WhatsApp.exe", Some("Hi Sam")), &s), "read, it would have had the selection");
        assert!(!may_copy_selection(&ctx("terminal", "WindowsTerminal.exe", None), &s));
        assert!(!may_copy_selection(&ctx("editor", "Code.exe", None), &s));
        assert!(!may_copy_selection(&ctx("other", "1Password.exe", None), &s), "never read");
        assert!(!may_copy_selection(&ctx("chat", "", None), &s), "an unknown app");
        assert!(!may_copy_selection(&AppContext { hwnd: 0, ..ctx("chat", "WhatsApp.exe", None) }, &s), "no window");
    }

    /// Tom selected a sentence and said "Navi, scratch that": Navi said the text had changed, as
    /// selecting had moved the caret. Over a selection, "scratch that" now deletes the selection.
    #[test]
    fn scratch_that_over_a_selection_deletes_it() {
        assert!(deletes_selection("chat", Some("so I like the images and the graphics")));
        assert!(!deletes_selection("chat", None), "nothing selected: scratch what Navi typed, as before");
        assert!(!deletes_selection("chat", Some("   ")), "only spaces");
        assert!(!deletes_selection("terminal", Some("ls -la")), "a terminal can't delete a selection with Delete");
        assert!(same_words("So I like the images.", "so i like the images"));
        assert!(!same_words("so I like the images", "so I like the graphics"));
        assert!(!same_words("", ""));
    }

    #[test]
    fn a_key_bounce_is_not_a_double_tap() {
        // The hook's catch-up of a release it missed sends the release and the press together.
        assert!(is_key_bounce(0));
        // Fingers settling on Ctrl + Win: up for a few milliseconds.
        assert!(is_key_bounce(20));
        assert!(is_key_bounce(44));
        // A deliberate double tap leaves the keys up for longer.
        assert!(!is_key_bounce(45));
        assert!(!is_key_bounce(120));
        // Both windows stay within the double tap's own limits.
        assert!(KEY_BOUNCE_MS < DOUBLE_TAP_MS && (KEY_BOUNCE_MS as u64) < DOUBLE_TAP_WINDOW_MS);
    }

    #[test]
    fn a_bounce_never_finishes_a_hands_free_take_or_a_ramble() {
        let t = Instant::now();
        let ms = |n: u64| t + Duration::from_millis(n);
        // The second tap of a double tap bounced: let go, and down again 12 ms later.
        assert_eq!(press_while_recording(Kind::HandsFree, false, Some(ms(500)), ms(512)), Press::Bounce);
        // Started a ramble, and the keys bounced on the way up.
        assert_eq!(press_while_recording(Kind::Ramble, false, Some(ms(2000)), ms(2030)), Press::Bounce);
        // A real press afterwards finishes either.
        assert_eq!(press_while_recording(Kind::HandsFree, false, Some(ms(500)), ms(1500)), Press::Finish);
        assert_eq!(press_while_recording(Kind::Ramble, false, Some(ms(2000)), ms(2045)), Press::Finish);
        // A take "Navi" started has seen no release: the first press finishes it.
        assert_eq!(press_while_recording(Kind::HandsFree, false, None, ms(10)), Press::Finish);
        // The first tap's quick release, as before: a bounce carries on holding to talk, a real
        // second tap goes hands-free.
        assert_eq!(press_while_recording(Kind::Talk, true, Some(ms(100)), ms(120)), Press::Bounce);
        assert_eq!(press_while_recording(Kind::Talk, true, Some(ms(100)), ms(250)), Press::HandsFree);
        assert_eq!(press_while_recording(Kind::Talk, true, None, ms(250)), Press::Bounce, "no release kept: no gap at all");
        assert_eq!(press_while_recording(Kind::Talk, false, None, ms(250)), Press::Held);
    }

    #[test]
    fn the_double_tap_window_counts_from_the_hooks_release() {
        let released = Instant::now();
        let closes = double_tap_closes(released);
        assert_eq!(closes - released, Duration::from_millis(DOUBLE_TAP_WINDOW_MS));
        // A slow start: the controller reaches the release long after the hook saw it. The window
        // is fixed by the hook's times, so a second tap seen 200 ms after the release still
        // counts, and a press seen after the window is a new take, whenever they are handled.
        let second = released + Duration::from_millis(200);
        assert!(!window_closed(closes, second));
        assert_eq!(press_while_recording(Kind::Talk, true, Some(released), second), Press::HandsFree);
        assert!(!window_closed(closes, closes));
        assert!(window_closed(closes, released + Duration::from_millis(DOUBLE_TAP_WINDOW_MS + 1)));
    }

    #[test]
    fn an_unread_field_takes_only_a_fresh_untouched_insert() {
        let secs = |n: u64| Some(Duration::from_secs(n));
        let typed = "See you on Friday.";
        // Nothing read (reading is off, or the app doesn't say): just typed, no key since.
        assert_eq!(select_back_check(None, false, typed, secs(3), false), Ok(()));
        assert_eq!(select_back_check(None, false, typed, secs(15), false), Ok(()));
        // A key since: words may have been typed after Navi's.
        assert_eq!(select_back_check(None, false, typed, secs(3), true), Err(KEYS_SINCE.to_string()));
        // Too long ago: the caret may have been moved with the mouse.
        assert_eq!(select_back_check(None, false, typed, secs(16), false), Err(too_old_to_edit_blind()));
        // The second look, after the AI: only keys count.
        assert_eq!(select_back_check(None, false, typed, None, false), Ok(()));
        assert_eq!(select_back_check(None, false, typed, None, true), Err(KEYS_SINCE.to_string()));
        // A terminal counts as unread, whatever its screen reads back.
        assert_eq!(select_back_check(Some("PS C:\\> git status"), true, "git status", secs(3), false), Ok(()));
        assert_eq!(select_back_check(Some("PS C:\\> git status"), true, "git status", secs(3), true), Err(KEYS_SINCE.to_string()));
        assert_eq!(select_back_check(Some("PS C:\\> git status"), true, "git status", secs(60), false), Err(too_old_to_edit_blind()));
        // A field that was read decides by itself, as before.
        assert_eq!(select_back_check(Some("Thanks. See you on Friday."), false, typed, secs(300), true), Ok(()));
        assert_eq!(select_back_check(Some("See you on Friday. Cheers Alex"), false, typed, secs(1), false), Err(TEXT_CHANGED.to_string()));
        // Every refusal fits the bubble (short_error keeps 80 characters).
        for m in [TEXT_CHANGED.to_string(), KEYS_SINCE.to_string(), too_old_to_edit_blind()] {
            assert!(m.chars().count() <= 80, "{m}");
        }
    }

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
    fn nothing_to_compare_counts_but_nothing_read_does_not() {
        // Nothing read is left to select_back_check, which asks for a fresh insert and no keys.
        assert!(!ends_with_insert(None, "anything at all"));
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
    fn a_take_waits_for_an_engine_on_its_way() {
        // A restart: the old engine stopped, the new one starting or warming up.
        for state in ["starting", "stopped", "ready"] {
            assert!(engine_on_its_way(state, false), "{state}");
        }
        // One that can't start isn't waited for, unless a new start is already under way (the
        // model was just downloaded, or Settings changed it).
        for state in ["missing-model", "missing-engine", "error"] {
            assert!(!engine_on_its_way(state, false), "{state}");
            assert!(engine_on_its_way(state, true), "{state}");
        }
        // About 10 seconds on the processor: well within the wait, and the bubble keeps saying so.
        assert!(ENGINE_WAIT >= Duration::from_secs(25));
        assert!(ENGINE_WAIT_NOTE_EVERY < Duration::from_secs(4), "bubble.js hides a notice after four seconds");
        assert!(!ENGINE_WAIT_NOTE.contains('\u{2014}'));
    }

    #[test]
    fn the_bubble_names_why_the_ai_was_skipped() {
        let skipped = |e: &str| ai_fallback_note(&format!("AI step skipped: {e} Your words were tidied offline instead."));
        assert_eq!(skipped("Ollama isn't installed or isn't running. Nothing answered at http://127.0.0.1:11434."), "Ollama isn't running, so Navi typed a plain tidy instead.");
        assert_eq!(skipped("Claude rejected the API key."), "Claude rejected the API key, so Navi typed a plain tidy instead.");
        assert_eq!(skipped("No connection to OpenAI. Check your internet connection."), "No connection to OpenAI, so Navi typed a plain tidy instead.");
        assert_eq!(skipped("Couldn't reach the AI server. Is it running at http://127.0.0.1:1234/v1?"), "Couldn't reach the AI server, so Navi typed a plain tidy instead.");
        assert_eq!(skipped("Ollama doesn't have the model \"qwen2.5:14b\". Run: ollama pull qwen2.5:14b"), "Ollama doesn't have the model \"qwen2.5:14b\", so Navi typed a plain tidy instead.");
        assert_eq!(skipped("Claude had a server error (500). Try again shortly."), "Claude had a server error, so Navi typed a plain tidy instead.");
        assert_eq!(skipped("Claude Code isn't signed in. Open Settings > AI and press Sign in on its card, then try again."), "Claude Code isn't signed in, so Navi typed a plain tidy instead.");
        assert_eq!(skipped("Ollama took longer than 60 seconds to reply."), "The AI didn't answer in time, so Navi typed a plain tidy instead.");
        assert_eq!(skipped("Claude Code didn't answer: no reply within 25 seconds"), "The AI didn't answer in time, so Navi typed a plain tidy instead.");
        // Raw detail stays in History, not the bubble.
        assert_eq!(skipped("Claude refused the request: messages: text content blocks must be non-empty"), "The AI step didn't work, so Navi typed a plain tidy instead.");
        assert_eq!(skipped("Codex: error: unexpected argument found"), "The AI step didn't work, so Navi typed a plain tidy instead.");
        // The pipeline's own warnings keep their notes.
        assert_eq!(ai_fallback_note("AI step skipped: no reply in time. Your words were tidied offline instead."), "The AI didn't answer in time, so Navi typed a plain tidy instead.");
        assert_eq!(ai_fallback_note("The AI replied to your words instead of rewriting them, so Navi kept the offline version."), "The AI answered instead of rewriting, so Navi typed a plain tidy instead.");
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
    fn spoken_words_in_an_error_never_reach_the_log() {
        let e = said_error("Couldn't find \"Tuesday\" in what you just said.".into(), "the words weren't found");
        // The bubble says which words; the log doesn't.
        assert_eq!(short_error(&e), "Couldn't find \"Tuesday\" in what you just said.");
        assert_eq!(for_log(&e), "the words weren't found");
        assert!(!for_log(&e.context("running the command")).contains("Tuesday"), "wrapped, too");
        // Reworded for the bubble (the clipboard note), it stays out of the log.
        let e = said_error("Couldn't find \"Tuesday\" in what you just said.".into(), "the words weren't found");
        let noted = reworded(&e, with_note(&short_error(&e), "Your words are on the clipboard."));
        assert!(short_error(&noted).contains("Tuesday") && short_error(&noted).ends_with("Your words are on the clipboard."));
        assert!(!for_log(&noted).contains("Tuesday"));
        // Every other error is logged as it is.
        let plain = anyhow!("There's nothing recent to scratch here.");
        assert_eq!(for_log(&plain), "There's nothing recent to scratch here.");
        assert_eq!(for_log(&reworded(&plain, "Again.".into())), "Again.");
    }

    fn pasted(take_id: &str, secs_ago: u64, checked: bool) -> LastInsert {
        let at = Instant::now().checked_sub(Duration::from_secs(secs_ago)).expect("an instant in the past");
        LastInsert { hwnd: 1, text: "See you on Tuesday.".into(), take_id: take_id.into(), app_label: "Notepad".into(), at, checked, replaced: None }
    }

    #[test]
    fn each_learning_timer_looks_only_at_its_own_take() {
        // Take A's timer fires after take B was pasted: B is left for its own timer.
        assert!(!learning_due(&pasted("b", 5, false), Some("a")));
        assert!(learning_due(&pasted("b", 25, false), Some("b")));
        // The check at the next take looks at whatever was pasted last.
        assert!(learning_due(&pasted("b", 5, false), None));
        // Never twice, and never straight after the paste.
        assert!(!learning_due(&pasted("b", 25, true), Some("b")));
        assert!(!learning_due(&pasted("b", 1, false), None));
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

    #[test]
    fn a_ramble_the_ai_couldnt_write_says_so() {
        assert_eq!(ramble_summary(2, " · 2 tasks to ClickUp", None), "Saved to Notes · 2 tasks · 2 tasks to ClickUp");
        assert_eq!(ramble_summary(0, "", None), "Saved to Notes");
        // Rough notes: said, with the reason, never "Saved to Notes" as if all went well.
        let rough = ramble_summary(1, "", Some("Claude Code took too long"));
        assert_eq!(rough, "Saved rough notes: the AI couldn't write them (Claude Code took too long) · 1 task");
        let rough = ramble_summary(0, " · not sent (Local only)", Some(&short_error(&anyhow!("Couldn't reach Ollama at http://localhost:11434. Is it running?"))));
        assert_eq!(rough, "Saved rough notes: the AI couldn't write them (Couldn't reach Ollama at http://localhost:11434) · not sent (Local only)");
        assert!(!rough.contains("?.") && !rough.contains('\u{2014}'));
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
    fn a_colour_after_the_wake_word_is_never_typed() {
        let m = matcher_with_wake(&["Navi", "Hey Navi"]);
        assert_eq!(after_take("Navi, turn gold.", &m, true, true, true), AfterTake::Spoken(Action::Colour("gold")));
        assert_eq!(after_take("Navi, change to teal", &m, false, false, false), AfterTake::Spoken(Action::Colour("teal")), "from the shortcut too");
        for said in ["Turn gold.", "The gold price rose again.", "Navi, the gold price rose again.", "Navi, turn gold into cash.", "Navi, turn orange."] {
            assert!(matches!(after_take(said, &m, true, true, false), AfterTake::Dictate { .. }), "{said}");
        }
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
