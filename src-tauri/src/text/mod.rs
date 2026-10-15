//! Everything that turns a raw transcript into the text you wanted.
//!
//! OWNER: text agent. Pure functions wherever possible, each with unit tests.

pub mod agent;
pub mod britishise;
pub mod cleanup;
pub mod cli_ai;
pub mod learn;
pub mod llm;
pub mod modes;
pub mod pipeline;
pub mod ramble;
pub mod voice;

/// Shared helper: spans of machine text (code, links, paths) that prose rules must not touch.
mod spans;

/// Test helper: a local mock HTTP server for the AI and integration tests.
#[cfg(test)]
pub(crate) mod testkit;
