//! Colors and brightness.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// A 24-bit RGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
}

/// Error returned when a color string cannot be parsed.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ColorParseError {
    /// The string is not six hexadecimal digits, optionally prefixed with `#`.
    #[error("expected a color like #RRGGBB, got {0:?}")]
    InvalidFormat(String),
}

impl Rgb {
    /// Fully off.
    pub const BLACK: Rgb = Rgb::new(0, 0, 0);
    /// Fully on, all channels.
    pub const WHITE: Rgb = Rgb::new(255, 255, 255);

    /// Builds a color from its channels.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Parses `#RRGGBB` or `RRGGBB`, case-insensitive.
    ///
    /// # Errors
    /// Returns [`ColorParseError::InvalidFormat`] for anything else.
    pub fn from_hex(text: &str) -> Result<Self, ColorParseError> {
        let digits = text.strip_prefix('#').unwrap_or(text);
        if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ColorParseError::InvalidFormat(text.to_owned()));
        }
        let channel = |range: std::ops::Range<usize>| {
            u8::from_str_radix(&digits[range], 16)
                .map_err(|_| ColorParseError::InvalidFormat(text.to_owned()))
        };
        Ok(Self::new(channel(0..2)?, channel(2..4)?, channel(4..6)?))
    }

    /// Formats the color as `#RRGGBB` in upper case.
    #[must_use]
    pub fn to_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }

    /// Scales every channel by `level`, clamped to `0.0..=1.0`.
    ///
    /// This is how brightness is applied in software to devices whose direct
    /// mode has no hardware brightness control.
    #[must_use]
    pub fn scaled(self, level: f32) -> Self {
        let level = level.clamp(0.0, 1.0);
        // Rounding a value in 0..=255 back to u8 cannot truncate.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let scale = |channel: u8| (f32::from(channel) * level).round() as u8;
        Self::new(scale(self.r), scale(self.g), scale(self.b))
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for Rgb {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Rgb::from_hex(&text).map_err(serde::de::Error::custom)
    }
}

/// Brightness as a whole percentage, `0..=100`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Brightness(u8);

/// Error returned when a brightness percentage is out of range.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("brightness must be between {min} and {max} percent, got {found}")]
pub struct BrightnessError {
    /// The rejected value.
    pub found: u8,
    /// Lowest accepted value.
    pub min: u8,
    /// Highest accepted value.
    pub max: u8,
}

impl Brightness {
    /// Lowest percentage.
    pub const MIN: Brightness = Brightness(0);
    /// Highest percentage.
    pub const MAX: Brightness = Brightness(100);

    /// Builds a brightness from a percentage.
    ///
    /// # Errors
    /// Returns [`BrightnessError`] when `percent` exceeds 100.
    pub fn new(percent: u8) -> Result<Self, BrightnessError> {
        if percent > Self::MAX.0 {
            return Err(BrightnessError {
                found: percent,
                min: Self::MIN.0,
                max: Self::MAX.0,
            });
        }
        Ok(Self(percent))
    }

    /// The percentage value.
    #[must_use]
    pub const fn percent(self) -> u8 {
        self.0
    }

    /// The brightness as a level in `0.0..=1.0`.
    #[must_use]
    pub fn as_level(self) -> f32 {
        f32::from(self.0) / f32::from(Self::MAX.0)
    }
}

impl TryFrom<u8> for Brightness {
    type Error = BrightnessError;

    fn try_from(percent: u8) -> Result<Self, Self::Error> {
        Self::new(percent)
    }
}

impl From<Brightness> for u8 {
    fn from(value: Brightness) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_with_and_without_prefix() {
        assert_eq!(Rgb::from_hex("#40E0D0"), Ok(Rgb::new(0x40, 0xE0, 0xD0)));
        assert_eq!(Rgb::from_hex("40e0d0"), Ok(Rgb::new(0x40, 0xE0, 0xD0)));
    }

    #[test]
    fn rejects_malformed_hex() {
        assert!(matches!(
            Rgb::from_hex("#FFF"),
            Err(ColorParseError::InvalidFormat(_))
        ));
        assert!(matches!(
            Rgb::from_hex("#GGGGGG"),
            Err(ColorParseError::InvalidFormat(_))
        ));
    }

    #[test]
    fn hex_round_trips_upper_case() {
        assert_eq!(Rgb::from_hex("#40e0d0").unwrap().to_hex(), "#40E0D0");
    }

    #[test]
    fn scaling_rounds_and_clamps() {
        assert_eq!(Rgb::WHITE.scaled(0.5), Rgb::new(128, 128, 128));
        assert_eq!(Rgb::WHITE.scaled(2.0), Rgb::WHITE);
        assert_eq!(Rgb::WHITE.scaled(-1.0), Rgb::BLACK);
    }

    #[test]
    fn brightness_enforces_bounds() {
        assert_eq!(Brightness::new(100), Ok(Brightness::MAX));
        assert_eq!(
            Brightness::new(101),
            Err(BrightnessError {
                found: 101,
                min: 0,
                max: 100
            })
        );
        assert!((Brightness::new(50).unwrap().as_level() - 0.5).abs() < f32::EPSILON);
    }
}
