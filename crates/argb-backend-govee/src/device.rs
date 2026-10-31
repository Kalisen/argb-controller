//! `LightingDevice` for one Govee light.
//!
//! A static apply is one short connection: power on, color, brightness when
//! it changed, then disconnect; the light keeps the state on its own. Direct
//! mode keeps the connection open and streams color frames, throttled to
//! what a BLE link sustains.

use std::time::{Duration, Instant};

use argb_domain::{Brightness, DeviceAlias, DeviceCapabilities, PulsePeriod, Rgb};
use argb_engine::{DeviceError, LightingDevice, Persist};
use tracing::debug;

use crate::error::GoveeError;
use crate::model::Model;
use crate::protocol;
use crate::transport::Link;

/// Shortest interval between direct-mode color frames. The engine ticks at
/// 30 Hz for OpenRGB; a Govee light takes a frame every 100 ms or so, so
/// intermediate ticks are skipped and the light samples the same waveform
/// at its own rate.
pub const DIRECT_FRAME_INTERVAL: Duration = Duration::from_millis(100);

/// What every supported light can do: streamed color, a true brightness
/// control, no breathing effect of its own, and a state that survives a
/// disconnect and a power cycle.
pub const CAPABILITIES: DeviceCapabilities = DeviceCapabilities {
    direct: true,
    hardware_brightness: true,
    hardware_pulse: false,
    persistent_memory: true,
};

/// One Govee light under an alias.
pub struct GoveeDevice<L: Link> {
    alias: DeviceAlias,
    link: L,
    /// Brightness last written, so it is not re-sent on every apply: the
    /// light flashes on a brightness write.
    brightness_sent: Option<u8>,
    /// When the last direct-mode frame went out.
    last_frame: Option<Instant>,
}

impl<L: Link> GoveeDevice<L> {
    /// A device over `link`.
    #[must_use]
    pub fn new(alias: DeviceAlias, link: L) -> Self {
        Self {
            alias,
            link,
            brightness_sent: None,
            last_frame: None,
        }
    }

    /// The link, for inspection.
    #[must_use]
    pub fn link(&self) -> &L {
        &self.link
    }

    fn error(&self, error: &GoveeError) -> DeviceError {
        DeviceError::Backend {
            alias: self.alias.clone(),
            message: error.to_string(),
        }
    }

    fn model(&self) -> Result<Model, GoveeError> {
        self.link
            .model()
            .ok_or_else(|| GoveeError::UnsupportedModel("unknown".into()))
    }

    fn send_color(&mut self, color: Rgb) -> Result<(), GoveeError> {
        let layout = self.model()?.layout();
        self.link
            .send(&protocol::color(layout, color.r, color.g, color.b))
    }

    fn send_brightness(&mut self, brightness: Brightness) -> Result<(), GoveeError> {
        let percent = brightness.percent();
        if self.brightness_sent == Some(percent) {
            return Ok(());
        }
        self.link.send(&protocol::brightness(percent))?;
        self.brightness_sent = Some(percent);
        Ok(())
    }

    fn show(&mut self, color: Rgb, brightness: Brightness) -> Result<(), GoveeError> {
        self.link.connect()?;
        self.link.send(&protocol::power(true))?;
        self.send_color(color)?;
        self.send_brightness(brightness)
    }
}

impl<L: Link> LightingDevice for GoveeDevice<L> {
    fn alias(&self) -> &DeviceAlias {
        &self.alias
    }

    fn capabilities(&self) -> DeviceCapabilities {
        CAPABILITIES
    }

    /// Power on, color, brightness, then the link is closed: the light keeps
    /// the state, so `persist` needs no action.
    fn apply_static(
        &mut self,
        color: Rgb,
        brightness: Brightness,
        _persist: Persist,
    ) -> Result<(), DeviceError> {
        let result = self.show(color, brightness);
        self.link.disconnect();
        self.last_frame = None;
        result.map_err(|e| self.error(&e))
    }

    fn apply_hardware_pulse(
        &mut self,
        _color: Rgb,
        _period: PulsePeriod,
        _persist: Persist,
    ) -> Result<(), DeviceError> {
        Err(DeviceError::Unsupported {
            alias: self.alias.clone(),
            capability: "hardware pulse",
        })
    }

    /// Opens the link and sets full brightness, since the engine scales the
    /// color itself while pulsing.
    fn begin_direct(&mut self) -> Result<(), DeviceError> {
        self.last_frame = None;
        let result = self
            .link
            .connect()
            .and_then(|()| self.link.send(&protocol::power(true)))
            .and_then(|()| self.send_brightness(Brightness::MAX));
        result.map_err(|e| self.error(&e))
    }

    /// Writes the color unless the previous frame went out less than
    /// [`DIRECT_FRAME_INTERVAL`] ago. A dropped link is reopened once.
    fn write_direct(&mut self, color: Rgb) -> Result<(), DeviceError> {
        if self
            .last_frame
            .is_some_and(|at| at.elapsed() < DIRECT_FRAME_INTERVAL)
        {
            return Ok(());
        }
        let result = match self.send_color(color) {
            Ok(()) => Ok(()),
            Err(error) => {
                debug!(alias = %self.alias, %error, "direct frame failed; reconnecting");
                self.link.connect().and_then(|()| self.send_color(color))
            }
        };
        self.last_frame = Some(Instant::now());
        result.map_err(|e| self.error(&e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Layout;

    /// A link that records frames and can fail on demand.
    struct FakeLink {
        model: Option<Model>,
        connected: bool,
        connects: usize,
        frames: Vec<[u8; protocol::FRAME_LEN]>,
        fail_sends: usize,
    }

    impl FakeLink {
        fn new(model: Model) -> Self {
            Self {
                model: Some(model),
                connected: false,
                connects: 0,
                frames: Vec::new(),
                fail_sends: 0,
            }
        }
    }

    impl Link for FakeLink {
        fn connect(&mut self) -> Result<(), GoveeError> {
            self.connected = true;
            self.connects += 1;
            Ok(())
        }
        fn send(&mut self, frame: &[u8; protocol::FRAME_LEN]) -> Result<(), GoveeError> {
            if self.fail_sends > 0 {
                self.fail_sends -= 1;
                self.connected = false;
                return Err(GoveeError::at("writing", "link dropped"));
            }
            assert!(self.connected, "sent while disconnected");
            self.frames.push(*frame);
            Ok(())
        }
        fn disconnect(&mut self) {
            self.connected = false;
        }
        fn is_connected(&self) -> bool {
            self.connected
        }
        fn model(&self) -> Option<Model> {
            self.model
        }
    }

    const TURQUOISE: Rgb = Rgb::new(0x40, 0xE0, 0xD0);

    fn device(model: Model) -> GoveeDevice<FakeLink> {
        GoveeDevice::new(DeviceAlias::new("lamp").unwrap(), FakeLink::new(model))
    }

    #[test]
    fn static_apply_is_power_color_brightness_then_disconnect() {
        let mut device = device(Model::LyraFloorLamp);
        device
            .apply_static(
                TURQUOISE,
                Brightness::new(60).unwrap(),
                Persist::SaveToDevice,
            )
            .unwrap();
        assert_eq!(
            device.link.frames,
            vec![
                protocol::power(true),
                protocol::color(Layout::Lamp, 0x40, 0xE0, 0xD0),
                protocol::brightness(60),
            ]
        );
        assert!(!device.link.is_connected());
    }

    #[test]
    fn unchanged_brightness_is_not_sent_again() {
        let mut device = device(Model::GlideHexa);
        let brightness = Brightness::new(60).unwrap();
        device
            .apply_static(TURQUOISE, brightness, Persist::Volatile)
            .unwrap();
        device
            .apply_static(Rgb::WHITE, brightness, Persist::Volatile)
            .unwrap();
        let brightness_frames = device
            .link
            .frames
            .iter()
            .filter(|f| f[1] == protocol::Operation::Brightness as u8)
            .count();
        assert_eq!(brightness_frames, 1);
    }

    #[test]
    fn direct_mode_streams_at_most_one_frame_per_interval_and_keeps_the_link() {
        let mut device = device(Model::GlideHexa);
        device.begin_direct().unwrap();
        assert_eq!(
            device.link.frames,
            vec![protocol::power(true), protocol::brightness(100)]
        );
        device.write_direct(TURQUOISE).unwrap();
        device.write_direct(Rgb::WHITE).unwrap();
        assert_eq!(
            device.link.frames.len(),
            3,
            "the second frame came too soon"
        );
        assert_eq!(
            device.link.frames[2],
            protocol::color(Layout::Hexa, 0x40, 0xE0, 0xD0)
        );
        assert!(device.link.is_connected());
    }

    #[test]
    fn a_dropped_link_is_reopened_once_for_the_next_frame() {
        let mut device = device(Model::GlideHexa);
        device.begin_direct().unwrap();
        device.link.fail_sends = 1;
        device.write_direct(TURQUOISE).unwrap();
        assert_eq!(device.link.connects, 2);
        assert_eq!(device.link.frames.len(), 3);
    }

    #[test]
    fn hardware_pulse_is_unsupported() {
        let mut device = device(Model::LyraFloorLamp);
        let period = PulsePeriod::from_millis(4000).unwrap();
        assert!(matches!(
            device.apply_hardware_pulse(TURQUOISE, period, Persist::Volatile),
            Err(DeviceError::Unsupported { .. })
        ));
    }

    #[test]
    fn an_unknown_model_is_a_backend_error() {
        let mut link = FakeLink::new(Model::GlideHexa);
        link.model = None;
        let mut device = GoveeDevice::new(DeviceAlias::new("lamp").unwrap(), link);
        assert!(matches!(
            device.apply_static(TURQUOISE, Brightness::MAX, Persist::Volatile),
            Err(DeviceError::Backend { .. })
        ));
    }
}
