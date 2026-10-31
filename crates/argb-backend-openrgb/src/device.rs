//! `LightingDevice` for one OpenRGB controller.
//!
//! Every controller exposes modes by name. This adapter picks the modes that
//! matter to the engine (direct, static, pulse), derives the device's
//! capabilities from their flags, and translates the engine's requests into
//! mode updates and LED writes.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use argb_domain::{Brightness, DeviceAlias, DeviceCapabilities, PulsePeriod, Rgb};
use argb_engine::{DeviceError, LightingDevice, Persist};
use tracing::debug;

use crate::client::{ControllerCommands, OpenRgbError};
use crate::description::{ControllerDescription, ModeDescription};
use crate::protocol::{ColorMode, mode_flags};

/// Name of the per-LED streaming mode on every OpenRGB controller.
pub const DIRECT_MODE_NAME: &str = "Direct";
/// Names used for a hardware single-color mode, in order of preference.
/// Corsair DRAM calls it `Custom`.
pub const STATIC_MODE_NAMES: [&str; 2] = ["Static", "Custom"];
/// Names used for a hardware breathing mode, in order of preference.
/// Corsair DRAM calls it `Color Pulse`.
pub const PULSE_MODE_NAMES: [&str; 3] = ["Breathing", "Color Pulse", "Pulse"];

/// Pause between committing a hardware mode and writing its per-LED colors.
/// Controllers that store colors in their own memory (Corsair DRAM over
/// SMBus, for one) are still busy right after the mode commit and corrupt a
/// color write that follows at once; OpenRGB's own window never hits this
/// because a person separates the two actions by seconds.
pub const MODE_SWITCH_SETTLE: Duration = Duration::from_millis(500);

/// A client connection shared by every device on it.
pub type SharedClient<C> = Arc<Mutex<C>>;

/// Which of a controller's modes play which role for the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModeMap {
    /// Index of the direct mode.
    pub direct: Option<usize>,
    /// Index of the hardware static mode.
    pub static_mode: Option<usize>,
    /// Index of the hardware breathing mode.
    pub pulse: Option<usize>,
}

impl ModeMap {
    /// Looks up the roles by mode name.
    #[must_use]
    pub fn analyze(controller: &ControllerDescription) -> Self {
        let first_named = |names: &[&str]| {
            names
                .iter()
                .find_map(|name| controller.find_mode(name).map(|(index, _)| index))
        };
        Self {
            direct: controller
                .find_mode(DIRECT_MODE_NAME)
                .map(|(index, _)| index),
            static_mode: first_named(&STATIC_MODE_NAMES)
                .filter(|&index| carries_a_color(&controller.modes[index])),
            pulse: first_named(&PULSE_MODE_NAMES),
        }
    }

    /// Capabilities implied by the mapped modes' flags.
    #[must_use]
    pub fn capabilities(&self, controller: &ControllerDescription) -> DeviceCapabilities {
        let mode = |index: Option<usize>| index.and_then(|i| controller.modes.get(i));
        let static_mode = mode(self.static_mode);
        let pulse = mode(self.pulse);
        let persists = |m: &ModeDescription| {
            m.has_flag(mode_flags::MANUAL_SAVE) || m.has_flag(mode_flags::AUTOMATIC_SAVE)
        };
        DeviceCapabilities {
            direct: self.direct.is_some(),
            hardware_brightness: static_mode.is_some_and(|m| {
                m.has_flag(mode_flags::HAS_BRIGHTNESS) && m.brightness_max > m.brightness_min
            }),
            hardware_pulse: pulse.is_some(),
            persistent_memory: static_mode.is_some_and(persists) || pulse.is_some_and(persists),
        }
    }
}

/// Whether a mode has somewhere to put the requested color: either the
/// controller's per-LED array or at least one mode-specific color slot.
/// The Logitech G203 advertises a `Static` mode with neither, which only
/// turns the light off.
fn carries_a_color(mode: &ModeDescription) -> bool {
    mode.has_flag(mode_flags::HAS_PER_LED_COLOR)
        || (mode.has_flag(mode_flags::HAS_MODE_SPECIFIC_COLOR) && mode.colors_max >= 1)
}

/// Maps a brightness percentage onto a mode's hardware brightness range.
#[must_use]
pub fn hardware_brightness(mode: &ModeDescription, brightness: Brightness) -> u32 {
    let span = u64::from(mode.brightness_max.saturating_sub(mode.brightness_min));
    let scaled = span * u64::from(brightness.percent()) / u64::from(Brightness::MAX.percent());
    mode.brightness_min + u32::try_from(scaled).unwrap_or(u32::MAX)
}

/// Maps a pulse period onto a mode's speed range. OpenRGB defines `speed_min`
/// as the slowest setting and `speed_max` as the fastest whatever their
/// numeric order, so the shortest period maps to `speed_max`.
#[must_use]
pub fn hardware_speed(mode: &ModeDescription, period: PulsePeriod) -> u32 {
    let slow = i64::from(mode.speed_min);
    let fast = i64::from(mode.speed_max);
    let position = i64::try_from(period.as_millis() - PulsePeriod::MIN_MS).unwrap_or(i64::MAX);
    let range = i64::try_from(PulsePeriod::MAX_MS - PulsePeriod::MIN_MS).unwrap_or(i64::MAX);
    let speed = fast + (slow - fast) * position / range;
    u32::try_from(speed).unwrap_or(mode.speed_max)
}

/// Returns a copy of `mode` configured to show `color`, plus whether the
/// controller's per-LED array must be written as well.
#[must_use]
pub fn colorize(mode: &ModeDescription, color: Rgb) -> (ModeDescription, bool) {
    let mut mode = mode.clone();
    if mode.has_flag(mode_flags::HAS_MODE_SPECIFIC_COLOR) {
        mode.color_mode = ColorMode::ModeSpecific as u32;
        // The server rejects a mode whose color count is outside the declared
        // range, so stay inside it even when that leaves no slot for the color.
        let count = usize::try_from(mode.colors_min.max(1).min(mode.colors_max)).unwrap_or(0);
        mode.colors = vec![color; count];
        (mode, false)
    } else if mode.has_flag(mode_flags::HAS_PER_LED_COLOR) {
        mode.color_mode = ColorMode::PerLed as u32;
        (mode, true)
    } else {
        (mode, false)
    }
}

/// One OpenRGB controller driven through a shared client.
pub struct OpenRgbDevice<C: ControllerCommands> {
    alias: DeviceAlias,
    index: u32,
    description: ControllerDescription,
    modes: ModeMap,
    capabilities: DeviceCapabilities,
    client: SharedClient<C>,
}

impl<C: ControllerCommands> OpenRgbDevice<C> {
    /// Wraps controller `index`, described by `description`, under `alias`.
    #[must_use]
    pub fn new(
        alias: DeviceAlias,
        index: u32,
        description: ControllerDescription,
        client: SharedClient<C>,
    ) -> Self {
        let modes = ModeMap::analyze(&description);
        let capabilities = modes.capabilities(&description);
        debug!(%alias, index, name = %description.name, ?modes, ?capabilities, "mapped controller");
        Self {
            alias,
            index,
            description,
            modes,
            capabilities,
            client,
        }
    }

    /// Controller index on the server.
    #[must_use]
    pub const fn index(&self) -> u32 {
        self.index
    }

    /// The server's description of the controller.
    #[must_use]
    pub const fn description(&self) -> &ControllerDescription {
        &self.description
    }

    /// Mode roles resolved for this controller.
    #[must_use]
    pub const fn modes(&self) -> ModeMap {
        self.modes
    }

    fn unsupported(&self, capability: &'static str) -> DeviceError {
        DeviceError::Unsupported {
            alias: self.alias.clone(),
            capability,
        }
    }

    fn backend_error(&self, error: &OpenRgbError) -> DeviceError {
        DeviceError::Backend {
            alias: self.alias.clone(),
            message: error.to_string(),
        }
    }

    fn with_client<T>(
        &self,
        operation: impl FnOnce(&mut C) -> Result<T, OpenRgbError>,
    ) -> Result<T, DeviceError> {
        let mut client = self.client.lock().map_err(|_| DeviceError::Backend {
            alias: self.alias.clone(),
            message: "OpenRGB client lock poisoned".to_owned(),
        })?;
        operation(&mut client).map_err(|error| self.backend_error(&error))
    }

    fn mode_at(&self, index: usize) -> &ModeDescription {
        &self.description.modes[index]
    }

    /// Sends a mode, saving it when asked and when the controller can.
    fn send_mode(
        &self,
        index: usize,
        mode: &ModeDescription,
        persist: Persist,
    ) -> Result<(), DeviceError> {
        let mode_index = u32::try_from(index).expect("mode index fits in u32");
        let save = persist == Persist::SaveToDevice && mode.has_flag(mode_flags::MANUAL_SAVE);
        self.with_client(|client| {
            if save {
                client.save_mode(self.index, mode_index, mode)
            } else {
                client.update_mode(self.index, mode_index, mode)
            }
        })
    }

    fn fill_leds(&self, color: Rgb) -> Result<(), DeviceError> {
        let colors = vec![color; self.description.led_count()];
        self.with_client(|client| client.update_leds(self.index, &colors))
    }
}

impl<C: ControllerCommands> LightingDevice for OpenRgbDevice<C> {
    fn alias(&self) -> &DeviceAlias {
        &self.alias
    }

    fn capabilities(&self) -> DeviceCapabilities {
        self.capabilities
    }

    fn apply_static(
        &mut self,
        color: Rgb,
        brightness: Brightness,
        persist: Persist,
    ) -> Result<(), DeviceError> {
        let Some(index) = self.modes.static_mode.or(self.modes.direct) else {
            return Err(self.unsupported("a static or direct mode"));
        };
        let template = self.mode_at(index);
        let shown = if self.capabilities.hardware_brightness {
            color
        } else {
            color.scaled(brightness.as_level())
        };
        let (mut mode, needs_leds) = colorize(template, shown);
        if self.capabilities.hardware_brightness {
            mode.brightness = hardware_brightness(template, brightness);
        }
        self.send_mode(index, &mode, persist)?;
        if needs_leds {
            std::thread::sleep(MODE_SWITCH_SETTLE);
            self.fill_leds(shown)?;
        }
        Ok(())
    }

    fn apply_hardware_pulse(
        &mut self,
        color: Rgb,
        period: PulsePeriod,
        persist: Persist,
    ) -> Result<(), DeviceError> {
        let Some(index) = self.modes.pulse else {
            return Err(self.unsupported("a hardware pulse mode"));
        };
        let template = self.mode_at(index);
        let (mut mode, needs_leds) = colorize(template, color);
        if template.has_flag(mode_flags::HAS_SPEED) {
            mode.speed = hardware_speed(template, period);
        }
        if template.has_flag(mode_flags::HAS_BRIGHTNESS) {
            mode.brightness = template.brightness_max;
        }
        self.send_mode(index, &mode, persist)?;
        if needs_leds {
            self.fill_leds(color)?;
        }
        Ok(())
    }

    fn begin_direct(&mut self) -> Result<(), DeviceError> {
        let Some(index) = self.modes.direct else {
            return Err(self.unsupported("a direct mode"));
        };
        let mode = self.mode_at(index).clone();
        self.send_mode(index, &mode, Persist::Volatile)
    }

    fn write_direct(&mut self, color: Rgb) -> Result<(), DeviceError> {
        self.fill_leds(color)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::description::tests::sample_controller;
    use crate::protocol::ColorMode;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Sent {
        Update(u32, u32, ModeDescription),
        Save(u32, u32, ModeDescription),
        Leds(u32, Vec<Rgb>),
    }

    #[derive(Default)]
    struct Recorder(Vec<Sent>);

    impl ControllerCommands for Recorder {
        fn update_mode(&mut self, c: u32, i: u32, m: &ModeDescription) -> Result<(), OpenRgbError> {
            self.0.push(Sent::Update(c, i, m.clone()));
            Ok(())
        }
        fn save_mode(&mut self, c: u32, i: u32, m: &ModeDescription) -> Result<(), OpenRgbError> {
            self.0.push(Sent::Save(c, i, m.clone()));
            Ok(())
        }
        fn update_leds(&mut self, c: u32, colors: &[Rgb]) -> Result<(), OpenRgbError> {
            self.0.push(Sent::Leds(c, colors.to_vec()));
            Ok(())
        }
    }

    const TURQUOISE: Rgb = Rgb::new(0x40, 0xE0, 0xD0);

    fn device(
        description: ControllerDescription,
    ) -> (OpenRgbDevice<Recorder>, SharedClient<Recorder>) {
        let client = Arc::new(Mutex::new(Recorder::default()));
        let device = OpenRgbDevice::new(
            DeviceAlias::new("board").unwrap(),
            7,
            description,
            Arc::clone(&client),
        );
        (device, client)
    }

    fn sent(client: &SharedClient<Recorder>) -> Vec<Sent> {
        client.lock().unwrap().0.clone()
    }

    /// A controller shaped like Corsair DRAM: Direct, a per-LED `Custom`
    /// static mode without brightness, and an automatically saved `Color Pulse`.
    fn corsair_like() -> ControllerDescription {
        let mut controller = sample_controller();
        controller.name = "Corsair Vengeance RGB DDR5".into();
        controller.modes = vec![
            ModeDescription {
                name: "Direct".into(),
                flags: mode_flags::HAS_PER_LED_COLOR,
                color_mode: ColorMode::PerLed as u32,
                ..ModeDescription::default()
            },
            ModeDescription {
                name: "Custom".into(),
                flags: mode_flags::HAS_PER_LED_COLOR | mode_flags::AUTOMATIC_SAVE,
                color_mode: ColorMode::PerLed as u32,
                ..ModeDescription::default()
            },
            ModeDescription {
                name: "Color Pulse".into(),
                flags: mode_flags::HAS_SPEED
                    | mode_flags::HAS_MODE_SPECIFIC_COLOR
                    | mode_flags::HAS_BRIGHTNESS
                    | mode_flags::AUTOMATIC_SAVE,
                speed_min: 3,
                speed_max: 1,
                brightness_min: 0,
                brightness_max: 100,
                colors_min: 1,
                colors_max: 2,
                color_mode: ColorMode::ModeSpecific as u32,
                ..ModeDescription::default()
            },
        ];
        controller
    }

    #[test]
    fn mode_map_and_capabilities_follow_mode_names_and_flags() {
        let gigabyte = sample_controller();
        let map = ModeMap::analyze(&gigabyte);
        assert_eq!(
            map,
            ModeMap {
                direct: Some(0),
                static_mode: Some(1),
                pulse: Some(2)
            }
        );
        assert_eq!(
            map.capabilities(&gigabyte),
            DeviceCapabilities {
                direct: true,
                hardware_brightness: true,
                hardware_pulse: true,
                persistent_memory: true
            }
        );

        let corsair = corsair_like();
        let map = ModeMap::analyze(&corsair);
        assert_eq!(
            map,
            ModeMap {
                direct: Some(0),
                static_mode: Some(1),
                pulse: Some(2)
            }
        );
        assert_eq!(
            map.capabilities(&corsair),
            DeviceCapabilities {
                direct: true,
                hardware_brightness: false,
                hardware_pulse: true,
                persistent_memory: true
            }
        );
    }

    #[test]
    fn a_static_mode_without_color_slots_is_ignored_in_favor_of_direct() {
        let mut controller = sample_controller();
        controller.modes[1] = ModeDescription {
            name: "Static".into(),
            flags: mode_flags::HAS_MODE_SPECIFIC_COLOR,
            colors_min: 0,
            colors_max: 0,
            color_mode: ColorMode::ModeSpecific as u32,
            ..ModeDescription::default()
        };
        let map = ModeMap::analyze(&controller);
        assert_eq!(map.static_mode, None);

        let (mut device, client) = device(controller);
        device
            .apply_static(TURQUOISE, Brightness::MAX, Persist::Volatile)
            .unwrap();
        assert!(matches!(sent(&client)[0], Sent::Update(7, 0, _)));
    }

    #[test]
    fn brightness_maps_onto_the_hardware_range() {
        let mode = ModeDescription {
            brightness_min: 10,
            brightness_max: 60,
            ..ModeDescription::default()
        };
        assert_eq!(hardware_brightness(&mode, Brightness::MIN), 10);
        assert_eq!(hardware_brightness(&mode, Brightness::new(50).unwrap()), 35);
        assert_eq!(hardware_brightness(&mode, Brightness::MAX), 60);
    }

    #[test]
    fn speed_maps_short_periods_to_fast_whatever_the_numeric_order() {
        let ascending = ModeDescription {
            speed_min: 0,
            speed_max: 4,
            ..ModeDescription::default()
        };
        let descending = ModeDescription {
            speed_min: 3,
            speed_max: 1,
            ..ModeDescription::default()
        };
        let fastest = PulsePeriod::from_millis(PulsePeriod::MIN_MS).unwrap();
        let slowest = PulsePeriod::from_millis(PulsePeriod::MAX_MS).unwrap();
        assert_eq!(hardware_speed(&ascending, fastest), 4);
        assert_eq!(hardware_speed(&ascending, slowest), 0);
        assert_eq!(hardware_speed(&descending, fastest), 1);
        assert_eq!(hardware_speed(&descending, slowest), 3);
    }

    #[test]
    fn static_with_hardware_brightness_saves_a_mode_specific_color() {
        let (mut device, client) = device(sample_controller());
        device
            .apply_static(
                TURQUOISE,
                Brightness::new(40).unwrap(),
                Persist::SaveToDevice,
            )
            .unwrap();
        let expected = ModeDescription {
            brightness: 40,
            colors: vec![TURQUOISE],
            ..sample_controller().modes[1].clone()
        };
        assert_eq!(sent(&client), vec![Sent::Save(7, 1, expected)]);
    }

    #[test]
    fn static_without_hardware_brightness_scales_the_color_and_fills_leds() {
        let (mut device, client) = device(corsair_like());
        device
            .apply_static(
                Rgb::WHITE,
                Brightness::new(50).unwrap(),
                Persist::SaveToDevice,
            )
            .unwrap();
        let grey = Rgb::new(128, 128, 128);
        let led_count = corsair_like().led_count();
        // AUTOMATIC_SAVE needs no save packet; an update is enough.
        assert_eq!(
            sent(&client),
            vec![
                Sent::Update(7, 1, corsair_like().modes[1].clone()),
                Sent::Leds(7, vec![grey; led_count]),
            ]
        );
    }

    #[test]
    fn hardware_pulse_sets_speed_color_and_full_brightness() {
        let (mut device, client) = device(corsair_like());
        let period = PulsePeriod::from_millis(PulsePeriod::MIN_MS).unwrap();
        device
            .apply_hardware_pulse(Rgb::new(255, 0, 0), period, Persist::Volatile)
            .unwrap();
        let expected = ModeDescription {
            speed: 1,
            brightness: 100,
            colors: vec![Rgb::new(255, 0, 0)],
            ..corsair_like().modes[2].clone()
        };
        assert_eq!(sent(&client), vec![Sent::Update(7, 2, expected)]);
    }

    #[test]
    fn direct_mode_streams_every_led() {
        let (mut device, client) = device(sample_controller());
        device.begin_direct().unwrap();
        device.write_direct(TURQUOISE).unwrap();
        assert_eq!(
            sent(&client),
            vec![
                Sent::Update(7, 0, sample_controller().modes[0].clone()),
                Sent::Leds(7, vec![TURQUOISE; 6]),
            ]
        );
    }

    #[test]
    fn missing_modes_are_reported_as_unsupported() {
        let mut controller = sample_controller();
        controller.modes.clear();
        let (mut device, _) = device(controller);
        assert!(matches!(
            device.begin_direct(),
            Err(DeviceError::Unsupported { .. })
        ));
        assert!(matches!(
            device.apply_hardware_pulse(
                Rgb::WHITE,
                PulsePeriod::from_millis(1000).unwrap(),
                Persist::Volatile
            ),
            Err(DeviceError::Unsupported { .. })
        ));
    }
}
