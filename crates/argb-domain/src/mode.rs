//! Lighting modes a device can show.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::color::{Brightness, Rgb};

/// Duration of one full pulse cycle (dim, bright, dim).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct PulsePeriod {
    millis: u64,
}

/// Error returned when a pulse period is outside the supported range.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("pulse period must be between {min_ms} and {max_ms} ms, got {found_ms}")]
pub struct PulsePeriodError {
    /// The rejected value in milliseconds.
    pub found_ms: u64,
    /// Shortest accepted period in milliseconds.
    pub min_ms: u64,
    /// Longest accepted period in milliseconds.
    pub max_ms: u64,
}

impl PulsePeriod {
    /// Shortest period, in milliseconds, that still reads as a pulse rather than a flicker.
    pub const MIN_MS: u64 = 500;
    /// Longest period, in milliseconds; beyond this the light looks static.
    pub const MAX_MS: u64 = 60_000;
    /// [`Self::MIN_MS`] as a duration.
    pub const MIN: Duration = Duration::from_millis(Self::MIN_MS);
    /// [`Self::MAX_MS`] as a duration.
    pub const MAX: Duration = Duration::from_millis(Self::MAX_MS);

    /// Builds a period from milliseconds.
    ///
    /// # Errors
    /// Returns [`PulsePeriodError`] when outside [`Self::MIN_MS`]..=[`Self::MAX_MS`].
    pub const fn from_millis(millis: u64) -> Result<Self, PulsePeriodError> {
        if millis < Self::MIN_MS || millis > Self::MAX_MS {
            return Err(PulsePeriodError {
                found_ms: millis,
                min_ms: Self::MIN_MS,
                max_ms: Self::MAX_MS,
            });
        }
        Ok(Self { millis })
    }

    /// The period as a [`Duration`].
    #[must_use]
    pub const fn as_duration(self) -> Duration {
        Duration::from_millis(self.millis)
    }

    /// The period in milliseconds.
    #[must_use]
    pub const fn as_millis(self) -> u64 {
        self.millis
    }
}

impl TryFrom<u64> for PulsePeriod {
    type Error = PulsePeriodError;

    fn try_from(ms: u64) -> Result<Self, Self::Error> {
        Self::from_millis(ms)
    }
}

impl From<PulsePeriod> for u64 {
    fn from(value: PulsePeriod) -> Self {
        value.as_millis()
    }
}

/// What a device should display.
///
/// Serialized as an internally tagged table, which maps directly onto a TOML
/// table such as `mode = "pulse"`, `color = "#FF0000"`, `period_ms = 8000`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum LightingMode {
    /// One constant color at a given intensity.
    Static {
        /// The color to show.
        color: Rgb,
        /// Intensity; applied as true hardware brightness where the device supports it.
        brightness: Brightness,
    },
    /// One color breathing between off and full over `period`.
    Pulse {
        /// The color at the peak of the pulse.
        color: Rgb,
        /// Length of one full cycle.
        #[serde(rename = "period_ms")]
        period: PulsePeriod,
    },
}

impl LightingMode {
    /// The base color of the mode.
    #[must_use]
    pub const fn color(&self) -> Rgb {
        match self {
            LightingMode::Static { color, .. } | LightingMode::Pulse { color, .. } => *color,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn period_enforces_bounds() {
        assert!(PulsePeriod::from_millis(500).is_ok());
        assert!(PulsePeriod::from_millis(60_000).is_ok());
        assert_eq!(
            PulsePeriod::from_millis(499),
            Err(PulsePeriodError {
                found_ms: 499,
                min_ms: 500,
                max_ms: 60_000
            })
        );
        assert!(PulsePeriod::from_millis(60_001).is_err());
    }

    #[test]
    fn mode_exposes_its_color() {
        let color = Rgb::new(1, 2, 3);
        let static_mode = LightingMode::Static {
            color,
            brightness: Brightness::MAX,
        };
        let pulse = LightingMode::Pulse {
            color,
            period: PulsePeriod::from_millis(1000).unwrap(),
        };
        assert_eq!(static_mode.color(), color);
        assert_eq!(pulse.color(), color);
    }
}
