# cpal 0.16.0, patched for Navi

This is cpal 0.16.0 from crates.io (Apache-2.0, see LICENSE), used through `[patch.crates-io]`
in `src-tauri/Cargo.toml`. One change, in `src/host/coreaudio/macos/enumerate.rs` and
`src/host/coreaudio/macos/mod.rs` only:

- Every `data_size` that Core Audio writes back into (`AudioObjectGetPropertyDataSize` and
  `AudioObjectGetPropertyData`) is now `let mut` and passed as `NonNull::from(&mut data_size)`.

cpal 0.16.0 declared them immutable and passed `NonNull::from(&data_size)`. Writing through that
pointer is undefined behaviour: in an optimised build the compiler assumes the size stays 0, so
listing the audio devices reserves room for none and Core Audio writes the list to address 0x4.
Navi's release build crashed at start-up (`EXC_BAD_ACCESS`, `KERN_INVALID_ADDRESS at 0x4` in
`HALDeviceList::GetData`, from `cpal::host::coreaudio::macos::enumerate::audio_devices`) when the
voice-command listener opened the microphone. Debug builds never showed it.

Nothing outside the macOS Core Audio files changed, so the Windows build compiles the same code
as before. Drop this folder and the patch once Navi moves to a cpal release with the fix.
