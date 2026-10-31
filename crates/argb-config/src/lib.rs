//! Configuration file handling for argb-controller.
//!
//! [`schema`] mirrors the TOML document and converts it into the domain
//! [`argb_domain::LightingProfile`]. [`store`] reads and writes the file
//! under an advisory lock with atomic replacement, so the service and the CLI
//! never observe a half-written file.

pub mod paths;
pub mod schema;
pub mod store;

pub use schema::{BootSettings, ConfigFile, DeviceSchema, OpenRgbSettings, StateDefaultsSchema};
pub use store::{ConfigError, ConfigStore};

/// The schema version this build reads and writes.
pub const SCHEMA_VERSION: u32 = 1;
