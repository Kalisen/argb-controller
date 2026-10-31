//! Govee Bluetooth LE backend (Phase 2).
//!
//! Drives Govee lights directly over Bluetooth LE with the frames the vendor
//! app sends, including the encrypted channel newer lights use. See
//! `docs/phase2-govee-ble.md` for the protocol facts and their sources.
//!
//! - [`protocol`]: the 20-byte frames and per-model layouts.
//! - [`crypto`]: the channel cipher and the pre-shared key.
//! - [`model`]: the supported models, recognized from advertised names.
//! - [`transport`]: the shared Bluetooth runtime and the link per light.
//! - [`device`]: the [`argb_engine::LightingDevice`] adapter.
//! - [`discovery`]: devices from the profile, and scanning for new lights.

pub mod crypto;
pub mod device;
pub mod discovery;
pub mod error;
pub mod model;
pub mod protocol;
pub mod transport;

pub use device::GoveeDevice;
pub use discovery::{SeenLight, configured_devices, scan_lights};
pub use error::GoveeError;
pub use model::Model;
pub use transport::{BleLink, BleRuntime, Link, SCAN_DURATION, SharedRuntime};
