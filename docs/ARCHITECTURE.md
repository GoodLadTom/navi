# Navi architecture

Navi is a Windows-first desktop dictation app (Tauri v2: Rust core, HTML/CSS/JS interface). Hold a
shortcut, speak, release: the words appear in whatever app has focus, transcribed on this machine.

## Processes and windows

- One Rust process. Windows: `main` (the app: home, history, notes, modes, speak, dictionary,
  packs, settings, onboarding), `pill` (the recording overlay: transparent, always on top, never
  focused, click-through, covering the screen's work area so the firefly can hover in its top-right
  corner while you talk) and `prompt` (the card in the bottom-right corner for meeting notes and
  for answers such as "what's wrong with this?", shown without taking the focus). All load plain
  HTML from `ui/` (no build step). A tray icon keeps Navi running when the main window is closed.
- A `whisper-server.exe` child process (whisper.cpp, CUDA build when there is an NVIDIA GPU) keeps
  the speech model loaded, so takes don't wait for it to load each time. Transcription is quick on
  an NVIDIA card and slower on the processor.

## One take, end to end

1. `hotkey` (low-level keyboard hook thread) sees the talk shortcut (default Ctrl + Win, or any
   single key such as Right Ctrl or Caps Lock) go down and sends `HotkeyEvent::TalkDown` to the
   controller. Keys other software injects (a keyboard app's macro key) count; Navi's own
   keystrokes, tagged `NAVI_INPUT_TAG`, never do. The hook reinstalls itself after a quiet minute,
   because Windows silently removes a hook that once keeps it waiting.
2. `controller` starts `audio` capture at once, captures `context` (app, window title, text near
   the cursor, selection and, in a browser, the address bar's host; for an app under "Never read
   these apps" only the app, not even its title) and picks a mode (the active mode if one is
   chosen, else from the app or site, see Modes below).
   After a short grace (110 to 180 ms) it plays the start cue (`sounds`: the chosen sound set from
   ui/sounds/sets) and shows the `overlay` pill, so a quick tap or a shortcut that merely starts
   with the talk keys ends silently. At the same moment `media` pauses what's playing (Windows'
   media sessions) or mutes other apps' audio sessions, as Settings > Sound > While you talk says
   (Pause by default), and puts back only what it changed once the take stops recording. Muting
   sets each session's mute flag and never its volume, Navi's own sounds are never muted, and
   nothing changes during call notes. What it muted is kept in `media-muted.json` until it's
   unmuted, so the next start can unmute it after a crash.
3. On `TalkUp` (or, in hands-free, the next press of the talk keys or Enter), audio stops. The
   audio recorded while the start cue played (at most about 0.1 s: the cue's length, capped at
   160 ms, less its last 60 ms) is cut out so speaker bleed never becomes text; anything said
   before the cue is kept. Nothing is cut in a wake-word take, with sounds off, or when Settings >
   Microphone > Ignore the start cue is off. `engine.transcribe` then runs with a vocabulary
   prompt from the dictionary.
4. `text::voice::parse` checks for a wake-word command ("Navi, scratch that"). A rewrite or replace
   only runs when Navi typed something into this window in the last ten minutes; otherwise the words
   are dictation ("Hello Navi, this is my first note"). Then `text::pipeline::run` applies spoken
   punctuation, snippets, the dictionary, offline cleanup, the optional AI step for the mode
   (`text::llm`, which hands Claude Code and Codex to `text::cli_ai`), the no-em-dash and British
   spelling rules, then the dictionary again.
5. `insert` pastes into the app that has the focus when the text is ready (clipboard snapshot,
   Ctrl+V, restore; or typed, as Settings choose). In a window running as administrator Windows
   blocks the keystrokes, so the text is left on the clipboard. Unless Save history is off, the
   take is saved (`store::history` + a WAV in the audio folder, deleted after
   `privacy.keepAudioDays`, 30 by default). The pill shows done, the done cue plays.
6. Later (25 seconds after pasting, or at the next take), `context::read_focused_text` reads the
   field again and `text::learn::find_corrections` spots words the user fixed. Each take's timer
   looks only at that take (`learning_due`), so a newer paste is never checked early by an older
   timer. A fix seen twice is added to the dictionary by itself when `text::learn::can_auto_add`
   allows it (the misheard word isn't everyday English); otherwise it waits as a suggestion on the
   Dictionary page. This needs "Read the text near my cursor" on, and `read_focused_text` never
   reads a window whose app is under "Never read these apps" (checked when it reads).

Modes (`text::modes`): the built-ins (Raw, Clean, Message, Email, Prompt, and Notes for rambles)
come from the code; the `modes` table only holds custom modes and the user's edits of built-ins.
On Auto, `pick_mode` takes the first of: a mode listing the app's process; Prompt for a browser on
chatgpt.com, chat.openai.com or claude.ai; then by category (email apps and browsers titled Gmail
or Outlook get Email, chat apps Message, terminals and browsers titled ChatGPT or Claude Prompt);
else the default mode. Prompt lists Windows Terminal, VS Code, Cursor, Windsurf, BB, the Claude,
Codex and ChatGPT apps, PowerShell and Command Prompt. The host comes from the browser's address
bar, read with UI Automation on a worker of its own while the field is read; the bar is found once
per window (Chromium's omnibox, or Firefox's `urlbar-input`) and kept, and a take waits at most
150 ms for it before the title alone decides. Only the host is kept, and it is never logged,
saved or sent to the AI. Edits of "Interview me" and Speak, built-ins Navi no longer has, stay in
the table unread; an active mode that was one of them goes back to Auto, a default one to Clean,
and their mode keys go (`forget_missing_modes`). A Prompt mode the user edited before Codex and
ChatGPT joined its list gets them once (`store::modes::add_new_prompt_apps`, marked done in
`meta`).

Mode keys (HotkeySettings::mode_keys) fire `HotkeyEvent::ModeKey(n)`; `controller::switch_mode`
saves the active mode, ticks the tray and shows "Mode: Email" in the bubble (or switches a take
being recorded hands-free).

Ramble (Space while holding the chord, with its own sound) records hands-free in the Notes mode.
Every 12 seconds it transcribes the last 30 seconds, only for the live preview in the bubble (the
`ramble-partial` event). When it ends, the whole recording is transcribed once and `text::ramble`
turns it into notes (title, summary, key points, decisions and tasks; rough offline notes when
there is no AI or no reply within 90 seconds), saved as Markdown, kept on the Notes page
(`meeting::save_ramble`) and optionally sent to ClickUp (the tasks) or Notion (the note without its transcript)
(`integrations`). A hands-free take or a ramble finishes on Enter (`HotkeyEvent::Finish`, swallowed)
or on the talk shortcut.

Meeting notes (`meeting`): a watcher reads which apps are using the microphone (the registry key
behind Windows' privacy light, every 3 seconds) and offers notes on the `prompt` card when a call
app starts. A meeting records two tracks, the microphone and the speakers in WASAPI loopback (an
input stream on the output device), and transcribes both every 20 seconds, cutting at a quiet
moment. Loopback sends nothing while the computer is silent, so the gap is filled with silence to
keep each line's time. Lines are marked You or Them, and microphone lines that echo the other side
are dropped. On stop (or 20 seconds after the call app lets go of the microphone, or after 20
minutes with nothing said when there is no call app to follow) the AI writes the notes around what
you typed; with no AI the note keeps the transcript and your own notes. They are saved in the
`notes` table (`store::notes`, schema version 2) and as Markdown in the notes folder. The audio is
never kept.

Along the way, as Settings > Meeting notes allows:
- Jobs (`jobs`): a line from your side that opens with the wake word ("Navi," with a pause by
  default, or "Hey Navi"; a greeting and the name always count) and carries an instruction of three
  or more words goes straight to the AI for longer jobs with the call so far; the answer is kept
  with the note. The AI only writes.
- Screenshots (`screen`): every 15 seconds a look at the screen, kept when enough of it changed,
  its text read with Windows OCR. Only that text, the app's name and (while "Read the window
  title" is on) the window's title go to the AI writing the notes.
- Actions: the bullets under the notes' Actions heading go to ClickUp or Notion.
Jobs and screenshots live in the note's `extras` JSON (schema version 3); the Short ideas that
notes saved by older versions keep there are skipped when read. Jobs can finish
after the call, so a note's jobs are always read, merged and saved under `NOTE_LOCK`, and a finished
job is never overwritten by an older copy that still says "working".

Voice commands (`wake` and `spoken`, off by default; Settings > Voice commands). Every action has
a list of phrases the user can change: wake ("Navi", "Hey Navi"), scratch that, watch my screen,
stop watching, take notes, stop taking notes, cancel ("don't worry"), finish ("that's all") and
change colour ("turn", "change to", "switch to", "make it"). A colour phrase only counts with a
colour after it: `Matcher` adds each colour's words from `colours` (its name, plus yellow, green,
blue, pink, purple, grey and a few more) and hears them exactly, so "make it Greek" is never heard
as green.
`spoken.rs` holds the logic as plain state machines (a segmenter, the phrase `Matcher`, an
`IdleListener` and a `TakeListener`); `wake.rs` and `controller.rs` are thin shells around them,
and the unit tests and the live measurement run the same code.

- Idle: a microphone stream of its own feeds a ring at 16 kHz. While an utterance goes on, the
  whisper server checks it every 0.3 s from 0.4 s in (one check at a time; the wake phrases go in
  the first check's prompt only, so Whisper never echoes them). The wake word must open the
  utterance. On its own it starts a hands-free take with the audio heard so far prepended; followed
  by a command ("Navi watch my screen", in one breath or after a short pause), the command runs at
  once with no take. Sliding checks switch off while the engine is slow (another app on the GPU).
- During a take: `controller::spawn_take_listener` reads the take's audio and checks short
  utterances between pauses. Cancel and finish count only as a whole utterance with half a second
  of quiet after it, so "don't worry about the deadline" is dictation; finish cuts the phrase off
  before transcribing. On a long take or a ramble, the first cancel asks to be said again.
- On calls the listener only acts on stop taking notes, watch my screen and stop watching; "Navi,
  ..." on a call is still a job.
- A wake take ends after six seconds of quiet (`WAKE_QUIET`), on the finish phrase, Enter or the
  talk shortcut, or after two minutes (`WAKE_MAX`). It pauses while Navi reads aloud, so it never
  wakes itself. Measured on the owner's PC (18 Oct 2026, generated speech): the bubble about 0.3 s
  after the wake word, one-breath commands in 0.2 to 0.3 s, cancel and finish about 0.5 s after the
  phrase.

Colours (`colours`): Lilac (the default), Gold, Teal, Sky, Rose and Graphite, saved as
`general.colour`. `ui/css/tokens.css` holds each palette for light and dark, and `ui/js/theme.js`
puts `data-colour` on the root of every window. Picking one in Settings or saying "Navi, turn gold"
saves it and emits `settings-changed`, so the bubble, its firefly and the corner card change at
once; the voice command shows "Colour: Gold" in the bubble and types nothing. The logo in the
sidebar and on the welcome screen is drawn in CSS: the firefly from one mask
(`ui/img/firefly-mark.png`) on a tile in the colour. `colours::tray_icon` redraws
`src-tauri/icons/tray.png` in the colour's tile and mark, at start-up and, through the tray's
`set_icon`, whenever the colour changes; the installed app icon on the taskbar stays lilac.

"Watch my screen" (`watch`): started by "Navi, watch" (said with the shortcut or to the wake-word
listener), the tray or the Notes page. Every 10 seconds by default (Settings: 5, 10, 20 or 30) it
looks at the screen and keeps a screenshot when enough of it changed, until "stop watching" (or an
hour). The AI for longer jobs then gets each screenshot's time, app, window title (kept only while
"Read the window title" is on) and OCR text (never the picture), picks up to 20 that matter with
captions and writes a short account; the rest are deleted. Without an AI every screenshot stays,
listed by time and window. The result is a note
whose app is "Watch". Screenshots live in the app data folder (`shots/<note id>/`), which the
asset protocol may read, so the interface shows them with `convertFileSrc`.

"What's wrong with this?" (`VoiceCommand::Critique`): the selection (or the last insertion) goes to
the AI with a prompt that asks for flaws, never praise, and the answer appears on the `prompt`
card (`PromptState::Answer`) to read and copy. It runs in the background so the next take is never
held up.

Updates (`update`): tauri-plugin-updater reads `latest.json` from the latest GitHub release of
GoodLadTom/navi (20 seconds after start, then every six hours, unless Settings or Local only says
no; Settings > About can always check). That needs a published release anyone can download: the
repository is private and has none yet, so the URL returns 404 and a check reports "No updates have
been published yet." Nothing downloads until the user presses Update now; the installer must match
the minisign public key in tauri.conf.json and runs in passive mode (a progress window, nothing to
click). A plain `npm run build` makes no update files (tauri.conf.json sets
`createUpdaterArtifacts` to false) and needs no key. `scripts/release.py` bumps the version in
tauri.conf.json, Cargo.toml and package.json, builds with `src-tauri/tauri.release.conf.json`
(which turns the update files on) and the private key (`NAVI_UPDATER_KEY`, by default
`%USERPROFILE%\.tauri\navi-updater.key`), writes `latest.json`, and with `--publish` creates the
GitHub release. A dry run (no `--publish`) still bumps the version files, so publishing the same
version afterwards needs them reset first. Updates never install while a take or a meeting is
recording, the screen is being watched, or notes are being written up.

## Where data lives

`paths` puts the app's files under the per-user app data folder, `%APPDATA%\com.goodlad.navi` (the
app identifier): `settings.json`, the SQLite database `navi.db` (which also holds installed packs:
the `packs` table, plus words, snippets and modes tagged with the pack id), the take recordings
(`audio/`), the speech models (`models/`), the downloaded engine (`engine/`) and screenshots
(`shots/<note id>/`). It also creates `logs/` and `packs/` there, but nothing writes to them. The
log is `%LOCALAPPDATA%\com.goodlad.navi\logs\Navi.log` (tauri-plugin-log's default targets), and
the WebView2 profile is in `%LOCALAPPDATA%\com.goodlad.navi` too. Notes default to
`Documents\Navi Notes`. Installs from before the identifier changed keep using their old data
folder beside it (`com.navaigate.navi`), because the database stores absolute paths
(`paths::choose_data_dir`). API keys and tokens are in the Windows Credential Manager under the
service "Navi", never in these files.

## Module map

The app's Rust code is in `src-tauri/src`, plus `src-tauri/build.rs` and the
`src-tauri/examples/card_check.rs` helper. The doc comment at the top of each module file (every
file but main.rs) describes it.

| Area | Files |
|---|---|
| App setup, tray, shared state and types | lib.rs, main.rs, state.rs, types.rs, paths.rs, settings.rs, colours.rs |
| Takes and the interface's commands | controller.rs, commands.rs |
| Windows systems | hotkey.rs, insert.rs, overlay.rs, context.rs |
| Audio and the speech engine | audio.rs, sounds.rs, media.rs, engine.rs, models.rs |
| Text | text/*.rs (pipeline, cleanup, britishise, spans, modes, voice, learn, ramble, agent, llm, cli_ai; testkit for tests), integrations.rs |
| Calls, voice and screen | meeting.rs, jobs.rs, screen.rs, watch.rs, wake.rs, speak.rs |
| Updates | update.rs |
| Storage | store/*.rs (db, history, notes, dictionary, modes, packs, secrets) |
| Interface | ui/** (plain HTML, CSS and JavaScript; ui/js/mock.js supplies sample data in a browser) |

## Rules

- British English in everything the user reads. No em dashes in UI strings.
- Never log API keys, tokens, prompts, transcripts, window titles or anything else the user said,
  at any level. An error whose message quotes their words (`Couldn't find "Tuesday" in what you
  just said.`) is a `controller::SaidError`: the bubble shows the message, the log only its
  description (`for_log`). The speech engine's output is kept for the log without the segments
  some whisper.cpp builds print as they transcribe (`engine::is_transcript_line`).
- What goes to the AI with a take's words: the app's name, the window title (Settings > Context
  and privacy > Read the window title), up to 1,000 characters before the cursor (Read the text
  near my cursor), the user's name (Settings > General), the style notes, the dictionary terms
  used in the take, the mode's own instructions and, in Prompt mode, up to 60 file paths from the
  project folder the window title matches (or the active project). An app under "Never read these
  apps" gives only its name: no title, field text, selection, address, OCR, screenshots or
  learning from corrections.
- Nothing leaves the machine except the update check and update download (GitHub), the
  silence-model download (Hugging Face, tried at each start until it's there), engine and model
  downloads the user starts (GitHub and Hugging Face), packs installed or refreshed from a link,
  and the AI provider, ElevenLabs or integrations the user turned on (with their Test buttons).
  `privacy.localOnly` blocks all of those, the ClickUp and Notion Test buttons included, except
  engine and model downloads the user starts, a manual "Check for updates" and "Update now", and
  an Ollama or custom endpoint whose address is this machine (localhost, 127.x or ::1).
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
  from the keyboard even when starting a take is slow (a Bluetooth microphone waking up). A
  quick release followed by a press less than 45 ms later is a key bounce while the fingers settle
  on the chord, not a double tap, so the take carries on as hold-to-talk; every real double tap and
  every ignored bounce is logged with its timings.
- Every take logs its steps at info level (started, stopped with speech seconds and peak level,
  words heard, typed or dropped and why), never the words themselves.
- The wake-word listener never logs or keeps what it hears. Screenshots never leave the computer:
  the AI only ever gets the text read off them, the app's name and, while "Read the window title"
  is on, the window's title (`screen::capture` keeps no title otherwise). Apps under "Never read
  these apps" are never captured.
