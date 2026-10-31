//! OpenRGB backend.
//!
//! Talks to a running OpenRGB SDK server over its TCP protocol. The protocol
//! is small and stable, and the one published Rust client predates the
//! features this project needs (mode brightness, save-to-device), so the
//! client is implemented here directly. See `docs/architecture.md`.
//!
//! - [`protocol`]: packet header, ids, flags and color packing.
//! - [`description`]: controller descriptions and update payloads.
//! - [`client`]: the blocking TCP client.
//! - [`device`]: the [`argb_engine::LightingDevice`] adapter per controller.

pub mod client;
pub mod description;
pub mod device;
pub mod discovery;
pub mod protocol;
pub mod session;

pub use client::{ControllerCommands, OpenRgbClient, OpenRgbError};
pub use description::{ControllerDescription, ModeDescription};
pub use device::{ModeMap, OpenRgbDevice, SharedClient};
pub use discovery::{Assignment, NewDevice, assign_aliases, new_devices};
pub use session::{OpenRgbSession, open_devices, open_session};
