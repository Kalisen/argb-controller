//! The lighting profile: which mode each device shows in each power state.

use std::collections::BTreeMap;

use crate::device::{DeviceAlias, DeviceSelector};
use crate::mode::LightingMode;
use crate::state::PowerState;

/// The modes used by every device that has no override for a state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateDefaults {
    /// Shown while the machine is active.
    pub active: LightingMode,
    /// Shown while the machine sleeps.
    pub sleep: LightingMode,
}

impl StateDefaults {
    /// The default mode for `state`.
    #[must_use]
    pub const fn for_state(&self, state: PowerState) -> &LightingMode {
        match state {
            PowerState::Active => &self.active,
            PowerState::Sleep => &self.sleep,
        }
    }
}

/// Configuration of one device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSettings {
    /// How the device is found in its backend.
    pub selector: DeviceSelector,
    /// Modes that replace the defaults for specific states.
    pub overrides: BTreeMap<PowerState, LightingMode>,
}

/// Everything the engine needs to decide what each device shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LightingProfile {
    /// Fallback modes per state.
    pub defaults: StateDefaults,
    /// Configured devices by alias.
    pub devices: BTreeMap<DeviceAlias, DeviceSettings>,
}

impl LightingProfile {
    /// The mode `alias` shows in `state`: its override if any, else the default.
    /// Unknown aliases get the defaults, so newly discovered devices light up
    /// consistently without configuration.
    #[must_use]
    pub fn mode_for(&self, alias: &DeviceAlias, state: PowerState) -> &LightingMode {
        self.devices
            .get(alias)
            .and_then(|device| device.overrides.get(&state))
            .unwrap_or_else(|| self.defaults.for_state(state))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{Brightness, Rgb};
    use crate::device::Backend;
    use crate::mode::PulsePeriod;

    fn profile() -> LightingProfile {
        let mut overrides = BTreeMap::new();
        overrides.insert(
            PowerState::Sleep,
            LightingMode::Static {
                color: Rgb::BLACK,
                brightness: Brightness::MIN,
            },
        );
        let mut devices = BTreeMap::new();
        devices.insert(
            DeviceAlias::new("gpu").unwrap(),
            DeviceSettings {
                selector: DeviceSelector {
                    backend: Backend::OpenRgb,
                    match_name: "TUF".into(),
                },
                overrides,
            },
        );
        LightingProfile {
            defaults: StateDefaults {
                active: LightingMode::Static {
                    color: Rgb::WHITE,
                    brightness: Brightness::MAX,
                },
                sleep: LightingMode::Pulse {
                    color: Rgb::new(255, 0, 0),
                    period: PulsePeriod::from_millis(4000).unwrap(),
                },
            },
            devices,
        }
    }

    #[test]
    fn override_wins_for_its_state_only() {
        let profile = profile();
        let gpu = DeviceAlias::new("gpu").unwrap();
        assert_eq!(
            profile.mode_for(&gpu, PowerState::Sleep),
            &LightingMode::Static {
                color: Rgb::BLACK,
                brightness: Brightness::MIN
            }
        );
        assert_eq!(
            profile.mode_for(&gpu, PowerState::Active),
            &profile.defaults.active
        );
    }

    #[test]
    fn unknown_alias_falls_back_to_defaults() {
        let profile = profile();
        let unknown = DeviceAlias::new("fan-hub").unwrap();
        assert_eq!(
            profile.mode_for(&unknown, PowerState::Sleep),
            &profile.defaults.sleep
        );
    }
}
