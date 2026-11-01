//! Sonix OEM keyboard backend (Phase 2.1).
//!
//! Drives keyboards built on a Sonix MCU and configured by the generic Sonix
//! `DeviceDriver.exe` application, such as the Cloud Nine C959 ErgoTKL, with
//! the HID feature reports that application sends. See
//! `docs/phase2-1-peripherals.md` for the protocol facts, their sources and
//! the capture they were read from.
//!
//! - [`protocol`]: the command reports, the session, and the lighting block.
//! - [`model`]: the supported keyboards, recognized from their USB ids.
//! - [`transport`]: the HID link to one keyboard and the write session.
//! - [`device`]: the [`argb_engine::LightingDevice`] adapter.
//! - [`discovery`]: devices from the profile, and the keyboards plugged in.

pub mod device;
pub mod discovery;
pub mod error;
pub mod model;
pub mod protocol;
pub mod transport;

pub use device::SonixDevice;
pub use discovery::{SeenKeyboard, configured_devices, list_keyboards};
pub use error::SonixError;
pub use model::Model;
pub use transport::{HidLink, Link};
