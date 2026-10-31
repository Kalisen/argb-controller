//! OpenRGB backend.
//!
//! Talks to a running OpenRGB SDK server over its TCP protocol. The protocol
//! is small and stable, and the one published Rust client predates the
//! features this project needs (mode brightness, save-to-device), so the
//! client is implemented here directly. See `docs/architecture.md`.

pub mod client;
pub mod protocol;

pub use client::{OpenRgbClient, OpenRgbError};
