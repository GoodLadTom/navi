//! macOS building blocks shared by the Mac halves of `context`, `insert`, `hotkey`, `overlay`,
//! `meeting` and `screen`: the Accessibility API (`ax`), the app in front (`front`) and Core Audio
//! for call notes (`audio`).
//!
//! Navi for Mac. Copyright (c) 2026 GoodLad. GNU GPL v3 only (see LICENSE and NOTICE).

pub mod audio;
pub mod ax;
pub mod front;
