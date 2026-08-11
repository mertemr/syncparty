//! Everything syncparty does, with no dependency on Tauri.
//!
//! The rule is one-directional: `core` never imports from `ipc`. Events reach
//! the UI through [`events::EventBus`], which the Tauri layer implements. That
//! is what lets the whole of this module be exercised by `cargo test` without
//! a webview anywhere in sight.

pub mod config;
pub mod deps;
pub mod diagnostics;
pub mod error;
pub mod events;
pub mod invite;
pub mod net;
pub mod notify;
pub mod paths;
pub mod process;
pub mod session;
pub mod syncplay;
