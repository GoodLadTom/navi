//! Everything the interface can ask the Rust side to do. Errors go back as readable strings.

use crate::controller::{self, all_modes, make_llm};
use crate::meeting::{self, MeetingStatus, PromptState};
use crate::state::App;
use crate::store::notes::MeetingNote;
use crate::text::{modes, pipeline};
use crate::types::*;
use crate::{audio, context, engine, hotkey, insert, integrations, models, settings as settings_store, sounds::Cue, store::secrets};
use serde::Serialize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{Manager, State};

type Res<T> = Result<T, String>;
fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    version: String,
    chord_label: String,
    data_dir: String,
    engine: EngineStatus,
    onboarded: bool,
    /// "windows" or "mac", so the interface can use each system's words and keys.
    platform: String,
}

#[tauri::command]
pub fn app_info(app: State<'_, Arc<App>>) -> AppInfo {
    let s = app.settings();
    AppInfo {
        version: app.handle.package_info().version.to_string(),
        chord_label: hotkey::display_chord_label(&s.hotkeys.talk),
        data_dir: app.paths.data.display().to_string(),
        engine: app.engine_status.lock().clone(),
        onboarded: s.onboarded,
        platform: if cfg!(target_os = "macos") { "mac" } else if cfg!(windows) { "windows" } else { "linux" }.into(),
    }
}

#[tauri::command]
pub fn get_settings(app: State<'_, Arc<App>>) -> Settings {
    app.settings()
}

#[tauri::command]
pub async fn save_settings(app: State<'_, Arc<App>>, settings: Settings) -> Res<Settings> {
    let app = app.inner().clone();
    // Phrases are checked and tidied here, so a bad one is refused with a reason and the window
    // stores what was really saved.
    let mut settings = settings;
    let before = app.settings();
    settings.voice_commands = match settings.voice_commands.normalised_from(Some(&before.voice_commands)) {
        Ok(vc) => vc,
        // Phrases already in settings.json that today's checks refuse (edited by hand, or saved
        // by a newer Navi) never stop you saving something else, such as the theme.
        Err(_) if settings.voice_commands == before.voice_commands => {
            log::warn!("the saved voice commands don't pass today's checks; kept as they were");
            settings.voice_commands
        }
        Err(e) => return Err(e),
    };
    if before.hotkeys.talk != settings.hotkeys.talk {
        hotkey::validate_chord(&settings.hotkeys.talk)?;
    }
    settings_store::save(&app.paths, &settings).map_err(err)?;
    *app.settings.write() = settings.clone();

    if before.hotkeys.talk != settings.hotkeys.talk || before.hotkeys.ramble_key != settings.hotkeys.ramble_key || before.hotkeys.cancel_key != settings.hotkeys.cancel_key {
        if let Some(h) = app.hotkeys.lock().as_ref() {
            h.set_chord(settings.hotkeys.talk.clone(), settings.hotkeys.ramble_key.clone(), settings.hotkeys.cancel_key.clone());
        }
        log::info!("talk shortcut is now {}", hotkey::display_chord_label(&settings.hotkeys.talk));
        crate::set_tray_tooltip(&app);
    }
    if before.hotkeys.mode_keys != settings.hotkeys.mode_keys || before.hotkeys.action_keys != settings.hotkeys.action_keys {
        if let Some(h) = app.hotkeys.lock().as_ref() {
            h.set_extras(&crate::extra_key_chords(&settings));
        }
    }
    if before.modes.active != settings.modes.active {
        crate::sync_tray_modes(&app);
    }
    if before.general.launch_at_login != settings.general.launch_at_login {
        crate::apply_autostart(&app.handle, settings.general.launch_at_login);
    }
    let e1 = &before.engine;
    let e2 = &settings.engine;
    if e1.model != e2.model || e1.use_gpu != e2.use_gpu || e1.language != e2.language || e1.vad != e2.vad || e1.server_path != e2.server_path || e1.model_path != e2.model_path {
        // Trim silences turned on fetches its model first, so it works straight away.
        crate::restart_engine_for_settings(app.clone());
    } else if before.privacy.local_only && !settings.privacy.local_only {
        // Local only kept the silence model from downloading; now it can.
        crate::ensure_vad_model(app.clone());
    }
    if before.privacy.keep_audio_days != settings.privacy.keep_audio_days {
        crate::purge_old_audio(&app);
    }
    // The bubble and the meeting card switch between light and dark, and change colour, too.
    if before.general.theme != settings.general.theme || before.general.colour != settings.general.colour || before.general.show_firefly != settings.general.show_firefly {
        app.emit("settings-changed", ());
    }
    if before.general.colour != settings.general.colour {
        crate::colours::set_tray(&app);
    }
    if before.voice_commands != settings.voice_commands {
        crate::sync_tray_toggles(&app);
    }
    Ok(settings)
}

/// Settings > Voice commands > Reset: the phrases Navi ships with (enabled false; the UI keeps its own).
#[tauri::command]
pub fn voice_command_defaults() -> VoiceCommandSettings {
    VoiceCommandSettings::default()
}

#[tauri::command]
pub fn get_stats(app: State<'_, Arc<App>>) -> Res<Stats> {
    app.db.stats().map_err(err)
}

#[tauri::command]
pub fn list_takes(app: State<'_, Arc<App>>, query: HistoryQuery) -> Res<Vec<Take>> {
    app.db.list_takes(&query).map_err(err)
}

#[tauri::command]
pub fn get_take(app: State<'_, Arc<App>>, id: String) -> Res<Option<Take>> {
    app.db.get_take(&id).map_err(err)
}

#[tauri::command]
pub fn delete_take(app: State<'_, Arc<App>>, id: String) -> Res<()> {
    if let Some(path) = app.db.delete_take(&id).map_err(err)? {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

#[tauri::command]
pub fn clear_history(app: State<'_, Arc<App>>) -> Res<()> {
    for path in app.db.clear_history().map_err(err)? {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

#[tauri::command]
pub fn app_names(app: State<'_, Arc<App>>) -> Res<Vec<String>> {
    app.db.app_names().map_err(err)
}

/// Transcribe a kept recording again and run it through a (possibly different) mode.
#[tauri::command]
pub async fn rerun_take(app: State<'_, Arc<App>>, id: String, mode_id: Option<String>) -> Res<Take> {
    let app = app.inner().clone();
    let mut take = app.db.get_take(&id).map_err(err)?.ok_or("That take no longer exists.")?;
    let path = take.audio_path.clone().ok_or("No recording was kept for this take.")?;
    let samples = tokio::task::spawn_blocking(move || audio::read_wav_16k(std::path::Path::new(&path))).await.map_err(err)?.map_err(err)?;
    let settings = app.settings();
    let engine = app.engine.read().await.clone().ok_or("The speech engine is not ready yet.")?;
    let prompt = app.db.vocabulary_prompt(600).unwrap_or_default();
    let tr = engine.transcribe(&samples, &engine::TranscribeOptions { language: settings.engine.language.clone(), prompt }).await.map_err(err)?;
    let all = all_modes(&app);
    let wanted = mode_id.unwrap_or_else(|| take.mode_id.clone());
    let mode = mode_or_clean(&all, &wanted);
    let words = app.db.list_words().unwrap_or_default();
    let snippets = app.db.list_snippets().unwrap_or_default();
    let llm = if mode.use_ai { make_llm(&settings) } else { None };
    // The title kept with the take goes to the AI only while "Read the window title" is still on.
    let window_title = context::kept_title(take.window_title.clone(), settings.context.read_app, false);
    let ctx = AppContext { app_label: take.app_name.clone(), window_title, category: "other".into(), ..Default::default() };
    let out = pipeline::run(pipeline::PipelineInput { raw: &tr.text, mode: &mode, ctx: &ctx, settings: &settings, words: &words, snippets: &snippets, llm: llm.as_ref(), project_files: &[] })
        .await
        .map_err(err)?;
    take.raw_text = tr.text;
    take.final_text = out.text;
    take.words = take.final_text.split_whitespace().count() as u32;
    take.mode_id = mode.id;
    take.model = settings.engine.model.clone();
    mark_rescued(&mut take);
    app.db.update_take(&take).map_err(err)?;
    Ok(take)
}

/// A failed take that has now been transcribed stops showing as Failed, and its words count in the
/// stats (which leave out failed takes). Any other outcome stays as it was.
fn mark_rescued(take: &mut Take) {
    if take.status == "error" {
        take.status = "rerun".into();
        if let Some(meta) = take.meta.as_object_mut() {
            meta.remove("error");
        }
    }
}

#[tauri::command]
pub fn list_modes(app: State<'_, Arc<App>>) -> Vec<Mode> {
    all_modes(&app)
}

#[tauri::command]
pub fn save_mode(app: State<'_, Arc<App>>, mode: Mode) -> Res<Mode> {
    let saved = app.db.save_mode(&mode).map_err(err)?;
    crate::modes_changed(app.inner());
    Ok(saved)
}

#[tauri::command]
pub fn delete_mode(app: State<'_, Arc<App>>, id: String) -> Res<()> {
    app.db.delete_mode(&id).map_err(err)?;
    // Its mode key, and the active or default mode if it was this one, go with it.
    crate::modes_changed(app.inner());
    Ok(())
}

#[tauri::command]
pub async fn preview_mode(app: State<'_, Arc<App>>, mode: Mode, text: String) -> Res<String> {
    let settings = app.settings();
    let words = app.db.list_words().unwrap_or_default();
    let snippets = app.db.list_snippets().unwrap_or_default();
    let llm = if mode.use_ai { make_llm(&settings) } else { None };
    let ctx = AppContext { category: "other".into(), ..Default::default() };
    let out = pipeline::run(pipeline::PipelineInput { raw: &text, mode: &mode, ctx: &ctx, settings: &settings, words: &words, snippets: &snippets, llm: llm.as_ref(), project_files: &[] })
        .await
        .map_err(err)?;
    Ok(out.text)
}

#[tauri::command]
pub fn set_active_mode(app: State<'_, Arc<App>>, id: String) {
    controller::switch_mode(app.inner(), &id);
}

#[tauri::command]
pub fn list_words(app: State<'_, Arc<App>>) -> Res<Vec<DictEntry>> {
    app.db.list_words().map_err(err)
}

#[tauri::command]
pub fn save_word(app: State<'_, Arc<App>>, word: DictEntry) -> Res<DictEntry> {
    app.db.upsert_word(&word).map_err(err)
}

#[tauri::command]
pub fn delete_word(app: State<'_, Arc<App>>, id: String) -> Res<()> {
    app.db.delete_word(&id).map_err(err)
}

#[tauri::command]
pub fn list_snippets(app: State<'_, Arc<App>>) -> Res<Vec<Snippet>> {
    app.db.list_snippets().map_err(err)
}

#[tauri::command]
pub fn save_snippet(app: State<'_, Arc<App>>, snippet: Snippet) -> Res<Snippet> {
    app.db.upsert_snippet(&snippet).map_err(err)
}

#[tauri::command]
pub fn delete_snippet(app: State<'_, Arc<App>>, id: String) -> Res<()> {
    app.db.delete_snippet(&id).map_err(err)
}

#[tauri::command]
pub fn list_suggestions(app: State<'_, Arc<App>>) -> Res<Vec<Suggestion>> {
    app.db.list_suggestions().map_err(err)
}

#[tauri::command]
pub fn accept_suggestion(app: State<'_, Arc<App>>, id: String) -> Res<Option<DictEntry>> {
    app.db.accept_suggestion(&id).map_err(err)
}

#[tauri::command]
pub fn dismiss_suggestion(app: State<'_, Arc<App>>, id: String) -> Res<()> {
    app.db.dismiss_suggestion(&id).map_err(err)
}

#[tauri::command]
pub fn list_packs(app: State<'_, Arc<App>>) -> Res<Vec<PackSummary>> {
    app.db.list_packs().map_err(err)
}

#[tauri::command]
pub fn export_pack(app: State<'_, Arc<App>>, path: String, name: String, author: String, description: String) -> Res<PackSummary> {
    app.db.export_pack(std::path::Path::new(&path), &name, &author, &description).map_err(err)
}

// Installing, updating or removing a pack can add, rename or drop modes, so each ends with
// `modes_changed`.

#[tauri::command]
pub fn import_pack_file(app: State<'_, Arc<App>>, path: String) -> Res<PackSummary> {
    let pack = app.db.install_pack_file(std::path::Path::new(&path)).map_err(err)?;
    crate::modes_changed(app.inner());
    Ok(pack)
}

#[tauri::command]
pub async fn install_pack_url(app: State<'_, Arc<App>>, url: String) -> Res<PackSummary> {
    if app.settings().privacy.local_only {
        return Err("Local only is on, so Navi won't fetch packs from the internet. Download the file and import it instead.".into());
    }
    let pack = crate::store::packs::install_pack_from_url(&app.db, &url).await.map_err(err)?;
    crate::modes_changed(app.inner());
    Ok(pack)
}

#[tauri::command]
pub fn install_pack_json(app: State<'_, Arc<App>>, json: String) -> Res<PackSummary> {
    let pack: Pack = serde_json::from_str(&json).map_err(|e| format!("That pack file is not valid: {e}"))?;
    let summary = app.db.install_pack(&pack).map_err(err)?;
    crate::modes_changed(app.inner());
    Ok(summary)
}

#[tauri::command]
pub fn remove_pack(app: State<'_, Arc<App>>, id: String) -> Res<()> {
    app.db.remove_pack(&id).map_err(err)?;
    crate::modes_changed(app.inner());
    Ok(())
}

#[tauri::command]
pub fn list_devices() -> Vec<DeviceInfo> {
    audio::list_devices()
}

#[tauri::command]
pub fn mic_test_start(app: State<'_, Arc<App>>, device: Option<String>) -> Res<()> {
    if app.audio.is_recording() && !app.mic_test.load(Ordering::SeqCst) {
        return Err("Navi is recording a take right now.".into());
    }
    app.audio.cancel();
    app.mic_test.store(true, Ordering::SeqCst);
    // Meter only: a test left running (window closed to the tray) keeps nothing, so memory stays flat.
    app.audio.start_meter(device.as_deref().filter(|d| !d.is_empty())).map_err(|e| {
        app.mic_test.store(false, Ordering::SeqCst);
        err(e)
    })
}

#[tauri::command]
pub fn mic_test_stop(app: State<'_, Arc<App>>) {
    if app.mic_test.swap(false, Ordering::SeqCst) {
        app.audio.cancel();
    }
}

#[tauri::command]
pub async fn capture_shortcut(app: State<'_, Arc<App>>) -> Res<Vec<String>> {
    let rx = {
        let guard = app.hotkeys.lock();
        let h = guard.as_ref().ok_or("The shortcut listener is not running.")?;
        h.pause(true);
        h.capture_next_chord()
    };
    let result = tokio::time::timeout(std::time::Duration::from_secs(20), rx).await;
    if let Some(h) = app.hotkeys.lock().as_ref() {
        // Back to how it was: still paused if the tray's "Pause the shortcut" is on.
        h.pause(app.paused.load(Ordering::SeqCst));
    }
    match result {
        // Esc on its own backs out (it can't be the shortcut: it cancels takes).
        Ok(Ok(keys)) if keys == ["Escape"] => Err("Cancelled.".into()),
        // Keys pressed in another app (Ctrl + V to paste) must never become the shortcut.
        Ok(Ok(_)) if !crate::main_window_in_front(&app) => Err("Recording only works while Navi is in front, so your shortcut is unchanged. Click Any key and press it here.".into()),
        Ok(Ok(keys)) if !keys.is_empty() => Ok(keys),
        // Stopped because you left Navi or the screen.
        Ok(_) => Err("Recording stopped, so your shortcut is unchanged.".into()),
        Err(_) => {
            if let Some(h) = app.hotkeys.lock().as_ref() {
                h.cancel_capture();
            }
            Err("Nothing was pressed, so your shortcut is unchanged.".into())
        }
    }
}

/// What Settings shows about a shortcut before saving it: its labels, and an error or a warning.
#[tauri::command]
pub fn check_shortcut(app: State<'_, Arc<App>>, keys: Vec<String>) -> hotkey::ChordCheck {
    hotkey::check_chord_with(&keys, &app.settings().hotkeys.ramble_key, &hotkey::display_label)
}

/// The same for a mode key; `mode` is the mode it is for, so its own current key is not a clash.
/// `others`: the other modes' keys to check against, when Settings is about to replace several at
/// once ("Use F1 to F5"); the saved ones otherwise.
#[tauri::command]
pub fn check_mode_key(app: State<'_, Arc<App>>, keys: Vec<String>, mode: String, others: Option<Vec<Vec<String>>>) -> hotkey::ChordCheck {
    let s = app.settings();
    let others: Vec<Vec<String>> = others.unwrap_or_else(|| s.hotkeys.mode_keys.iter().filter(|k| k.mode != mode).map(|k| k.keys.clone()).collect());
    let mut check = hotkey::check_mode_key(&keys, &s.hotkeys.talk, &others, &hotkey::display_label);
    if check.error.is_none() {
        // Nor may it be one of the action keys.
        let actions = action_keys_taken(&s, None);
        check = hotkey::check_action_key(&keys, &s.hotkeys.talk, &actions, &hotkey::display_label);
    }
    check
}

/// The same for an action key. `index`: its place in HotkeySettings::action_keys when it is
/// already there (being changed), so its own current key is not a clash.
#[tauri::command]
pub fn check_action_key(app: State<'_, Arc<App>>, keys: Vec<String>, index: Option<usize>) -> hotkey::ChordCheck {
    let s = app.settings();
    let mut taken = action_keys_taken(&s, index);
    let modes = crate::controller::all_modes(&app);
    for k in &s.hotkeys.mode_keys {
        let name = match k.mode.as_str() {
            "auto" => "Auto".to_string(),
            "next" => "the next mode".to_string(),
            id => modes.iter().find(|m| m.id == id).map_or_else(|| id.to_string(), |m| m.name.clone()),
        };
        taken.push((k.keys.clone(), format!("switching to {name}")));
    }
    hotkey::check_action_key(&keys, &s.hotkeys.talk, &taken, &hotkey::display_label)
}

/// The action keys already set, each with what it does, leaving out the one at `skip`.
fn action_keys_taken(s: &crate::types::Settings, skip: Option<usize>) -> Vec<(Vec<String>, String)> {
    s.hotkeys
        .action_keys
        .iter()
        .enumerate()
        .filter(|(i, k)| Some(*i) != skip && !k.keys.is_empty())
        .map(|(_, k)| (k.keys.clone(), crate::types::action_key_label(&k.action).to_string()))
        .collect()
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct KeySeenPayload {
    name: String,
    label: String,
    up: bool,
    injected: bool,
}

/// Shows the keys the hook sees to the shortcut picker (as "key-seen" events), so the keycaps press
/// down as you hold them. It never holds a take back: the shortcut works as normal meanwhile. Only
/// while Navi's window is in front, and it lapses 45 seconds after the last call, so the interface
/// calls this again while the picker is showing.
#[tauri::command]
pub fn key_monitor(app: State<'_, Arc<App>>, on: bool) -> Res<()> {
    let app = app.inner().clone();
    let in_use = app.handle.get_webview_window("main").is_some_and(|w| w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false))
        && crate::main_window_in_front(&app);
    if !on || !in_use {
        crate::stop_key_monitor(&app);
        return Ok(());
    }
    let until = Instant::now() + Duration::from_secs(45);
    let already_open = {
        let mut slot = app.key_monitor_until.lock();
        let open = slot.is_some_and(|t| Instant::now() < t);
        *slot = Some(until);
        open
    };
    if already_open {
        return Ok(());
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<hotkey::KeySeen>();
    {
        let guard = app.hotkeys.lock();
        let h = guard.as_ref().ok_or("The shortcut listener is not running, so Navi can't hear any keys. Restart Navi.")?;
        h.set_monitor(Some(tx));
    }
    let forward = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(k) = rx.recv().await {
            if !forward.key_monitor_open() {
                crate::stop_key_monitor(&forward);
                break;
            }
            let name = hotkey::vk_name(k.vk);
            forward.emit("key-seen", KeySeenPayload { label: hotkey::display_label(&name), name, up: k.up, injected: k.injected });
        }
    });
    Ok(())
}

/// Abandons a half-made recording of a new shortcut (the picker was closed).
#[tauri::command]
pub fn cancel_shortcut_capture(app: State<'_, Arc<App>>) {
    if let Some(h) = app.hotkeys.lock().as_ref() {
        h.cancel_capture();
    }
}

/// What to show on each key's cap, in this keyboard's layout (a UK keyboard's ` key is "`").
#[tauri::command]
pub fn key_labels(keys: Vec<String>) -> Vec<String> {
    keys.iter().map(|k| hotkey::display_label(k)).collect()
}

/// Other dictation apps running now (they may listen for the same keys).
#[tauri::command]
pub async fn other_dictation_apps() -> Vec<String> {
    tokio::task::spawn_blocking(hotkey::other_dictation_apps).await.unwrap_or_default()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutStatus {
    running: bool,
    label: String,
    health: hotkey::HookHealth,
}

#[tauri::command]
pub fn shortcut_status(app: State<'_, Arc<App>>) -> ShortcutStatus {
    let guard = app.hotkeys.lock();
    ShortcutStatus {
        running: guard.is_some(),
        label: hotkey::display_chord_label(&app.settings().hotkeys.talk),
        health: guard.as_ref().map(|h| h.health()).unwrap_or_default(),
    }
}

/// The window's close button.
#[tauri::command]
pub fn hide_main(app: State<'_, Arc<App>>) {
    crate::close_main_window(app.inner());
}

#[tauri::command]
pub fn list_models(app: State<'_, Arc<App>>) -> Vec<ModelInfo> {
    models::catalogue(&app.paths, &app.settings().engine)
}

/// Model ids (and "engine") downloading now. A second download of the same file appended to the
/// same .part file as the first, and the mixed-up result could be installed as the model.
static DOWNLOADING: parking_lot::Mutex<Vec<String>> = parking_lot::const_mutex(Vec::new());

/// Holds `id`'s place in DOWNLOADING until the download ends, however it ends.
struct DownloadSlot(String);

impl DownloadSlot {
    fn claim(id: &str) -> Res<Self> {
        let mut list = DOWNLOADING.lock();
        if list.iter().any(|d| d == id) {
            return Err("That download is already running.".into());
        }
        list.push(id.to_string());
        Ok(Self(id.to_string()))
    }
}

impl Drop for DownloadSlot {
    fn drop(&mut self) {
        DOWNLOADING.lock().retain(|d| *d != self.0);
    }
}

#[tauri::command]
pub async fn download_model(app: State<'_, Arc<App>>, id: String) -> Res<()> {
    let _slot = DownloadSlot::claim(&id)?;
    let app = app.inner().clone();
    let h = app.clone();
    let cb: models::ProgressCallback = Arc::new(move |p: DownloadProgress| h.emit("download", p));
    models::download_model(&id, &app.paths, cb).await.map_err(err)?;
    if app.settings().engine.model == id {
        crate::start_engine(app.clone());
    }
    Ok(())
}

#[tauri::command]
pub async fn download_engine(app: State<'_, Arc<App>>) -> Res<()> {
    let _slot = DownloadSlot::claim("engine")?;
    let app = app.inner().clone();
    let h = app.clone();
    let cb: models::ProgressCallback = Arc::new(move |p: DownloadProgress| h.emit("download", p));
    let gpu = models::detect_gpu().is_some();
    models::download_engine(&app.paths, gpu, cb).await.map_err(err)?;
    Ok(())
}

#[tauri::command]
pub fn engine_status(app: State<'_, Arc<App>>) -> EngineStatus {
    app.engine_status.lock().clone()
}

/// Restarts the speech engine and waits for it. An engine that didn't start is an error, so set-up
/// and Settings can say why rather than "Done" (the "engine" event carries the status either way).
#[tauri::command]
pub async fn restart_engine(app: State<'_, Arc<App>>) -> Res<EngineStatus> {
    let app = app.inner().clone();
    crate::start_engine_and_wait(app.clone()).await;
    let status = app.engine_status.lock().clone();
    engine_started(status)
}

fn engine_started(status: EngineStatus) -> Res<EngineStatus> {
    if status.state == "ready" {
        Ok(status)
    } else if status.message.trim().is_empty() {
        Err("The speech engine didn't start. Restart it in Settings > Speech engine.".into())
    } else {
        Err(status.message)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupInfo {
    gpu: Option<String>,
    server_found: bool,
    server_path: Option<String>,
    model_found: Option<String>,
    model_path: Option<String>,
    model_id: Option<String>,
}

#[tauri::command]
pub async fn detect_setup(app: State<'_, Arc<App>>) -> Res<SetupInfo> {
    let app = app.inner().clone();
    tokio::task::spawn_blocking(move || {
        let settings = app.settings();
        let gpu = models::detect_gpu().map(|g| format!("{} ({} GB)", g.name, (g.vram_mb as f64 / 1024.0).round()));
        let server = models::find_server(&settings.engine, &app.paths);
        // The model already chosen, if it is on disk ("Run set-up again" keeps what was picked in
        // Settings); otherwise the best model already on disk.
        let catalogue = models::catalogue(&app.paths, &settings.engine);
        let installed = || catalogue.iter().filter(|m| m.installed && m.id != "vad");
        let best = installed().find(|m| m.id == settings.engine.model).or_else(|| installed().max_by_key(|m| (m.accuracy, m.speed)));
        SetupInfo {
            gpu,
            server_found: server.is_some(),
            server_path: server.map(|p| p.display().to_string()),
            model_found: best.and_then(|m| m.path.clone()),
            model_path: best.and_then(|m| m.path.clone()),
            model_id: best.map(|m| m.id.clone()),
        }
    })
    .await
    .map_err(err)
}

#[tauri::command]
pub fn play_cue(app: State<'_, Arc<App>>, name: String) {
    if let Some(cue) = Cue::from_key(&name) {
        app.sounds.play(cue, &app.settings().sounds);
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SoundSets {
    default: String,
    sets: Vec<crate::sounds::SoundSet>,
}

/// The sound sets for the picker (soft, low cues; all seven in each).
#[tauri::command]
pub fn list_sound_sets(app: State<'_, Arc<App>>) -> SoundSets {
    SoundSets { default: app.sounds.default_set().to_string(), sets: app.sounds.sets().to_vec() }
}

#[tauri::command]
pub fn set_secret(name: String, value: String) -> Res<()> {
    secrets::set(&name, &value).map_err(err)
}

#[tauri::command]
pub fn has_secret(name: String) -> bool {
    secrets::has(&name)
}

#[tauri::command]
pub async fn test_ai(app: State<'_, Arc<App>>) -> Res<String> {
    let settings = app.settings();
    // Why make_llm (llm.rs from_settings) turned the provider down, in its own terms: a custom
    // server needs an address and a model (its key is optional), Claude Code and Codex need no key.
    let llm = make_llm(&settings).ok_or_else(|| {
        let provider = settings.ai.provider.trim().to_ascii_lowercase();
        let filled = |v: Option<&str>| v.is_some_and(|s| !s.trim().is_empty());
        if provider.is_empty() || provider == "off" {
            "Choose a provider first.".to_string()
        } else if provider == "custom" && !(filled(settings.ai.base_url.as_deref()) && filled(Some(&settings.ai.model))) {
            "Enter the server address and model first.".to_string()
        } else if settings.privacy.local_only {
            "Local only is on, so AI that runs off this computer (Claude Code and Codex included) is blocked.".to_string()
        } else if provider == "anthropic" || provider == "openai" {
            "Save an API key first.".to_string()
        } else if provider == "custom" {
            "Check the server address and model.".to_string()
        } else {
            "Choose a provider first.".to_string()
        }
    })?;
    llm.test().await.map_err(err)
}

/// Settings > AI with Ollama: whether Ollama answers at its address, and the models it has pulled
/// to choose from (GET /api/tags). Always asks, so a moment after Ollama starts it shows as running.
#[tauri::command]
pub async fn ollama_status(app: State<'_, Arc<App>>) -> Res<crate::text::llm::OllamaStatus> {
    let s = app.settings();
    // The same field holds Claude Code's or Codex's path while one of them is in use.
    let address = if s.ai.provider == "ollama" { s.ai.base_url.clone() } else { None };
    Ok(crate::text::llm::ollama_status(address.as_deref(), s.privacy.local_only).await)
}

/// The subscription tools on this computer (Claude Code, Codex), for Settings > AI. The tool in use
/// is looked for with its path from Settings first, as it will be when it runs, so a tool Navi
/// couldn't find on its own shows as found, at the place that path leads to.
#[tauri::command]
pub async fn detect_ai_tools(app: State<'_, Arc<App>>) -> Res<Vec<crate::text::cli_ai::CliTool>> {
    use crate::text::cli_ai;
    let ai = app.settings().ai;
    tokio::task::spawn_blocking(move || {
        let mut tools = cli_ai::detect();
        let path = ai.base_url.as_deref().map(str::trim).filter(|p| !p.is_empty());
        if let Some(tool) = tools.iter_mut().find(|t| t.id == ai.provider).filter(|_| path.is_some()) {
            let found = if tool.id == "codex" { cli_ai::find_codex(path) } else { cli_ai::find_claude(path) };
            if let Some(l) = found {
                tool.found = true;
                // As cli_ai shows it: the script for a Node tool, otherwise the program.
                tool.path = Some(l.script.as_ref().unwrap_or(&l.program).display().to_string());
            }
        }
        tools
    })
    .await
    .map_err(err)
}

/// What Settings' Find button found.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FoundAiTool {
    pub found: bool,
    /// Where it is, as the card shows it.
    pub shown: Option<String>,
    /// What to keep as the tool's path in Settings. None when Navi finds it there anyway.
    pub path: Option<String>,
    /// From the tool's own status command; None when it couldn't tell.
    pub signed_in: Option<bool>,
}

/// Searches this computer for Claude Code or Codex (cli_ai::search) and asks whether it is
/// signed in, so a card can connect it with one click instead of asking for a path.
#[tauri::command]
pub async fn find_ai_tool(id: String) -> Res<FoundAiTool> {
    use crate::text::cli_ai;
    if !cli_ai::is_cli(&id) {
        return Err(format!("Navi doesn't know the AI tool \u{201c}{id}\u{201d}."));
    }
    let search_for = id.clone();
    let Some(found) = tokio::task::spawn_blocking(move || cli_ai::search(&search_for)).await.map_err(err)? else {
        log::info!("AI tools: the search found no {id}");
        return Ok(FoundAiTool { found: false, shown: None, path: None, signed_in: None });
    };
    let signed_in = cli_ai::signed_in(&id, &found.launcher).await;
    let said = match signed_in {
        Some(true) => "signed in",
        Some(false) => "not signed in",
        None => "sign-in not known",
    };
    log::info!("AI tools: found {id} {}, {said}", if found.path.is_some() { "by searching" } else { "where Navi looks" });
    Ok(FoundAiTool { found: true, shown: Some(found.launcher.shown()), path: found.path.map(|p| p.display().to_string()), signed_in })
}

/// The tool `id` as Navi would start it: with the path in Settings while it is the AI in use, else
/// where Navi looks, else by searching.
fn ai_tool_launcher(app: &App, id: &str) -> Option<crate::text::cli_ai::Launcher> {
    use crate::text::cli_ai;
    let ai = app.settings().ai;
    let path = ai.base_url.as_deref().map(str::trim).filter(|p| ai.provider == id && !p.is_empty() && !p.contains("://"));
    let usual = if id == "codex" { cli_ai::find_codex(path) } else { cli_ai::find_claude(path) };
    usual.or_else(|| cli_ai::search(id).map(|f| f.launcher))
}

async fn ai_tool_launcher_for(app: &Arc<App>, id: &str) -> Res<crate::text::cli_ai::Launcher> {
    if !crate::text::cli_ai::is_cli(id) {
        return Err(format!("Navi doesn't know the AI tool \u{201c}{id}\u{201d}."));
    }
    let (app, look_for) = (app.clone(), id.to_string());
    tokio::task::spawn_blocking(move || ai_tool_launcher(&app, &look_for)).await.map_err(err)?.ok_or_else(|| "Navi can\u{2019}t find it on this computer.".to_string())
}

/// Whether Claude Code or Codex is signed in (null when it couldn't tell), for the card's check
/// after the sign-in window opened.
#[tauri::command]
pub async fn ai_tool_signed_in(app: State<'_, Arc<App>>, id: String) -> Res<Option<bool>> {
    let launcher = ai_tool_launcher_for(app.inner(), &id).await?;
    Ok(crate::text::cli_ai::signed_in(&id, &launcher).await)
}

/// Opens a terminal window with the tool's own sign-in (`claude auth login`, `codex login`).
#[tauri::command]
pub async fn sign_in_ai_tool(app: State<'_, Arc<App>>, id: String) -> Res<()> {
    let launcher = ai_tool_launcher_for(app.inner(), &id).await?;
    crate::text::cli_ai::open_sign_in(&id, &launcher).map_err(err)?;
    log::info!("AI tools: opened the sign-in for {id}");
    Ok(())
}

fn eleven_key(app: &App) -> Res<String> {
    if app.settings().privacy.local_only {
        return Err("Local only is on, so ElevenLabs is blocked.".into());
    }
    secrets::get("elevenlabs").ok_or_else(|| "Save your ElevenLabs API key first.".to_string())
}

/// The voices in the ElevenLabs account, for the voice picker.
#[tauri::command]
pub async fn eleven_voices(app: State<'_, Arc<App>>) -> Res<Vec<crate::speak::Voice>> {
    let key = eleven_key(&app)?;
    crate::speak::voices(&key).await.map_err(err)
}

/// The text-to-speech models the account can use (v4 first).
#[tauri::command]
pub async fn eleven_models(app: State<'_, Arc<App>>) -> Res<Vec<crate::speak::VoiceModel>> {
    let key = eleven_key(&app)?;
    crate::speak::models(&key).await.map_err(err)
}

/// The clip `speak_preview` made last, with the voice, model and text it was made from, so Download
/// saves exactly what was heard without asking ElevenLabs (and the plan) again.
type SpeechKey = (String, String, String);
static LAST_SPEECH: parking_lot::Mutex<Option<(SpeechKey, Vec<u8>)>> = parking_lot::const_mutex(None);

/// `text` read in the chosen voice (or `voice_id`), as a WAV data URL the interface can play.
#[tauri::command]
pub async fn speak_preview(app: State<'_, Arc<App>>, text: String, voice_id: Option<String>) -> Res<String> {
    use base64::Engine as _;
    let key = eleven_key(&app)?;
    let s = app.settings().speak;
    let voice = voice_id.filter(|v| !v.trim().is_empty()).unwrap_or(s.voice_id);
    let samples = crate::speak::speak(&key, &voice, &s.model, &text).await.map_err(err)?;
    let wav = crate::speak::wav(&samples).map_err(err)?;
    *LAST_SPEECH.lock() = Some(((voice, s.model, text), wav.clone()));
    // The page plays it as soon as it arrives: voice commands stop listening meanwhile, with a
    // second's grace for the page to start, so Navi never wakes itself.
    crate::wake::hold_off(samples.len() as f32 / crate::speak::SAMPLE_RATE as f32 + 1.0);
    Ok(format!("data:audio/wav;base64,{}", base64::engine::general_purpose::STANDARD.encode(wav)))
}

/// Saves a voice clip as a WAV file at `path`: the clip just played when it is the same text and
/// voice, otherwise a new one from ElevenLabs. Returns where it went.
#[tauri::command]
pub async fn save_speech(app: State<'_, Arc<App>>, text: String, voice_id: Option<String>, path: String) -> Res<String> {
    let s = app.settings().speak;
    let voice = voice_id.filter(|v| !v.trim().is_empty()).unwrap_or(s.voice_id.clone());
    let wanted: SpeechKey = (voice.clone(), s.model.clone(), text.clone());
    let heard = LAST_SPEECH.lock().as_ref().filter(|(k, _)| *k == wanted).map(|(_, wav)| wav.clone());
    let wav = match heard {
        Some(wav) => wav,
        None => {
            let key = eleven_key(&app)?;
            let samples = crate::speak::speak(&key, &voice, &s.model, &text).await.map_err(err)?;
            let wav = crate::speak::wav(&samples).map_err(err)?;
            *LAST_SPEECH.lock() = Some((wanted, wav.clone()));
            wav
        }
    };
    let mut path = std::path::PathBuf::from(path.trim());
    if path.extension().is_none() {
        path.set_extension("wav");
    }
    std::fs::write(&path, &wav).map_err(|e| format!("Couldn't save the clip there ({e})."))?;
    log::info!("saved a voice clip ({} KB)", wav.len() / 1024);
    Ok(path.display().to_string())
}

/// Where Download suggests saving a clip: the Downloads folder, named after the voice and the first
/// few words ("Alistair - Hi Priya just to say the.wav").
#[tauri::command]
pub fn suggest_clip_path(text: String, voice_name: Option<String>) -> String {
    let words = text.split_whitespace().take(7).collect::<Vec<_>>().join(" ");
    // ElevenLabs names often carry a description after a dash: keep the name itself.
    let voice = voice_name.map(|v| v.split(['\u{2013}', '\u{2014}', '-', ',', '(']).next().unwrap_or_default().trim().to_string()).filter(|v| !v.is_empty());
    let name = match voice {
        Some(v) if !words.is_empty() => format!("{v} - {words}"),
        Some(v) => v,
        None if !words.is_empty() => words,
        None => "Navi voice clip".to_string(),
    };
    let dir = dirs::download_dir().or_else(dirs::home_dir).unwrap_or_default();
    dir.join(format!("{}.wav", integrations::safe_file_title(&name))).display().to_string()
}

/// Shows a file Navi saved (a voice clip) in File Explorer.
#[tauri::command]
pub fn reveal_file(path: String) -> Res<()> {
    if !std::path::Path::new(&path).is_file() {
        return Err("That file isn't there any more.".into());
    }
    tauri_plugin_opener::reveal_item_in_dir(path).map_err(err)
}

/// Opens a web link in the browser. Only http and https: nothing else gets handed to Windows.
#[tauri::command]
pub fn open_link(url: String) -> Res<()> {
    let lower = url.trim().to_ascii_lowercase();
    if !(lower.starts_with("https://") || lower.starts_with("http://")) {
        return Err("Navi only opens web links.".into());
    }
    tauri_plugin_opener::open_url(url.trim(), None::<&str>).map_err(err)
}

/// The Test button beside a ClickUp or Notion token: asks the service who the token belongs to.
#[tauri::command]
pub async fn test_integration(app: State<'_, Arc<App>>, name: String) -> Res<String> {
    if let Some(why) = test_integration_blocked(&app.settings(), &name) {
        return Err(why);
    }
    let token = secrets::get(&name).ok_or("Save a token first.")?;
    match name.as_str() {
        "clickup" => integrations::test_clickup(&token).await.map(|u| format!("Connected as {u}")).map_err(err),
        "notion" => integrations::test_notion(&token).await.map(|u| format!("Connected as {u}")).map_err(err),
        _ => Err("Unknown integration.".into()),
    }
}

/// Why the Test button for `name` won't go online: Local only, which blocks ClickUp and Notion here
/// as it does when notes and tasks are sent. None for a name that isn't a service.
fn test_integration_blocked(settings: &Settings, name: &str) -> Option<String> {
    let service = match name {
        "clickup" => "ClickUp",
        "notion" => "Notion",
        _ => return None,
    };
    settings.privacy.local_only.then(|| format!("Local only is on, so Navi won't contact {service}. Turn it off in Settings > Context and privacy."))
}

#[tauri::command]
pub async fn generate_style_notes(app: State<'_, Arc<App>>, paths: Vec<String>) -> Res<String> {
    let settings = app.settings();
    let llm = make_llm(&settings).ok_or("This needs an AI provider. Turn one on in Settings > AI.")?;
    let mut samples = String::new();
    for p in paths.iter().take(12) {
        let path = std::path::Path::new(p);
        let is_word = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("docx"));
        // A Word file is a zip, so reading it as text failed and it was quietly skipped.
        let text = if is_word { docx_text(path) } else { std::fs::read_to_string(path).ok() };
        if let Some(text) = text {
            samples.push_str(&text.chars().take(4000).collect::<String>());
            samples.push_str("\n\n---\n\n");
        }
        if samples.len() > 16000 {
            break;
        }
    }
    if samples.trim().is_empty() {
        return Err("Those files had no readable text. Plain text, Markdown, Word documents and saved emails work best.".into());
    }
    let system = "You describe how a person writes, so another writer can imitate them. Read the samples and write 4 to 6 short lines in the second person (\"You write...\"), covering: sentence length and rhythm, tone and warmth, words and phrases they like, how they open and sign off messages, punctuation habits, and anything they avoid. Plain British English. No em dashes. No preamble, no headings, no bullet symbols: one point per line.";
    let notes = llm.complete(system, &samples, 500, 0.3).await.map_err(err)?;
    Ok(crate::text::cleanup::no_em_dashes(notes.trim()))
}

/// The words in a Word document (.docx), a line per paragraph. None when it can't be read.
fn docx_text(path: &std::path::Path) -> Option<String> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    let mut xml = String::new();
    archive.by_name("word/document.xml").ok()?.take(8_000_000).read_to_string(&mut xml).ok()?;
    Some(text_from_word_xml(&xml))
}

/// The text runs (<w:t>) of a Word document's XML, with its tabs, line breaks and paragraphs.
fn text_from_word_xml(xml: &str) -> String {
    let mut out = String::new();
    let mut rest = xml;
    let mut in_text = false;
    // A paragraph's tab stops (<w:tabs><w:tab w:pos="720"/></w:tabs>) are settings, not tabs.
    let mut in_tab_stops = false;
    while let Some(lt) = rest.find('<') {
        if in_text {
            out.push_str(&decode_xml_entities(&rest[..lt]));
        }
        let Some(gt) = rest[lt..].find('>') else { break };
        let tag = &rest[lt + 1..lt + gt];
        rest = &rest[lt + gt + 1..];
        let closing = tag.starts_with('/');
        let empty = tag.ends_with('/');
        let name = tag.trim_start_matches('/').split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("");
        match name {
            "w:t" => in_text = !closing && !empty,
            "w:tabs" => in_tab_stops = !closing && !empty,
            "w:tab" if !closing && !in_tab_stops => out.push('\t'),
            "w:br" | "w:cr" if !closing => out.push('\n'),
            "w:p" if closing || empty => out.push('\n'),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// `&amp;`, `&lt;`, `&#8217;` and the like back to the characters they stand for.
fn decode_xml_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let decoded = after.find(';').filter(|&semi| semi <= 10).and_then(|semi| {
            let c = match &after[..semi] {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                code => code
                    .strip_prefix("#x")
                    .or_else(|| code.strip_prefix("#X"))
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .or_else(|| code.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                    .and_then(char::from_u32),
            };
            c.map(|c| (c, semi))
        });
        match decoded {
            Some((c, semi)) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[tauri::command]
pub fn add_project(path: String) -> Res<ProjectRef> {
    let p = std::path::Path::new(&path);
    if !p.is_dir() {
        return Err("That folder does not exist.".into());
    }
    let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.clone());
    Ok(ProjectRef { id: uuid::Uuid::new_v4().to_string()[..8].to_string(), name, path })
}

#[tauri::command]
pub fn finish_onboarding(app: State<'_, Arc<App>>) -> Res<()> {
    let mut s = app.settings();
    s.onboarded = true;
    settings_store::save(&app.paths, &s).map_err(err)?;
    crate::apply_autostart(&app.handle, s.general.launch_at_login);
    *app.settings.write() = s;
    Ok(())
}

#[tauri::command]
pub fn copy_text(text: String) -> Res<()> {
    insert::set_clipboard_text(&text).map_err(err)
}

#[tauri::command]
pub fn open_data_folder(app: State<'_, Arc<App>>) -> Res<()> {
    tauri_plugin_opener::open_path(app.paths.data.display().to_string(), None::<&str>).map_err(err)
}

#[tauri::command]
pub async fn quit_app(app: State<'_, Arc<App>>) -> Res<()> {
    crate::quit(app.inner().clone()).await;
    Ok(())
}

#[tauri::command]
pub fn user_first_name() -> String {
    context::user_first_name()
}

#[tauri::command]
pub async fn check_learning(app: State<'_, Arc<App>>) -> Res<()> {
    controller::learning_check_now(app.inner()).await;
    Ok(())
}

/// Transcribe a WAV file and run it through a mode. Returns (raw transcript, final text, engine ms).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileResult {
    raw: String,
    text: String,
    engine_ms: u64,
    total_ms: u64,
}

#[tauri::command]
pub async fn transcribe_file(app: State<'_, Arc<App>>, path: String, mode_id: Option<String>) -> Res<FileResult> {
    let started = std::time::Instant::now();
    let app = app.inner().clone();
    let p = path.clone();
    let samples = tokio::task::spawn_blocking(move || audio::read_wav_16k(std::path::Path::new(&p))).await.map_err(err)?.map_err(err)?;
    let settings = app.settings();
    let engine = app.engine.read().await.clone().ok_or("The speech engine is not ready yet.")?;
    let prompt = app.db.vocabulary_prompt(600).unwrap_or_default();
    let tr = engine.transcribe(&samples, &engine::TranscribeOptions { language: settings.engine.language.clone(), prompt }).await.map_err(err)?;
    let all = all_modes(&app);
    let wanted = mode_id.unwrap_or_else(|| settings.modes.default_mode.clone());
    let mode = mode_or_clean(&all, &wanted);
    let words = app.db.list_words().unwrap_or_default();
    let snippets = app.db.list_snippets().unwrap_or_default();
    let llm = if mode.use_ai { make_llm(&settings) } else { None };
    let ctx = AppContext { category: "other".into(), ..Default::default() };
    let out = pipeline::run(pipeline::PipelineInput { raw: &tr.text, mode: &mode, ctx: &ctx, settings: &settings, words: &words, snippets: &snippets, llm: llm.as_ref(), project_files: &[] })
        .await
        .map_err(err)?;
    Ok(FileResult { raw: tr.text, text: out.text, engine_ms: tr.ms, total_ms: started.elapsed().as_millis() as u64 })
}

/// The mode with this id, or Clean when it no longer exists.
fn mode_or_clean(all: &[Mode], id: &str) -> Mode {
    all.iter()
        .find(|m| m.id == id)
        .or_else(|| all.iter().find(|m| m.id == "clean"))
        .cloned()
        .unwrap_or_else(|| modes::builtin_modes().into_iter().find(|m| m.id == "clean").unwrap_or_else(|| modes::builtin_modes().remove(0)))
}

// ---------------------------------------------------------------------------------------------
// Meeting notes (meeting.rs)
// ---------------------------------------------------------------------------------------------

/// Starts taking notes. `source` is the call app when the card offered it.
#[tauri::command]
pub async fn start_meeting(app: State<'_, Arc<App>>, source: Option<String>) -> Res<String> {
    let app = app.inner().clone();
    // Opening the audio devices can take a moment, so not on the main thread.
    tokio::task::spawn_blocking(move || meeting::start(&app, source)).await.map_err(err)?.map_err(err)
}

#[tauri::command]
pub fn stop_meeting(app: State<'_, Arc<App>>) -> Res<()> {
    meeting::stop(&app).map_err(err)
}

#[tauri::command]
pub fn meeting_status(app: State<'_, Arc<App>>) -> MeetingStatus {
    meeting::status(&app)
}

#[tauri::command]
pub fn set_my_notes(app: State<'_, Arc<App>>, text: String) -> Res<()> {
    meeting::set_my_notes(&app, text).map_err(err)
}

/// Names the meeting in progress; the title stays when the notes are written.
#[tauri::command]
pub fn set_meeting_title(app: State<'_, Arc<App>>, title: String) -> Res<()> {
    meeting::set_title(&app, title).map_err(err)
}

/// Newest first, without transcripts.
#[tauri::command]
pub fn list_notes(app: State<'_, Arc<App>>) -> Res<Vec<MeetingNote>> {
    app.db.list_notes(500).map_err(err)
}

#[tauri::command]
pub fn get_note(app: State<'_, Arc<App>>, id: String) -> Res<Option<MeetingNote>> {
    app.db.get_note(&id).map_err(err)
}

#[tauri::command]
pub fn update_note(app: State<'_, Arc<App>>, id: String, title: Option<String>, notes_md: Option<String>) -> Res<MeetingNote> {
    meeting::update(&app, &id, title, notes_md).map_err(err)
}

/// Writes the notes again with the AI; on failure they stay as they were.
#[tauri::command]
pub async fn rewrite_note(app: State<'_, Arc<App>>, id: String) -> Res<MeetingNote> {
    meeting::rewrite(app.inner(), &id).await.map_err(err)
}

/// Removes the note from Navi. Its Markdown file in the notes folder stays.
#[tauri::command]
pub fn delete_note(app: State<'_, Arc<App>>, id: String) -> Res<()> {
    if app.meeting.lock().as_ref().is_some_and(|s| s.id == id) {
        return Err("Stop taking notes first.".into());
    }
    app.db.delete_note(&id).map_err(err)?;
    // Its screenshots are Navi's own files; the Markdown copy in the notes folder stays.
    crate::screen::remove_dir_for(&app, &id);
    Ok(())
}

/// Sends a note's actions to ClickUp or Notion, as Settings > Meeting notes says.
#[tauri::command]
pub async fn send_note_actions(app: State<'_, Arc<App>>, id: String) -> Res<String> {
    let note = app.db.get_note(&id).map_err(err)?.ok_or("That note has gone.")?;
    let settings = app.settings();
    if settings.notes.actions_to == "none" {
        return Err("Choose where actions go in Settings > Meeting notes first.".into());
    }
    if meeting::actions_from(&note.notes_md).is_empty() {
        return Err("These notes have no actions to send.".into());
    }
    // send_actions only says whether it worked, so name what's missing first rather than always
    // blaming the connection.
    if let Some(why) = actions_blocked(&settings, secrets::has) {
        return Err(why);
    }
    // Then the service's own reason ("ClickUp rejected the token. Check it in Settings.").
    meeting::try_send_actions(&settings, &note).await
}

/// Why actions can't go where Settings > Meeting notes sends them, before trying: Local only, or
/// a token or list missing. `has_token` asks Windows Credential Manager (a stand-in in tests).
fn actions_blocked(settings: &Settings, has_token: impl Fn(&str) -> bool) -> Option<String> {
    let (service, token, id, id_name) = match settings.notes.actions_to.as_str() {
        "clickup" => ("ClickUp", "clickup", settings.ramble.clickup_list_id.as_deref(), "list ID"),
        "notion" => ("Notion", "notion", settings.ramble.notion_database_id.as_deref(), "database ID"),
        _ => return None,
    };
    if settings.privacy.local_only {
        Some(format!("Local only is on, so Navi won't send anything to {service}. Turn it off in Settings > Context and privacy."))
    } else if !has_token(token) {
        Some(format!("Save your {service} token in Settings > Notes and tasks first."))
    } else if id.is_none_or(|id| id.trim().is_empty()) {
        Some(format!("Add the {service} {id_name} in Settings > Notes and tasks first."))
    } else {
        None
    }
}

#[tauri::command]
pub fn watch_status() -> crate::watch::WatchStatus {
    crate::watch::status()
}

#[tauri::command]
pub fn start_watch(app: State<'_, Arc<App>>) -> Res<()> {
    crate::watch::start(app.inner()).map_err(err)
}

#[tauri::command]
pub fn stop_watch(app: State<'_, Arc<App>>) -> Res<()> {
    crate::watch::stop(app.inner()).map_err(err)
}

/// What the meeting card is showing (it asks when it loads).
#[tauri::command]
pub fn prompt_state() -> PromptState {
    meeting::prompt_state()
}

/// The answer in the middle of the screen was clicked: show all of it.
#[tauri::command]
pub fn expand_reply(app: State<'_, Arc<App>>) {
    crate::meeting::expand_reply(&app);
}

#[tauri::command]
pub fn dismiss_meeting_prompt(app: State<'_, Arc<App>>) {
    meeting::dismiss(&app);
}

/// Shows a note's Markdown copy in File Explorer.
#[tauri::command]
pub fn reveal_note_file(app: State<'_, Arc<App>>, id: String) -> Res<()> {
    let note = app.db.get_note(&id).map_err(err)?.ok_or("That note has gone.")?;
    let path = note.file_path.filter(|p| std::path::Path::new(p).is_file()).ok_or("The Markdown copy isn't there any more.")?;
    tauri_plugin_opener::reveal_item_in_dir(path).map_err(err)
}

/// Opens the Notes page, at one note when `id` is given.
#[tauri::command]
pub fn open_note(app: State<'_, Arc<App>>, id: Option<String>) {
    match id.filter(|id| !id.is_empty()) {
        Some(id) => app.navigate(&format!("notes/{id}")),
        None => app.navigate("notes"),
    }
}

// ---------------------------------------------------------------------------------------------
// Updates (update.rs)
// ---------------------------------------------------------------------------------------------

#[tauri::command]
pub fn update_status(app: State<'_, Arc<App>>) -> crate::update::UpdateStatus {
    crate::update::status(&app)
}

/// "Check for updates": asks GitHub now, whatever the settings say.
#[tauri::command]
pub async fn check_for_updates(app: State<'_, Arc<App>>) -> Res<crate::update::UpdateStatus> {
    Ok(crate::update::check(app.inner(), true).await)
}

/// Downloads and installs the update on offer; Navi closes and the new version opens.
#[tauri::command]
pub async fn install_update(app: State<'_, Arc<App>>) -> Res<()> {
    crate::update::install(app.inner()).await.map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_xml_gives_its_text_a_line_per_paragraph() {
        let xml = r#"<?xml version="1.0"?><w:document><w:body><w:p w:rsidR="00A1"><w:pPr><w:tabs/></w:pPr><w:r><w:t>Hi Sam,</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">Cheers for </w:t></w:r><w:r><w:t>Friday</w:t><w:tab/><w:t>&amp; the notes &#8211; and &lt;more&gt;.</w:t><w:br/><w:t>Alex</w:t></w:r></w:p><w:p/><w:tbl><w:tr><w:tc><w:p><w:r><w:t>Cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>"#;
        assert_eq!(text_from_word_xml(xml), "Hi Sam,\nCheers for Friday\t& the notes \u{2013} and <more>.\nAlex\n\nCell");
    }

    #[test]
    fn tab_stops_in_a_paragraphs_settings_are_not_tabs() {
        let xml = r#"<w:p><w:pPr><w:tabs><w:tab w:val="left" w:pos="720"/><w:tab w:val="right" w:pos="9026"/></w:tabs></w:pPr><w:r><w:t>Dear Sam</w:t><w:tab/><w:t>14 October</w:t></w:r></w:p>"#;
        assert_eq!(text_from_word_xml(xml), "Dear Sam\t14 October");
    }

    #[test]
    fn a_failed_take_that_is_re_run_counts_again() {
        let mut take = Take { status: "error".into(), meta: serde_json::json!({ "error": "The speech engine isn't ready", "app": "x" }), ..Default::default() };
        mark_rescued(&mut take);
        assert_eq!(take.status, "rerun");
        assert_eq!(take.meta, serde_json::json!({ "app": "x" }), "the old failure goes");
        let mut pasted = Take { status: "pasted".into(), ..Default::default() };
        mark_rescued(&mut pasted);
        assert_eq!(pasted.status, "pasted", "other outcomes stay as they were");
    }

    #[test]
    fn an_engine_that_did_not_start_is_an_error() {
        let ready = EngineStatus { state: "ready".into(), ..Default::default() };
        assert!(engine_started(ready).is_ok());
        let missing = EngineStatus { state: "missing-model".into(), message: "The Whisper Turbo model is not downloaded yet".into(), ..Default::default() };
        assert_eq!(engine_started(missing).unwrap_err(), "The Whisper Turbo model is not downloaded yet");
        assert!(engine_started(EngineStatus { state: "error".into(), ..Default::default() }).unwrap_err().contains("didn't start"));
    }

    #[test]
    fn xml_entities_are_decoded_and_stray_ampersands_kept() {
        assert_eq!(decode_xml_entities("Alex&apos;s &quot;draft&quot; &#x2019; &#65;"), "Alex's \"draft\" \u{2019} A");
        assert_eq!(decode_xml_entities("R&D & more &nonsense;"), "R&D & more &nonsense;");
    }

    #[test]
    fn sending_actions_says_what_is_missing_first() {
        let mut s = Settings::default();
        assert_eq!(actions_blocked(&s, |_| true), None, "actions go nowhere");
        s.notes.actions_to = "clickup".into();
        assert!(actions_blocked(&s, |_| false).unwrap().contains("ClickUp token"));
        assert!(actions_blocked(&s, |_| true).unwrap().contains("ClickUp list ID"));
        s.ramble.clickup_list_id = Some("901".into());
        assert_eq!(actions_blocked(&s, |_| true), None);
        s.privacy.local_only = true;
        assert!(actions_blocked(&s, |_| true).unwrap().contains("Local only"));
        s.privacy.local_only = false;
        s.notes.actions_to = "notion".into();
        assert!(actions_blocked(&s, |name| name == "notion").unwrap().contains("Notion database ID"));
    }

    #[test]
    fn local_only_stops_the_clickup_and_notion_test_buttons() {
        let mut s = Settings::default();
        assert_eq!(test_integration_blocked(&s, "clickup"), None);
        assert_eq!(test_integration_blocked(&s, "notion"), None);
        s.privacy.local_only = true;
        assert_eq!(
            test_integration_blocked(&s, "clickup").as_deref(),
            Some("Local only is on, so Navi won't contact ClickUp. Turn it off in Settings > Context and privacy.")
        );
        assert!(test_integration_blocked(&s, "notion").unwrap().contains("won't contact Notion"));
        assert_eq!(test_integration_blocked(&s, "dropbox"), None, "not a service: Unknown integration says so");
    }

    #[test]
    fn one_download_of_a_file_at_a_time() {
        let first = DownloadSlot::claim("test-model").unwrap();
        assert!(DownloadSlot::claim("test-model").is_err(), "a second download of the same file is refused");
        assert!(DownloadSlot::claim("test-other").is_ok(), "other files can download meanwhile");
        drop(first);
        assert!(DownloadSlot::claim("test-model").is_ok(), "free again once the first ends");
    }
}
