"""Publishes a Navi release on GitHub, so every installed copy offers it as an update.

    python scripts/release.py 0.1.1 "What changed, in a line or two"             (a dry run)
    python scripts/release.py 0.1.1 "What changed, in a line or two" --publish   (uploads it)

It sets the version in tauri.conf.json, Cargo.toml and package.json, builds the installer with
src-tauri/tauri.release.conf.json (which turns on the signed update files), signs it with the
updater key and writes latest.json next to the installer. With --publish it creates the GitHub
release v<version> on GoodLadTom/navi carrying the installer, its signature and latest.json.
Installed copies read https://github.com/GoodLadTom/navi/releases/latest/download/latest.json
(see src-tauri/src/update.rs).

The updater key is read from %USERPROFILE%\\.tauri\\navi-updater.key (~/.tauri/navi-updater.key);
set NAVI_UPDATER_KEY to use another path, and NAVI_UPDATER_KEY_PASSWORD if the key has a password.
The key is never printed or committed. A plain `npm run build` needs no key: it makes an unsigned
installer without update files.

Needs Python 3, the project's npm packages (npm install) and, for --publish, an authenticated gh.
The repository must exist and have at least one commit before the first --publish.
"""
import argparse
import datetime
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
REPO = "GoodLadTom/navi"
KEY = pathlib.Path(os.environ.get("NAVI_UPDATER_KEY") or pathlib.Path.home() / ".tauri" / "navi-updater.key")
RELEASE_CONFIG = ROOT / "src-tauri" / "tauri.release.conf.json"


def read(path):
    return path.read_bytes().decode("utf-8")


def write_like(path, text, original):
    """Writes `text`, keeping the line endings the file had."""
    crlf = "\r\n" in original
    text = text.replace("\r\n", "\n")
    path.write_bytes((text.replace("\n", "\r\n") if crlf else text).encode("utf-8"))


def current_version():
    return json.loads(read(ROOT / "src-tauri" / "tauri.conf.json"))["version"]


def as_tuple(version):
    return tuple(int(x) for x in re.findall(r"\d+", version)[:3])


def set_versions(version):
    conf = ROOT / "src-tauri" / "tauri.conf.json"
    original = read(conf)
    data = json.loads(original)
    data["version"] = version
    write_like(conf, json.dumps(data, indent=2) + "\n", original)

    cargo = ROOT / "src-tauri" / "Cargo.toml"
    original = read(cargo)
    # The first `version = "..."` is the [package] version.
    updated = re.sub(r'(?m)^(version\s*=\s*)"[^"]*"', lambda m: f'{m.group(1)}"{version}"', original, count=1)
    write_like(cargo, updated, original)

    package = ROOT / "package.json"
    original = read(package)
    data = json.loads(original)
    data["version"] = version
    write_like(package, json.dumps(data, indent=2) + "\n", original)


def check_key():
    if not KEY.is_file():
        sys.exit(f"There is no updater key at {KEY}, so the update can't be signed. "
                 "Set NAVI_UPDATER_KEY to its path.")


def build():
    # The pinned CLI from node_modules (npm run tauri), never one fetched at build time, and no
    # shell. Tauri reads the key from the file itself, so only its path goes into the environment.
    npm = shutil.which("npm")
    if not npm:
        sys.exit("npm isn't on PATH; install Node.js 18 or newer, then run npm install.")
    if not (ROOT / "node_modules" / "@tauri-apps" / "cli").is_dir():
        sys.exit("The Tauri CLI isn't installed; run npm install first.")
    if not RELEASE_CONFIG.is_file():
        sys.exit(f"{RELEASE_CONFIG} is missing; it turns on the signed update files.")
    env = dict(
        os.environ,
        TAURI_SIGNING_PRIVATE_KEY=str(KEY),
        TAURI_SIGNING_PRIVATE_KEY_PASSWORD=os.environ.get("NAVI_UPDATER_KEY_PASSWORD", ""),
    )
    # The CLI runs from the project root, so the config path is relative to it.
    config = RELEASE_CONFIG.relative_to(ROOT).as_posix()
    subprocess.run([npm, "run", "tauri", "--", "build", "--config", config], cwd=ROOT, env=env, check=True)


def main():
    parser = argparse.ArgumentParser(description="Build, sign and publish a Navi update.")
    parser.add_argument("version", help="the new version, e.g. 0.1.1")
    parser.add_argument("notes", help="what changed, shown to people when Navi offers the update")
    parser.add_argument("--publish", action="store_true", help="create the GitHub release (otherwise a dry run)")
    args = parser.parse_args()

    was = current_version()
    if as_tuple(args.version) <= as_tuple(was):
        sys.exit(f"{args.version} isn't newer than {was}; installed copies would never offer it.")
    # Before anything is changed, so a missing key never leaves the version files bumped.
    check_key()

    set_versions(args.version)
    build()

    nsis = ROOT / "src-tauri" / "target" / "release" / "bundle" / "nsis"
    installer = nsis / f"Navi_{args.version}_x64-setup.exe"
    signature = installer.with_name(installer.name + ".sig")
    for path in (installer, signature):
        if not path.is_file():
            sys.exit(f"The build didn't make {path}.")
    latest = {
        "version": args.version,
        "notes": args.notes,
        "pub_date": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "platforms": {
            "windows-x86_64": {
                "signature": signature.read_text(encoding="utf-8").strip(),
                "url": f"https://github.com/{REPO}/releases/download/v{args.version}/{installer.name}",
            }
        },
    }
    manifest = nsis / "latest.json"
    manifest.write_text(json.dumps(latest, indent=2) + "\n", encoding="utf-8")
    print(f"Built and signed Navi {args.version}:\n  {installer}\n  {signature}\n  {manifest}")

    if not args.publish:
        print("Dry run: nothing uploaded. Add --publish to create the GitHub release.")
        return
    subprocess.run(
        ["gh", "release", "create", f"v{args.version}", str(installer), str(signature), str(manifest),
         "--repo", REPO, "--title", f"Navi {args.version}", "--notes", args.notes],
        check=True,
    )
    print(f"Published: https://github.com/{REPO}/releases/tag/v{args.version}")


if __name__ == "__main__":
    main()
