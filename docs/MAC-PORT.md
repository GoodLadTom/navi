# Navi for Mac: the plan

Read this first if you are the developer (or the Claude Code session) building the Mac version.
It was written on 15 October 2026, on the Windows PC where Navi was built, from a review of every
Windows-only part of the code. Nothing here has been tried on a Mac yet: treat API details as the
starting point and check them on the machine.

## What we are building

- **Everything the Windows version does**, on macOS: hold-to-talk and the "Navi" wake word, typing
  into the app in front, AI clean-up, voice edits ("Navi, scratch that"), modes that follow the
  app, call notes with both sides of the call, screenshots and Watch my screen, Speak, History,
  Dictionary, Modes, Packs, the floating bubble and firefly, the corner card and the menu bar item.
- **Built from source.** People clone the repo and build it on their own Mac. An app you compile
  yourself is not quarantined, so Gatekeeper does not block it and no Apple Developer account is
  needed. A signed, notarised download can come later. While the repo is private, cloning it needs
  access to GoodLadTom/navi and a signed-in git or GitHub CLI (`gh auth login`).
- **Windows must keep working exactly as it does.** Add Mac code beside the Windows code; don't
  rewrite shared logic unless it truly has to change. The Windows build and its 500 or so tests are
  the reference.
- **Bundle identifier: `com.goodlad.navi`** (already set in `src-tauri/tauri.conf.json`). Mac
  privacy permissions attach to it, so don't change it.

## Setting up the Mac

```
xcode-select --install                     # Apple's command-line tools (compiler, git)
curl https://sh.rustup.rs -sSf | sh         # Rust (1.90 or newer)
# Node.js 18 or newer, from nodejs.org or Homebrew (brew install node)
# CMake, to build the speech engine: brew install cmake
git clone https://github.com/GoodLadTom/navi.git
cd navi && npm install
npm run dev                                 # runs the app with live reload
```

Check the Mac first: Apple menu > About This Mac. Apple silicon (M1 or later) runs the speech engine
on the GPU through Metal and is the main target. Some features need macOS 14.2 or later (system audio
capture, process objects). Intel Macs would run the engine on the CPU only, with a smaller default
model, as Windows PCs without an NVIDIA card get Base (English) today.

**Permissions attach to the process that launched the app.** Under `npm run dev` the app is started
from Terminal, so macOS may give Terminal (not Navi) the Accessibility and Microphone grants. Test
permission flows with a built `.app`, not only with `npm run dev`. Today `tauri.conf.json` bundles
only the Windows `nsis` target, so `npm run build` makes a `.app` only once
`src-tauri/tauri.macos.conf.json` sets the `app` and `dmg` targets (see Files to add).

## A stable signature (do this before testing permissions)

macOS stores each privacy grant against the app's code signature. An ad-hoc signature changes with
every build, so the Accessibility grant silently stops working after each rebuild and has to be
removed and added again. Fix: create a local code-signing certificate once and sign every local
build with it.

1. Keychain Access > Certificate Assistant > Create a Certificate. Name `Navi Local`, Identity Type
   Self-Signed Root, Certificate Type Code Signing.
2. Point the Mac build at it in `src-tauri/tauri.macos.conf.json`:
   `{"bundle": {"macOS": {"signingIdentity": "Navi Local"}}}`.
3. Later, a setup script (`scripts/mac-setup.sh`) should do this for anyone building from source.

Check that grants survive a rebuild before relying on it; this behaviour is reported by Mac
developers rather than documented by Tauri.

## How the code is laid out

Every Windows-only piece sits in a `#[cfg(windows)]` block or a `mod win`, with a
`#[cfg(not(windows))]` stub beside it. Today those stubs return errors or nothing, so a Mac build
should compile (untested: nothing has been built for macOS yet) but can't hear the shortcut or
type. The work is to replace each stub with a real
`#[cfg(target_os = "macos")]` implementation, keeping the same function signatures so the rest of
the app doesn't change.

The keyboard logic (`hotkey.rs`, `HookMachine`) is plain Rust over Windows virtual-key numbers and
already has its tests. Keep it: translate Mac keycodes into those numbers at the edge and feed the
machine unchanged.

## Part by part

| Windows file and mechanism | Mac approach | Permission |
|---|---|---|
| `hotkey.rs`: `WH_KEYBOARD_LL` hook | `CGEventTapCreate` (session tap, head insert, default option) for keyDown, keyUp and flagsChanged, on its own thread with a CFRunLoop. Return NULL to swallow Space, Esc and Enter during takes. Modifiers come as flagsChanged: work out left and right from the keycode (Control 0x3B/0x3E, Option 0x3A/0x3D, Command 0x37/0x36, Shift 0x38/0x3C, Fn 0x3F). Re-enable the tap on `kCGEventTapDisabledByTimeout`; check `CGEventTapIsEnabled` every few seconds. Tag Navi's own posted events (`kCGEventSourceUserData`) so the tap ignores them | Accessibility |
| Shortcut presets and labels (`ui/js/ui.js`, `shortcut.js`) | Keep stored names; show Win as Command (⌘), Alt as Option (⌥), Ctrl as Control (⌃), Enter as Return. Mac presets: Fn, Right Option, Right Command, Control + Option. Mac clash list: Cmd+C/V/X/Z/A/S/Q/W/Tab, Cmd+Space, Ctrl+Space, Cmd+Shift+3/4/5 | none |
| Fn key and Caps Lock | Fn's own action (emoji picker, Apple Dictation) must be turned off by the user in System Settings > Keyboard; say so in set-up. Caps Lock only reaches a tap after it has toggled: don't offer it on Mac, or remap it with `hidutil` | none |
| F-key mode keys | The top row sends brightness and media keys by default: tell users to hold fn, or suggest F13 to F19 | Accessibility |
| `insert.rs`: clipboard and Ctrl+V | `NSPasteboard` write (LF line breaks), then Cmd+V posted with `CGEventPost`, flags set to Command only, the 'v' keycode looked up for the current layout (`UCKeyTranslate`, on the main thread). Snapshot every pasteboard item first and restore it only if `changeCount` is unchanged. Mark Navi's text with `org.nspasteboard.TransientType` and `AutoGeneratedType` so clipboard managers skip it | Accessibility |
| Typing method | `CGEventCreateKeyboardEvent` + `CGEventKeyboardSetUnicodeString`, at most 20 UTF-16 units per event, Return for line breaks | Accessibility |
| Elevated windows (UIPI) | No equivalent. The nearest is Secure Event Input (password fields, Terminal's Secure Keyboard Entry): check `IsSecureEventInputEnabled` and fall back to clipboard-only with a Mac-worded message | none |
| Voice edits: select back and replace | Shift+Left counted in grapheme clusters (Cocoa moves one composed character per press). Where the field supports it, set `kAXSelectedTextRangeAttribute` instead of pressing keys | Accessibility |
| `context.rs`: app in front | `NSWorkspace.frontmostApplication` (name, bundle ID, pid). Key app categories and modes on bundle IDs (`com.microsoft.VSCode`, `com.apple.Terminal`, `com.googlecode.iterm2`, `com.tinyspeck.slackmacgap` and so on), with migration from stored `.exe` names | none |
| Window title | AX: focused window of the app, then `kAXTitleAttribute`. Avoid `kCGWindowName`, which needs Screen Recording | Accessibility |
| A browser's site (`context.rs` `host`, so Auto picks Prompt on chatgpt.com or claude.ai) | AX: the address field of the browser's focused window, keeping only the host (`host_from_address`). Left empty, the tab's title decides, as it does on Windows when the read is too slow | Accessibility |
| Window identity (the `hwnd`) | No global handle. Use pid plus the frontmost layer-0 `CGWindowID`, or the AX focused-window element compared with `CFEqual`. **Don't stub this as 0**: `controller.rs` checks "same window still in front" before voice edits, and a 0 would always pass | none |
| Text before the cursor, the selection | AX on a worker thread: system-wide focused element, skip `kAXSecureTextFieldSubrole`, then `kAXSelectedTextRangeAttribute` and `kAXStringForRangeParameterizedAttribute` for the text before it. Chromium and Electron apps need `AXManualAccessibility` set | Accessibility |
| `audio.rs`: microphone | cpal's CoreAudio backend works as is. Add `NSMicrophoneUsageDescription` to Info.plist | Microphone |
| `wake.rs`: the "Navi" listener | Portable as written. Prefer the built-in mic over AirPods (holding an AirPods mic open drops them into low-quality call audio). The orange mic dot stays on while it listens; say so in Settings | Microphone |
| `meeting.rs`: noticing calls | macOS 14.2+: Core Audio process objects (`kAudioHardwarePropertyProcessObjectList`, then `kAudioProcessPropertyIsRunningInput`, bundle ID, pid). Leave out Navi's own pid | none |
| `meeting.rs`: the other side of the call | Core Audio process tap (`AudioHardwareCreateProcessTap` with a `CATapDescription`) in a private aggregate device. Either upgrade cpal to 0.18 (loopback on macOS 14.6+, but it changes the Windows audio code too, so retest Windows) or write a small `objc2-core-audio` module. Try it for a day before committing to one | System Audio Recording (`NSAudioCaptureUsageDescription`) |
| `screen.rs`: screenshots | ScreenCaptureKit: the display holding the front window, excluding Navi and the "never read" apps. macOS re-asks about screen recording from time to time | Screen Recording |
| OCR (`context.rs ocr_bgra`) | Vision `VNRecognizeTextRequest`, accurate level, en-US | none |
| `engine.rs` and `models.rs`: speech engine | whisper.cpp publishes no macOS server binary. Build `whisper-server` with Metal (`cmake -B build -DGGML_METAL=ON && cmake --build build --config Release`) in the setup script, or use Homebrew's whisper.cpp as a stopgap. Name it `whisper-server` (no `.exe`), add a "metal" device, and don't let the Windows engine download run on a Mac | none |
| `text/cli_ai.rs`: Claude Code and Codex | Looks for `.exe` files and `%APPDATA%\npm`. On a Mac search `~/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin`, npm's global bin and nvm folders. A Finder-launched app has a short PATH, so don't rely on PATH alone | none |
| `overlay.rs`: the bubble and firefly | A non-activating `NSPanel` via the `tauri-nspanel` plugin: borderless, never key, status level, joins all Spaces, ignores the mouse. Check the full-screen transparent firefly's frame rate on a Retina screen | none |
| Corner card | A second NSPanel that may become key (its buttons work and text can be copied) without taking focus from the call app. The Windows title-bar guard isn't needed | none |
| Tray menu | Becomes the menu bar item: a black-and-alpha template icon, menu on left click | none |
| Main window | Native traffic lights (`TitleBarStyle::Overlay`, hidden title); hide the custom Windows buttons with a `body.mac` class. Dock icon only while the main window is open; handle `RunEvent::Reopen` | none |
| `store/secrets.rs` | keyring feature `apple-native` (login Keychain). Without it, keys go to an in-memory store and vanish | none |
| Start at login | Already uses `MacosLauncher::LaunchAgent`; check it on the Mac | none |
| Updates | Self-built copies don't need the updater; leave it off on Mac until there is a signed build | none |

## Files to add or change for the Mac build

- `src-tauri/Info.plist`: `NSMicrophoneUsageDescription`, `NSAudioCaptureUsageDescription`, and
  Documents and Downloads folder descriptions.
- `src-tauri/Entitlements.plist`: `com.apple.security.device.audio-input`.
- `src-tauri/tauri.macos.conf.json`: bundle targets `app` and `dmg`, `macOSPrivateApi`, the signing
  identity, minimum macOS version (13 or later; the UI uses `color-mix()`, `:has()` and container
  queries).
- `src-tauri/Cargo.toml`: a `[target.'cfg(target_os = "macos")'.dependencies]` section (objc2 crates,
  `tauri-nspanel`), keyring `apple-native`.
- `scripts/mac-setup.sh`: checks the tools, builds whisper-server with Metal, creates the local
  signing certificate, then builds Navi.
- UI wording: pass a platform flag from `commands::app_info` and switch Windows words (Credential
  Manager, tray, Explorer, "Match Windows") for Mac ones (Keychain, menu bar, Finder, "Match macOS").

## Things the Mac can't match

- In a password field, or with Terminal's Secure Keyboard Entry on, macOS hides ordinary keys from
  every app: Space, Esc, Enter and F-key mode keys won't reach Navi. Modifier-only shortcuts still
  work.
- Up to four permission prompts (Accessibility, Microphone, Screen Recording, System Audio). Build a
  set-up page that checks each one live and opens the right System Settings pane.
- The orange microphone dot while the wake word listens, and the purple indicator while recording
  a call or the screen.
- Reading text from Chrome and Electron apps is patchier than on Windows.

## Test list (on a real Mac)

- First run on a fresh macOS user account: every permission prompt, in order, with clear wording.
- Hold-to-talk with each chord type (a modifier alone, two modifiers, a key), double tap, ramble
  (Space), Esc, Enter, and the "Navi" wake word.
- Typing into Safari, Chrome, Slack, VS Code or Cursor, Terminal and iTerm2, Mail, Notes, Word, and a
  password field (should fall back to the clipboard and say why).
- A UK layout and one non-QWERTY layout.
- "Scratch that", "make that shorter" and a selection edit.
- A real call (Zoom, Google Meet in Chrome, Teams) with speakers and with headphones: both sides
  in the notes.
- Watch my screen and call screenshots.
- Rebuild, and check every permission is still granted.
- Then run the Windows build and its tests again on Windows to make sure nothing there changed.

## Building it yourself (for the README, once it works)

Once the Mac version runs, the README gets a short "Build on a Mac" section: install the tools
above, clone, run `scripts/mac-setup.sh`, then open `Navi.app` from
`src-tauri/target/release/bundle/macos`. To update: `git pull` and run the script again.
