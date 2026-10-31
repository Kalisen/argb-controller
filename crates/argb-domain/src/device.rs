//! Device identity, selection and capabilities.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The subsystem that talks to a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    /// Devices exposed by an OpenRGB SDK server (Phase 1).
    #[serde(rename = "openrgb")]
    OpenRgb,
    /// Govee devices reached over Bluetooth LE (Phase 2).
    GoveeBle,
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Backend::OpenRgb => f.write_str("openrgb"),
            Backend::GoveeBle => f.write_str("govee_ble"),
        }
    }
}

/// A user-chosen short name for a device, e.g. `ram` or `gpu`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DeviceAlias(String);

/// Error returned for an alias that is empty or uses disallowed characters.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("device alias {0:?} must be 1 to {max} characters of a-z, 0-9, '-' or '_'", max = DeviceAlias::MAX_LEN)]
pub struct DeviceAliasError(String);

impl DeviceAlias {
    /// Longest accepted alias.
    pub const MAX_LEN: usize = 32;

    /// Validates and builds an alias.
    ///
    /// # Errors
    /// Returns [`DeviceAliasError`] when the alias is empty, too long, or
    /// contains anything other than lower-case ASCII letters, digits, `-` or `_`.
    pub fn new(alias: impl Into<String>) -> Result<Self, DeviceAliasError> {
        let alias = alias.into();
        let valid_char =
            |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_';
        if alias.is_empty() || alias.len() > Self::MAX_LEN || !alias.chars().all(valid_char) {
            return Err(DeviceAliasError(alias));
        }
        Ok(Self(alias))
    }

    /// Alias used when `derive_from` is given a name with no usable characters.
    pub const FALLBACK: &'static str = "device";

    /// Builds an alias from a free-form device name: lower-cased, runs of
    /// anything but letters and digits collapsed into one `-`, trimmed and
    /// truncated to [`Self::MAX_LEN`]. Names with nothing usable become
    /// [`Self::FALLBACK`].
    #[must_use]
    pub fn derive_from(name: &str) -> Self {
        let mut alias = String::with_capacity(name.len());
        for c in name.chars() {
            if c.is_ascii_alphanumeric() {
                alias.push(c.to_ascii_lowercase());
            } else if !alias.ends_with('-') {
                alias.push('-');
            }
        }
        alias.truncate(Self::MAX_LEN);
        let trimmed = alias.trim_matches('-');
        if trimmed.is_empty() {
            Self(Self::FALLBACK.to_owned())
        } else {
            Self(trimmed.to_owned())
        }
    }

    /// The alias text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for DeviceAlias {
    type Error = DeviceAliasError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<DeviceAlias> for String {
    fn from(value: DeviceAlias) -> Self {
        value.0
    }
}

impl std::fmt::Display for DeviceAlias {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// How a configured device is matched to a device reported by a backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSelector {
    /// Backend that owns the device.
    pub backend: Backend,
    /// Case-insensitive substring of the name the backend reports.
    pub match_name: String,
}

impl DeviceSelector {
    /// Whether a device reported by `backend` under `reported_name` is this device.
    #[must_use]
    pub fn matches(&self, backend: Backend, reported_name: &str) -> bool {
        self.backend == backend
            && reported_name
                .to_ascii_lowercase()
                .contains(&self.match_name.to_ascii_lowercase())
    }
}

/// What a device can do natively. Drives the engine's choice between hardware
/// modes and software-driven direct control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent capability flags, not a state machine"
)]
pub struct DeviceCapabilities {
    /// Per-frame color streaming from the host.
    pub direct: bool,
    /// A hardware static mode with a true brightness control.
    pub hardware_brightness: bool,
    /// A hardware breathing/pulse mode that runs without the host.
    pub hardware_pulse: bool,
    /// The device can store its current mode so it survives power cycles.
    pub persistent_memory: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_accepts_lowercase_digits_dash_underscore() {
        assert!(DeviceAlias::new("ram-2_b").is_ok());
    }

    #[test]
    fn alias_rejects_empty_uppercase_and_long_values() {
        assert!(DeviceAlias::new("").is_err());
        assert!(DeviceAlias::new("RAM").is_err());
        assert!(DeviceAlias::new("a".repeat(DeviceAlias::MAX_LEN + 1)).is_err());
    }

    #[test]
    fn derive_from_normalizes_reported_names() {
        assert_eq!(
            DeviceAlias::derive_from("Corsair Vengeance RGB DDR5").as_str(),
            "corsair-vengeance-rgb-ddr5"
        );
        assert_eq!(
            DeviceAlias::derive_from("  ASUS TUF (4080) ").as_str(),
            "asus-tuf-4080"
        );
        assert_eq!(
            DeviceAlias::derive_from("***").as_str(),
            DeviceAlias::FALLBACK
        );
        let long = DeviceAlias::derive_from(&"a".repeat(DeviceAlias::MAX_LEN + 10));
        assert_eq!(long.as_str().len(), DeviceAlias::MAX_LEN);
        assert!(DeviceAlias::new(long.as_str()).is_ok());
    }

    #[test]
    fn selector_matches_case_insensitive_substring_on_same_backend() {
        let selector = DeviceSelector {
            backend: Backend::OpenRgb,
            match_name: "vengeance".into(),
        };
        assert!(selector.matches(Backend::OpenRgb, "Corsair Vengeance RGB DDR5"));
        assert!(!selector.matches(Backend::OpenRgb, "Gigabyte RGB Fusion 2 USB"));
        assert!(!selector.matches(Backend::GoveeBle, "Corsair Vengeance RGB DDR5"));
    }
}
