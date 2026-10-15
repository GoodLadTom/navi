# Navi architecture

Navi is a Windows-first desktop dictation app (Tauri v2: Rust core, HTML/CSS/JS interface). Hold a
shortcut, speak, release: the words appear in whatever app has focus, transcribed on this machine.

## Processes and windows

- One Rust process. Windows: `main` (the app: home, history, notes, modes, speak, dictionary,
  packs, settings, onboarding), `pill` (the recording overlay: transparent, always on top, never
  focused, click-through, covering the screen's work area so the firefly can hover in its top-right
  corner while you talk) and `prompt` (the card in the bottom-right corner for meeting notes and
  for answers such as "what's wrong with this?", shown without taking the focus). All load plain HTML from `ui/` (no build step). A tray icon keeps Navi running when the
  main window is closed.
- A `whisper-server.exe` child process (whisper.cpp, CUDA build when there is an NVIDIA GPU) keeps
  the speech model loaded so each take is transcribed in well under a second.

## One take, end to end

1. `hotkey` (low-level keyboard hook thread) sees the talk shortcut (default Ctrl + Win, or any
   single key such as Right Ctrl or Caps Lock) go down and sends `HotkeyEvent::TalkDown` to the
   controller. Keys other software injects (a keyboard app's macro key) count; Navi's own
   keystrokes, tagged `NAVI_INPUT_TAG`, never do. The hook reinstalls itself after a quiet minute,
   because Windows silently removes a hook that once keeps it waiting.
2. `controller` starts `audio` capture at once, captures `context` (app, window title, text near
   the cursor, selection) and picks a mode (the active mode if one is chosen, else from the app).
   After a short grace (110 to 180 ms) it plays the start cue (`sounds`: the chosen sound set from
   ui/sounds/sets) and shows the `overlay` pill, so a quick tap or a shortcut that merely starts
   with the talk keys ends silently.
3. On `TalkUp` (or the second tap in hands-free), audio stops; the stretch up to the end of the cue
   is trimmed off the start; `engine.transcribe` runs with a vocabulary prompt from the dictionary.
4. `text::voice::parse` checks for a wake-word command ("Navi, scratch that"). A rewrite or replace
   only runs when Navi typed something into this window in the last ten minutes; otherwise the words
   are dictation ("Hello Navi, this is my first note"). Then `text::pipeline::run` applies snippets,
   dictionary, offline cleanup, the optional AI step for the mode (`text::llm`), no-em-dash and
   British spelling rules.
5. `insert` pastes into the original app (clipboard snapshot, Ctrl+V, restore) and the take is saved
   (`store::history` + a WAV in the audio folder). The pill shows done, the done cue plays.
6. Later, `context::read_focused_text` reads the field again and `text::learn::find_corrections`
   spots words the user fixed; repeated fixes are added to the dictionary automatically.

Mode keys (HotkeySettings::mode_keys) fire `HotkeyEvent::ModeKey(n)`; `controller::switch_mode`
saves the active mode, ticks the tray and shows "Mode: Email" in the bubble (or switches a take
being recorded hands-free).

Ramble (Space while holding the chord, with its own sound) records hands-free, transcribes in
chunks as it goes, then `text::ramble` turns the talk into notes and tasks, saved as Markdown, kept
on the Notes page (`meeting::save_ramble`) and optionally sent to ClickUp or Notion
(`integrations`). A hands-free take or a ramble finishes on Enter (`HotkeyEvent::Finish`, swallowed)
or on the talk shortcut.

Meeting notes (`meeting`): a watcher reads which apps are using the microphone (the registry key
behind Windows' privacy light, every 3 seconds) and offers notes on the `prompt` card when a call
app starts. A meeting records two tracks, the microphone and the speakers in WASAPI loopback (an
input stream on the output device), and transcribes both every 20 seconds, cutting at a quiet
moment. Loopback sends nothing while the computer is silent, so the gap is filled with silence to
keep each line's time. Lines are marked You or Them, and microphone lines that echo the other side
are dropped. On stop (or 20 seconds after the call app lets go of the microphone) the AI writes the
notes around what you typed. They are saved in the `notes` table (`store::notes`, schema version
2) and as Markdown in the notes folder. The audio is never kept.

Along the way, as Settings > Meeting notes allows:
- Short ideas (`ideas`): every four minutes of call, the AI for longer jobs reads the last nine
  minutes with the user's brief and suggests new Shorts, each with the exact lines to cut. When the
  call ends one more pass reads the whole transcript and returns the final list. Every quote is
  checked against the transcript and takes the time of the line it came from; anything not in the
  transcript is dropped.
- Jobs (`jobs`): a line from your side that opens with "Hey Navi" and carries an instruction goes
  straight to the AI with the call so far; the answer is kept with the note. The AI only writes.
- Screenshots (`screen`): every 15 seconds a look at the screen, kept when enough of it changed,
  its text read with Windows OCR. Only that text goes to the AI writing the notes.
- Actions: the bullets under the notes' Actions heading go to ClickUp or Notion.
Ideas, jobs and screenshots live in the note's `extras` JSON (schema version 3). Jobs can finish
after the call, so a note's jobs are always read, merged and saved under `NOTE_LOCK`, and a finished
job is never overwritten by an older copy that still says "working".

"Hey Navi" (`wake`, off by default): a microphone stream of its own feeds a ten-second ring at
16 kHz; an energy voice detector finds where each utterance starts, and its first 1.8 seconds go to
the engine. If the words open with the wake phrase, `controller::start_from_wake` starts a
hands-free take with the audio from the start of the utterance prepended (`AudioService::prepend`),
so nothing said while Navi woke up is lost; the take ends after six seconds of quiet (`WAKE_QUIET`),
or sooner on Enter. Only real commands count in such a take (`voice::parse_after_wake`), and the
wake phrase is stripped from the dictation. The listener pauses during takes, call notes, a paused
shortcut and microphone tests.

"Watch my screen" (`watch`): a screenshot whenever the screen changes, until "stop watching" (or
an hour). The AI then gets each screenshot's time, window and OCR text (never the picture), picks
the ones that matter with captions and writes a short account; the rest are deleted. The result is
a note whose app is "Watch". Screenshots live in the app data folder (`shots/<note id>/`), which the
asset protocol may read, so the interface shows them with `convertFileSrc`.

"What's wrong with this?" (`VoiceCommand::Critique`): the selection (or the last insertion) goes to
the AI with a prompt that asks for flaws, never praise, and the answer appears on the `prompt`
card (`PromptState::Answer`) to read and copy. It runs in the background so the next take is never
held up. The "Interview me" mode turns a ramble into a prompt that has the AI question you first.

Updates (`update`): tauri-plugin-updater reads `latest.json` from the latest GitHub release of
GoodLadTom/navi (20 seconds after start, then every six hours, unless Settings or Local only says
no; Settings > About can always check). The installer it downloads must match the minisign public
key in tauri.conf.json. A plain `npm run build` makes no update files (tauri.conf.json sets
`createUpdaterArtifacts` to false) and needs no key. `scripts/release.py` bumps the version, builds
with `src-tauri/tauri.release.conf.json` (which turns the update files on) and the private key
(`NAVI_UPDATER_KEY`, by default `%USERPROFILE%\.tauri\navi-updater.key`), writes `latest.json` and
publishes the release. Updates wait while a take or a meeting is recording.

## Where data lives

`paths` puts everything under the per-user app data folder, `%APPDATA%\com.goodlad.navi` (the app
identifier): `settings.json`, the SQLite database `navi.db`, the take recordings (`audio/`), the
speech models (`models/`), the downloaded engine (`engine/`), logs, packs and screenshots
(`shots/`). Notes default to `Documents\Navi Notes`. Installs from before the identifier changed
keep using their `%APPDATA%\com.navaigate.navi` folder, because the database stores absolute paths
(`paths::choose_data_dir`). API keys and tokens are in the Windows Credential Manager, never in
these files.

## Module map

All Rust code is in `src-tauri/src`; the doc comment at the top of each file describes its module.

| Area | Files |
|---|---|
| App setup, tray, shared state and types | lib.rs, main.rs, state.rs, types.rs, paths.rs, settings.rs |
| Takes and the interface's commands | controller.rs, commands.rs |
| Windows systems | hotkey.rs, insert.rs, overlay.rs, context.rs |
| Audio and the speech engine | audio.rs, sounds.rs, engine.rs, models.rs |
| Text | text/*.rs (pipeline, cleanup, britishise, spans, modes, voice, learn, ramble, agent, llm, cli_ai; testkit for tests), integrations.rs |
| Calls, voice and screen | meeting.rs, ideas.rs, jobs.rs, screen.rs, watch.rs, wake.rs, speak.rs |
| Updates | update.rs |
| Storage | store/*.rs (db, history, notes, dictionary, modes, packs, secrets) |
| Interface | ui/** (plain HTML, CSS and JavaScript; ui/js/mock.js supplies sample data in a browser) |

## Rules

- British English in everything the user reads. No em dashes in UI strings.
- Never log API keys, tokens, prompts or transcripts at info level.
- Nothing leaves the machine unless the user turned on an AI provider or an integration;
  `privacy.localOnly` blocks even those.
- Raw device events are off (`DeviceEventFilter::Always` on the Tauri builder): with tao's default
  raw keyboard registration, Windows stops calling the low-level hook while Navi's own window has
  the focus, which killed the shortcut, "Any key" and the key display inside Navi
  (tauri-apps/wry#1761).
- The meeting card (`prompt`) is shown and hidden with Win32 as a plain pop-up (`meeting::win`),
  never with tao's show/hide: tao gives every window a title-bar style and only hides it while
  drawing, and the card once appeared as an ordinary framed window with its buttons cut off.
- Everything slow runs off the UI thread; the hotkey hook callback never blocks (it waits at most
  20 ms for the shared state, then lets the key through).
- The shortcut picker's live key display (`key_monitor`) only runs while the main window is in
  front (checked against the real foreground window), never holds a take back, stops when the
  window loses focus, and lapses 45 seconds after the interface last renewed it.
- Hotkey events carry the time the hook saw them (`HotkeyMsg`), so taps and double taps are timed
  from the keyboard even when starting a take is slow (a Bluetooth microphone waking up).
- Every take logs its steps at info level (started, stopped with speech seconds and peak level,
  words heard, typed or dropped and why), never the words themselves.
- The "Hey Navi" listener never logs or keeps what it hears. Screenshots never leave the computer:
  the AI only ever gets the text read off them and the window names, and apps under "Never read
  these apps" are never captured.
