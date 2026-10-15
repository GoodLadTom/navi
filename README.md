# Navi

Hold a shortcut, talk, and let go. Your words appear wherever you were typing, tidied up and
spelt the British way. Navi runs speech recognition on your own computer, so what you say is
never uploaded to be transcribed.

Free and open source (MIT). Windows first; a Mac version will follow.

## What it does

- **Talk anywhere.** Hold Ctrl + Win (or one key of your choice, such as Right Ctrl or Caps Lock)
  in any app, speak, release. Double-tap for hands-free.
- **Just say "Navi".** Turn it on and you can start talking without touching the keyboard:
  "Navi," a short pause, and carry on. Navi types what you say and finishes after six seconds of
  quiet, or when you press Enter. Would rather say "Hey Navi", which is harder to set off by
  accident? Pick it in Settings. The listening happens on your computer and nothing it hears is
  kept.
- **Modes at your fingertips.** Switch between Raw, Clean, Message, Email, Prompt, Interview me
  and your own modes from the Modes page, the tray, or a key per mode (F1 to F5, say). The bubble
  shows which mode you are on. Interview me turns what you want into a prompt that has the AI ask
  you questions before it starts.
- **Ramble mode.** Tap Space while holding the shortcut. Talk for as long as you like and press
  Enter when you are done; Navi turns it into notes with a summary, key points and tasks, keeps
  them on the Notes page and as Markdown, and can send the tasks to ClickUp or Notion.
- **Notes from your calls.** When Teams, Zoom, Google Meet, Slack or Discord starts a call, Navi
  offers to take notes. It writes down both sides, marked You and Them, while you type your own
  notes, then turns it all into a summary, decisions and actions, which can go to ClickUp or
  Notion. The audio is never kept.
- **Ideas for Shorts as the call happens.** Give Navi your rules for a good Short and every few
  minutes it looks through the call for moments worth posting, with the exact lines to cut and
  when they were said.
- **Jobs on calls.** Say "Hey Navi, draft the follow-up email" on a call and your AI does it while
  you talk. Navi can also take a screenshot whenever the screen changes, so the notes can say what
  was shown; click one to see it full size. Screenshots of Navi itself, and black ones from apps
  that block capture, are skipped.
- **Watch my screen.** "Hey Navi, watch", work, then "Hey Navi, stop watching": your AI picks the
  screenshots that matter and writes up what you did. Screenshots stay on your computer; the AI
  only gets the text read off them.
- **Built for talking to AI agents.** In a terminal or code editor, Prompt mode turns a ramble
  into a clear brief for Claude Code or Cursor, with `@file` references to the files you meant.
- **Learns from your corrections.** Fix a word after Navi pastes it and it notices. Fix the same
  word twice and it is added to your dictionary for good.
- **Edit by voice.** "Navi, scratch that." "Navi, make that shorter." "Navi, change Tuesday to
  Wednesday." Select text and say "Navi, turn this into bullet points", or "Navi, what's wrong with
  this?" for a blunt list of its problems in a card you can copy from.
- **Writes like you.** British spelling, no em dashes if you prefer, and a style profile that every
  AI rewrite follows.
- **Knows where you are typing.** The app and the text near your cursor (read on your computer
  only) help it pick the right mode and spell names correctly.
- **Never loses a take.** Every recording is kept with its text, so you can play it back, copy it
  again, or re-run it through another mode.
- **Packs.** Share your words, snippets and modes with your team as one file.

Sound cues: eleven sets of soft, low sounds (Hush, Felt, Oak, Kalimba, Bubble, Bowl and more)
made to be heard all day. Or use your own WAV files, or none at all.

## Speech engine

Navi starts a [whisper.cpp](https://github.com/ggml-org/whisper.cpp) server in the background and
keeps the model loaded. With an NVIDIA graphics card it runs on CUDA; otherwise it uses the
processor with a smaller model. First run detects what you have and downloads what is missing: the
engine from whisper.cpp's GitHub releases, and the models from
[huggingface.co/ggerganov/whisper.cpp](https://huggingface.co/ggerganov/whisper.cpp) (Whisper Large
v3 Turbo is about 1.6 GB), each model checked against its known size and SHA-256. A
`whisper-server.exe` already on your PATH is used rather than downloading another.

Settings, history, recordings, the engine and the models live in `%APPDATA%\com.goodlad.navi`.
Notes are saved to `Documents\Navi Notes` unless you choose another folder.

## AI (optional)

Navi tidies punctuation, capitals, fillers and spelling on your computer with no AI at all. For
the rewriting modes, voice edits, call notes, Short ideas and screen watches, use the AI you
already pay for: Navi can borrow Claude Code (your Claude plan) or Codex (your ChatGPT plan) if
they are installed, with no API key. Or connect Claude, OpenAI, a local Ollama model or any
OpenAI-compatible endpoint with a key. Jobs nobody waits on (call notes, Short ideas, screen
watches) can use a stronger model than dictation; through Claude Code that is Sonnet unless you
pick another. Keys are stored in Windows Credential Manager. Local only mode blocks every network
call except downloads you start.

## Voice playback (optional)

Connect an ElevenLabs key and Navi reads your words back in any voice in your account: on the Speak
page, from History, or with the Speak mode, which reads a take aloud instead of typing it.

## Building from source

Navi builds on Windows only for now; a Mac version is coming. You need:

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

Navi looks for a new version on this repository's GitHub releases when it starts and every six
hours, and Settings > About has a **Check for updates** button and a switch to turn the automatic
check off. When there is one, a card offers it; Navi downloads it, checks its signature against the
public key built into the app, installs it quietly and restarts.

To publish an update:

```
python scripts/release.py 0.1.1 "What changed, in a line or two"            # builds, uploads nothing
python scripts/release.py 0.1.1 "What changed, in a line or two" --publish  # creates the release
```

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
  [huggingface.co/ggerganov/whisper.cpp](https://huggingface.co/ggerganov/whisper.cpp). Voice
  detection by the Silero VAD model (MIT).
- Built with [Tauri](https://tauri.app) (MIT or Apache 2.0).
- Typeface Poppins by Indian Type Foundry, under the SIL Open Font Licence 1.1
  (`ui/fonts/OFL.txt`).
- The engine warm-up clip (`ui/sounds/warmup.wav`) is a line from John F. Kennedy's 1961
  inaugural address, a US government work in the public domain, taken from whisper.cpp's
  `samples/jfk.wav`. Navi never plays it; it only primes the engine.
- Sound cues generated by `scripts/make_sounds.py`.

Made by [GoodLad](https://github.com/GoodLadTom).
