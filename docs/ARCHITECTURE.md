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
   (Pause by default), and puts back only what it changed once the take stops recording. It keeps
   the media sessions it paused themselves, not their app ids (a browser's tabs share one), so a
   tab the user had paused stays paused; a COM usage hold (`CoIncrementMTAUsage`) keeps their
   apartment open until they are let go. Muting sets each session's mute flag and never its
   volume, Navi's own sounds are never muted, and nothing changes during call notes. What it muted
   is kept in `media-muted.json` until it's unmuted, so the next start can unmute it after a crash.
3. On `TalkUp` (or, in hands-free, the next press of the talk keys, or Enter if Settings allows), audio stops. The
   audio recorded while the start cue played (at most about 0.1 s: the cue's length, capped at
   160 ms, less its last 60 ms) is cut out so speaker bleed never becomes text; anything said
   before the cue is kept. Nothing is cut in a wake-word take, with sounds off, or when Settings >
   Microphone > Ignore the start cue is off. `engine.transcribe` then runs with a vocabulary
   prompt from the dictionary. A restart (`start_engine_and_wait`: a new model, the graphics card
   switched, the silence model fetched) takes the old engine away at once, and the new one is
   there only once warmed up (2 to 3 seconds on the graphics card, about 10 on the processor), so
   `controller::engine_for_take` waits up to 30 seconds for an engine on its way (a start under
   way, or a status that isn't missing-model, missing-engine or error), the bubble saying
   "Waiting for the speech engine to start…" every 3 seconds, as call notes hold their audio.
4. `text::voice::parse` checks for a wake-word command ("Navi, scratch that"), given the user's
   voice commands: Scratch that, Watch my screen and Stop watching are off there too when their
   phrase list is empty, and their words are then dictation. A rewrite or replace only runs when
   Navi typed something into this window in the last ten minutes; otherwise the words are
   dictation ("Hello Navi, this is my first note"). Before an edit selects back over what Navi
   typed, `controller::select_back_check` looks at the text before the caret (as the take read it,
   and again just before the keys): Navi's words must still end there. A field that can't be read
   (reading off, an app that doesn't say, or a terminal) has nothing to compare, so only an insert
   from the last 15 seconds (`BLIND_EDIT_WINDOW`) with no key pressed since counts. The hook keeps
   when a key last reached an app (`hotkey::keys_pressed_since`: not Navi's own keystrokes, not
   keys it swallowed, not a lone modifier or a media key); mouse clicks are not seen, and the
   second look after the AI checks keys only. Then `text::pipeline::run` applies spoken
   punctuation, snippets, the dictionary, offline cleanup, the optional AI step for the mode
   (`text::llm`, which hands Claude Code and Codex to `text::cli_ai`), the no-em-dash and British
   spelling rules, then the dictionary again. When the AI step fails, the words get the offline
   tidy and the bubble names why from the error's first sentence (`controller::ai_fallback_note`:
   "Ollama isn't running, so Navi typed a plain tidy instead.").
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

Mode keys (HotkeySettings::mode_keys) and action keys (HotkeySettings::action_keys) are the hook's
"extras", numbered mode keys first (`extra_key_chords`): each fires `HotkeyEvent::ExtraKey(n)`.
For a mode key `controller::switch_mode` saves the active mode, ticks the tray and shows "Mode:
Email" in the bubble (or switches a take being recorded hands-free). Settings checks every mode key
with `hotkey::check_mode_key` (the `check_mode_key` command) before saving it: by hand, and for each
key of Use F1 to F5, against the mode keys as they will be (its `others`), and against the action
keys; one refused leaves them all as they were. Action keys are checked the same way
(`check_action_key`), against every other key.

An action key (`controller::action_key`) does what a voice command does, without the voice:
- **ask**: a hands-free take (`Recording::ask`) that finishes 2 s after you stop talking, or with a
  tap of the key or the talk keys. The selection is read when it starts. `run_ask` sends what was
  said and the selection to the AI for longer jobs (`voice::ask_messages`), which replies in
  `<edit>` (pasted over the selection; "scratch that" puts it back) or `<answer>` (shown in the
  middle of the screen as `PromptState::Reply`, short until clicked, then `expand_reply` makes it
  bigger). With nothing selected it always answers. Both go in History.
- **rewrite** (with its own instruction, or `DEFAULT_REWRITE`), **scratch_that** and **critique**
  claim the session as a take does, then run `run_command` on the selection or the last insertion,
  with nothing saved to History.
- **watch** and **notes** toggle, through `run_spoken`.
Where the field can't be read, the rewrite, critique and ask keys copy the selection with Ctrl + C
(Command + C) and put the clipboard back (`copied_selection`), but never in a terminal, a code
editor (which copies the whole line when nothing is selected) or an app under "Never read these apps".

Whenever the AI works on a take or a key, the bubble shows `PillState::Working` ("AI is rewriting",
"AI is working on it", "AI is writing" for an AI mode) with a clock, and goes back to it after a
"Still finishing" note (`Session::working`).

Ramble (Space while holding the chord, with its own sound) records hands-free in the Notes mode.
Every 12 seconds it transcribes the last 30 seconds, only for the live preview in the bubble (the
`ramble-partial` event); only those 30 seconds are copied (`audio.tail_16k`), however long the
ramble. When it ends, the whole recording is transcribed once and `text::ramble`
turns it into notes (title, summary, key points, decisions and tasks; rough offline notes when
there is no AI or no reply within 90 seconds), saved as Markdown, kept on the Notes page
(`meeting::save_ramble`) and optionally sent to ClickUp (the tasks) or Notion (the note without its transcript)
(`integrations`). When the AI was on and failed, the rough notes carry
`ramble::rough_notes_notice` under the title and date (the AI's error and how to try again), and
the bubble says "Saved rough notes" with the reason instead of "Saved to Notes". Rambles go to the
Notes page and the notes folder even with Save history off: notes are documents the user asked
for, and only the take in History follows that setting. Write again on the Notes page
(`meeting::rewrite_ramble`) writes a ramble's notes again from what was said, with the ramble
prompt and the same AI, and its Markdown file keeps the ramble layout (`meeting::ramble_file`).
A hands-free take or a ramble finishes on the talk shortcut, or on Enter (`HotkeyEvent::Finish`,
swallowed) when Settings > Shortcuts allows it (`hotkeys.enterFinishes`, off by default: people
press Enter in other apps while Navi listens).

Meeting notes (`meeting`): a watcher reads which apps are using the microphone (the registry key
behind Windows' privacy light, every 3 seconds) and offers notes on the `prompt` card when a call
app starts. An entry with a start time and no stop time means "on the microphone", but a program
that crashed or was replaced leaves its entry like that for good (each Discord version has its
own folder and entry), so `entry_in_use` ignores entries started before Windows last started and
NonPackaged entries whose program (the entry's name, with # for \) is no longer there. Without
that, notes following Discord never finished by themselves, and notes started by hand during a
Teams or Zoom call followed no app.
A meeting records two tracks, the microphone and the speakers in WASAPI loopback (an
input stream on the output device), and transcribes both every 20 seconds, cutting at a quiet
moment. Loopback sends nothing while the computer is silent, so the gap is filled with silence to
keep each line's time. On a Mac (14.2 or later, `mac::audio`) the watcher reads Core Audio's
process objects instead of the registry, with each app under its Windows name ("zoom.exe"), and
the other side comes from a process tap of every app but Navi in a private aggregate device; the
tap sends zeros through silence, so nothing needs filling. Lines are marked You or Them, and microphone lines that echo the other side
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
as green. An action whose list is emptied is off everywhere: `text::voice::parse`, which also
knows built-in ways of saying scratch that ("delete that", "undo") and watch ("start watching",
"watch off") for takes with the shortcut held, is given the settings and skips Scratch that, Watch
my screen and Stop watching when they have no phrases.
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
  utterances between pauses. Cancel and finish count only word for word, with half a second of
  quiet after them, so "don't worry about the deadline" is dictation. Cancel must be a whole
  utterance (said once or more); finish cuts the phrase off before transcribing. Finish also counts
  as the last sentence of dictation it ran on from with no pause ("...the Mac port yet? That's
  all."), so of every longer utterance the last 2.2 s are checked too (`TAIL_CHECK`); that take
  keeps its audio and the phrase, said once or more, comes off the text. On a long take or a
  ramble, the first cancel asks to be said again.
- On calls the listener only acts on stop taking notes, watch my screen and stop watching; "Navi,
  ..." on a call is still a job.
- A wake take ends after six seconds of quiet (`WAKE_QUIET`), on the finish phrase, the talk
  shortcut (or Enter, if allowed), or after two minutes (`WAKE_MAX`). It pauses while Navi reads aloud, so it never
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
listed by time and window. The result is a note whose app is "Watch". Write again on the Notes page
(`watch::rewrite`, after a confirm) writes a finished watch up the same way from the text and window
names saved with its screenshots (still never the pictures), so one the AI failed on or that was
saved with no AI can be written up; the screenshots the AI leaves out are deleted, and its title
replaces only the one Navi gave the watch. Screenshots live in the app data folder
(`shots/<note id>/`), which the
asset protocol may read, so the interface shows them with `convertFileSrc`.

AI providers (`text::llm`): Claude and OpenAI by API key, Ollama, any OpenAI-compatible server,
and Claude Code or Codex through `text::cli_ai`. Ollama is asked at http://127.0.0.1:11434 unless
Settings gives another address (Ollama listens on IPv4 only, and every new connection to
localhost tried ::1 first). Every call sends `keep_alive: "30m"`, so the model stays loaded
between takes (Ollama's own default unloads it after five minutes), and jobs someone waits on send
`think: false`, retried without it if the model refuses; `make_llm_long` (`Llm::for_long_jobs`)
leaves thinking to the model, with 2,048 more tokens of room. A server on this computer gets a
one-second connection timeout, because Windows takes about two seconds to refuse a connection to
a closed port, and one that didn't take the connection is skipped for a minute, so later takes go
straight to the offline tidy. The Settings Test button (`Llm::test`) and the status check always
ask again. Settings > AI with Ollama calls `ollama_status` (GET /api/tags): running or not, and the
models that can chat, to pick from, with a link to ollama.com/download when nothing answers. A 404
only counts as a missing model when its text names the model; otherwise it's a wrong address.
Measured on the owner's PC (20 Oct 2026, Ollama 0.40.6, llama3.2 already loaded): the status check
6 to 7 ms, the Test 21 to 60 ms, a Message, Email or Prompt rewrite 0.2 to 0.3 s, ramble notes
0.7 s; with nothing listening, the first call gave up after 1.0 s (2.0 to 2.3 s before) and the
next at once.

Claude Code and Codex in Settings (`cli_ai::detect`, then the Find it button): a card for a tool
Navi doesn't see offers Find it, never just a path box. `find_ai_tool` (`cli_ai::search`) looks
where Navi always looks, then on the PATH as the system has it now (the registry on Windows, so a
tool installed after Navi started counts; the login shell's on a Mac, whose Finder gives apps a
short one), then in installer and Node version-manager folders (npm, nvm, fnm, Volta, pnpm, Yarn,
Scoop, WinGet; Homebrew on a Mac), then in VS Code, Cursor and Windsurf extensions (Claude Code's
carries its own claude.exe). The card connects what it finds, keeping a path in Settings only when
Navi wouldn't find it anyway, and asks the tool's own status command (`claude auth status`, `codex
login status`; no model call, 0.1 to 0.2 s) whether it is signed in. If not, Sign in
(`sign_in_ai_tool`) opens a terminal window running `claude auth login` or `codex login`, and the
card checks every 3 s for three minutes. The tool in use is checked when Settings opens, and an AI
step that fails because it isn't signed in points to that button.

"What's wrong with this?" (`VoiceCommand::Critique`): the selection (or the last insertion) goes to
the AI with a prompt that asks for flaws, never praise, and the answer appears on the `prompt`
card (`PromptState::Answer`) to read and copy. It runs in the background so the next take is never
held up.

Updates (`update`): tauri-plugin-updater reads `latest.json` from the latest GitHub release of
GoodLadTom/navi (20 seconds after start, then every six hours, unless Settings or Local only says
no; Settings > About can always check). That needs a published release anyone can download: until
there is one, the URL returns 404 and a check reports "No updates have been published yet." Nothing downloads until the user presses Update now; the installer must match
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
app identifier): `settings.json` (fields older versions saved but nothing read, `version`,
`writing.styleSamples` and `speak.voiceName`, are skipped when read and dropped on the next save),
the SQLite database `navi.db` (which also holds installed packs:
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
  silence-model download (Hugging Face, tried at each start until it's there, and straight away
  when Trim silences is turned on or Local only off), engine and model
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
  from the keyboard even when starting a take is slow (a Bluetooth microphone waking up). The
  double tap's window (380 ms) closes that long after the hook saw the quick release: the
  controller's event loop waits for it (`tap_window`) and handles every event the hook saw before
  then first, so a release that reached it late never stretches the window. A press less than
  45 ms after a release is a key bounce while the fingers settle on the chord or come off it
  (`controller::press_while_recording`): after a quick first tap the take carries on as
  hold-to-talk, and in a hands-free take or a ramble it carries on rather than finishing (the
  bounce on a double tap's second tap, or while letting go after starting a ramble). Every real
  double tap and every ignored bounce is logged with its timings only.
- Every take logs its steps at info level (started, stopped with speech seconds and peak level,
  words heard, typed or dropped and why), never the words themselves.
- The wake-word listener never logs or keeps what it hears. Screenshots never leave the computer:
  the AI only ever gets the text read off them, the app's name and, while "Read the window title"
  is on, the window's title (`screen::capture` keeps no title otherwise). Apps under "Never read
  these apps" are never captured.
