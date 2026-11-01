//! `LightingDevice` for one Sonix keyboard.
//!
//! Every apply rewrites the whole lighting block, which the firmware stores
//! in flash: the keyboard keeps the state across a power cycle on its own,
//! so `Persist` needs no action. For the same reason the block is written
//! only when it differs from the last one written, and the keyboard offers
//! no direct mode: a session takes close to a second and would wear the
//! flash at the pulse tick rate.

use argb_domain::{Brightness, DeviceAlias, DeviceCapabilities, PulsePeriod, Rgb};
use argb_engine::{DeviceError, LightingDevice, Persist};
use tracing::debug;

use crate::error::SonixError;
use crate::protocol::{BrightnessLevel, Command, EffectRecord, LightingBlock, Speed};
use crate::transport::{Link, write_section};

/// What every supported keyboard can do: a static color with a true
/// brightness, its own breathing effect, a state kept in flash, and no
/// streaming.
pub const CAPABILITIES: DeviceCapabilities = DeviceCapabilities {
    direct: false,
    hardware_brightness: true,
    hardware_pulse: true,
    persistent_memory: true,
};

/// One keyboard under an alias.
pub struct SonixDevice<L: Link> {
    alias: DeviceAlias,
    link: L,
    /// The block last written successfully, so an identical apply (a retry,
    /// a rebuild on the same state) does not rewrite the flash.
    written: Option<LightingBlock>,
}

impl<L: Link> SonixDevice<L> {
    /// A device over `link`.
    #[must_use]
    pub fn new(alias: DeviceAlias, link: L) -> Self {
        Self {
            alias,
            link,
            written: None,
        }
    }

    /// The link, for inspection.
    #[must_use]
    pub fn link(&self) -> &L {
        &self.link
    }

    fn error(&self, error: &SonixError) -> DeviceError {
        DeviceError::Backend {
            alias: self.alias.clone(),
            message: error.to_string(),
        }
    }

    fn write(&mut self, block: LightingBlock) -> Result<(), DeviceError> {
        if self.written.as_ref() == Some(&block) {
            debug!(alias = %self.alias, "lighting unchanged; not rewritten");
            return Ok(());
        }
        match write_section(&mut self.link, Command::Lighting, &block.pages()) {
            Ok(()) => {
                self.written = Some(block);
                Ok(())
            }
            Err(error) => {
                self.written = None;
                Err(self.error(&error))
            }
        }
    }
}

impl<L: Link> LightingDevice for SonixDevice<L> {
    fn alias(&self) -> &DeviceAlias {
        &self.alias
    }

    fn capabilities(&self) -> DeviceCapabilities {
        CAPABILITIES
    }

    /// The static effect at the nearest brightness level; zero brightness
    /// writes a block with no active record, which turns the LEDs off.
    fn apply_static(
        &mut self,
        color: Rgb,
        brightness: Brightness,
        _persist: Persist,
    ) -> Result<(), DeviceError> {
        let level = BrightnessLevel::from_brightness(brightness);
        let block = if level == BrightnessLevel::OFF {
            LightingBlock::off()
        } else {
            LightingBlock::running(EffectRecord::static_color(color, level))
        };
        self.write(block)
    }

    /// The keyboard's breathing effect at the speed nearest to `period`.
    fn apply_hardware_pulse(
        &mut self,
        color: Rgb,
        period: PulsePeriod,
        _persist: Persist,
    ) -> Result<(), DeviceError> {
        self.write(LightingBlock::running(EffectRecord::breathing(
            color,
            Speed::from_period(period),
        )))
    }

    fn begin_direct(&mut self) -> Result<(), DeviceError> {
        Err(DeviceError::Unsupported {
            alias: self.alias.clone(),
            capability: "direct mode",
        })
    }

    fn write_direct(&mut self, _color: Rgb) -> Result<(), DeviceError> {
        Err(DeviceError::Unsupported {
            alias: self.alias.clone(),
            capability: "direct mode",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{LightingBlock, REPORT_LEN};
    use crate::transport::fake::{Answer, FakeLink};

    const TURQUOISE: Rgb = Rgb::new(0x40, 0xE0, 0xD0);

    fn device() -> SonixDevice<FakeLink> {
        SonixDevice::new(DeviceAlias::new("keyboard").unwrap(), FakeLink::new())
    }

    /// The active record of the last block sent, read back from the pages.
    fn active_record(link: &FakeLink) -> [u8; EffectRecord::LEN] {
        let pages: Vec<&[u8; REPORT_LEN]> = link
            .sent
            .iter()
            .rev()
            .skip(2)
            .take(LightingBlock::PAGES)
            .collect();
        let page = LightingBlock::ACTIVE_OFFSET / crate::protocol::PAGE_LEN;
        let offset = 1 + LightingBlock::ACTIVE_OFFSET % crate::protocol::PAGE_LEN;
        let mut record = [0; EffectRecord::LEN];
        record.copy_from_slice(
            &pages[LightingBlock::PAGES - 1 - page][offset..offset + EffectRecord::LEN],
        );
        record
    }

    #[test]
    fn a_static_apply_writes_the_block_with_the_static_record_active() {
        let mut device = device();
        device
            .apply_static(
                TURQUOISE,
                Brightness::new(50).unwrap(),
                Persist::SaveToDevice,
            )
            .unwrap();
        assert_eq!(device.link.sent.len(), 2 + LightingBlock::PAGES + 2);
        assert_eq!(
            active_record(&device.link),
            [
                0x01, 0x40, 0xE0, 0xD0, 0, 0, 0, 0, 0, 3, 6, 0, 0, 0, 0xAA, 0x55
            ]
        );
    }

    #[test]
    fn zero_brightness_turns_the_leds_off() {
        let mut device = device();
        device
            .apply_static(TURQUOISE, Brightness::MIN, Persist::Volatile)
            .unwrap();
        assert_eq!(active_record(&device.link), [0; EffectRecord::LEN]);
    }

    #[test]
    fn the_same_lighting_is_not_written_twice() {
        let mut device = device();
        device
            .apply_static(TURQUOISE, Brightness::MAX, Persist::Volatile)
            .unwrap();
        let sent = device.link.sent.len();
        device
            .apply_static(TURQUOISE, Brightness::MAX, Persist::SaveToDevice)
            .unwrap();
        assert_eq!(device.link.sent.len(), sent);
        device
            .apply_static(Rgb::WHITE, Brightness::MAX, Persist::Volatile)
            .unwrap();
        assert_eq!(device.link.sent.len(), 2 * sent);
    }

    #[test]
    fn a_hardware_pulse_runs_the_breathing_effect() {
        let mut device = device();
        let period = PulsePeriod::from_millis(PulsePeriod::MAX_MS).unwrap();
        device
            .apply_hardware_pulse(TURQUOISE, period, Persist::SaveToDevice)
            .unwrap();
        let record = active_record(&device.link);
        assert_eq!(record[0], 0x02);
        assert_eq!(record[10], Speed::SLOWEST.value());
    }

    #[test]
    fn a_failed_write_is_a_backend_error_and_the_next_apply_writes_again() {
        let mut device = device();
        device.link.answer = Answer::Refuse;
        assert!(matches!(
            device.apply_static(TURQUOISE, Brightness::MAX, Persist::Volatile),
            Err(DeviceError::Backend { .. })
        ));
        assert!(!device.link.is_open());
        device.link.answer = Answer::Ack;
        device
            .apply_static(TURQUOISE, Brightness::MAX, Persist::Volatile)
            .unwrap();
        assert_eq!(device.link.opens, 2);
    }

    #[test]
    fn direct_mode_is_unsupported() {
        let mut device = device();
        assert!(matches!(
            device.begin_direct(),
            Err(DeviceError::Unsupported { .. })
        ));
        assert!(matches!(
            device.write_direct(TURQUOISE),
            Err(DeviceError::Unsupported { .. })
        ));
    }
}
