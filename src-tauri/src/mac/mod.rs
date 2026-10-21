//! macOS building blocks shared by the Mac halves of `context`, `insert`, `hotkey`, `overlay`,
//! `meeting` and `screen`: the Accessibility API (`ax`), the app in front (`front`), Core Audio
//! for call notes (`audio`) and pictures of the screen (`capture`).
//!
//! Navi for Mac. Copyright (c) 2026 GoodLad. GNU GPL v3 only (see LICENSE and NOTICE).

pub mod audio;
pub mod ax;
pub mod capture;
pub mod front;
