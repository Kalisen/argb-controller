//! Building devices from the profile, and finding lights the profile does
//! not name yet.

use std::time::Duration;

use argb_domain::{Backend, DeviceAlias, LightingProfile};

use crate::device::GoveeDevice;
use crate::error::GoveeError;
use crate::model::{Model, is_govee_name, model_code};
use crate::transport::{Advertisement, BleLink, BleRuntime, SharedRuntime};

/// One device per configured Govee entry. Nothing is scanned or connected
/// here: each device finds its light on first use, so a light that is off
/// at service start is picked up by the engine's retry later.
#[must_use]
pub fn configured_devices(
    profile: &LightingProfile,
    runtime: &SharedRuntime,
) -> Vec<GoveeDevice<BleLink>> {
    profile
        .devices
        .iter()
        .filter(|(_, settings)| settings.selector.backend == Backend::GoveeBle)
        .map(|(alias, settings)| {
            let match_name = &settings.selector.match_name;
            let link = BleLink::new(runtime.clone(), match_name, Model::from_text(match_name));
            GoveeDevice::new(alias.clone(), link)
        })
        .collect()
}

/// A light seen advertising, with the entry it would get.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenLight {
    /// Alias derived from the advertised name.
    pub alias: DeviceAlias,
    /// The advertised name, stored as `match_name`.
    pub name: String,
    /// Model code from the name, if any.
    pub model_code: Option<String>,
    /// Whether this backend can drive the model.
    pub supported: bool,
    /// Whether a configured device already claims it.
    pub configured: bool,
    /// Whether the light uses the encrypted channel.
    pub encrypted: bool,
    /// Signal strength, when reported.
    pub rssi: Option<i16>,
}

/// Scans for `duration` and reports every Govee light heard, marking those
/// a configured device already matches.
///
/// # Errors
/// [`GoveeError`] when Bluetooth is unavailable or the scan fails.
pub fn scan_lights(
    profile: &LightingProfile,
    runtime: &SharedRuntime,
    duration: Duration,
) -> Result<Vec<SeenLight>, GoveeError> {
    let runtime = BleRuntime::shared(runtime)?;
    let mut lights: Vec<SeenLight> = runtime
        .scan(duration)?
        .into_iter()
        .map(|(_, advert)| advert)
        .filter(|advert| is_govee_name(&advert.name))
        .map(|advert| seen_light(profile, &advert))
        .collect();
    lights.sort_by(|a, b| a.name.cmp(&b.name));
    lights.dedup_by(|a, b| a.name == b.name);
    Ok(lights)
}

fn seen_light(profile: &LightingProfile, advert: &Advertisement) -> SeenLight {
    let configured = profile
        .devices
        .values()
        .any(|settings| settings.selector.matches(Backend::GoveeBle, &advert.name));
    SeenLight {
        alias: DeviceAlias::derive_from(&advert.name),
        name: advert.name.clone(),
        model_code: model_code(&advert.name).map(str::to_owned),
        supported: Model::from_text(&advert.name).is_some(),
        configured,
        encrypted: advert.encrypted,
        rssi: advert.rssi,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use argb_domain::{DeviceSelector, DeviceSettings, LightingMode, Rgb, StateDefaults};

    use super::*;
    use crate::transport::Link;

    fn profile(match_name: &str) -> LightingProfile {
        let mode = LightingMode::Static {
            color: Rgb::WHITE,
            brightness: argb_domain::Brightness::MAX,
        };
        LightingProfile {
            defaults: StateDefaults {
                active: mode,
                sleep: mode,
            },
            devices: BTreeMap::from([(
                DeviceAlias::new("hexa").unwrap(),
                DeviceSettings {
                    selector: DeviceSelector {
                        backend: Backend::GoveeBle,
                        match_name: match_name.into(),
                    },
                    overrides: BTreeMap::new(),
                },
            )]),
        }
    }

    #[test]
    fn a_seen_light_is_described_and_matched_against_the_profile() {
        let advert = Advertisement {
            name: "Govee_H6061_7867".into(),
            encrypted: false,
            rssi: Some(-50),
        };
        let light = seen_light(&profile("H6061"), &advert);
        assert_eq!(light.alias.as_str(), "govee-h6061-7867");
        assert_eq!(light.model_code.as_deref(), Some("H6061"));
        assert!(light.supported);
        assert!(light.configured);

        let other = Advertisement {
            name: "Govee_H5151_2737".into(),
            encrypted: false,
            rssi: None,
        };
        let light = seen_light(&profile("H6061"), &other);
        assert!(!light.supported);
        assert!(!light.configured);
    }

    #[test]
    fn configured_devices_come_from_govee_entries_only() {
        let runtime: SharedRuntime = SharedRuntime::default();
        let devices = configured_devices(&profile("H6076"), &runtime);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].link().model(), Some(Model::LyraFloorLamp));
    }
}
