//! State application and the pulse clock.

use std::time::Duration;

use argb_domain::{Brightness, LightingMode, LightingProfile, PowerState, Rgb};
use tracing::{debug, warn};

use crate::device::{DeviceError, LightingDevice, Persist};
use crate::pulse::PulseWaveform;

/// Whether a host process will keep driving the devices after the apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attendance {
    /// The service keeps running: software pulses may be used.
    Attended,
    /// The host is about to sleep or shut down, or the caller is a one-shot
    /// tool: only effects the devices run on their own may be used.
    Unattended,
}

impl Attendance {
    /// The attendance the service has while showing `state`.
    #[must_use]
    pub const fn for_state(state: PowerState) -> Self {
        match state {
            PowerState::Active => Attendance::Attended,
            PowerState::Sleep => Attendance::Unattended,
        }
    }
}

/// A device currently driven by the software pulse.
#[derive(Debug, Clone, Copy)]
struct SoftwarePulse {
    device_index: usize,
    waveform: PulseWaveform,
    color: Rgb,
}

/// Drives a set of devices according to a profile.
pub struct Engine<D: LightingDevice> {
    profile: LightingProfile,
    devices: Vec<D>,
    state: Option<PowerState>,
    pulsing: Vec<SoftwarePulse>,
}

impl<D: LightingDevice> Engine<D> {
    /// An engine over `devices`, in no state yet.
    #[must_use]
    pub fn new(profile: LightingProfile, devices: Vec<D>) -> Self {
        Self {
            profile,
            devices,
            state: None,
            pulsing: Vec::new(),
        }
    }

    /// The last state applied, if any.
    #[must_use]
    pub const fn state(&self) -> Option<PowerState> {
        self.state
    }

    /// Whether [`Self::tick`] has work to do.
    #[must_use]
    pub fn has_software_pulse(&self) -> bool {
        !self.pulsing.is_empty()
    }

    /// The devices under control.
    #[must_use]
    pub fn devices(&self) -> &[D] {
        &self.devices
    }

    /// Replaces the profile. Call [`Self::apply_state`] afterwards to show it.
    pub fn set_profile(&mut self, profile: LightingProfile) {
        self.profile = profile;
    }

    /// Puts every device into its mode for `state`.
    ///
    /// Static modes always use the hardware static mode. Pulse modes use the
    /// synchronized software pulse while attended (if the device supports
    /// direct control) and the device's own breathing effect otherwise,
    /// since nothing can stream frames once the host sleeps. A device with
    /// no breathing effect shows the pulse color statically when unattended.
    ///
    /// Errors are collected rather than aborting, so one failing device does
    /// not leave the others in the previous state.
    pub fn apply_state(
        &mut self,
        state: PowerState,
        persist: Persist,
        attendance: Attendance,
    ) -> Vec<DeviceError> {
        self.state = Some(state);
        self.pulsing.clear();
        let mut errors = Vec::new();

        for (index, device) in self.devices.iter_mut().enumerate() {
            let mode = *self.profile.mode_for(device.alias(), state);
            debug!(device = %device.alias(), %state, ?mode, "applying");
            let result = match mode {
                LightingMode::Static { color, brightness } => {
                    device.apply_static(color, brightness, persist)
                }
                LightingMode::Pulse { color, period } => {
                    let caps = device.capabilities();
                    if attendance == Attendance::Attended && caps.direct {
                        device.begin_direct().map(|()| {
                            self.pulsing.push(SoftwarePulse {
                                device_index: index,
                                waveform: PulseWaveform::new(period),
                                color,
                            });
                        })
                    } else if caps.hardware_pulse {
                        device.apply_hardware_pulse(color, period, persist)
                    } else {
                        warn!(device = %device.alias(), "cannot pulse; showing static color instead");
                        device.apply_static(color, Brightness::MAX, persist)
                    }
                }
            };
            if let Err(error) = result {
                warn!(device = %device.alias(), %error, "device update failed");
                errors.push(error);
            }
        }
        errors
    }

    /// Writes the current pulse frame to every software-pulsed device.
    ///
    /// `elapsed` is time since one shared clock started; the caller owns that
    /// clock so that all devices sample the same instant.
    pub fn tick(&mut self, elapsed: Duration) -> Vec<DeviceError> {
        let mut errors = Vec::new();
        for pulse in &self.pulsing {
            let color = pulse.waveform.color_at(pulse.color, elapsed);
            if let Err(error) = self.devices[pulse.device_index].write_direct(color) {
                errors.push(error);
            }
        }
        errors
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use argb_domain::{
        Backend, DeviceAlias, DeviceCapabilities, DeviceSelector, DeviceSettings, PulsePeriod,
        StateDefaults,
    };

    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Static(Rgb, Brightness, Persist),
        HardwarePulse(Rgb, PulsePeriod, Persist),
        BeginDirect,
        Direct(Rgb),
    }

    struct FakeDevice {
        alias: DeviceAlias,
        caps: DeviceCapabilities,
        calls: Vec<Call>,
    }

    impl FakeDevice {
        fn new(alias: &str, caps: DeviceCapabilities) -> Self {
            Self {
                alias: DeviceAlias::new(alias).unwrap(),
                caps,
                calls: Vec::new(),
            }
        }
    }

    impl LightingDevice for FakeDevice {
        fn alias(&self) -> &DeviceAlias {
            &self.alias
        }
        fn capabilities(&self) -> DeviceCapabilities {
            self.caps
        }
        fn apply_static(&mut self, c: Rgb, b: Brightness, p: Persist) -> Result<(), DeviceError> {
            self.calls.push(Call::Static(c, b, p));
            Ok(())
        }
        fn apply_hardware_pulse(
            &mut self,
            c: Rgb,
            period: PulsePeriod,
            p: Persist,
        ) -> Result<(), DeviceError> {
            if !self.caps.hardware_pulse {
                return Err(DeviceError::Unsupported {
                    alias: self.alias.clone(),
                    capability: "hardware pulse",
                });
            }
            self.calls.push(Call::HardwarePulse(c, period, p));
            Ok(())
        }
        fn begin_direct(&mut self) -> Result<(), DeviceError> {
            self.calls.push(Call::BeginDirect);
            Ok(())
        }
        fn write_direct(&mut self, c: Rgb) -> Result<(), DeviceError> {
            self.calls.push(Call::Direct(c));
            Ok(())
        }
    }

    const FULL: DeviceCapabilities = DeviceCapabilities {
        direct: true,
        hardware_brightness: true,
        hardware_pulse: true,
        persistent_memory: true,
    };
    const DIRECT_ONLY: DeviceCapabilities = DeviceCapabilities {
        direct: true,
        hardware_brightness: false,
        hardware_pulse: false,
        persistent_memory: false,
    };
    const STATIC_ONLY: DeviceCapabilities = DeviceCapabilities {
        direct: false,
        hardware_brightness: false,
        hardware_pulse: false,
        persistent_memory: false,
    };

    const RED: Rgb = Rgb::new(255, 0, 0);
    const TURQUOISE: Rgb = Rgb::new(0x40, 0xE0, 0xD0);

    fn period() -> PulsePeriod {
        PulsePeriod::from_millis(2000).unwrap()
    }

    fn profile() -> LightingProfile {
        LightingProfile {
            defaults: StateDefaults {
                active: LightingMode::Pulse {
                    color: TURQUOISE,
                    period: period(),
                },
                sleep: LightingMode::Pulse {
                    color: RED,
                    period: period(),
                },
            },
            devices: BTreeMap::from([(
                DeviceAlias::new("gpu").unwrap(),
                DeviceSettings {
                    selector: DeviceSelector {
                        backend: Backend::OpenRgb,
                        match_name: "TUF".into(),
                    },
                    overrides: BTreeMap::from([(
                        PowerState::Active,
                        LightingMode::Static {
                            color: Rgb::WHITE,
                            brightness: Brightness::new(40).unwrap(),
                        },
                    )]),
                },
            )]),
        }
    }

    #[test]
    fn active_pulse_uses_direct_mode_and_ticks_in_sync() {
        let devices = vec![
            FakeDevice::new("board", FULL),
            FakeDevice::new("ram", DIRECT_ONLY),
        ];
        let mut engine = Engine::new(profile(), devices);
        assert!(
            engine
                .apply_state(PowerState::Active, Persist::Volatile, Attendance::Attended)
                .is_empty()
        );
        assert!(engine.has_software_pulse());
        assert!(engine.tick(Duration::from_millis(1000)).is_empty());
        for device in engine.devices() {
            assert_eq!(
                device.calls,
                vec![Call::BeginDirect, Call::Direct(TURQUOISE)]
            );
        }
    }

    #[test]
    fn sleep_pulse_uses_hardware_mode_with_persistence() {
        let mut engine = Engine::new(profile(), vec![FakeDevice::new("board", FULL)]);
        assert!(
            engine
                .apply_state(
                    PowerState::Sleep,
                    Persist::SaveToDevice,
                    Attendance::Unattended
                )
                .is_empty()
        );
        assert!(!engine.has_software_pulse());
        assert_eq!(
            engine.devices()[0].calls,
            vec![Call::HardwarePulse(RED, period(), Persist::SaveToDevice)]
        );
    }

    #[test]
    fn unattended_pulse_without_hardware_effect_shows_the_color_statically() {
        let devices = vec![
            FakeDevice::new("ram", DIRECT_ONLY),
            FakeDevice::new("lamp", STATIC_ONLY),
        ];
        let mut engine = Engine::new(profile(), devices);
        assert!(
            engine
                .apply_state(PowerState::Sleep, Persist::Volatile, Attendance::Unattended)
                .is_empty()
        );
        for device in engine.devices() {
            assert_eq!(
                device.calls,
                vec![Call::Static(RED, Brightness::MAX, Persist::Volatile)]
            );
        }
    }

    #[test]
    fn attended_active_state_without_direct_uses_the_hardware_pulse() {
        let no_direct = DeviceCapabilities {
            direct: false,
            ..FULL
        };
        let mut engine = Engine::new(profile(), vec![FakeDevice::new("board", no_direct)]);
        engine.apply_state(PowerState::Active, Persist::Volatile, Attendance::Attended);
        assert_eq!(
            engine.devices()[0].calls,
            vec![Call::HardwarePulse(TURQUOISE, period(), Persist::Volatile)]
        );
    }

    #[test]
    fn static_override_is_applied_with_true_brightness() {
        let mut engine = Engine::new(profile(), vec![FakeDevice::new("gpu", FULL)]);
        assert!(
            engine
                .apply_state(
                    PowerState::Active,
                    Persist::SaveToDevice,
                    Attendance::Attended
                )
                .is_empty()
        );
        assert_eq!(
            engine.devices()[0].calls,
            vec![Call::Static(
                Rgb::WHITE,
                Brightness::new(40).unwrap(),
                Persist::SaveToDevice
            )]
        );
        assert!(!engine.has_software_pulse());
    }

    #[test]
    fn state_change_clears_previous_pulses() {
        let mut engine = Engine::new(profile(), vec![FakeDevice::new("board", FULL)]);
        engine.apply_state(PowerState::Active, Persist::Volatile, Attendance::Attended);
        assert!(engine.has_software_pulse());
        engine.apply_state(PowerState::Sleep, Persist::Volatile, Attendance::Unattended);
        assert!(!engine.has_software_pulse());
        assert_eq!(engine.state(), Some(PowerState::Sleep));
    }
}
