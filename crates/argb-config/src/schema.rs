//! Serde mirror of the TOML configuration document.

use std::collections::BTreeMap;

use argb_domain::{
    Backend, DeviceAlias, DeviceSelector, DeviceSettings, LightingMode, LightingProfile,
    PowerState, StateDefaults,
};
use serde::{Deserialize, Serialize};

use crate::SCHEMA_VERSION;
use crate::store::ConfigError;

/// The whole configuration file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    /// Version of this document's layout. Must equal [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// OpenRGB SDK connection.
    #[serde(default)]
    pub openrgb: OpenRgbSettings,
    /// What devices show before the operating system runs.
    #[serde(default)]
    pub boot: BootSettings,
    /// Default modes per power state.
    pub states: StateDefaultsSchema,
    /// Configured devices by alias.
    #[serde(default)]
    pub devices: BTreeMap<DeviceAlias, DeviceSchema>,
}

/// Connection to the OpenRGB SDK server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenRgbSettings {
    /// Host name or address. Should stay on the loopback interface; the SDK is unauthenticated.
    #[serde(default = "OpenRgbSettings::default_host")]
    pub host: String,
    /// TCP port.
    #[serde(default = "OpenRgbSettings::default_port")]
    pub port: u16,
}

impl OpenRgbSettings {
    /// OpenRGB's loopback default.
    pub const DEFAULT_HOST: &'static str = "127.0.0.1";
    /// OpenRGB's default SDK port.
    pub const DEFAULT_PORT: u16 = 6742;

    fn default_host() -> String {
        Self::DEFAULT_HOST.to_owned()
    }

    const fn default_port() -> u16 {
        Self::DEFAULT_PORT
    }
}

impl Default for OpenRgbSettings {
    fn default() -> Self {
        Self {
            host: Self::default_host(),
            port: Self::default_port(),
        }
    }
}

/// Which state's lighting is stored in device memory for the boot sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootSettings {
    /// State whose modes are persisted to the devices.
    #[serde(default = "BootSettings::default_follow")]
    pub follow: PowerState,
}

impl BootSettings {
    /// By default devices boot showing the active-state lighting.
    pub const DEFAULT_FOLLOW: PowerState = PowerState::Active;

    const fn default_follow() -> PowerState {
        Self::DEFAULT_FOLLOW
    }
}

impl Default for BootSettings {
    fn default() -> Self {
        Self {
            follow: Self::DEFAULT_FOLLOW,
        }
    }
}

/// `[states.*]` tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateDefaultsSchema {
    /// `[states.active]`.
    pub active: LightingMode,
    /// `[states.sleep]`.
    pub sleep: LightingMode,
}

/// `[devices.<alias>]` table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSchema {
    /// Backend that owns the device.
    pub backend: Backend,
    /// Case-insensitive substring of the backend's reported device name.
    pub match_name: String,
    /// `[devices.<alias>.states.<state>]` overrides.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub states: BTreeMap<PowerState, LightingMode>,
}

/// Sleep pulse period of the starter configuration: a slow breath, four
/// seconds in and four out.
pub const STARTER_SLEEP_PERIOD_MS: u64 = 8000;

impl ConfigFile {
    /// Parses and validates a TOML document.
    ///
    /// # Errors
    /// Returns [`ConfigError::Parse`] for malformed TOML and the validation
    /// errors described on [`Self::validate`].
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    /// Serializes to TOML.
    ///
    /// # Errors
    /// Returns [`ConfigError::Serialize`] if serialization fails.
    pub fn to_toml(&self) -> Result<String, ConfigError> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Checks invariants that the type system does not capture.
    ///
    /// # Errors
    /// [`ConfigError::UnsupportedSchemaVersion`] when the version differs from
    /// [`SCHEMA_VERSION`]; [`ConfigError::Invalid`] for an empty `match_name`.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ConfigError::UnsupportedSchemaVersion {
                found: self.schema_version,
                supported: SCHEMA_VERSION,
            });
        }
        for (alias, device) in &self.devices {
            if device.match_name.trim().is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "device '{alias}' has an empty match_name"
                )));
            }
        }
        Ok(())
    }

    /// Converts the document into the domain profile.
    #[must_use]
    pub fn to_profile(&self) -> LightingProfile {
        let devices = self
            .devices
            .iter()
            .map(|(alias, device)| {
                let settings = DeviceSettings {
                    selector: DeviceSelector {
                        backend: device.backend,
                        match_name: device.match_name.clone(),
                    },
                    overrides: device.states.clone(),
                };
                (alias.clone(), settings)
            })
            .collect();
        LightingProfile {
            defaults: StateDefaults {
                active: self.states.active,
                sleep: self.states.sleep,
            },
            devices,
        }
    }

    /// A minimal valid document: turquoise static when active, slow red pulse asleep.
    #[must_use]
    #[expect(
        clippy::missing_panics_doc,
        reason = "the only expect() is on an in-range constant"
    )]
    pub fn starter() -> Self {
        use argb_domain::{Brightness, PulsePeriod, Rgb};
        Self {
            schema_version: SCHEMA_VERSION,
            openrgb: OpenRgbSettings::default(),
            boot: BootSettings::default(),
            states: StateDefaultsSchema {
                active: LightingMode::Static {
                    color: Rgb::new(0x40, 0xE0, 0xD0),
                    brightness: Brightness::MAX,
                },
                sleep: LightingMode::Pulse {
                    color: Rgb::new(0xFF, 0, 0),
                    period: PulsePeriod::from_millis(STARTER_SLEEP_PERIOD_MS)
                        .expect("constant within bounds"),
                },
            },
            devices: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argb_domain::{Brightness, Rgb};

    #[test]
    fn a_device_without_overrides_serializes_without_a_states_table() {
        let mut config = ConfigFile::starter();
        config.devices.insert(
            DeviceAlias::new("ram").unwrap(),
            DeviceSchema {
                backend: Backend::OpenRgb,
                match_name: "Vengeance".into(),
                states: BTreeMap::new(),
            },
        );
        let text = config.to_toml().unwrap();
        assert!(text.contains("[devices.ram]"));
        assert!(!text.contains("[devices.ram.states]"));
        assert_eq!(ConfigFile::parse(&text).unwrap(), config);
    }

    const EXAMPLE: &str = include_str!("../../../config.example.toml");

    #[test]
    fn example_file_parses_and_resolves_overrides() {
        let config = ConfigFile::parse(EXAMPLE).expect("example must stay valid");
        let profile = config.to_profile();
        let gpu = DeviceAlias::new("gpu").unwrap();
        let ram = DeviceAlias::new("ram").unwrap();
        assert_eq!(
            profile.mode_for(&gpu, PowerState::Sleep),
            &LightingMode::Static {
                color: Rgb::BLACK,
                brightness: Brightness::MIN
            }
        );
        assert_eq!(
            profile.mode_for(&ram, PowerState::Sleep),
            &profile.defaults.sleep
        );
        assert_eq!(config.openrgb.port, OpenRgbSettings::DEFAULT_PORT);
        assert_eq!(config.boot.follow, PowerState::Active);
    }

    #[test]
    fn toml_round_trips() {
        let config = ConfigFile::parse(EXAMPLE).unwrap();
        let text = config.to_toml().unwrap();
        assert_eq!(ConfigFile::parse(&text).unwrap(), config);
    }

    #[test]
    fn rejects_unknown_fields() {
        let text = format!("{EXAMPLE}\n[typo]\nvalue = 1\n");
        assert!(matches!(
            ConfigFile::parse(&text),
            Err(ConfigError::Parse(_))
        ));
    }

    #[test]
    fn rejects_other_schema_versions() {
        let text = EXAMPLE.replace("schema_version = 1", "schema_version = 2");
        assert!(matches!(
            ConfigFile::parse(&text),
            Err(ConfigError::UnsupportedSchemaVersion {
                found: 2,
                supported: 1
            })
        ));
    }

    #[test]
    fn rejects_empty_match_name() {
        let text = EXAMPLE.replace("match_name = \"TUF\"", "match_name = \"  \"");
        assert!(matches!(
            ConfigFile::parse(&text),
            Err(ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn starter_is_valid() {
        ConfigFile::starter().validate().unwrap();
    }
}
