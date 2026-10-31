//! The software pulse waveform.

use std::f64::consts::TAU;
use std::time::Duration;

use argb_domain::{PulsePeriod, Rgb};

/// Frames per second written to devices in direct mode while pulsing.
pub const DEFAULT_TICK_RATE_HZ: u32 = 30;

/// A raised-cosine breathing curve: off at the start of a cycle, full at the
/// middle, off again at the end. Smooth at both ends, which is what makes it
/// read as breathing rather than blinking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PulseWaveform {
    period: Duration,
}

impl PulseWaveform {
    /// A waveform with the given cycle length.
    #[must_use]
    pub const fn new(period: PulsePeriod) -> Self {
        Self {
            period: period.as_duration(),
        }
    }

    /// Intensity in `0.0..=1.0` at `elapsed` since the shared clock started.
    ///
    /// Every pulsing device is sampled from the same `elapsed`, so devices
    /// with the same period stay in phase and devices with different periods
    /// still start each cycle together at the least common multiple.
    #[must_use]
    pub fn level_at(&self, elapsed: Duration) -> f32 {
        let period = self.period.as_secs_f64();
        let phase = (elapsed.as_secs_f64() % period) / period;
        // The cosine term is within [-1, 1], so the result is within [0, 1].
        #[allow(clippy::cast_possible_truncation)]
        let level = (0.5 - 0.5 * (TAU * phase).cos()) as f32;
        level
    }

    /// `base` scaled by [`Self::level_at`].
    #[must_use]
    pub fn color_at(&self, base: Rgb, elapsed: Duration) -> Rgb {
        base.scaled(self.level_at(elapsed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waveform() -> PulseWaveform {
        PulseWaveform::new(PulsePeriod::from_millis(2000).unwrap())
    }

    #[test]
    fn starts_dark_peaks_at_half_period_and_repeats() {
        let wave = waveform();
        assert!(wave.level_at(Duration::ZERO).abs() < 1e-6);
        assert!((wave.level_at(Duration::from_millis(1000)) - 1.0).abs() < 1e-6);
        assert!(wave.level_at(Duration::from_millis(2000)).abs() < 1e-6);
        assert!((wave.level_at(Duration::from_millis(500)) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn color_follows_level() {
        let wave = waveform();
        assert_eq!(
            wave.color_at(Rgb::WHITE, Duration::from_millis(1000)),
            Rgb::WHITE
        );
        assert_eq!(wave.color_at(Rgb::WHITE, Duration::ZERO), Rgb::BLACK);
    }
}
