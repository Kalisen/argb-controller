//! Core domain model for argb-controller.
//!
//! This crate knows nothing about files, networks or hardware. It defines the
//! vocabulary shared by every other crate: colors, lighting modes, power
//! states, the lighting profile that maps devices to modes per state, and the
//! capabilities a device backend can advertise.

pub mod color;
pub mod device;
pub mod mode;
pub mod profile;
pub mod state;

pub use color::{Brightness, BrightnessError, ColorParseError, Rgb};
pub use device::{Backend, DeviceAlias, DeviceAliasError, DeviceCapabilities, DeviceSelector};
pub use mode::{LightingMode, PulsePeriod, PulsePeriodError};
pub use profile::{DeviceSettings, LightingProfile, StateDefaults};
pub use state::PowerState;
