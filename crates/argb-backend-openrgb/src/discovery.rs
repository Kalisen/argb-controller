//! Matching the controllers a server reports to the devices a profile names.

use std::collections::BTreeSet;

use argb_domain::{Backend, DeviceAlias, LightingProfile};
use tracing::warn;

use crate::description::ControllerDescription;

/// Separator between a derived alias and the counter that makes it unique.
const DUPLICATE_SEPARATOR: char = '-';

/// One controller and the alias it will be driven under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    /// Controller index on the server.
    pub index: u32,
    /// Alias the engine will look the device up by.
    pub alias: DeviceAlias,
    /// Whether the alias comes from the profile (true) or was derived from
    /// the controller name (false, so the device follows the defaults).
    pub configured: bool,
}

/// Pairs every controller with an alias.
///
/// A controller takes the first configured OpenRGB device whose selector
/// matches its name; each configured alias binds at most one controller.
/// Unmatched controllers get an alias derived from their name, made unique
/// with a numeric suffix when needed.
#[must_use]
#[expect(
    clippy::missing_panics_doc,
    reason = "the only expect() is on a controller count the server sent as u32"
)]
pub fn assign_aliases(
    profile: &LightingProfile,
    controllers: &[ControllerDescription],
) -> Vec<Assignment> {
    let mut taken = BTreeSet::new();
    let mut assignments = Vec::with_capacity(controllers.len());

    for (index, controller) in controllers.iter().enumerate() {
        let index = u32::try_from(index).expect("controller count fits in u32");
        let configured = profile.devices.iter().find(|(alias, settings)| {
            !taken.contains(*alias)
                && settings
                    .selector
                    .matches(Backend::OpenRgb, &controller.name)
        });
        let (alias, configured) = match configured {
            Some((alias, _)) => (alias.clone(), true),
            None => (
                unique_alias(DeviceAlias::derive_from(&controller.name), &taken),
                false,
            ),
        };
        taken.insert(alias.clone());
        assignments.push(Assignment {
            index,
            alias,
            configured,
        });
    }

    for (alias, settings) in &profile.devices {
        if settings.selector.backend == Backend::OpenRgb && !taken.contains(alias) {
            warn!(%alias, "configured device matched no OpenRGB controller");
        }
    }
    assignments
}

#[expect(
    clippy::maybe_infinite_iter,
    reason = "taken is finite, so some counter is always free"
)]
fn unique_alias(base: DeviceAlias, taken: &BTreeSet<DeviceAlias>) -> DeviceAlias {
    if !taken.contains(&base) {
        return base;
    }
    (2..)
        .map(|n| {
            let suffix = format!("{DUPLICATE_SEPARATOR}{n}");
            let mut text = base.as_str().to_owned();
            text.truncate(DeviceAlias::MAX_LEN - suffix.len());
            DeviceAlias::new(format!(
                "{}{suffix}",
                text.trim_end_matches(DUPLICATE_SEPARATOR)
            ))
            .expect("truncated alias plus numeric suffix stays valid")
        })
        .find(|candidate| !taken.contains(candidate))
        .expect("an unbounded counter eventually yields a free alias")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use argb_domain::{
        Brightness, DeviceSelector, DeviceSettings, LightingMode, PowerState, Rgb, StateDefaults,
    };

    use super::*;
    use crate::description::tests::sample_controller;

    fn named(name: &str) -> ControllerDescription {
        ControllerDescription {
            name: name.into(),
            ..sample_controller()
        }
    }

    fn profile(devices: &[(&str, Backend, &str)]) -> LightingProfile {
        let mode = LightingMode::Static {
            color: Rgb::WHITE,
            brightness: Brightness::MAX,
        };
        LightingProfile {
            defaults: StateDefaults {
                active: mode,
                sleep: mode,
            },
            devices: devices
                .iter()
                .map(|(alias, backend, match_name)| {
                    (
                        DeviceAlias::new(*alias).unwrap(),
                        DeviceSettings {
                            selector: DeviceSelector {
                                backend: *backend,
                                match_name: (*match_name).to_owned(),
                            },
                            overrides: BTreeMap::from([(PowerState::Active, mode)]),
                        },
                    )
                })
                .collect(),
        }
    }

    fn aliases(assignments: &[Assignment]) -> Vec<(u32, &str, bool)> {
        assignments
            .iter()
            .map(|a| (a.index, a.alias.as_str(), a.configured))
            .collect()
    }

    #[test]
    fn configured_selectors_win_and_the_rest_are_derived() {
        let profile = profile(&[
            ("ram", Backend::OpenRgb, "vengeance"),
            ("gpu", Backend::OpenRgb, "4080"),
            ("lamp", Backend::GoveeBle, "H6076"),
        ]);
        let controllers = [
            named("Gigabyte RGB Fusion 2 USB"),
            named("Corsair Vengeance RGB DDR5"),
            named("ASUS TUF RTX 4080"),
        ];
        let assignments = assign_aliases(&profile, &controllers);
        assert_eq!(
            aliases(&assignments),
            vec![
                (0, "gigabyte-rgb-fusion-2-usb", false),
                (1, "ram", true),
                (2, "gpu", true)
            ]
        );
    }

    #[test]
    fn an_alias_binds_one_controller_and_duplicates_get_suffixes() {
        let profile = profile(&[("ram", Backend::OpenRgb, "vengeance")]);
        let controllers = [
            named("Corsair Vengeance RGB DDR5"),
            named("Corsair Vengeance RGB DDR5"),
            named("Corsair Vengeance RGB DDR5"),
        ];
        let assignments = assign_aliases(&profile, &controllers);
        assert_eq!(
            aliases(&assignments),
            vec![
                (0, "ram", true),
                (1, "corsair-vengeance-rgb-ddr5", false),
                (2, "corsair-vengeance-rgb-ddr5-2", false)
            ]
        );
    }

    #[test]
    fn suffixed_aliases_stay_within_the_length_limit() {
        let long_name = "x".repeat(DeviceAlias::MAX_LEN * 2);
        let controllers = [named(&long_name), named(&long_name)];
        let assignments = assign_aliases(&profile(&[]), &controllers);
        assert!(assignments[1].alias.as_str().ends_with("-2"));
        assert_eq!(assignments[1].alias.as_str().len(), DeviceAlias::MAX_LEN);
    }
}
