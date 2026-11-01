//! The feature-report protocol of the Sonix `DeviceDriver.exe` keyboards,
//! as captured from the Cloud Nine application and documented for the
//! sibling keyboards (see `docs/phase2-1-peripherals.md`).
//!
//! Every report is 64 payload bytes behind report id 0. A command is
//! `04 CMD`, with an argument at payload byte 8; the keyboard echoes it on
//! the next feature read with payload byte 3 set to [`ACK`]. A write is a
//! session: begin, select a section with its page count, the data pages,
//! commit, apply. The lighting section is one fixed block of 18 pages
//! ([`LightingBlock`]); a partial write leaves its active record empty and
//! the keyboard dark.

use std::time::Duration;

use argb_domain::{Brightness, PulsePeriod, Rgb};

/// Report id of every report; the keyboard uses unnumbered reports.
pub const REPORT_ID: u8 = 0;
/// A report on the wire: the report id followed by the payload.
pub type Report = [u8; REPORT_LEN];
/// Report length on the wire.
pub const REPORT_LEN: usize = 1 + PAGE_LEN;
/// Payload bytes of one report, and of one data page.
pub const PAGE_LEN: usize = 64;
/// Payload byte 0 of every command.
pub const COMMAND_MARKER: u8 = 0x04;
/// Payload byte the keyboard sets in its echo.
pub const ACK_OFFSET: usize = 3;
/// Echo value for an accepted command (`0xFF` is a refusal).
pub const ACK: u8 = 0x01;
/// Payload byte carrying a command's argument (the page count of a section).
pub const ARGUMENT_OFFSET: usize = 8;
/// Pause the vendor application keeps between reports.
pub const PACKET_GAP: Duration = Duration::from_millis(35);
/// How many times a command is sent before giving up on its acknowledgement.
pub const ACK_RETRIES: usize = 3;
/// Marker closing a record.
pub const RECORD_END: [u8; 2] = [0xAA, 0x55];

/// Commands (payload byte 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Command {
    /// Commit the pages written since [`Command::Begin`].
    Commit = 0x02,
    /// Select the lighting section; the argument is its page count.
    Lighting = 0x13,
    /// Open a configuration session.
    Begin = 0x18,
    /// Apply the committed configuration.
    Apply = 0xF0,
}

/// Builds a command report with `argument` at [`ARGUMENT_OFFSET`].
#[must_use]
pub fn command(command: Command, argument: u8) -> Report {
    let mut report = [0; REPORT_LEN];
    report[0] = REPORT_ID;
    report[1] = COMMAND_MARKER;
    report[2] = command as u8;
    report[1 + ARGUMENT_OFFSET] = argument;
    report
}

/// Whether an echo acknowledges the command it answers.
#[must_use]
pub fn acknowledged(echo: &Report) -> bool {
    echo[1 + ACK_OFFSET] == ACK
}

/// Effects, by the mode byte of their record. The ids follow the order of
/// the vendor application's visible effect list (not its string table):
/// the ones checked on the keyboard are named here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Effect {
    /// "Static color": one color on every key.
    Static = 0x01,
    /// "Falling": the color raining down the keyboard.
    Falling = 0x03,
    /// "Breath": one color breathing.
    Breathing = 0x05,
    /// "User defined": the per-key table of the lighting block.
    PerKey = 0x80,
}

impl Effect {
    /// Last mode byte the vendor application keeps a preset for.
    pub const LAST_PRESET: u8 = 0x13;
    /// First effect whose preset the application marks as rainbow.
    pub const FIRST_RAINBOW: u8 = 0x07;
}

/// Brightness as the keyboard stores it: 0 is off, [`BrightnessLevel::MAX`]
/// is what the application calls "High".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BrightnessLevel(u8);

impl BrightnessLevel {
    /// Off.
    pub const OFF: BrightnessLevel = BrightnessLevel(0);
    /// Highest level, the application's "High".
    pub const MAX: BrightnessLevel = BrightnessLevel(6);
    /// Level the application stores in the presets it writes.
    pub const PRESET: BrightnessLevel = BrightnessLevel(6);
    /// Level the application stores in the per-key preset.
    pub const PER_KEY_PRESET: BrightnessLevel = BrightnessLevel(5);

    /// The nearest level to a percentage; anything above zero lights up.
    #[must_use]
    pub fn from_brightness(brightness: Brightness) -> Self {
        if brightness == Brightness::MIN {
            return Self::OFF;
        }
        let scaled = u32::from(brightness.percent()) * u32::from(Self::MAX.0);
        let rounded = scaled.div_ceil(u32::from(Brightness::MAX.percent()));
        // The quotient is at most MAX.
        #[allow(clippy::cast_possible_truncation)]
        Self((rounded as u8).max(1))
    }

    /// A level as stored, clamped to [`Self::MAX`].
    #[must_use]
    pub const fn from_level(level: u8) -> Self {
        if level > Self::MAX.0 {
            Self::MAX
        } else {
            Self(level)
        }
    }

    /// The stored byte.
    #[must_use]
    pub const fn value(self) -> u8 {
        self.0
    }
}

/// Effect speed as the keyboard stores it. The application wrote 5, 6 and 9
/// for its slider; the mapping from a period is provisional until measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Speed(u8);

impl Speed {
    /// Slowest value seen from the application.
    pub const SLOWEST: Speed = Speed(5);
    /// Fastest value seen from the application.
    pub const FASTEST: Speed = Speed(9);
    /// Value the application stores in the presets it writes.
    pub const PRESET: Speed = Speed(6);

    /// The speed for a pulse period, linear in the logarithm of the period
    /// between [`PulsePeriod::MIN`] (fastest) and [`PulsePeriod::MAX`]
    /// (slowest).
    #[must_use]
    pub fn from_period(period: PulsePeriod) -> Self {
        let min = PulsePeriod::MIN.as_secs_f64().ln();
        let max = PulsePeriod::MAX.as_secs_f64().ln();
        let position = (period.as_duration().as_secs_f64().ln() - min) / (max - min);
        let span = f64::from(Self::FASTEST.0 - Self::SLOWEST.0);
        // position is within 0..=1, so the result is within SLOWEST..=FASTEST.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Self(Self::FASTEST.0 - (position * span).round() as u8)
    }

    /// A speed as stored, clamped to [`Self::FASTEST`].
    #[must_use]
    pub const fn from_value(value: u8) -> Self {
        if value > Self::FASTEST.0 {
            Self::FASTEST
        } else {
            Self(value)
        }
    }

    /// The stored byte.
    #[must_use]
    pub const fn value(self) -> u8 {
        self.0
    }
}

/// One preset or active record of the lighting block:
/// `MODE R G B 00 00 00 00 RAINBOW BRIGHTNESS SPEED DIRECTION 00 00 AA 55`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectRecord {
    /// Mode byte.
    pub mode: u8,
    /// The effect's color.
    pub color: Rgb,
    /// 1 when the effect cycles through the rainbow instead of `color`.
    pub rainbow: u8,
    /// Brightness level.
    pub brightness: BrightnessLevel,
    /// Effect speed.
    pub speed: Speed,
    /// 0 normal, 2 inverted on the directional effects.
    pub direction: u8,
}

impl EffectRecord {
    /// Length of a record.
    pub const LEN: usize = 16;
    const COLOR_OFFSET: usize = 1;
    const RAINBOW_OFFSET: usize = 8;
    const BRIGHTNESS_OFFSET: usize = 9;
    const SPEED_OFFSET: usize = 10;
    const DIRECTION_OFFSET: usize = 11;
    const END_OFFSET: usize = 14;

    /// A record for `effect` with the application's preset speed and no
    /// rainbow or direction.
    #[must_use]
    pub const fn new(effect: Effect, color: Rgb, brightness: BrightnessLevel) -> Self {
        Self {
            mode: effect as u8,
            color,
            rainbow: 0,
            brightness,
            speed: Speed::PRESET,
            direction: 0,
        }
    }

    /// One color on every key.
    #[must_use]
    pub const fn static_color(color: Rgb, brightness: BrightnessLevel) -> Self {
        Self::new(Effect::Static, color, brightness)
    }

    /// One color breathing at `speed`.
    #[must_use]
    pub const fn breathing(color: Rgb, speed: Speed) -> Self {
        let mut record = Self::new(Effect::Breathing, color, BrightnessLevel::MAX);
        record.speed = speed;
        record
    }

    /// The per-key table at `brightness`.
    #[must_use]
    pub const fn per_key(brightness: BrightnessLevel) -> Self {
        let mut record = Self::new(Effect::PerKey, Rgb::BLACK, brightness);
        record.speed = Speed(0);
        record
    }

    fn write_to(self, slot: &mut [u8]) {
        slot.fill(0);
        slot[0] = self.mode;
        slot[Self::COLOR_OFFSET] = self.color.r;
        slot[Self::COLOR_OFFSET + 1] = self.color.g;
        slot[Self::COLOR_OFFSET + 2] = self.color.b;
        slot[Self::RAINBOW_OFFSET] = self.rainbow;
        slot[Self::BRIGHTNESS_OFFSET] = self.brightness.0;
        slot[Self::SPEED_OFFSET] = self.speed.0;
        slot[Self::DIRECTION_OFFSET] = self.direction;
        slot[Self::END_OFFSET..].copy_from_slice(&RECORD_END);
    }
}

/// The block the vendor application writes into the lighting section on
/// every change: 18 pages, 1152 bytes.
///
/// | Offset | Content |
/// |---|---|
/// | 0 | 20 preset records: one per effect 1 to 19, then the per-key mode |
/// | 320 | 192 zero bytes |
/// | 512 | 144 per-key entries `80 R G B` |
/// | 1088 | the active record: a copy of the preset the keyboard runs |
/// | 1104 | zero to the end |
///
/// A block without an active record (all zero there) turns the LEDs off.
#[derive(Clone, PartialEq, Eq)]
pub struct LightingBlock {
    bytes: [u8; LightingBlock::LEN],
}

impl std::fmt::Debug for LightingBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LightingBlock")
            .field(
                "active",
                &&self.bytes[Self::ACTIVE_OFFSET..Self::ACTIVE_OFFSET + EffectRecord::LEN],
            )
            .finish_non_exhaustive()
    }
}

impl LightingBlock {
    /// Pages the section is declared with.
    pub const PAGES: usize = 18;
    /// Length of the block.
    pub const LEN: usize = Self::PAGES * PAGE_LEN;
    /// Effects 1 to 19 plus the per-key mode.
    pub const PRESETS: usize = 20;
    /// Offset of the per-key table.
    pub const PER_KEY_OFFSET: usize = 512;
    /// Entries in the per-key table.
    pub const PER_KEY_ENTRIES: usize = 144;
    /// Length of one per-key entry.
    pub const PER_KEY_ENTRY_LEN: usize = 4;
    /// Flag opening every per-key entry.
    pub const PER_KEY_FLAG: u8 = 0x80;
    /// Offset of the active record.
    pub const ACTIVE_OFFSET: usize = 1088;
    /// Color the application stores in the presets of effects 6 to 19.
    const PRESET_COLOR: Rgb = Rgb::new(0xA0, 0x20, 0xF0);
    /// Colors the application stores in the presets of effects 1 to 5.
    const PRESET_COLORS: [Rgb; 5] = [
        Rgb::WHITE,
        Rgb::new(0x00, 0xFF, 0xFF),
        Rgb::new(0xFF, 0x00, 0x00),
        Rgb::new(0xFF, 0xFF, 0x00),
        Rgb::new(0x00, 0x00, 0xFF),
    ];

    /// The presets as the application writes them, a black per-key table
    /// and no active record: the LEDs off.
    #[must_use]
    pub fn off() -> Self {
        let mut block = Self {
            bytes: [0; Self::LEN],
        };
        for mode in 1..=Effect::LAST_PRESET {
            let index = usize::from(mode - 1);
            let record = EffectRecord {
                mode,
                color: Self::PRESET_COLORS
                    .get(index)
                    .copied()
                    .unwrap_or(Self::PRESET_COLOR),
                rainbow: u8::from(mode >= Effect::FIRST_RAINBOW),
                brightness: BrightnessLevel::PRESET,
                speed: Speed::PRESET,
                direction: 0,
            };
            block.put_preset(index, record);
        }
        block.put_preset(
            Self::PRESETS - 1,
            EffectRecord::per_key(BrightnessLevel::PER_KEY_PRESET),
        );
        block.fill_keys(Rgb::BLACK);
        block
    }

    /// A block running `record`: stored as the preset of its mode and as
    /// the active record.
    #[must_use]
    pub fn running(record: EffectRecord) -> Self {
        let mut block = Self::off();
        block.activate(record);
        block
    }

    /// A block running the per-key table with every key set to `color`.
    #[must_use]
    pub fn per_key(color: Rgb, brightness: BrightnessLevel) -> Self {
        let mut block = Self::off();
        block.fill_keys(color);
        block.activate(EffectRecord::per_key(brightness));
        block
    }

    fn activate(&mut self, record: EffectRecord) {
        let preset = if record.mode == Effect::PerKey as u8 {
            Self::PRESETS - 1
        } else {
            usize::from(record.mode.saturating_sub(1)).min(Self::PRESETS - 2)
        };
        self.put_preset(preset, record);
        record.write_to(
            &mut self.bytes[Self::ACTIVE_OFFSET..Self::ACTIVE_OFFSET + EffectRecord::LEN],
        );
    }

    fn put_preset(&mut self, index: usize, record: EffectRecord) {
        let offset = index * EffectRecord::LEN;
        record.write_to(&mut self.bytes[offset..offset + EffectRecord::LEN]);
    }

    fn fill_keys(&mut self, color: Rgb) {
        for entry in 0..Self::PER_KEY_ENTRIES {
            let at = Self::PER_KEY_OFFSET + entry * Self::PER_KEY_ENTRY_LEN;
            self.bytes[at..at + Self::PER_KEY_ENTRY_LEN].copy_from_slice(&[
                Self::PER_KEY_FLAG,
                color.r,
                color.g,
                color.b,
            ]);
        }
    }

    /// The block's bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The block as the data pages of a session.
    #[must_use]
    pub fn pages(&self) -> Vec<Report> {
        self.bytes
            .chunks(PAGE_LEN)
            .map(|chunk| {
                let mut page = [0; REPORT_LEN];
                page[0] = REPORT_ID;
                page[1..=chunk.len()].copy_from_slice(chunk);
                page
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TURQUOISE: Rgb = Rgb::new(0x40, 0xE0, 0xD0);

    fn record(block: &LightingBlock, offset: usize) -> &[u8] {
        &block.as_bytes()[offset..offset + EffectRecord::LEN]
    }

    #[test]
    fn a_command_carries_the_marker_and_its_argument() {
        let report = command(Command::Lighting, 0x12);
        assert_eq!(&report[..3], &[0x00, 0x04, 0x13]);
        assert_eq!(report[9], 0x12);
        assert!(report[3..9].iter().all(|&b| b == 0));
    }

    #[test]
    fn an_echo_acknowledges_with_byte_three() {
        let mut echo = command(Command::Begin, 0);
        assert!(!acknowledged(&echo));
        echo[4] = ACK;
        assert!(acknowledged(&echo));
        echo[4] = 0xFF;
        assert!(!acknowledged(&echo));
    }

    #[test]
    fn brightness_levels_follow_the_percentage() {
        assert_eq!(
            BrightnessLevel::from_brightness(Brightness::MIN),
            BrightnessLevel::OFF
        );
        assert_eq!(
            BrightnessLevel::from_brightness(Brightness::new(1).unwrap()).value(),
            1
        );
        assert_eq!(
            BrightnessLevel::from_brightness(Brightness::new(50).unwrap()).value(),
            3
        );
        assert_eq!(
            BrightnessLevel::from_brightness(Brightness::MAX),
            BrightnessLevel::MAX
        );
    }

    #[test]
    fn speed_spans_the_observed_range_over_the_period_bounds() {
        let fastest = PulsePeriod::from_millis(PulsePeriod::MIN_MS).unwrap();
        let slowest = PulsePeriod::from_millis(PulsePeriod::MAX_MS).unwrap();
        let middle = PulsePeriod::from_millis(8000).unwrap();
        assert_eq!(Speed::from_period(fastest), Speed::FASTEST);
        assert_eq!(Speed::from_period(slowest), Speed::SLOWEST);
        assert_eq!(Speed::from_period(middle).value(), 7);
    }

    #[test]
    fn the_block_spans_eighteen_pages_of_the_report_id_and_sixty_four_bytes() {
        let pages = LightingBlock::off().pages();
        assert_eq!(pages.len(), LightingBlock::PAGES);
        assert!(pages.iter().all(|page| page[0] == REPORT_ID));
        assert_eq!(&pages[0][1..17], record(&LightingBlock::off(), 0));
    }

    #[test]
    fn the_presets_match_the_capture_and_off_has_no_active_record() {
        let block = LightingBlock::off();
        assert_eq!(
            record(&block, 0),
            [
                0x01, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0, 0, 6, 6, 0, 0, 0, 0xAA, 0x55
            ]
        );
        assert_eq!(
            record(&block, 6 * 16),
            [
                0x07, 0xA0, 0x20, 0xF0, 0, 0, 0, 0, 1, 6, 6, 0, 0, 0, 0xAA, 0x55
            ]
        );
        assert_eq!(
            record(&block, 19 * 16),
            [0x80, 0, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0, 0, 0xAA, 0x55]
        );
        assert!(block.as_bytes()[320..512].iter().all(|&b| b == 0));
        assert_eq!(&block.as_bytes()[512..516], &[0x80, 0, 0, 0]);
        assert!(
            record(&block, LightingBlock::ACTIVE_OFFSET)
                .iter()
                .all(|&b| b == 0)
        );
    }

    #[test]
    fn a_running_effect_is_its_preset_and_the_active_record() {
        let block = LightingBlock::running(EffectRecord::static_color(
            Rgb::new(0x00, 0xFF, 0x37),
            BrightnessLevel::from_brightness(Brightness::new(10).unwrap()),
        ));
        let expected = [
            0x01, 0x00, 0xFF, 0x37, 0, 0, 0, 0, 0, 1, 6, 0, 0, 0, 0xAA, 0x55,
        ];
        assert_eq!(record(&block, 0), expected);
        assert_eq!(record(&block, LightingBlock::ACTIVE_OFFSET), expected);
    }

    #[test]
    fn breathing_carries_its_speed() {
        let block = LightingBlock::running(EffectRecord::breathing(TURQUOISE, Speed::FASTEST));
        assert_eq!(
            record(&block, LightingBlock::ACTIVE_OFFSET),
            [
                0x05, 0x40, 0xE0, 0xD0, 0, 0, 0, 0, 0, 6, 9, 0, 0, 0, 0xAA, 0x55
            ]
        );
        assert_eq!(record(&block, 4 * 16)[0], 0x05);
    }

    #[test]
    fn the_per_key_block_fills_every_entry() {
        let block = LightingBlock::per_key(Rgb::new(1, 2, 3), BrightnessLevel::MAX);
        assert_eq!(&block.as_bytes()[512..516], &[0x80, 1, 2, 3]);
        assert_eq!(&block.as_bytes()[1084..1088], &[0x80, 1, 2, 3]);
        let active = record(&block, LightingBlock::ACTIVE_OFFSET);
        assert_eq!(active[0], 0x80);
        assert_eq!(active[9], 6);
        assert_eq!(record(&block, 19 * 16)[9], 6);
    }
}
