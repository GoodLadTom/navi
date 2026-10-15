fn main() {
    // tauri-build bakes icons/icon.ico into navi.exe (the window, taskbar and Explorer icon), but it
    // only asks Cargo to re-run it when tauri.conf.json or the resources change. Without this line a
    // new icon never reached the exe: builds kept the old orange mark for hours.
    println!("cargo:rerun-if-changed=icons");
    // The helper programs in examples/ (card_check) get no app manifest, so Windows gave them the old
    // Common Controls and they stopped with STATUS_ENTRYPOINT_NOT_FOUND. navi.exe has its own.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-examples=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-examples=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'");
    }
    tauri_build::build()
}
