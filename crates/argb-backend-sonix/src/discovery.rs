//! Building devices from the profile, and listing the keyboards plugged in.

use argb_domain::{Backend, DeviceAlias, LightingProfile};

use crate::device::SonixDevice;
use crate::error::SonixError;
use crate::model::Model;
use crate::transport::{HidLink, attached_keyboards};

/// One device per configured Sonix entry. Nothing is opened here: each
/// device finds its keyboard on first use, so one unplugged at service
/// start is picked up by the engine's retry later.
#[must_use]
pub fn configured_devices(profile: &LightingProfile) -> Vec<SonixDevice<HidLink>> {
    profile
        .devices
        .iter()
        .filter(|(_, settings)| settings.selector.backend == Backend::Sonix)
        .map(|(alias, settings)| {
            SonixDevice::new(alias.clone(), HidLink::new(&settings.selector.match_name))
        })
        .collect()
}

/// A supported keyboard plugged in, with the entry it would get.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenKeyboard {
    /// Alias derived from the reported name.
    pub alias: DeviceAlias,
    /// The reported name, stored as `match_name`.
    pub name: String,
    /// The model.
    pub model: Model,
    /// Whether a configured device already claims it.
    pub configured: bool,
}

/// Every supported keyboard plugged in, marking those a configured device
/// already matches.
///
/// # Errors
/// [`SonixError::Hid`] when the HID library cannot enumerate.
pub fn list_keyboards(profile: &LightingProfile) -> Result<Vec<SeenKeyboard>, SonixError> {
    let mut keyboards: Vec<SeenKeyboard> = attached_keyboards()?
        .into_iter()
        .map(|attached| seen_keyboard(profile, attached.model))
        .collect();
    keyboards.sort_by(|a, b| a.name.cmp(&b.name));
    keyboards.dedup_by(|a, b| a.name == b.name);
    Ok(keyboards)
}

fn seen_keyboard(profile: &LightingProfile, model: Model) -> SeenKeyboard {
    let name = model.reported_name();
    let configured = profile
        .devices
        .values()
        .any(|settings| settings.selector.matches(Backend::Sonix, &name));
    SeenKeyboard {
        alias: DeviceAlias::derive_from(model.name()),
        name,
        model,
        configured,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use argb_domain::{
        Brightness, DeviceSelector, DeviceSettings, LightingMode, Rgb, StateDefaults,
    };

    use super::*;

    fn profile(match_name: &str) -> LightingProfile {
        let mode = LightingMode::Static {
            color: Rgb::WHITE,
            brightness: Brightness::MAX,
        };
        LightingProfile {
            defaults: StateDefaults {
                active: mode,
                sleep: mode,
            },
            devices: BTreeMap::from([(
                DeviceAlias::new("keyboard").unwrap(),
                DeviceSettings {
                    selector: DeviceSelector {
                        backend: Backend::Sonix,
                        match_name: match_name.into(),
                    },
                    overrides: BTreeMap::new(),
                },
            )]),
        }
    }

    #[test]
    fn a_seen_keyboard_is_described_and_matched_against_the_profile() {
        let seen = seen_keyboard(&profile("c959"), Model::CloudNineC959);
        assert_eq!(seen.alias.as_str(), "cloud-nine-c959-ergotkl");
        assert_eq!(seen.name, "Cloud Nine C959 ErgoTKL (0C45:8008)");
        assert!(seen.configured);
        assert!(!seen_keyboard(&profile("ak820"), Model::CloudNineC959).configured);
    }

    #[test]
    fn configured_devices_come_from_sonix_entries_only() {
        let devices = configured_devices(&profile("0C45:8008"));
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].link().match_name(), "0C45:8008");
        let mut other = profile("x");
        other
            .devices
            .get_mut(&DeviceAlias::new("keyboard").unwrap())
            .unwrap()
            .selector
            .backend = Backend::OpenRgb;
        assert!(configured_devices(&other).is_empty());
    }
}
