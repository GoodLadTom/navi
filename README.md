# Navi

Hold a shortcut, talk, and let go. Your words appear wherever you were typing, tidied up and
spelt the British way. Navi runs speech recognition on your own computer, so what you say is
never uploaded to be transcribed.

Free and open source under the GNU General Public License, version 3 (see [Licence](#licence)).
The source is on GitHub in a private repository for now, so only people with access can read it. Navi runs on Windows. A Mac version is planned
([docs/MAC-PORT.md](docs/MAC-PORT.md)) but not built yet.

## What it does

- **Talk anywhere.** Hold Ctrl + Win (or one key of your choice, such as Right Ctrl or Caps Lock)
  in any app, speak and let go. Double-tap for hands-free, then press Enter or the shortcut to
  finish; Esc cancels. Windows won't let Navi type into an app running as administrator, so there
  the text is left on your clipboard instead.
- **Voice commands.** Turn them on and talk to Navi without touching the keyboard. Say "Navi"
  (or "Hey Navi") and keep talking, and Navi types what you say; it finishes after six seconds of
  quiet, when you say "that's all", or when you press Enter. Say the wake word and a command and it
  happens straight away: "Navi, watch my screen", "Navi, take notes", "Navi, scratch that",
  "Navi, turn gold". Say
  "don't worry" on its own during a take to throw it away. Every phrase can be changed in Settings >
  Voice commands, to any words you like, and a command with no phrases left is off, with the
  shortcut held too. The listening happens on your computer, no AI is involved, and nothing it
  hears is kept.
- **Modes at your fingertips.** Switch between Raw, Clean, Message, Email, Prompt and your own
  modes from the Modes page, the tray, or a key per mode (F1 to F5, say). The bubble shows which
  mode you are on. Settings refuses a mode key that would clash with your talk shortcut or
  another mode's key, including when you press Use F1 to F5.
- **Ramble mode.** Tap Space while holding the shortcut. Talk for as long as you like and press
  Enter when you are done. Your AI turns it into notes with a title, summary, key points,
  decisions and tasks (with no AI, Navi makes rough notes on your computer). If the AI fails, Navi
  saves rough notes and says why, in the bubble and at the top of the notes; press Write again on
  the Notes page to have them written up. They're kept on the Notes page and as Markdown, and the
  tasks can go to ClickUp or Notion.
- **Notes from your calls.** When Teams, Zoom, Google Meet, Slack, Discord, WhatsApp or another
  call app starts using your microphone, Navi offers to take notes. It writes down both sides,
  marked You and Them, while you type your own notes. Your AI then turns it all into a summary,
  key points, decisions and actions, and the actions can go to ClickUp or Notion. The audio is
  never kept.
- **Jobs on calls.** Say "Navi, draft the follow-up email" on a call (with a pause after the name)
  and your AI writes it while you talk. It only writes: nothing is sent or
  booked. Turn on call screenshots and Navi also looks at the screen every 15 seconds and keeps a
  screenshot when it has changed, so the notes can say what was shown; click one to see it full
  size. Screenshots of Navi itself, and black ones from apps that block capture, are skipped.
- **Watch my screen.** Say "Navi, watch" (with the shortcut held, or with listening switched on),
  work, then say "Navi, stop watching". The tray and the Notes page can start and stop it too.
  Navi looks at the screen every 10 seconds, keeps a screenshot when it has changed and stops
  after an hour. Your AI then keeps up to 20 that matter and writes up what you did (with no AI,
  every screenshot stays, listed by time). If the AI failed or wasn't on, press Write again on the
  Notes page to have it written up later; the screenshots it leaves out are then deleted.
  Screenshots stay on your computer; the AI only gets the text read off them and the app and window
  names (only the app's while "Read the window title" is off).
- **Built for talking to AI agents.** In a terminal, VS Code, Cursor, Windsurf, BB, the Claude,
  ChatGPT and Codex apps, or a browser tab on ChatGPT or Claude, Navi picks Prompt mode. It turns
  what you say into a clear brief for an AI tool such as Claude Code or ChatGPT, with `@file`
  references to files in a project folder you've added. With no AI it adds a "Relevant files:"
  line instead of rewriting. A browser tab counts by its title or, once ChatGPT has renamed the
  tab after your chat, by the site in the address bar.
- **Learns from your corrections.** Fix a word after Navi pastes it and it notices (this needs
  "Read the text near my cursor", which is on unless you turn it off, and never happens in apps
  under "Never read these apps"). Fix the same word twice and Navi adds it to your dictionary. A
  fix to an everyday word waits on the Dictionary page as a suggestion instead, so it can't change
  that word in everything you say.
- **Edit by voice.** "Navi, scratch that." "Navi, make that shorter." "Navi, change Tuesday to
  Wednesday." These work on what Navi typed into the same window in the last ten minutes, once
  Navi has read the text before your cursor and found its words still there. Where it can't read
  that text (in a terminal, in an app that doesn't share it, or with "Read the text near my
  cursor" off), they only work on what Navi typed in the last 15 seconds with no key pressed since,
  so your own typing is never selected over. Select text and say "Navi, turn this into bullet
  points", or "Navi, what's wrong with this?" for a blunt list of its problems in a card you can
  copy from.
- **Writes like you.** British spelling and no em dashes (both on unless you turn them off), and a
  style profile that every AI rewrite follows.
- **Six colours.** Lilac, Gold, Teal, Sky, Rose or Graphite, each in light and dark. Pick one in
  Settings > General, or say "Navi, turn gold" with the shortcut held or listening switched on
  ("change to teal", "switch to rose" and "make it sky" work too). The window, the bubble, the
  firefly, the card in the corner and the tray icon change together. Navi's own icon on the
  taskbar stays lilac.
- **Knows where you are typing.** Navi reads the app and the text near your cursor on your
  computer, which helps it pick the right mode and spell names correctly. In a browser it also
  looks at the address bar and keeps only the site's name, such as chatgpt.com, to pick the mode;
  that is never saved or sent anywhere. When an AI mode runs, your words go to your AI with the
  app's name, the window title, up to 1,000 characters before the cursor, the name in Settings >
  General, your style notes and any dictionary words you used. In Prompt mode the paths of up to
  60 files from the matching project folder go too. You can stop Navi reading the window title or
  the text near your cursor in Settings > Context and privacy. From apps under "Never read these
  apps", Navi takes nothing but the app's name.
- **Keeps your takes.** Every take is kept with its text, and its recording for 30 days unless you
  choose 7 or 90 days or for ever. While the recording is there you can play it back or re-run
  it through another mode; copying the text works at any time. Turn Save history off and your
  takes and their recordings aren't kept. Notes are documents you asked for, so they are still
  saved on the Notes page and as Markdown: rambles and call notes with what was said, and screen
  watches with their screenshots.
- **Packs.** Share your words, snippets and modes with your team as one file.

Sound cues: ten sets of soft, low sounds (Bubble, Hush, Felt, Oak, Kalimba and more)
made to be heard all day. Or use your own WAV files, or none at all.

While you talk, Navi pauses music and videos and plays them again when you finish, so you can
hear yourself. Only what Navi paused plays again: a video you had paused yourself stays paused,
even in another tab of the same browser. You can have it mute other apps instead (they come back
at the same volume), do both, or leave everything playing (Settings > Sound > While you talk).
Nothing changes while Navi takes call notes.

## Speech engine

Navi starts a [whisper.cpp](https://github.com/ggml-org/whisper.cpp) server in the background and
keeps the model loaded. With an NVIDIA graphics card it runs Whisper Large v3 Turbo (about 1.6 GB)
on CUDA; otherwise it runs Whisper Base (English only, about 150 MB) on the processor. You can
pick another model in Settings. English is the default language; the Large v3 Turbo models can
also detect 99 languages, while Base and Small understand English only.

With Settings > Speech engine > Use the graphics card turned off, Large v3 Turbo takes about 10
seconds a take on the processor (Base is quicker), and the wake word and "don't worry" or "that's
all" during a take get slower or stop working. Changing the model or the graphics card setting
restarts the engine, which takes 2 to 3 seconds on the graphics card and about 10 on the
processor. A take made meanwhile waits for it, and the bubble says so.

Set-up detects what you have and downloads what is missing: the engine from whisper.cpp's GitHub
releases and the speech model from
[huggingface.co/ggerganov/whisper.cpp](https://huggingface.co/ggerganov/whisper.cpp). Navi then
fetches a small silence-trimming model (Silero VAD, under 1 MB) from
[huggingface.co/ggml-org/whisper-vad](https://huggingface.co/ggml-org/whisper-vad) by itself,
unless Local only is on: at start, or as soon as you turn Trim silences on, so it works straight
away. Each model is checked against its known size and SHA-256. A
`whisper-server.exe` already on your PATH is used rather than downloading another.

Settings, history, recordings, screenshots, the engine and the models live in
`%APPDATA%\com.goodlad.navi`. Notes are saved to `Documents\Navi Notes` unless you choose another
folder.

## AI (optional)

Navi tidies punctuation, capitals, fillers and spelling on your computer with no AI at all, and
the AI stays off until you turn it on in Settings > AI. It's used for the rewriting modes
(Message, Email, Prompt), voice edits, ramble notes, call notes, jobs on calls, screen watch
write-ups and "what's wrong with this?". Only text is sent, never audio or pictures.

Settings offers the AI you already pay for first. If Claude Code (your Claude plan) or Codex
(your ChatGPT plan) is installed, Navi runs it for you and needs no API key. A rewrite takes a few
seconds this way, so Clean mode skips it and stays instant. You can also use an API key for
Claude (Anthropic) or OpenAI, which is usually quicker but paid per use. Or use any
OpenAI-compatible endpoint, where the key is optional.

Or run the AI on your own computer with [Ollama](https://ollama.com/download), free and with no
key: install it, get a model (`ollama pull llama3.2` in a terminal) and choose Ollama in
Settings > AI. Settings shows whether Ollama is running, lists the models it has to pick from, and
links to the download when it can't find it. Navi asks Ollama to keep the model loaded for 30
minutes after each use, so takes don't wait for it to load again. If Ollama isn't running, the
bubble says "Ollama isn't running, so Navi typed a plain tidy instead." and, for the next minute,
takes go straight to the tidy without trying it again; the Test button always tries.

Jobs nobody waits on (call notes, screen watches, jobs on calls and "what's wrong with this?") can
use a stronger model than dictation; through Claude Code that is Sonnet unless you pick another.
When the AI is skipped, the bubble says why in a few words ("Claude rejected the API key"). Keys
are stored in Windows Credential Manager.

Local only blocks AI that runs off this computer (Claude Code and Codex included), ElevenLabs,
ClickUp and Notion (their Test buttons too), installing or refreshing packs from a link, the
silence-model download and the automatic update check. It still allows engine and model downloads
you start, **Check for updates** and **Update now** when you press them, and an Ollama or custom
endpoint running on this computer.

## Voice playback (optional)

Connect an ElevenLabs key and Navi reads your words back in any voice in your account, on the Speak
page or from History. It reads up to 5,000 characters at a time.

## Building from source

Navi builds on Windows only. A Mac version is planned but not built yet (see
[docs/MAC-PORT.md](docs/MAC-PORT.md)). While the repository is private, cloning it needs access to
GoodLadTom/navi. You need:

- Rust 1.90 or newer, with the MSVC toolchain (`rustup default stable-msvc`)
- Node.js 18 or newer
- Visual Studio Build Tools with the **Desktop development with C++** workload (it includes the
  Windows SDK; the bundled SQLite is compiled with it)
- The Microsoft Edge WebView2 runtime (built into Windows 11; on Windows 10, install it from
  Microsoft if it is missing)

```
npm install
npm run dev      # run with live reload
npm run build    # unsigned installer in src-tauri/target/release/bundle/nsis
```

`npm run build` needs no keys. It makes an unsigned installer without update files; only
`scripts/release.py` signs update files, and only it needs the updater key (see Updates).

The interface is plain HTML, CSS and JavaScript in `ui/` (no build step). Open
`ui/index.html` through any local web server to preview every screen with sample data.

The installer is not code-signed, so Windows SmartScreen may say "Windows protected your PC".
Choose **More info** then **Run anyway**.

## Updates

Navi looks for a new version on this repository's GitHub releases 20 seconds after it starts and
then every six hours. Settings > About has a **Check for updates** button and a switch to turn the
automatic check off, and Local only turns it off too. When there is a new version, a card offers
it. Nothing downloads until you press **Update now**. Navi then downloads it, checks its signature
against the public key built into the app, runs the installer (a small progress window with
nothing to click) and restarts. It won't install while a take or call is being recorded, the
screen is being watched, or notes are being written up.

No release has been published yet and the repository is private, so for now the automatic check
finds nothing and **Check for updates** says "No updates have been published yet."

To publish an update:

```
python scripts/release.py 0.1.1 "What changed, in a line or two" --publish
```

Leave out `--publish` for a dry run: it builds and signs everything but uploads nothing. A dry run
still writes the new version into `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml` and
`package.json`, so publishing the same version straight after it stops with "0.1.1 isn't newer
than 0.1.1". Put those files back first
(`git checkout -- src-tauri/tauri.conf.json src-tauri/Cargo.toml package.json`), or publish a
higher version.

The script builds with `src-tauri/tauri.release.conf.json`, which turns on the signed update
files. It needs Python 3, the private signing key (by default
`%USERPROFILE%\.tauri\navi-updater.key`; set `NAVI_UPDATER_KEY` to use another path, and
`NAVI_UPDATER_KEY_PASSWORD` if the key has a password) and, to publish, the GitHub CLI (`gh`)
signed in. Keep the key safe and never commit it: without it, no installed copy can be updated.

## Credits

- Speech recognition by [whisper.cpp](https://github.com/ggml-org/whisper.cpp) (MIT, the ggml
  authors).
- OpenAI's [Whisper](https://github.com/openai/whisper) model weights (MIT), downloaded in
  whisper.cpp's format from
  [huggingface.co/ggerganov/whisper.cpp](https://huggingface.co/ggerganov/whisper.cpp).
- Silence trimming by the Silero VAD model (MIT), downloaded in whisper.cpp's format from
  [huggingface.co/ggml-org/whisper-vad](https://huggingface.co/ggml-org/whisper-vad).
- Built with [Tauri](https://tauri.app) (MIT or Apache 2.0).
- Typeface Poppins by Indian Type Foundry, under the SIL Open Font Licence 1.1
  (`ui/fonts/OFL.txt`).
- The engine warm-up clip (`ui/sounds/warmup.wav`) is a line from John F. Kennedy's 1961
  inaugural address, a US government work in the public domain, taken from whisper.cpp's
  `samples/jfk.wav`. Navi never plays it; it only primes the engine.
- Sound cues generated by `scripts/make_sounds.py`.

## Licence

Navi is free and open source under the GNU General Public License, version 3 ([LICENSE](LICENSE)).
You can use it, change it and share it, and if you share a changed version you must share its
source under the same licence. Three extra terms come with it ([NOTICE](NOTICE)):

- Keep the "Made by GoodLad" credit in the app's About section and in NOTICE.
- Give a changed version its own name, and say in its About section that it's based on Navi by
  GoodLad.
- The licence gives no rights to the Navi name, the GoodLad name, or the Navi logo and firefly.

Made by [GoodLad](https://github.com/GoodLadTom).
