#!/usr/bin/env bash
# Navi: set up a Mac and build Navi.app from source.
#
# Copyright (C) 2026 GoodLad. Part of Navi, free software under the GNU General Public License,
# version 3 only, with the extra terms in NOTICE. It comes with no warranty. See LICENSE.
#
#   scripts/mac-setup.sh            check the tools, install what is missing (asking first), build
#   scripts/mac-setup.sh -y         the same, without asking
#   scripts/mac-setup.sh --check    only report what is present and what is missing; changes nothing
#   scripts/mac-setup.sh --brew     use Homebrew's whisper-cpp instead of building the engine
#
# Safe to run again: anything already in place is left alone. To update Navi, press Update in
# Navi's Settings > About, which runs `git pull` and this script with -y in Terminal; or do the
# same by hand in the Navi folder.
#
# What it does, in order:
#   1. Apple's command-line tools (compiler, git), Homebrew or CMake, Rust 1.90+, Node.js 18+.
#   2. The speech engine, whisper-server with Metal: built into ~/.local/tools/whisper.cpp, or
#      from Homebrew with --brew. Skipped when one is already where Navi looks (models.rs
#      find_server: ~/.local/tools/whisper.cpp/build/bin, ~/.local/bin, /opt/homebrew/bin,
#      /usr/local/bin).
#   3. "Navi Local", a self-signed code-signing certificate in the login keychain, so macOS keeps
#      Navi's permissions from one build to the next (tauri.macos.conf.json signs with it).
#   4. npm install and npm run build, then offers to copy Navi.app to /Applications.
# Nothing here uses sudo.

set -euo pipefail

IDENTITY="Navi Local"
WHISPER_REPO="https://github.com/ggml-org/whisper.cpp"
WHISPER_DIR="$HOME/.local/tools/whisper.cpp"
MIN_RUST="1.90"
MIN_NODE=18

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP="$ROOT/src-tauri/target/release/bundle/macos/Navi.app"

YES=0
CHECK=0
BREW_ENGINE=0

usage() {
  sed -n '2p;7,13p' "$0" | sed 's/^# \{0,1\}//'
}

for arg in "$@"; do
  case "$arg" in
    -y|--yes) YES=1 ;;
    --check) CHECK=1 ;;
    --brew) BREW_ENGINE=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown option: $arg" >&2; usage >&2; exit 2 ;;
  esac
done

# ------------------------------------------------------------------------------------- helpers

if [ -t 1 ]; then
  BOLD=$'\033[1m'; DIM=$'\033[2m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; RED=$'\033[31m'; RESET=$'\033[0m'
else
  BOLD=""; DIM=""; GREEN=""; YELLOW=""; RED=""; RESET=""
fi

MISSING=0
step() { printf '\n%s%s%s\n' "$BOLD" "$1" "$RESET"; }
ok() { printf '  %sok%s       %s\n' "$GREEN" "$RESET" "$1"; }
missing() { printf '  %smissing%s  %s\n' "$YELLOW" "$RESET" "$1"; MISSING=$((MISSING + 1)); }
note() { printf '  %s%s%s\n' "$DIM" "$1" "$RESET"; }
fail() { printf '\n%s%s%s\n' "$RED" "$1" "$RESET" >&2; exit 1; }

# Asks a yes/no question; -y answers yes. With no terminal to ask, the answer is no.
confirm() {
  if [ "$YES" = 1 ]; then return 0; fi
  if [ ! -t 0 ]; then return 1; fi
  local reply
  read -r -p "  $1 [y/N] " reply
  case "$reply" in [yY]|[yY][eE][sS]) return 0 ;; *) return 1 ;; esac
}

# version_at_least 1.91.0 1.90 -> true
version_at_least() {
  [ "$(printf '%s\n%s\n' "$2" "$1" | sort -t. -k1,1n -k2,2n -k3,3n | head -n1)" = "$2" ]
}

# Rust installed by rustup lives in ~/.cargo/bin, which a fresh Terminal may not have on its PATH.
load_cargo() {
  if [ -f "$HOME/.cargo/env" ]; then
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
  fi
}

# Homebrew on Apple silicon lives in /opt/homebrew, which may not be on the PATH yet either.
load_brew() {
  if ! command -v brew >/dev/null 2>&1; then
    for b in /opt/homebrew/bin/brew /usr/local/bin/brew; do
      if [ -x "$b" ]; then eval "$("$b" shellenv)"; break; fi
    done
  fi
}

# The first whisper-server in the places Navi looks for one (models.rs find_server).
find_engine() {
  local p
  for p in "$WHISPER_DIR/build/bin/whisper-server" "$HOME/.local/bin/whisper-server" \
           /opt/homebrew/bin/whisper-server /usr/local/bin/whisper-server; do
    if [ -x "$p" ]; then echo "$p"; return 0; fi
  done
  return 1
}

has_identity() {
  security find-identity -p codesigning 2>/dev/null | grep -q "\"$IDENTITY\""
}

# ------------------------------------------------------------------------------------- checks

[ "$(uname -s)" = "Darwin" ] || fail "This script is for macOS. On Windows, see Building from source in the README."
[ -f "$ROOT/package.json" ] && [ -d "$ROOT/src-tauri" ] || fail "Run this from the Navi folder you cloned (scripts/mac-setup.sh)."

load_cargo
load_brew

step "This Mac"
OS_VERSION="$(sw_vers -productVersion)"
ARCH="$(uname -m)"
if [ "$ARCH" = "arm64" ]; then
  ok "macOS $OS_VERSION on Apple silicon"
else
  ok "macOS $OS_VERSION on an Intel Mac"
  note "The speech engine runs on the processor here, so takes are slower than on Apple silicon."
fi
if ! version_at_least "$OS_VERSION" "14.2"; then
  note "Navi needs macOS 14.2 or later. Update in System Settings > General > Software Update."
  MISSING=$((MISSING + 1))
fi

step "Tools"
if xcode-select -p >/dev/null 2>&1; then
  ok "Apple's command-line tools ($(xcode-select -p))"
else
  missing "Apple's command-line tools (the compiler and git)"
  if [ "$CHECK" = 0 ]; then
    note "Opening Apple's installer. When it has finished, run this script again."
    xcode-select --install || true
    exit 1
  fi
fi

if command -v brew >/dev/null 2>&1; then
  ok "Homebrew ($(brew --prefix))"
  HAVE_BREW=1
else
  HAVE_BREW=0
  note "Homebrew isn't installed. It's optional, but the easy way to get CMake and Node.js: https://brew.sh"
fi

if command -v cmake >/dev/null 2>&1; then
  ok "CMake $(cmake --version | head -n1 | awk '{print $3}')"
elif [ "$BREW_ENGINE" = 1 ]; then
  note "CMake isn't installed (not needed with --brew)."
else
  missing "CMake, to build the speech engine"
  if [ "$CHECK" = 0 ]; then
    if [ "$HAVE_BREW" = 1 ] && confirm "Install CMake with Homebrew?"; then
      brew install cmake
    else
      fail "Install CMake (brew install cmake, or from cmake.org), then run this script again. Or use --brew for Homebrew's speech engine."
    fi
  fi
fi

if command -v cargo >/dev/null 2>&1 && command -v rustc >/dev/null 2>&1; then
  RUST_VERSION="$(rustc --version | awk '{print $2}')"
  if version_at_least "$RUST_VERSION" "$MIN_RUST"; then
    ok "Rust $RUST_VERSION"
  else
    missing "Rust $MIN_RUST or newer (this Mac has $RUST_VERSION)"
    if [ "$CHECK" = 0 ]; then
      if command -v rustup >/dev/null 2>&1 && confirm "Update Rust with rustup?"; then
        rustup update stable
      else
        fail "Update Rust (rustup update stable), then run this script again."
      fi
    fi
  fi
else
  missing "Rust $MIN_RUST or newer"
  if [ "$CHECK" = 0 ]; then
    note "Rust installs into your home folder with rustup (rust-lang.org) and needs no password."
    if confirm "Install Rust now?"; then
      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
      load_cargo
      command -v cargo >/dev/null 2>&1 || fail "Rust installed, but cargo isn't on the PATH. Open a new Terminal window and run this script again."
      ok "Rust $(rustc --version | awk '{print $2}')"
    else
      fail "Navi needs Rust to build. Install it from rust-lang.org, then run this script again."
    fi
  fi
fi

if command -v node >/dev/null 2>&1; then
  NODE_VERSION="$(node -v | sed 's/^v//')"
  if [ "${NODE_VERSION%%.*}" -ge "$MIN_NODE" ]; then
    ok "Node.js $NODE_VERSION"
  else
    missing "Node.js $MIN_NODE or newer (this Mac has $NODE_VERSION)"
    [ "$CHECK" = 1 ] || fail "Update Node.js (brew upgrade node, or from nodejs.org), then run this script again."
  fi
else
  missing "Node.js $MIN_NODE or newer"
  if [ "$CHECK" = 0 ]; then
    if [ "$HAVE_BREW" = 1 ] && confirm "Install Node.js with Homebrew?"; then
      brew install node
    else
      fail "Install Node.js $MIN_NODE or newer (brew install node, or from nodejs.org), then run this script again."
    fi
  fi
fi

# ------------------------------------------------------------------------------------- engine

step "Speech engine"
if ENGINE="$(find_engine)"; then
  ok "whisper-server at $ENGINE"
elif [ "$CHECK" = 1 ]; then
  missing "whisper-server (built into $WHISPER_DIR, or brew install whisper-cpp)"
elif [ "$BREW_ENGINE" = 1 ]; then
  [ "$HAVE_BREW" = 1 ] || fail "--brew needs Homebrew: https://brew.sh"
  confirm "Install whisper-cpp with Homebrew?" || fail "Navi needs the speech engine. Run this script again when you're ready."
  brew install whisper-cpp
  ENGINE="$(find_engine)" || fail "Homebrew installed whisper-cpp, but whisper-server isn't in $(brew --prefix)/bin."
  ok "whisper-server at $ENGINE"
else
  note "Navi builds whisper.cpp's server with Metal, so it runs on this Mac's GPU. This takes a few minutes."
  confirm "Build the speech engine into $WHISPER_DIR?" || fail "Navi needs the speech engine. Run this script again, or use --brew."
  if [ -d "$WHISPER_DIR/.git" ]; then
    note "Updating the copy of whisper.cpp already in $WHISPER_DIR"
    git -C "$WHISPER_DIR" pull --ff-only || note "Couldn't update it; building what is there."
  else
    mkdir -p "$(dirname "$WHISPER_DIR")"
    git clone --depth 1 "$WHISPER_REPO" "$WHISPER_DIR"
  fi
  METAL=ON
  [ "$ARCH" = "arm64" ] || METAL=OFF
  cmake -S "$WHISPER_DIR" -B "$WHISPER_DIR/build" -DCMAKE_BUILD_TYPE=Release -DGGML_METAL="$METAL" \
    -DGGML_METAL_EMBED_LIBRARY=ON -DBUILD_SHARED_LIBS=OFF -DWHISPER_BUILD_TESTS=OFF
  cmake --build "$WHISPER_DIR/build" --config Release --target whisper-server -j "$(sysctl -n hw.ncpu)"
  ENGINE="$WHISPER_DIR/build/bin/whisper-server"
  [ -x "$ENGINE" ] || fail "The build finished, but $ENGINE isn't there. Scroll up for the error."
  ok "whisper-server at $ENGINE"
fi

# ------------------------------------------------------------------------------------- signing

step "Signing certificate"
if has_identity; then
  ok "\"$IDENTITY\" is in your keychain"
elif [ "$CHECK" = 1 ]; then
  missing "\"$IDENTITY\", the certificate Navi is signed with"
else
  note "macOS ties Navi's permissions (Accessibility, Microphone and so on) to the certificate it is"
  note "signed with. A certificate of your own keeps them from one build to the next. It is made on"
  note "this Mac, stays in your login keychain and can only sign apps you build here."
  confirm "Create the \"$IDENTITY\" certificate?" || fail "Navi's build is signed with \"$IDENTITY\". Run this script again when you're ready."
  WORK="$(mktemp -d)"
  trap 'rm -rf "$WORK"' EXIT
  cat > "$WORK/cert.cnf" <<EOF
[ req ]
distinguished_name = dn
prompt = no
x509_extensions = ext
[ dn ]
CN = $IDENTITY
[ ext ]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
subjectKeyIdentifier = hash
EOF
  # macOS's own openssl (LibreSSL) writes a .p12 the keychain can read.
  /usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -config "$WORK/cert.cnf" \
    -keyout "$WORK/key.pem" -out "$WORK/cert.pem" 2>/dev/null
  P12_PASS="navi-$RANDOM-$RANDOM"
  /usr/bin/openssl pkcs12 -export -inkey "$WORK/key.pem" -in "$WORK/cert.pem" -name "$IDENTITY" \
    -out "$WORK/navi.p12" -passout "pass:$P12_PASS"
  KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"
  security import "$WORK/navi.p12" -k "$KEYCHAIN" -P "$P12_PASS" -T /usr/bin/codesign >/dev/null
  ok "Created \"$IDENTITY\" in your login keychain"
  note "Now trusting it for code signing. macOS may ask for your login password to allow this."
  if security add-trusted-cert -r trustRoot -p codeSign -k "$KEYCHAIN" "$WORK/cert.pem"; then
    ok "Trusted for code signing"
  else
    note "It wasn't trusted. Navi still builds; to trust it later, open Keychain Access, double-click"
    note "\"$IDENTITY\" and set Trust > Code Signing to Always Trust."
  fi
  note "The first build may ask to use the key \"$IDENTITY\": enter your login password and choose Always Allow."
fi

# ------------------------------------------------------------------------------------- build

if [ "$CHECK" = 1 ]; then
  step "Navi"
  if [ -d "$APP" ]; then ok "Navi.app built at $APP"; else note "Not built yet."; fi
  if [ -d /Applications/Navi.app ]; then ok "Navi.app is in Applications"; fi
  echo
  if [ "$MISSING" = 0 ]; then
    echo "Everything Navi needs is here. Run scripts/mac-setup.sh to build it."
  else
    if [ "$MISSING" = 1 ]; then echo "One thing is missing."; else echo "$MISSING things are missing."; fi
    echo "Run scripts/mac-setup.sh (without --check) to set up what is missing."
  fi
  exit 0
fi

step "Building Navi"
cd "$ROOT"
npm install
npm run build
[ -d "$APP" ] || fail "The build finished, but $APP isn't there. Scroll up for the error."
ok "Navi.app is at $APP"

# Lets Navi's Update button (Settings > About) find this folder: it fetches GitHub here, and
# updating runs git pull and this script again in Terminal.
NAVI_DATA="$HOME/Library/Application Support/com.goodlad.navi"
mkdir -p "$NAVI_DATA" && printf '%s\n' "$ROOT" > "$NAVI_DATA/source-folder"

if [ -d /Applications/Navi.app ]; then
  QUESTION="Replace Navi in your Applications folder with this build?"
else
  QUESTION="Copy Navi.app to your Applications folder?"
fi
if confirm "$QUESTION"; then
  if pgrep -x Navi >/dev/null; then
    note "Quitting the Navi that is running."
    osascript -e 'tell application "Navi" to quit' >/dev/null 2>&1 || true
    sleep 2
  fi
  rm -rf /Applications/Navi.app
  ditto "$APP" /Applications/Navi.app
  ok "Copied to /Applications/Navi.app"
  APP=/Applications/Navi.app
fi

echo
echo "${BOLD}Done.${RESET} Open Navi with:  open \"$APP\""
echo "The first time, it asks for Accessibility, Microphone, Screen Recording and System Audio"
echo "Recording. To update later, press Update in Navi's Settings > About."
