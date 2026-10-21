//! Navi: local, private dictation that writes like you.

pub mod audio;
pub mod colours;
pub mod commands;
pub mod context;
pub mod controller;
pub mod engine;
pub mod hotkey;
pub mod insert;
pub mod integrations;
pub mod jobs;
#[cfg(target_os = "macos")]
pub mod mac;
pub mod media;
pub mod meeting;
pub mod models;
pub mod overlay;
pub mod paths;
pub mod screen;
pub mod settings;
pub mod sounds;
pub mod speak;
pub mod spoken;
pub mod state;
pub mod store;
pub mod text;
pub mod types;
pub mod update;
pub mod wake;
pub mod watch;

use crate::state::App;
use crate::types::{EngineStatus, PillState};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_autostart::ManagerExt;

const LAUNCHED_AT_LOGIN: &str = "--minimised";

pub fn run() {
    let started_hidden = std::env::args().any(|a| a == LAUNCHED_AT_LOGIN);

    let app = tauri::Builder::default()
        // By default the window registers for raw keyboard input, and while it has the focus
        // Windows then stops calling Navi's low-level keyboard hook: the shortcut, "Any key" and
        // the live key display all went dead inside Navi's own window (tauri-apps/wry#1761).
        // Navi never uses raw device events, so turn them off.
        .device_event_filter(tauri::DeviceEventFilter::Always)
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(state) = app.try_state::<Arc<App>>() {
                state.show_main();
            }
        }))
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, Some(vec![LAUNCHED_AT_LOGIN])))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .level_for("tao", log::LevelFilter::Warn)
                .level_for("wry", log::LevelFilter::Warn)
                .max_file_size(2_000_000)
                .build(),
        )
        .setup(move |app| {
            // An error here used to become a panic, and with no console the process just
            // vanished on every launch. Say what went wrong, then close.
            if let Err(e) = setup(app.handle(), started_hidden) {
                startup_failed(&e);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::get_settings,
            commands::save_settings,
            commands::voice_command_defaults,
            commands::get_stats,
            commands::list_takes,
            commands::get_take,
            commands::delete_take,
            commands::clear_history,
            commands::app_names,
            commands::rerun_take,
            commands::list_modes,
            commands::save_mode,
            commands::delete_mode,
            commands::preview_mode,
            commands::set_active_mode,
            commands::check_mode_key,
            commands::list_words,
            commands::save_word,
            commands::delete_word,
            commands::list_snippets,
            commands::save_snippet,
            commands::delete_snippet,
            commands::list_suggestions,
            commands::accept_suggestion,
            commands::dismiss_suggestion,
            commands::list_packs,
            commands::export_pack,
            commands::import_pack_file,
            commands::install_pack_url,
            commands::install_pack_json,
            commands::remove_pack,
            commands::list_devices,
            commands::mic_test_start,
            commands::mic_test_stop,
            commands::capture_shortcut,
            commands::check_shortcut,
            commands::key_monitor,
            commands::cancel_shortcut_capture,
            commands::key_labels,
            commands::other_dictation_apps,
            commands::shortcut_status,
            commands::hide_main,
            commands::list_sound_sets,
            commands::detect_ai_tools,
            commands::eleven_voices,
            commands::eleven_models,
            commands::speak_preview,
            commands::save_speech,
            commands::suggest_clip_path,
            commands::reveal_file,
            commands::open_link,
            commands::start_meeting,
            commands::stop_meeting,
            commands::meeting_status,
            commands::set_my_notes,
            commands::set_meeting_title,
            commands::list_notes,
            commands::get_note,
            commands::update_note,
            commands::rewrite_note,
            commands::delete_note,
            commands::prompt_state,
            commands::dismiss_meeting_prompt,
            commands::open_note,
            commands::reveal_note_file,
            commands::send_note_actions,
            commands::watch_status,
            commands::start_watch,
            commands::stop_watch,
            commands::update_status,
            commands::check_for_updates,
            commands::install_update,
            commands::list_models,
            commands::download_model,
            commands::download_engine,
            commands::engine_status,
            commands::restart_engine,
            commands::detect_setup,
            commands::play_cue,
            commands::set_secret,
            commands::has_secret,
            commands::test_ai,
            commands::ollama_status,
            commands::test_integration,
            commands::generate_style_notes,
            commands::add_project,
            commands::finish_onboarding,
            commands::copy_text,
            commands::open_data_folder,
            commands::quit_app,
            commands::user_first_name,
            commands::check_learning,
            commands::transcribe_file,
        ])
        .build(tauri::generate_context!());
    let app = match app {
        Ok(app) => app,
        Err(e) => startup_failed(&anyhow::Error::new(e)),
    };

    app.run(|handle, event| {
        if let RunEvent::ExitRequested { api, code, .. } = event {
            // Closing the last window must not quit: Navi lives in the tray.
            let quitting = handle.try_state::<Arc<App>>().map(|s| s.quitting.load(Ordering::SeqCst)).unwrap_or(true);
            if !quitting && code.is_none() {
                api.prevent_exit();
            }
        }
    });
}

/// Navi can't start (a database from a newer Navi, a damaged one, a data folder it can't write
/// to): log it, show it in a message box, and close.
fn startup_failed(e: &anyhow::Error) -> ! {
    log::error!("Navi could not start: {e:#}");
    log::logger().flush();
    let text = startup_failure_text(e);
    #[cfg(windows)]
    {
        use windows::core::{HSTRING, PCWSTR};
        use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND};
        let (body, title) = (HSTRING::from(text.as_str()), HSTRING::from("Navi couldn't start"));
        unsafe { MessageBoxW(None, PCWSTR(body.as_ptr()), PCWSTR(title.as_ptr()), MB_OK | MB_ICONERROR | MB_SETFOREGROUND) };
    }
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display alert \"Navi couldn't start\" message {} as critical buttons {{\"OK\"}}",
            applescript_string(&text)
        );
        let _ = std::process::Command::new("/usr/bin/osascript").args(["-e", &script]).status();
    }
    #[cfg(not(windows))]
    eprintln!("{text}");
    std::process::exit(1);
}

/// The message box's text: the whole chain ("preparing the database ...: This data was saved by
/// a newer version of Navi ..."), starting with a capital.
fn startup_failure_text(e: &anyhow::Error) -> String {
    let chain = format!("{e:#}");
    let mut chars = chain.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "Something went wrong while Navi was starting.".into(),
    }
}

fn setup(handle: &AppHandle, started_hidden: bool) -> anyhow::Result<()> {
    let paths = paths::Paths::init(handle)?;
    // Other apps' sound a crash left muted mid-take (media.rs).
    let data_dir = paths.data.clone();
    std::thread::spawn(move || media::recover(data_dir));
    let mut settings = settings::load(&paths);
    let db = Arc::new(store::Db::open(&paths.db)?);
    // A Prompt mode the user edited before Prompt knew the Codex and ChatGPT apps gets them, once.
    match db.add_new_prompt_apps() {
        Ok(true) => log::info!("modes: added the Codex and ChatGPT apps to your edited Prompt mode"),
        Ok(false) => {}
        Err(e) => log::warn!("modes: couldn't add the Codex and ChatGPT apps to Prompt mode: {e:#}"),
    }
    // Mode keys (and the active or default mode) left pointing at a mode that has since gone,
    // "Interview me" and Speak among them.
    if let Some((modes, unreadable)) = modes_for_tidy(&db) {
        if forget_missing_modes(&mut settings, &modes, &unreadable) {
            if let Err(e) = settings::save(&paths, &settings) {
                log::warn!("could not save settings: {e:#}");
            }
        }
    }
    // Mode keys made only of modifiers (saved before Settings refused them) never fire: drop them
    // so Settings stops showing a key that does nothing. Too early for the bubble, so the log says so.
    if let Some(note) = hotkey::drop_dead_mode_keys(&mut settings.hotkeys.mode_keys) {
        log::warn!("{note}");
        if let Err(e) = settings::save(&paths, &settings) {
            log::warn!("could not save settings: {e:#}");
        }
    }
    if settings.user_name.is_empty() {
        settings.user_name = context::user_first_name();
    }

    let (level_tx, mut level_rx) = tokio::sync::mpsc::unbounded_channel::<f32>();
    let audio = audio::AudioService::new(level_tx);
    let sounds = sounds::SoundService::new(paths.sounds.clone());

    let app = Arc::new(App {
        handle: handle.clone(),
        paths,
        settings: parking_lot::RwLock::new(settings.clone()),
        db,
        engine: tokio::sync::RwLock::new(None),
        engine_start: tokio::sync::Mutex::new(()),
        engine_status: parking_lot::Mutex::new(EngineStatus { state: "starting".into(), message: "Starting the speech engine".into(), model: settings.engine.model.clone(), ..Default::default() }),
        audio,
        sounds,
        hotkeys: parking_lot::Mutex::new(None),
        session: parking_lot::Mutex::new(state::Session::default()),
        last_insert: parking_lot::Mutex::new(None),
        tray_mode_items: parking_lot::Mutex::new(Vec::new()),
        tray_toggles: parking_lot::Mutex::new(Vec::new()),
        mic_test: AtomicBool::new(false),
        quitting: AtomicBool::new(false),
        paused: AtomicBool::new(false),
        pill_gen: AtomicU64::new(0),
        project_index: parking_lot::Mutex::new(Default::default()),
        key_monitor_until: parking_lot::Mutex::new(None),
        meeting: parking_lot::Mutex::new(None),
    });
    handle.manage(app.clone());

    // Mic levels go to the bubble (and to Settings while testing the microphone).
    {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            while let Some(v) = level_rx.recv().await {
                app.emit("level", v);
            }
        });
    }

    // Stay in the tray only when Windows launched Navi at sign-in and the user chose that.
    build_windows(handle, &app, started_hidden && settings.onboarded && settings.general.start_minimised)?;
    build_tray(handle, &app)?;

    // Hotkeys.
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    match hotkey::HotkeyService::start(settings.hotkeys.talk.clone(), settings.hotkeys.ramble_key.clone(), settings.hotkeys.cancel_key.clone(), tx) {
        Ok(h) => {
            h.set_extras(&mode_key_chords(&settings));
            *app.hotkeys.lock() = Some(h);
            log::info!("listening for {}", hotkey::display_chord_label(&settings.hotkeys.talk));
        }
        Err(e) => log::error!("could not start the shortcut listener: {e:#}"),
    }
    controller::spawn(app.clone(), rx);
    set_tray_tooltip(&app);
    sync_tray_modes(&app);
    // Say in the log if Windows ever had to be given a fresh hook (see hotkey.rs).
    {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let mut reported = (0, 0);
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                let Some(health) = app.hotkeys.lock().as_ref().map(|h| h.health()) else { break };
                if (health.reinstalls, health.busy_misses) != reported && health.busy_misses > 0 {
                    log::info!("shortcut listener: {} keys let through while busy, {} reinstalls", health.busy_misses, health.reinstalls);
                }
                reported = (health.reinstalls, health.busy_misses);
            }
        });
    }

    // UI Automation takes tens of milliseconds to start; do it now, not on the first take.
    std::thread::spawn(context::warm_up);
    // Only start with Windows once the user has finished setting Navi up.
    apply_autostart(handle, settings.general.launch_at_login && settings.onboarded);
    start_engine(app.clone());
    ensure_vad_model(app.clone());
    purge_old_audio(&app);
    // And every few hours, as Navi often runs for weeks in the tray without a restart.
    {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
                purge_old_audio(&app);
            }
        });
    }
    // Meeting notes: close any a crash left half done, then watch for calls starting.
    meeting::recover(&app);
    meeting::spawn_watch(app.clone());
    // Voice commands: opens the microphone only while Settings say so.
    wake::spawn(app.clone());
    // New versions from GitHub.
    update::spawn_auto_check(app.clone());

    // Keep subscribed packs current, quietly, a minute after start.
    {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            if !app.settings().privacy.local_only && app.db.subscriptions().is_ok_and(|s| !s.is_empty()) {
                for e in store::packs::refresh_subscriptions(&app.db).await {
                    log::info!("pack refresh: {e}");
                }
                // A new version of a pack can add, rename or drop modes.
                modes_changed(&app);
            }
        });
    }
    Ok(())
}

fn build_windows(handle: &AppHandle, app: &Arc<App>, hidden: bool) -> anyhow::Result<()> {
    let main = WebviewWindowBuilder::new(handle, "main", WebviewUrl::App("index.html".into()))
        .title("Navi")
        .inner_size(1120.0, 760.0)
        .min_inner_size(920.0, 620.0)
        .center()
        .visible(!hidden);
    // Windows draws its own title bar buttons in the page; a Mac keeps its traffic lights over the
    // page, with no title (the page hides its own buttons when body has the "mac" class).
    #[cfg(target_os = "macos")]
    let main = main.title_bar_style(tauri::TitleBarStyle::Overlay).hidden_title(true);
    #[cfg(not(target_os = "macos"))]
    let main = main.decorations(false);
    let main = main.build()?;
    #[cfg(windows)]
    if let Ok(hwnd) = main.hwnd() {
        app_icon::give_window_big_icon(hwnd.0 as isize);
        app_icon::refresh_shell_icon();
    }
    {
        let app = app.clone();
        main.on_window_event(move |event| match event {
            WindowEvent::CloseRequested { api, .. } => {
                if !app.quitting.load(Ordering::SeqCst) {
                    api.prevent_close();
                    close_main_window(&app);
                }
            }
            // Recording a new shortcut and the live key display only work while you are in this
            // window, so a key pressed in another app (Ctrl + V to paste) never becomes the
            // shortcut. Checked a moment later against the real foreground window, so focus moving
            // into the page never counts.
            WindowEvent::Focused(false) => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
                    if !main_window_in_front(&app) {
                        leave_shortcut_picker(&app);
                    }
                });
            }
            _ => {}
        });
    }

    let pill = WebviewWindowBuilder::new(handle, "pill", WebviewUrl::App("pill.html".into()))
        .title("Navi bubble")
        // Hidden startup size; show_pill expands this transparent overlay to the selected monitor.
        .inner_size(520.0, 220.0)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .focused(false)
        .visible(false)
        .build()?;
    overlay::prepare_pill(&pill)?;

    // The meeting notes card ("Take notes?"), shown in the corner of the screen when a call starts.
    // No title: should Windows ever draw a title bar on it, there is nothing to show.
    let prompt = WebviewWindowBuilder::new(handle, "prompt", WebviewUrl::App("prompt.html".into()))
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
    // And it can never get a title bar back (this runs on the thread that made the window).
    #[cfg(windows)]
    if let Ok(hwnd) = prompt.hwnd() {
        meeting::guard_card(hwnd.0 as isize);
    }
    Ok(())
}

/// Navi's icon on the taskbar. tao only gives a window the small icon, so the taskbar fell back to
/// Windows' cached picture of navi.exe, which kept the old orange mark after an update.
#[cfg(windows)]
mod app_icon {
    use windows::core::{PCWSTR, HSTRING};
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_UPDATEITEM, SHCNF_PATHW};
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, LoadImageW, SendMessageW, ICON_BIG, IMAGE_ICON, LR_DEFAULTCOLOR, SM_CXICON, SM_CYICON, WM_SETICON};

    /// The icon baked into navi.exe by tauri-build (WINDOWS_APP_ICON_RESOURCE_ID).
    const APP_ICON_ID: u16 = 32512;

    /// Gives the window the large icon too, at the size the taskbar and Alt + Tab draw.
    pub fn give_window_big_icon(raw: isize) {
        unsafe {
            let Ok(module) = GetModuleHandleW(None) else { return };
            let (w, h) = (GetSystemMetrics(SM_CXICON), GetSystemMetrics(SM_CYICON));
            if let Ok(icon) = LoadImageW(Some(module.into()), PCWSTR(APP_ICON_ID as usize as *const u16), IMAGE_ICON, w, h, LR_DEFAULTCOLOR) {
                SendMessageW(HWND(raw as *mut std::ffi::c_void), WM_SETICON, Some(WPARAM(ICON_BIG as usize)), Some(LPARAM(icon.0 as isize)));
            }
        }
    }

    /// Tells Windows navi.exe may have a new icon, so the Start menu and desktop shortcuts stop
    /// showing the picture they cached before an update.
    pub fn refresh_shell_icon() {
        let Ok(exe) = std::env::current_exe() else { return };
        let path = HSTRING::from(exe.as_os_str());
        unsafe { SHChangeNotify(SHCNE_UPDATEITEM, SHCNF_PATHW, Some(path.as_ptr() as *const std::ffi::c_void), None) };
    }
}

fn build_tray(handle: &AppHandle, app: &Arc<App>) -> anyhow::Result<()> {
    let open = MenuItem::with_id(handle, "open", "Open Navi", true, None::<&str>)?;
    let history = MenuItem::with_id(handle, "history", "History", true, None::<&str>)?;
    let settings = MenuItem::with_id(handle, "settings", "Settings", true, None::<&str>)?;
    let auto = CheckMenuItem::with_id(handle, "mode:auto", "Pick from the app", true, true, None::<&str>)?;
    let mode_items = tray_mode_items(handle, app)?;
    let mut mode_refs: Vec<&dyn tauri::menu::IsMenuItem<tauri::Wry>> = vec![&auto];
    let sep_modes = PredefinedMenuItem::separator(handle)?;
    mode_refs.push(&sep_modes);
    for item in &mode_items {
        mode_refs.push(item);
    }
    let modes_menu = Submenu::with_items(handle, "Mode", true, &mode_refs)?;
    *TRAY_MODES.lock() = Some(modes_menu.clone());
    let pause = CheckMenuItem::with_id(handle, "pause", "Pause the shortcut", true, false, None::<&str>)?;
    let voice = app.settings().voice_commands;
    let hey_navi = CheckMenuItem::with_id(handle, "heynavi", listen_label(&voice), true, voice.enabled, None::<&str>)?;
    let watch_item = CheckMenuItem::with_id(handle, "watch", "Watch my screen", true, false, None::<&str>)?;
    let quit_item = MenuItem::with_id(handle, "quit", "Quit Navi", true, None::<&str>)?;
    let menu = Menu::with_items(
        handle,
        &[&open, &history, &settings, &PredefinedMenuItem::separator(handle)?, &modes_menu, &pause, &hey_navi, &watch_item, &PredefinedMenuItem::separator(handle)?, &quit_item],
    )?;
    *app.tray_toggles.lock() = vec![hey_navi.clone(), watch_item.clone()];

    // The logo on its tile, in the colour from Settings > General > Colour.
    let icon = colours::tray_icon(&app.settings().general.colour)?;
    let app_for_menu = app.clone();
    {
        let mut all = vec![auto.clone()];
        all.extend(mode_items.iter().cloned());
        *app.tray_mode_items.lock() = all;
    }
    TrayIconBuilder::with_id("navi")
        .icon(icon)
        .tooltip("Navi")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                if let Some(state) = tray.app_handle().try_state::<Arc<App>>() {
                    state.show_main();
                }
            }
        })
        .on_menu_event(move |_handle, event| {
            let id = event.id.as_ref().to_string();
            let app = app_for_menu.clone();
            match id.as_str() {
                "open" => app.show_main(),
                "history" => app.navigate("history"),
                "settings" => app.navigate("settings"),
                "pause" => {
                    let paused = !app.paused.load(Ordering::SeqCst);
                    app.paused.store(paused, Ordering::SeqCst);
                    if let Some(h) = app.hotkeys.lock().as_ref() {
                        h.pause(paused);
                    }
                }
                // The id stays "heynavi" (internal); it switches Settings > Voice commands on and off.
                "heynavi" => {
                    let mut next = app.settings();
                    next.voice_commands.enabled = !next.voice_commands.enabled;
                    match settings::save(&app.paths, &next) {
                        Ok(()) => {
                            *app.settings.write() = next;
                            app.emit("settings-changed", ());
                        }
                        Err(e) => log::warn!("could not save settings: {e:#}"),
                    }
                    sync_tray_toggles(&app);
                }
                "watch" => {
                    let result = if watch::is_watching() { watch::stop(&app) } else { watch::start(&app) };
                    if let Err(e) = result {
                        log::warn!("watch: {e:#}");
                    }
                    sync_tray_toggles(&app);
                }
                "quit" => {
                    tauri::async_runtime::spawn(async move { quit(app).await });
                }
                other if other.starts_with("mode:") => {
                    controller::switch_mode(&app, other.trim_start_matches("mode:"));
                }
                _ => {}
            }
        })
        .build(handle)?;
    Ok(())
}

/// The window's close button and Alt + F4: hide to the tray and keep listening, or quit if the
/// user chose that in Settings. The first time, a note in the bubble says where Navi went.
pub fn close_main_window(app: &Arc<App>) {
    let settings = app.settings();
    if !settings.general.close_to_tray {
        let app = app.clone();
        tauri::async_runtime::spawn(async move { quit(app).await });
        return;
    }
    if let Some(w) = app.handle.get_webview_window("main") {
        let _ = w.hide();
    }
    // A hidden window can't be recording a shortcut or showing keys.
    leave_shortcut_picker(app);
    // Nor testing the microphone: left running, it kept the mic open and blocked voice commands.
    if app.mic_test.swap(false, Ordering::SeqCst) {
        app.audio.cancel();
        app.emit("mic-test", false);
    }
    if !settings.general.tray_tip_shown {
        let label = hotkey::display_chord_label(&settings.hotkeys.talk);
        app.show_pill_window(0);
        let gen = app.pill(PillState::Notice { message: if cfg!(target_os = "macos") { format!("Navi is in the menu bar. Hold {label} to talk.") } else { format!("Navi is in the tray. Hold {label} to talk.") } });
        app.hide_pill_later(gen, 4500);
        let mut next = settings;
        next.general.tray_tip_shown = true;
        match settings::save(&app.paths, &next) {
            Ok(()) => {
                *app.settings.write() = next;
                app.emit("settings-changed", ());
            }
            Err(e) => log::warn!("could not save settings: {e:#}"),
        }
    }
}

/// `text` as an AppleScript string literal.
#[cfg(target_os = "macos")]
fn applescript_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// True when Navi's main window is the one in front (whatever inside it has focus).
pub fn main_window_in_front(app: &App) -> bool {
    #[cfg(windows)]
    {
        app.handle
            .get_webview_window("main")
            .and_then(|w| w.hwnd().ok())
            // Tauri's HWND comes from its own copy of the windows crate; only the raw value carries over.
            .is_some_and(|h| h.0 as isize == context::foreground_hwnd())
    }
    #[cfg(target_os = "macos")]
    {
        context::foreground_pid() == Some(std::process::id() as i32)
            && app.handle.get_webview_window("main").is_some_and(|w| w.is_focused().unwrap_or(false))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = app;
        true
    }
}

/// Stops showing keys to the shortcut picker.
pub fn stop_key_monitor(app: &App) {
    *app.key_monitor_until.lock() = None;
    if let Some(h) = app.hotkeys.lock().as_ref() {
        h.set_monitor(None);
    }
}

/// You left the shortcut picker (or Navi's window): stop showing keys and abandon a half-made
/// recording of a new shortcut. The shortcut you had stays as it was.
pub fn leave_shortcut_picker(app: &App) {
    stop_key_monitor(app);
    if let Some(h) = app.hotkeys.lock().as_ref() {
        h.cancel_capture();
    }
}

/// Ticks the tray's Mode submenu to match the active mode.
pub fn sync_tray_modes(app: &App) {
    let active = app.settings().modes.active;
    let wanted = if active.trim().is_empty() { "mode:auto".to_string() } else { format!("mode:{}", active.trim()) };
    // A copy, so the lock is never held while the menu waits on the main thread.
    let items = app.tray_mode_items.lock().clone();
    for item in items {
        let _ = item.set_checked(item.id().as_ref() == wanted);
    }
}

/// The tray's Mode submenu, kept so it can follow modes being made, renamed and deleted.
static TRAY_MODES: parking_lot::Mutex<Option<Submenu<tauri::Wry>>> = parking_lot::const_mutex(None);

/// One tray item per mode you can switch to (not Notes, which only rambles use).
fn tray_mode_items(handle: &AppHandle, app: &App) -> tauri::Result<Vec<CheckMenuItem<tauri::Wry>>> {
    controller::all_modes(app)
        .iter()
        .filter(|m| m.kind != types::ModeKind::Notes)
        .map(|m| CheckMenuItem::with_id(handle, format!("mode:{}", m.id), &m.name, true, false, None::<&str>))
        .collect()
}

/// Modes were made, renamed or deleted, or a pack brought some in or took some out. Settings stop
/// pointing at modes that have gone (a deleted mode's key went on swallowing its key in every app,
/// with no row in Settings to remove it), the tray's Mode submenu is rebuilt, and the window is
/// told so it can rename what it shows.
pub fn modes_changed(app: &Arc<App>) {
    if let Some((modes, unreadable)) = modes_for_tidy(&app.db) {
        let mut next = app.settings();
        if forget_missing_modes(&mut next, &modes, &unreadable) {
            match settings::save(&app.paths, &next) {
                Ok(()) => {
                    *app.settings.write() = next.clone();
                    if let Some(h) = app.hotkeys.lock().as_ref() {
                        h.set_extras(&mode_key_chords(&next));
                    }
                    app.emit("settings-changed", ());
                }
                Err(e) => log::warn!("could not save settings: {e:#}"),
            }
        }
    }
    refresh_tray_modes(app);
    app.emit("modes", ());
}

/// Swaps the Mode submenu's items for the modes there are now, keeping "Pick from the app". Always
/// on the main thread, so two changes at once can't both add their items.
fn refresh_tray_modes(app: &Arc<App>) {
    let app = app.clone();
    if let Err(e) = app.handle.clone().run_on_main_thread(move || rebuild_tray_modes(&app)) {
        log::warn!("could not rebuild the tray's modes: {e}");
    }
}

fn rebuild_tray_modes(app: &Arc<App>) {
    let Some(menu) = TRAY_MODES.lock().clone() else { return };
    let old = std::mem::take(&mut *app.tray_mode_items.lock());
    let mut fresh: Vec<CheckMenuItem<tauri::Wry>> = old.first().cloned().into_iter().collect();
    for item in old.iter().skip(1) {
        let _ = menu.remove(item);
    }
    match tray_mode_items(&app.handle, app) {
        Ok(items) => {
            for item in items {
                if menu.append(&item).is_ok() {
                    fresh.push(item);
                }
            }
        }
        Err(e) => log::warn!("could not rebuild the tray's modes: {e}"),
    }
    *app.tray_mode_items.lock() = fresh;
    sync_tray_modes(app);
}

/// Every mode there is, and the ids of stored modes Navi can't read. None when the database can't be
/// read, so nothing is forgotten because of it.
fn modes_for_tidy(db: &store::Db) -> Option<(Vec<types::Mode>, Vec<String>)> {
    let stored = db.list_stored_modes().ok()?;
    let ids = db.list_mode_ids().ok()?;
    let unreadable = ids.into_iter().filter(|id| !stored.iter().any(|m| m.id == *id)).collect();
    Some((text::modes::merge(text::modes::builtin_modes(), stored), unreadable))
}

/// Drops what settings say about modes that no longer exist (or can't be switched to): their mode
/// keys, and the active or default mode when it was one of them. True when anything changed.
/// `unreadable` are stored modes Navi can't read (saved by a newer Navi, or damaged): they still
/// exist, so whatever points at them stays, ready for when they can be read again.
pub fn forget_missing_modes(settings: &mut types::Settings, modes: &[types::Mode], unreadable: &[String]) -> bool {
    let usable = |id: &str| unreadable.iter().any(|u| u == id) || modes.iter().any(|m| m.id == id && m.kind != types::ModeKind::Notes);
    let keys_before = settings.hotkeys.mode_keys.len();
    settings.hotkeys.mode_keys.retain(|k| matches!(k.mode.as_str(), "auto" | "next") || usable(&k.mode));
    let mut changed = settings.hotkeys.mode_keys.len() != keys_before;
    if !settings.modes.active.trim().is_empty() && !usable(settings.modes.active.trim()) {
        settings.modes.active = String::new();
        changed = true;
    }
    if !usable(settings.modes.default_mode.trim()) {
        settings.modes.default_mode = types::ModeSettings::default().default_mode;
        changed = true;
    }
    changed
}

/// Ticks the tray's "Listen for ..." and "Watch my screen" to match, and names the first wake word
/// in Settings > Voice commands.
pub fn sync_tray_toggles(app: &App) {
    let voice = app.settings().voice_commands;
    let watching = watch::is_watching();
    for item in app.tray_toggles.lock().iter() {
        let on = match item.id().as_ref() {
            "heynavi" => {
                let _ = item.set_text(listen_label(&voice));
                voice.enabled
            }
            "watch" => watching,
            _ => continue,
        };
        let _ = item.set_checked(on);
    }
}

/// The tray item: "Listen for “Navi”", or whatever the first wake word is ("Listen for “Hey”").
fn listen_label(vc: &types::VoiceCommandSettings) -> String {
    format!("Listen for \u{201c}{}\u{201d}", vc.wake_label())
}

/// The mode keys' chords, in settings order, for the hook.
pub fn mode_key_chords(settings: &types::Settings) -> Vec<Vec<String>> {
    settings.hotkeys.mode_keys.iter().map(|k| k.keys.clone()).collect()
}

pub fn set_tray_tooltip(app: &App) {
    if let Some(tray) = app.handle.tray_by_id("navi") {
        let label = hotkey::display_chord_label(&app.settings().hotkeys.talk);
        let _ = tray.set_tooltip(Some(format!("Navi: hold {label} to talk")));
    }
}

pub fn apply_autostart(handle: &AppHandle, on: bool) {
    let launcher = handle.autolaunch();
    let result = if on { launcher.enable() } else { launcher.disable() };
    if let Err(e) = result {
        log::warn!("could not change start-with-Windows: {e}");
    }
}

/// (Re)start the speech engine in the background, reporting status to the UI as it goes.
pub fn start_engine(app: Arc<App>) {
    tauri::async_runtime::spawn(async move { start_engine_and_wait(app).await });
}

pub async fn start_engine_and_wait(app: Arc<App>) {
    let _one_at_a_time = app.engine_start.lock().await;
    if let Some(old) = app.engine.write().await.take() {
        old.shutdown().await;
    }
    let settings = app.settings();
    let status_app = app.clone();
    let cb: engine::StatusCallback = Arc::new(move |s: EngineStatus| {
        *status_app.engine_status.lock() = s.clone();
        status_app.emit("engine", s);
    });
    match engine::Engine::start(&settings.engine, &app.paths, cb.clone()).await {
        Ok(e) => {
            let status = e.status();
            // Voice commands on: their quick engine (macOS) is started now, ready for "Navi".
            if app.settings.read().voice_commands.enabled {
                e.warm_quick();
            }
            *app.engine.write().await = Some(Arc::new(e));
            if !status.state.is_empty() {
                cb(status);
            }
        }
        Err(e) => {
            log::warn!("speech engine did not start: {e:#}");
            let current = app.engine_status.lock().clone();
            if current.state == "starting" || current.state == "ready" {
                cb(EngineStatus { state: "error".into(), message: e.to_string(), model: settings.engine.model.clone(), ..Default::default() });
            }
        }
    }
}

/// The tiny silence-trimming model stops silence being transcribed as "Thank you."; fetch it once.
/// The engine runs without it meanwhile and restarts to use it when it arrives. Also used when
/// Local only is turned off with Trim silences on.
pub fn ensure_vad_model(app: Arc<App>) {
    if !wants_vad_download(&app) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        if let Ok(true) = fetch_vad_model(&app).await {
            log::info!("silence trimming model installed; restarting the engine to use it");
            start_engine_and_wait(app).await;
        }
    });
}

/// Settings changed the speech engine (the model, the graphics card, Trim silences...): restart
/// it. With Trim silences on and its model missing, the model (under 1 MB) is fetched first, so the
/// one restart trims silences straight away. Local only stops the fetch, and the engine then runs
/// without trimming (Settings says why).
pub fn restart_engine_for_settings(app: Arc<App>) {
    if !wants_vad_download(&app) {
        start_engine(app);
        return;
    }
    tauri::async_runtime::spawn(async move {
        // Restarted either way: whatever else changed applies too.
        let _ = fetch_vad_model(&app).await;
        start_engine_and_wait(app).await;
    });
}

fn wants_vad_download(app: &App) -> bool {
    let s = app.settings();
    vad_needs_fetching(s.engine.vad, s.privacy.local_only, models::find_vad_model(&app.paths).is_some())
}

/// Trim silences is on, its model is missing, and Local only allows the download.
fn vad_needs_fetching(vad_on: bool, local_only: bool, installed: bool) -> bool {
    vad_on && !local_only && !installed
}

/// Downloads the silence model, one fetch at a time. Ok(true) when this call installed it,
/// Ok(false) when it was there already (another fetch got it first).
async fn fetch_vad_model(app: &Arc<App>) -> anyhow::Result<bool> {
    static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _one = ONE_AT_A_TIME.lock().await;
    if models::find_vad_model(&app.paths).is_some() {
        return Ok(false);
    }
    let quiet: models::ProgressCallback = Arc::new(|_| {});
    match models::download_model(models::VAD_ID, &app.paths, quiet).await {
        Ok(_) => Ok(true),
        Err(e) => {
            log::warn!("could not fetch the silence trimming model: {e:#}");
            Err(e)
        }
    }
}

pub fn purge_old_audio(app: &Arc<App>) {
    let days = app.settings().privacy.keep_audio_days;
    if days == 0 {
        return;
    }
    match app.db.purge_audio_older_than(days) {
        Ok(files) => {
            for f in files {
                let _ = std::fs::remove_file(f);
            }
        }
        Err(e) => log::warn!("could not tidy old recordings: {e:#}"),
    }
}

pub async fn quit(app: Arc<App>) {
    app.quitting.store(true, Ordering::SeqCst);
    // Saving the notes below can take a while, and a take started meanwhile (out of habit) was cut
    // off by the exit part-way through and never pasted. So nothing new starts now; a take already
    // running can still be finished or cancelled, as when the shortcut is paused from the tray.
    app.paused.store(true, Ordering::SeqCst);
    if let Some(h) = app.hotkeys.lock().as_ref() {
        h.pause(true);
    }
    save_work_before_quit(&app).await;
    app.pill(PillState::Hidden);
    if let Some(engine) = app.engine.write().await.take() {
        engine.shutdown().await;
    }
    app.handle.exit(0);
}

/// How long quitting waits for notes to be saved and written up.
const QUIT_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// Quitting during a screen watch threw the whole watch away (it lives only in memory until it
/// stops), and quitting mid-call lost the last piece of the call. Stop both first, then wait (up to
/// QUIT_WAIT) until their notes are saved and written up, along with any write-up already under way
/// (meeting::busy counts them all). The engine is still running, so the call's last piece is
/// transcribed. A write-up the AI hasn't finished by then is closed off next start
/// (meeting::recover), and its note offers Write again.
async fn save_work_before_quit(app: &Arc<App>) {
    let watching = watch::is_watching();
    let taking_notes = app.meeting.lock().is_some();
    if !watching && !taking_notes && !meeting::busy(app) {
        return;
    }
    if let Some(w) = app.handle.get_webview_window("main") {
        let _ = w.hide();
    }
    if taking_notes {
        if let Err(e) = meeting::stop(app) {
            log::warn!("quit: {e:#}");
        }
    }
    if watching {
        if let Err(e) = watch::stop(app) {
            log::warn!("quit: {e:#}");
        }
    }
    // After the stops, so their own notices (and hide timers) don't replace this one.
    app.show_pill_window(0);
    app.pill(PillState::Notice { message: "Saving your notes, then Navi will close.".into() });
    let by = std::time::Instant::now() + QUIT_WAIT;
    while std::time::Instant::now() < by && meeting::busy(app) {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    if meeting::busy(app) {
        log::warn!("quit: notes still being written after {} s; they are closed off next start", QUIT_WAIT.as_secs());
    } else {
        log::info!("quit: saved the notes in progress");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Mode, ModeKey, ModeKind, Settings};

    fn modes_with(custom: &[(&str, ModeKind)]) -> Vec<Mode> {
        let mut all = text::modes::builtin_modes();
        for (id, kind) in custom {
            all.push(Mode { id: id.to_string(), name: id.to_string(), kind: kind.clone(), builtin: false, ..all[0].clone() });
        }
        all
    }

    fn key(mode: &str, k: &str) -> ModeKey {
        ModeKey { mode: mode.into(), keys: vec![k.into()] }
    }

    #[test]
    fn trim_silences_fetches_its_model_unless_local_only() {
        assert!(vad_needs_fetching(true, false, false), "turned on without its model: fetch it now");
        assert!(!vad_needs_fetching(true, true, false), "Local only stops the download");
        assert!(!vad_needs_fetching(true, false, true), "already there");
        assert!(!vad_needs_fetching(false, false, false), "turned off");
    }

    #[test]
    fn a_deleted_modes_key_and_choices_are_forgotten() {
        let mut s = Settings::default();
        s.hotkeys.mode_keys = vec![key("auto", "F1"), key("brief", "F6"), key("email", "F2"), key("next", "F12")];
        s.modes.active = "brief".into();
        s.modes.default_mode = "brief".into();
        assert!(forget_missing_modes(&mut s, &modes_with(&[]), &[]));
        let left: Vec<&str> = s.hotkeys.mode_keys.iter().map(|k| k.mode.as_str()).collect();
        assert_eq!(left, ["auto", "email", "next"], "Auto, Next and modes that exist keep their keys");
        assert_eq!(s.modes.active, "", "back to Auto");
        assert_eq!(s.modes.default_mode, "clean");
    }

    #[test]
    fn nothing_changes_while_every_mode_is_there() {
        let mut s = Settings::default();
        s.hotkeys.mode_keys = vec![key("brief", "F6"), key("clean", "F1")];
        s.modes.active = "brief".into();
        assert!(!forget_missing_modes(&mut s, &modes_with(&[("brief", ModeKind::Rewrite)]), &[]));
        assert_eq!(s.hotkeys.mode_keys.len(), 2);
        assert_eq!(s.modes.active, "brief");
    }

    #[test]
    fn a_notes_mode_cannot_be_switched_to_so_its_key_goes() {
        let mut s = Settings::default();
        s.hotkeys.mode_keys = vec![key("notes", "F7"), key("my-notes", "F8")];
        assert!(forget_missing_modes(&mut s, &modes_with(&[("my-notes", ModeKind::Notes)]), &[]));
        assert!(s.hotkeys.mode_keys.is_empty());
    }

    #[test]
    fn a_stored_mode_navi_cannot_read_keeps_its_key_and_choices() {
        let mut s = Settings::default();
        s.hotkeys.mode_keys = vec![key("from-newer-navi", "F9")];
        s.modes.active = "from-newer-navi".into();
        s.modes.default_mode = "from-newer-navi".into();
        assert!(!forget_missing_modes(&mut s, &modes_with(&[]), &["from-newer-navi".to_string()]));
        assert_eq!(s.hotkeys.mode_keys.len(), 1);
        assert_eq!(s.modes.active, "from-newer-navi");
        assert_eq!(s.modes.default_mode, "from-newer-navi");
    }

    #[test]
    fn interview_me_and_speak_fall_back_to_auto_even_with_a_saved_edit() {
        let db = store::Db::open_memory().unwrap();
        // An edit of each, saved while they were built-ins.
        for id in ["interview", "speak"] {
            let json = serde_json::to_string(&Mode { id: id.into(), name: id.into(), ..text::modes::builtin_modes()[1].clone() }).unwrap();
            db.conn.lock().execute("INSERT INTO modes (id, source, json) VALUES (?1, 'manual', ?2)", [id, json.as_str()]).unwrap();
        }
        let (modes, unreadable) = modes_for_tidy(&db).unwrap();
        assert!(!modes.iter().any(|m| m.id == "interview" || m.id == "speak"), "the saved edits are ignored");
        assert!(unreadable.is_empty());
        for id in ["interview", "speak"] {
            let mut s = Settings::default();
            s.hotkeys.mode_keys = vec![key(id, "F4"), key("email", "F2")];
            s.modes.active = id.into();
            s.modes.default_mode = id.into();
            assert!(forget_missing_modes(&mut s, &modes, &unreadable), "{id}");
            assert_eq!(s.modes.active, "", "{id}: the active mode goes back to Auto");
            assert_eq!(s.modes.default_mode, "clean", "{id}: the default goes back to the one Navi ships with");
            assert_eq!(s.hotkeys.mode_keys.iter().map(|k| k.mode.as_str()).collect::<Vec<_>>(), ["email"], "{id}: its key goes");
        }
    }

    #[test]
    fn the_tidy_up_lists_unreadable_modes_by_id() {
        let db = store::Db::open_memory().unwrap();
        db.save_mode(&Mode { id: String::new(), name: "Brief".into(), builtin: false, ..text::modes::builtin_modes()[0].clone() }).unwrap();
        db.conn.lock().execute("INSERT INTO modes (id, source, json) VALUES ('newer', 'manual', '{\"kind\":\"somethingNew\"}')", []).unwrap();
        let (modes, unreadable) = modes_for_tidy(&db).unwrap();
        assert!(modes.iter().any(|m| m.id == "brief"));
        assert_eq!(unreadable, ["newer"]);
    }

    #[test]
    fn the_tray_names_the_first_wake_word() {
        let mut vc = types::VoiceCommandSettings::default();
        assert_eq!(listen_label(&vc), "Listen for \u{201c}Navi\u{201d}");
        vc.set_phrases("wake", &["Hey"]);
        assert_eq!(listen_label(&vc), "Listen for \u{201c}Hey\u{201d}");
    }

    #[test]
    fn the_start_up_failure_names_the_cause_with_a_capital() {
        let e = anyhow::anyhow!("This data was saved by a newer version of Navi").context("preparing the database navi.db");
        assert_eq!(startup_failure_text(&e), "Preparing the database navi.db: This data was saved by a newer version of Navi");
    }
}
