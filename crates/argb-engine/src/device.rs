//! The contract every backend device fulfils.

use argb_domain::{Brightness, DeviceAlias, DeviceCapabilities, PowerState, PulsePeriod, Rgb};
use thiserror::Error;

/// Whether a hardware mode should be written to the device's own memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Persist {
    /// Apply only; lost at power cycle.
    Volatile,
    /// Apply and store, so it shows during boot and while the host is asleep.
    SaveToDevice,
}

impl Persist {
    /// The policy for entering `state` when boot lighting follows `boot_follow`.
    ///
    /// Sleep lighting is always stored: nothing can stream to the devices
    /// while the host sleeps. The state boot lighting follows is stored too,
    /// so that it is what the devices show from power-on until the service
    /// runs. Everything else is left volatile to spare the devices' memory.
    #[must_use]
    pub fn for_state(state: PowerState, boot_follow: PowerState) -> Self {
        if state == PowerState::Sleep || state == boot_follow {
            Persist::SaveToDevice
        } else {
            Persist::Volatile
        }
    }
}

/// Errors a device can report.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DeviceError {
    /// The device lacks a capability the engine asked for.
    #[error("device '{alias}' does not support {capability}")]
    Unsupported {
        /// Device concerned.
        alias: DeviceAlias,
        /// Human-readable capability name.
        capability: &'static str,
    },
    /// The backend failed to carry out the request.
    #[error("backend failure on '{alias}': {message}")]
    Backend {
        /// Device concerned.
        alias: DeviceAlias,
        /// Backend-specific description.
        message: String,
    },
}

/// A controllable light. Backends implement this; the engine only sees it.
pub trait LightingDevice {
    /// The alias the device is configured under.
    fn alias(&self) -> &DeviceAlias;

    /// What the device can do natively.
    fn capabilities(&self) -> DeviceCapabilities;

    /// Shows one color using the hardware static mode, with true brightness
    /// where [`DeviceCapabilities::hardware_brightness`] is set and software
    /// scaling otherwise.
    ///
    /// # Errors
    /// [`DeviceError`] from the backend.
    fn apply_static(
        &mut self,
        color: Rgb,
        brightness: Brightness,
        persist: Persist,
    ) -> Result<(), DeviceError>;

    /// Starts the device's own breathing effect, approximating `period`.
    ///
    /// # Errors
    /// [`DeviceError::Unsupported`] when [`DeviceCapabilities::hardware_pulse`] is false.
    fn apply_hardware_pulse(
        &mut self,
        color: Rgb,
        period: PulsePeriod,
        persist: Persist,
    ) -> Result<(), DeviceError>;

    /// Switches to direct mode so that [`Self::write_direct`] frames are shown.
    ///
    /// # Errors
    /// [`DeviceError::Unsupported`] when [`DeviceCapabilities::direct`] is false.
    fn begin_direct(&mut self) -> Result<(), DeviceError>;

    /// Shows `color` on every LED immediately. Called at the tick rate while pulsing.
    ///
    /// # Errors
    /// [`DeviceError`] from the backend.
    fn write_direct(&mut self, color: Rgb) -> Result<(), DeviceError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleep_and_the_boot_state_are_saved_to_device() {
        assert_eq!(
            Persist::for_state(PowerState::Sleep, PowerState::Active),
            Persist::SaveToDevice
        );
        assert_eq!(
            Persist::for_state(PowerState::Active, PowerState::Active),
            Persist::SaveToDevice
        );
        assert_eq!(
            Persist::for_state(PowerState::Active, PowerState::Sleep),
            Persist::Volatile
        );
    }
}
