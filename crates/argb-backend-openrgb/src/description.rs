//! Controller descriptions: parsing the server's replies and encoding the
//! payloads of mode and LED updates.
//!
//! Layouts follow OpenRGB's `RGBController` serialization for protocol
//! versions 3 and 4. Strings are a `u16` length (including the terminating
//! NUL) followed by the bytes.

use argb_domain::Rgb;

use crate::protocol::{
    DeviceType, MAX_PROTOCOL_VERSION, ProtocolError, color_from_wire, color_to_wire,
};

/// Protocol version from which zones carry segments.
const SEGMENTS_FROM_VERSION: u32 = 4;
/// Protocol version from which modes carry brightness fields.
const BRIGHTNESS_FROM_VERSION: u32 = 3;
/// Size of the `u32` length prefix that starts update payloads and data replies.
const SIZE_PREFIX_LEN: usize = 4;

/// One mode a controller offers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModeDescription {
    /// Display name, e.g. `Direct`, `Static`, `Breathing`.
    pub name: String,
    /// Controller-specific mode value.
    pub value: i32,
    /// Bit set of [`crate::protocol::mode_flags`].
    pub flags: u32,
    /// Slowest speed value (not necessarily the numerically smallest).
    pub speed_min: u32,
    /// Fastest speed value.
    pub speed_max: u32,
    /// Dimmest brightness value.
    pub brightness_min: u32,
    /// Brightest brightness value.
    pub brightness_max: u32,
    /// Fewest colors the mode accepts.
    pub colors_min: u32,
    /// Most colors the mode accepts.
    pub colors_max: u32,
    /// Current speed.
    pub speed: u32,
    /// Current brightness.
    pub brightness: u32,
    /// Current direction.
    pub direction: u32,
    /// Current color mode, see [`crate::protocol::ColorMode`].
    pub color_mode: u32,
    /// Current mode-specific colors.
    pub colors: Vec<Rgb>,
}

impl ModeDescription {
    /// Whether `flag` from [`crate::protocol::mode_flags`] is set.
    #[must_use]
    pub const fn has_flag(&self, flag: u32) -> bool {
        self.flags & flag != 0
    }

    fn decode(reader: &mut Reader<'_>, protocol_version: u32) -> Result<Self, ProtocolError> {
        let name = reader.string()?;
        let value = reader.i32()?;
        let flags = reader.u32()?;
        let speed_min = reader.u32()?;
        let speed_max = reader.u32()?;
        let (brightness_min, brightness_max) = if protocol_version >= BRIGHTNESS_FROM_VERSION {
            (reader.u32()?, reader.u32()?)
        } else {
            (0, 0)
        };
        let colors_min = reader.u32()?;
        let colors_max = reader.u32()?;
        let speed = reader.u32()?;
        let brightness = if protocol_version >= BRIGHTNESS_FROM_VERSION {
            reader.u32()?
        } else {
            0
        };
        let direction = reader.u32()?;
        let color_mode = reader.u32()?;
        let colors = reader.colors()?;
        Ok(Self {
            name,
            value,
            flags,
            speed_min,
            speed_max,
            brightness_min,
            brightness_max,
            colors_min,
            colors_max,
            speed,
            brightness,
            direction,
            color_mode,
            colors,
        })
    }

    fn encode(&self, out: &mut Vec<u8>, protocol_version: u32) {
        put_string(out, &self.name);
        out.extend_from_slice(&self.value.to_le_bytes());
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.extend_from_slice(&self.speed_min.to_le_bytes());
        out.extend_from_slice(&self.speed_max.to_le_bytes());
        if protocol_version >= BRIGHTNESS_FROM_VERSION {
            out.extend_from_slice(&self.brightness_min.to_le_bytes());
            out.extend_from_slice(&self.brightness_max.to_le_bytes());
        }
        out.extend_from_slice(&self.colors_min.to_le_bytes());
        out.extend_from_slice(&self.colors_max.to_le_bytes());
        out.extend_from_slice(&self.speed.to_le_bytes());
        if protocol_version >= BRIGHTNESS_FROM_VERSION {
            out.extend_from_slice(&self.brightness.to_le_bytes());
        }
        out.extend_from_slice(&self.direction.to_le_bytes());
        out.extend_from_slice(&self.color_mode.to_le_bytes());
        put_colors(out, &self.colors);
    }
}

/// LED matrix layout of a zone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatrixMap {
    /// Rows.
    pub height: u32,
    /// Columns.
    pub width: u32,
    /// `height * width` LED indices, row-major.
    pub map: Vec<u32>,
}

/// A named run of LEDs inside a zone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentDescription {
    /// Display name.
    pub name: String,
    /// Zone type value of the segment.
    pub segment_type: u32,
    /// First LED index.
    pub start_index: u32,
    /// Number of LEDs.
    pub leds_count: u32,
}

/// A group of LEDs on a controller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneDescription {
    /// Display name.
    pub name: String,
    /// Zone type value.
    pub zone_type: u32,
    /// Fewest LEDs the zone can be resized to.
    pub leds_min: u32,
    /// Most LEDs the zone can be resized to.
    pub leds_max: u32,
    /// Current LED count.
    pub leds_count: u32,
    /// Matrix layout, for matrix zones.
    pub matrix: Option<MatrixMap>,
    /// Segments, from protocol version 4.
    pub segments: Vec<SegmentDescription>,
}

/// One LED.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedDescription {
    /// Display name.
    pub name: String,
    /// Controller-specific LED value.
    pub value: u32,
}

/// Everything the server reports about one controller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerDescription {
    /// Kind of hardware.
    pub device_type: DeviceType,
    /// Display name, e.g. `Corsair Vengeance RGB DDR5`.
    pub name: String,
    /// Vendor.
    pub vendor: String,
    /// Free-text description.
    pub description: String,
    /// Firmware or driver version.
    pub version: String,
    /// Serial number, often empty.
    pub serial: String,
    /// Bus location, e.g. a USB path or I2C address.
    pub location: String,
    /// Index into `modes` of the mode currently active.
    pub active_mode: i32,
    /// Modes offered.
    pub modes: Vec<ModeDescription>,
    /// Zones.
    pub zones: Vec<ZoneDescription>,
    /// LEDs across all zones.
    pub leds: Vec<LedDescription>,
    /// Current per-LED colors, parallel to `leds`.
    pub colors: Vec<Rgb>,
}

impl ControllerDescription {
    /// Parses a description serialized by the server at `protocol_version`,
    /// without the leading `u32` size.
    ///
    /// # Errors
    /// [`ProtocolError::Truncated`] when the payload ends early, or
    /// [`ProtocolError::UnsupportedVersion`] above [`MAX_PROTOCOL_VERSION`].
    pub fn decode(payload: &[u8], protocol_version: u32) -> Result<Self, ProtocolError> {
        if protocol_version > MAX_PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedVersion(protocol_version));
        }
        let mut reader = Reader::new(payload);
        let device_type = DeviceType::from_raw(reader.i32()?);
        let name = reader.string()?;
        let vendor = reader.string()?;
        let description = reader.string()?;
        let version = reader.string()?;
        let serial = reader.string()?;
        let location = reader.string()?;

        let num_modes = reader.u16()?;
        let active_mode = reader.i32()?;
        let modes = (0..num_modes)
            .map(|_| ModeDescription::decode(&mut reader, protocol_version))
            .collect::<Result<Vec<_>, _>>()?;

        let num_zones = reader.u16()?;
        let zones = (0..num_zones)
            .map(|_| decode_zone(&mut reader, protocol_version))
            .collect::<Result<Vec<_>, _>>()?;

        let num_leds = reader.u16()?;
        let leds = (0..num_leds)
            .map(|_| {
                Ok(LedDescription {
                    name: reader.string()?,
                    value: reader.u32()?,
                })
            })
            .collect::<Result<Vec<_>, ProtocolError>>()?;

        let colors = reader.colors()?;

        Ok(Self {
            device_type,
            name,
            vendor,
            description,
            version,
            serial,
            location,
            active_mode,
            modes,
            zones,
            leds,
            colors,
        })
    }

    /// Serializes the description as the server would, without the size prefix.
    #[must_use]
    pub fn encode(&self, protocol_version: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(self.device_type as i32).to_le_bytes());
        for text in [
            &self.name,
            &self.vendor,
            &self.description,
            &self.version,
            &self.serial,
            &self.location,
        ] {
            put_string(&mut out, text);
        }
        put_u16_len(&mut out, self.modes.len());
        out.extend_from_slice(&self.active_mode.to_le_bytes());
        for mode in &self.modes {
            mode.encode(&mut out, protocol_version);
        }
        put_u16_len(&mut out, self.zones.len());
        for zone in &self.zones {
            encode_zone(zone, &mut out, protocol_version);
        }
        put_u16_len(&mut out, self.leds.len());
        for led in &self.leds {
            put_string(&mut out, &led.name);
            out.extend_from_slice(&led.value.to_le_bytes());
        }
        put_colors(&mut out, &self.colors);
        out
    }

    /// Finds a mode by name, case-insensitively.
    #[must_use]
    pub fn find_mode(&self, name: &str) -> Option<(usize, &ModeDescription)> {
        self.modes
            .iter()
            .enumerate()
            .find(|(_, mode)| mode.name.eq_ignore_ascii_case(name))
    }

    /// Number of LEDs, falling back to the color array when LEDs are unnamed.
    #[must_use]
    pub fn led_count(&self) -> usize {
        if self.leds.is_empty() {
            self.colors.len()
        } else {
            self.leds.len()
        }
    }
}

fn decode_zone(
    reader: &mut Reader<'_>,
    protocol_version: u32,
) -> Result<ZoneDescription, ProtocolError> {
    let name = reader.string()?;
    let zone_type = reader.u32()?;
    let leds_min = reader.u32()?;
    let leds_max = reader.u32()?;
    let leds_count = reader.u32()?;
    let matrix_len = usize::from(reader.u16()?);
    let matrix = if matrix_len == 0 {
        None
    } else {
        let mut matrix_reader = Reader::new(reader.take(matrix_len)?);
        let height = matrix_reader.u32()?;
        let width = matrix_reader.u32()?;
        let cells = usize::try_from(u64::from(height) * u64::from(width)).map_err(|_| {
            ProtocolError::Truncated {
                needed: usize::MAX,
                available: matrix_len,
            }
        })?;
        let map = (0..cells)
            .map(|_| matrix_reader.u32())
            .collect::<Result<Vec<_>, _>>()?;
        Some(MatrixMap { height, width, map })
    };
    let segments = if protocol_version >= SEGMENTS_FROM_VERSION {
        let count = reader.u16()?;
        (0..count)
            .map(|_| {
                Ok(SegmentDescription {
                    name: reader.string()?,
                    segment_type: reader.u32()?,
                    start_index: reader.u32()?,
                    leds_count: reader.u32()?,
                })
            })
            .collect::<Result<Vec<_>, ProtocolError>>()?
    } else {
        Vec::new()
    };
    Ok(ZoneDescription {
        name,
        zone_type,
        leds_min,
        leds_max,
        leds_count,
        matrix,
        segments,
    })
}

fn encode_zone(zone: &ZoneDescription, out: &mut Vec<u8>, protocol_version: u32) {
    put_string(out, &zone.name);
    out.extend_from_slice(&zone.zone_type.to_le_bytes());
    out.extend_from_slice(&zone.leds_min.to_le_bytes());
    out.extend_from_slice(&zone.leds_max.to_le_bytes());
    out.extend_from_slice(&zone.leds_count.to_le_bytes());
    match &zone.matrix {
        None => put_u16_len(out, 0),
        Some(matrix) => {
            let mut body = Vec::new();
            body.extend_from_slice(&matrix.height.to_le_bytes());
            body.extend_from_slice(&matrix.width.to_le_bytes());
            for cell in &matrix.map {
                body.extend_from_slice(&cell.to_le_bytes());
            }
            put_u16_len(out, body.len());
            out.extend_from_slice(&body);
        }
    }
    if protocol_version >= SEGMENTS_FROM_VERSION {
        put_u16_len(out, zone.segments.len());
        for segment in &zone.segments {
            put_string(out, &segment.name);
            out.extend_from_slice(&segment.segment_type.to_le_bytes());
            out.extend_from_slice(&segment.start_index.to_le_bytes());
            out.extend_from_slice(&segment.leds_count.to_le_bytes());
        }
    }
}

/// Payload of an `UpdateMode` or `SaveMode` packet: total size, mode index,
/// then the mode body.
#[must_use]
pub fn encode_mode_update(
    mode_index: u32,
    mode: &ModeDescription,
    protocol_version: u32,
) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&mode_index.to_le_bytes());
    mode.encode(&mut body, protocol_version);
    with_size_prefix(&body)
}

/// Payload of an `UpdateLeds` packet: total size, then the color array.
#[must_use]
pub fn encode_led_update(colors: &[Rgb]) -> Vec<u8> {
    let mut body = Vec::new();
    put_colors(&mut body, colors);
    with_size_prefix(&body)
}

/// Splits a size-prefixed reply into its body, checking the declared size.
///
/// # Errors
/// [`ProtocolError::Truncated`] when the declared size and the payload disagree.
pub fn strip_size_prefix(payload: &[u8]) -> Result<&[u8], ProtocolError> {
    let mut reader = Reader::new(payload);
    let declared = usize::try_from(reader.u32()?).unwrap_or(usize::MAX);
    if declared != payload.len() {
        return Err(ProtocolError::Truncated {
            needed: declared,
            available: payload.len(),
        });
    }
    Ok(&payload[SIZE_PREFIX_LEN..])
}

fn with_size_prefix(body: &[u8]) -> Vec<u8> {
    let total = u32::try_from(body.len() + SIZE_PREFIX_LEN).expect("payload fits in u32");
    let mut out = Vec::with_capacity(body.len() + SIZE_PREFIX_LEN);
    out.extend_from_slice(&total.to_le_bytes());
    out.extend_from_slice(body);
    out
}

fn put_u16_len(out: &mut Vec<u8>, len: usize) {
    out.extend_from_slice(&u16::try_from(len).expect("count fits in u16").to_le_bytes());
}

fn put_string(out: &mut Vec<u8>, text: &str) {
    put_u16_len(out, text.len() + 1);
    out.extend_from_slice(text.as_bytes());
    out.push(0);
}

fn put_colors(out: &mut Vec<u8>, colors: &[Rgb]) {
    put_u16_len(out, colors.len());
    for color in colors {
        out.extend_from_slice(&color_to_wire(*color).to_le_bytes());
    }
}

/// Cursor over a payload.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], ProtocolError> {
        let available = self.data.len() - self.pos;
        if len > available {
            return Err(ProtocolError::Truncated {
                needed: len,
                available,
            });
        }
        let slice = &self.data[self.pos..self.pos + len];
        self.pos += len;
        Ok(slice)
    }

    fn u16(&mut self) -> Result<u16, ProtocolError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, ProtocolError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn i32(&mut self) -> Result<i32, ProtocolError> {
        let bytes = self.take(4)?;
        Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn string(&mut self) -> Result<String, ProtocolError> {
        let len = usize::from(self.u16()?);
        let bytes = self.take(len)?;
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        Ok(String::from_utf8_lossy(&bytes[..end]).into_owned())
    }

    fn colors(&mut self) -> Result<Vec<Rgb>, ProtocolError> {
        let count = self.u16()?;
        (0..count)
            .map(|_| self.u32().map(color_from_wire))
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::protocol::{ColorMode, mode_flags};

    /// A controller shaped like the Gigabyte RGB Fusion 2 USB device.
    pub(crate) fn sample_controller() -> ControllerDescription {
        ControllerDescription {
            device_type: DeviceType::Motherboard,
            name: "Gigabyte RGB Fusion 2 USB".into(),
            vendor: "Gigabyte".into(),
            description: "RGB Fusion 2.0 USB controller".into(),
            version: "1.0.10.0".into(),
            serial: String::new(),
            location: "HID: \\\\?\\hid#vid_048d".into(),
            active_mode: 1,
            modes: vec![
                ModeDescription {
                    name: "Direct".into(),
                    value: 0xFFFF,
                    flags: mode_flags::HAS_PER_LED_COLOR,
                    color_mode: ColorMode::PerLed as u32,
                    ..ModeDescription::default()
                },
                ModeDescription {
                    name: "Static".into(),
                    value: 1,
                    flags: mode_flags::HAS_MODE_SPECIFIC_COLOR
                        | mode_flags::HAS_BRIGHTNESS
                        | mode_flags::MANUAL_SAVE,
                    brightness_min: 0,
                    brightness_max: 100,
                    brightness: 100,
                    colors_min: 1,
                    colors_max: 1,
                    color_mode: ColorMode::ModeSpecific as u32,
                    colors: vec![Rgb::new(255, 0, 0)],
                    ..ModeDescription::default()
                },
                ModeDescription {
                    name: "Breathing".into(),
                    value: 2,
                    flags: mode_flags::HAS_SPEED
                        | mode_flags::HAS_MODE_SPECIFIC_COLOR
                        | mode_flags::HAS_BRIGHTNESS
                        | mode_flags::MANUAL_SAVE,
                    speed_min: 0,
                    speed_max: 4,
                    speed: 2,
                    brightness_min: 0,
                    brightness_max: 100,
                    brightness: 100,
                    colors_min: 1,
                    colors_max: 1,
                    color_mode: ColorMode::ModeSpecific as u32,
                    colors: vec![Rgb::new(0, 0, 255)],
                    ..ModeDescription::default()
                },
            ],
            zones: vec![
                ZoneDescription {
                    name: "Motherboard".into(),
                    zone_type: 1,
                    leds_min: 4,
                    leds_max: 4,
                    leds_count: 4,
                    matrix: None,
                    segments: Vec::new(),
                },
                ZoneDescription {
                    name: "D_LED1".into(),
                    zone_type: 2,
                    leds_min: 0,
                    leds_max: 120,
                    leds_count: 2,
                    matrix: Some(MatrixMap {
                        height: 1,
                        width: 2,
                        map: vec![4, 5],
                    }),
                    segments: vec![SegmentDescription {
                        name: "Fans".into(),
                        segment_type: 1,
                        start_index: 0,
                        leds_count: 2,
                    }],
                },
            ],
            leds: (0..6)
                .map(|i| LedDescription {
                    name: format!("LED {i}"),
                    value: i,
                })
                .collect(),
            colors: vec![Rgb::new(1, 2, 3); 6],
        }
    }

    #[test]
    fn description_round_trips_at_version_4() {
        let original = sample_controller();
        let bytes = original.encode(4);
        assert_eq!(ControllerDescription::decode(&bytes, 4).unwrap(), original);
    }

    #[test]
    fn description_round_trips_at_version_3_without_segments() {
        let mut original = sample_controller();
        let bytes = original.encode(3);
        original.zones[1].segments.clear();
        assert_eq!(ControllerDescription::decode(&bytes, 3).unwrap(), original);
    }

    #[test]
    fn truncated_payload_is_reported() {
        let bytes = sample_controller().encode(4);
        let result = ControllerDescription::decode(&bytes[..bytes.len() / 2], 4);
        assert!(matches!(result, Err(ProtocolError::Truncated { .. })));
    }

    #[test]
    fn newer_versions_are_refused() {
        let bytes = sample_controller().encode(4);
        assert_eq!(
            ControllerDescription::decode(&bytes, MAX_PROTOCOL_VERSION + 1),
            Err(ProtocolError::UnsupportedVersion(MAX_PROTOCOL_VERSION + 1))
        );
    }

    #[test]
    fn mode_update_has_size_index_and_body() {
        let mode = sample_controller().modes[1].clone();
        let payload = encode_mode_update(1, &mode, 4);
        let total = u32::from_le_bytes(payload[0..4].try_into().unwrap());
        assert_eq!(total as usize, payload.len());
        assert_eq!(u32::from_le_bytes(payload[4..8].try_into().unwrap()), 1);
        let mut body = Vec::new();
        mode.encode(&mut body, 4);
        assert_eq!(&payload[8..], &body[..]);
    }

    #[test]
    fn led_update_has_size_count_and_colors() {
        let payload = encode_led_update(&[Rgb::new(1, 2, 3), Rgb::new(4, 5, 6)]);
        assert_eq!(payload.len(), 4 + 2 + 8);
        assert_eq!(u32::from_le_bytes(payload[0..4].try_into().unwrap()), 14);
        assert_eq!(u16::from_le_bytes([payload[4], payload[5]]), 2);
        assert_eq!(&payload[6..10], &[1, 2, 3, 0]);
    }

    #[test]
    fn size_prefix_is_checked() {
        let payload = encode_led_update(&[Rgb::BLACK]);
        assert_eq!(
            strip_size_prefix(&payload).unwrap().len(),
            payload.len() - 4
        );
        assert!(matches!(
            strip_size_prefix(&payload[..payload.len() - 1]),
            Err(ProtocolError::Truncated { .. })
        ));
    }

    #[test]
    fn find_mode_is_case_insensitive() {
        let controller = sample_controller();
        assert_eq!(controller.find_mode("static").map(|(i, _)| i), Some(1));
        assert!(controller.find_mode("Rainbow").is_none());
    }
}
