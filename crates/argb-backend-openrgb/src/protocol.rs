//! Wire format of the OpenRGB SDK protocol.
//!
//! Every message is a 16-byte little-endian header followed by a payload:
//! magic `ORGB`, device index, packet id, payload length. Multi-byte
//! integers in payloads are little-endian as well.

use argb_domain::Rgb;
use thiserror::Error;

/// First four bytes of every packet.
pub const MAGIC: [u8; 4] = *b"ORGB";
/// Size of the fixed header.
pub const HEADER_LEN: usize = 16;
/// Default SDK server port.
pub const DEFAULT_PORT: u16 = 6742;
/// Lowest protocol version that carries mode brightness and save-to-device,
/// both of which this project relies on.
pub const MIN_PROTOCOL_VERSION: u32 = 3;
/// Highest protocol version this client understands. Version 5 adds zone and
/// controller flags, version 6 changes device addressing and adds
/// acknowledgements; neither is needed here, and negotiating 4 keeps the
/// server's serialization on the layout this crate parses.
pub const MAX_PROTOCOL_VERSION: u32 = 4;
/// Device index used for packets that address the server rather than a device.
pub const SERVER_DEVICE_INDEX: u32 = 0;

/// Packet identifiers understood by this client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum PacketId {
    /// Ask how many controllers the server exposes. Reply: `u32`.
    RequestControllerCount = 0,
    /// Ask for one controller's description. Payload: protocol version.
    RequestControllerData = 1,
    /// Acknowledgement sent by version 6 servers; never seen at version 4.
    Ack = 10,
    /// Negotiate protocol version. Payload and reply: `u32`.
    RequestProtocolVersion = 40,
    /// Name this client in the server UI. Payload: NUL-terminated string.
    SetClientName = 50,
    /// Server name notification (version 6 servers only).
    SetServerName = 51,
    /// Server host name notification (version 6 servers only).
    SetServerHostname = 55,
    /// Server notification that the device list changed. No payload.
    DeviceListUpdated = 100,
    /// Server notification that a rescan started.
    DetectionStarted = 101,
    /// Server notification with rescan progress.
    DetectionProgressChanged = 102,
    /// Server notification that a rescan finished.
    DetectionComplete = 103,
    /// Ask the server to rescan hardware.
    RequestRescanDevices = 140,
    /// Write all LEDs of a controller. Payload: see `encode_led_update`.
    UpdateLeds = 1050,
    /// Write all LEDs of one zone.
    UpdateZoneLeds = 1051,
    /// Write one LED.
    UpdateSingleLed = 1052,
    /// Switch the controller to its direct/custom mode. No payload.
    SetCustomMode = 1100,
    /// Set the active mode and its parameters. Payload: see `encode_mode_update`.
    UpdateMode = 1101,
    /// Set the active mode and ask the controller to store it. Same payload.
    SaveMode = 1102,
}

impl PacketId {
    /// Whether the server sends this packet on its own, outside any request.
    #[must_use]
    pub const fn is_unsolicited(self) -> bool {
        matches!(
            self,
            PacketId::Ack
                | PacketId::SetServerName
                | PacketId::SetServerHostname
                | PacketId::DeviceListUpdated
                | PacketId::DetectionStarted
                | PacketId::DetectionProgressChanged
                | PacketId::DetectionComplete
        )
    }
}

/// Errors in decoding packets.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtocolError {
    /// The header did not start with [`MAGIC`].
    #[error("packet does not start with the ORGB magic")]
    BadMagic,
    /// The packet id is not one this client knows.
    #[error("unknown packet id {0}")]
    UnknownPacketId(u32),
    /// A payload ended before a field was complete.
    #[error("payload truncated: needed {needed} more bytes, {available} available")]
    Truncated {
        /// Bytes the next field required.
        needed: usize,
        /// Bytes left in the payload.
        available: usize,
    },
    /// The server serialized with a version this client cannot parse.
    #[error("protocol version {0} is not supported by this client")]
    UnsupportedVersion(u32),
}

impl TryFrom<u32> for PacketId {
    type Error = ProtocolError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => PacketId::RequestControllerCount,
            1 => PacketId::RequestControllerData,
            10 => PacketId::Ack,
            40 => PacketId::RequestProtocolVersion,
            50 => PacketId::SetClientName,
            51 => PacketId::SetServerName,
            55 => PacketId::SetServerHostname,
            100 => PacketId::DeviceListUpdated,
            101 => PacketId::DetectionStarted,
            102 => PacketId::DetectionProgressChanged,
            103 => PacketId::DetectionComplete,
            140 => PacketId::RequestRescanDevices,
            1050 => PacketId::UpdateLeds,
            1051 => PacketId::UpdateZoneLeds,
            1052 => PacketId::UpdateSingleLed,
            1100 => PacketId::SetCustomMode,
            1101 => PacketId::UpdateMode,
            1102 => PacketId::SaveMode,
            other => return Err(ProtocolError::UnknownPacketId(other)),
        })
    }
}

/// The fixed packet header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Controller the packet addresses, or [`SERVER_DEVICE_INDEX`].
    pub device_index: u32,
    /// What the packet is.
    pub packet_id: PacketId,
    /// Number of payload bytes that follow.
    pub payload_len: u32,
}

impl Header {
    /// Serializes the header.
    #[must_use]
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut bytes = [0u8; HEADER_LEN];
        bytes[0..4].copy_from_slice(&MAGIC);
        bytes[4..8].copy_from_slice(&self.device_index.to_le_bytes());
        bytes[8..12].copy_from_slice(&(self.packet_id as u32).to_le_bytes());
        bytes[12..16].copy_from_slice(&self.payload_len.to_le_bytes());
        bytes
    }

    /// Parses a header.
    ///
    /// # Errors
    /// [`ProtocolError::BadMagic`] or [`ProtocolError::UnknownPacketId`].
    pub fn decode(bytes: &[u8; HEADER_LEN]) -> Result<Self, ProtocolError> {
        let [
            m0,
            m1,
            m2,
            m3,
            d0,
            d1,
            d2,
            d3,
            i0,
            i1,
            i2,
            i3,
            l0,
            l1,
            l2,
            l3,
        ] = *bytes;
        if [m0, m1, m2, m3] != MAGIC {
            return Err(ProtocolError::BadMagic);
        }
        Ok(Self {
            device_index: u32::from_le_bytes([d0, d1, d2, d3]),
            packet_id: PacketId::try_from(u32::from_le_bytes([i0, i1, i2, i3]))?,
            payload_len: u32::from_le_bytes([l0, l1, l2, l3]),
        })
    }
}

/// Bit flags describing what a mode supports.
pub mod mode_flags {
    /// The mode has an adjustable speed.
    pub const HAS_SPEED: u32 = 1 << 0;
    /// The mode has a left/right direction.
    pub const HAS_DIRECTION_LR: u32 = 1 << 1;
    /// The mode has an up/down direction.
    pub const HAS_DIRECTION_UD: u32 = 1 << 2;
    /// The mode has a horizontal/vertical direction.
    pub const HAS_DIRECTION_HV: u32 = 1 << 3;
    /// The mode has a hardware brightness control.
    pub const HAS_BRIGHTNESS: u32 = 1 << 4;
    /// The mode shows the controller's per-LED color array.
    pub const HAS_PER_LED_COLOR: u32 = 1 << 5;
    /// The mode shows its own color list.
    pub const HAS_MODE_SPECIFIC_COLOR: u32 = 1 << 6;
    /// The mode can pick random colors.
    pub const HAS_RANDOM_COLOR: u32 = 1 << 7;
    /// The controller stores the mode only when asked (save packet).
    pub const MANUAL_SAVE: u32 = 1 << 8;
    /// The controller stores the mode whenever it changes.
    pub const AUTOMATIC_SAVE: u32 = 1 << 9;
}

/// How a mode is colored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ColorMode {
    /// The mode has no color input.
    None = 0,
    /// The mode shows the controller's per-LED color array.
    PerLed = 1,
    /// The mode shows its own color list.
    ModeSpecific = 2,
    /// The mode picks colors itself.
    Random = 3,
}

impl ColorMode {
    /// Decodes the wire value, if known.
    #[must_use]
    pub const fn from_raw(value: u32) -> Option<Self> {
        Some(match value {
            0 => ColorMode::None,
            1 => ColorMode::PerLed,
            2 => ColorMode::ModeSpecific,
            3 => ColorMode::Random,
            _ => return None,
        })
    }
}

/// Kind of hardware a controller drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum DeviceType {
    /// Motherboard, including its RGB headers.
    Motherboard = 0,
    /// Memory module.
    Dram = 1,
    /// Graphics card.
    Gpu = 2,
    /// CPU or liquid cooler.
    Cooler = 3,
    /// LED strip.
    LedStrip = 4,
    /// Keyboard.
    Keyboard = 5,
    /// Mouse.
    Mouse = 6,
    /// Mouse mat.
    MouseMat = 7,
    /// Headset.
    Headset = 8,
    /// Headset stand.
    HeadsetStand = 9,
    /// Gamepad.
    Gamepad = 10,
    /// Stand-alone light.
    Light = 11,
    /// Speaker.
    Speaker = 12,
    /// Virtual device.
    Virtual = 13,
    /// Storage drive.
    Storage = 14,
    /// Case.
    Case = 15,
    /// Microphone.
    Microphone = 16,
    /// Accessory.
    Accessory = 17,
    /// Keypad.
    Keypad = 18,
    /// Laptop.
    Laptop = 19,
    /// Monitor.
    Monitor = 20,
    /// Anything else, and any value this client does not know.
    Unknown = 21,
}

impl DeviceType {
    /// Decodes the wire value; unknown values map to [`DeviceType::Unknown`].
    #[must_use]
    pub const fn from_raw(value: i32) -> Self {
        match value {
            0 => DeviceType::Motherboard,
            1 => DeviceType::Dram,
            2 => DeviceType::Gpu,
            3 => DeviceType::Cooler,
            4 => DeviceType::LedStrip,
            5 => DeviceType::Keyboard,
            6 => DeviceType::Mouse,
            7 => DeviceType::MouseMat,
            8 => DeviceType::Headset,
            9 => DeviceType::HeadsetStand,
            10 => DeviceType::Gamepad,
            11 => DeviceType::Light,
            12 => DeviceType::Speaker,
            13 => DeviceType::Virtual,
            14 => DeviceType::Storage,
            15 => DeviceType::Case,
            16 => DeviceType::Microphone,
            17 => DeviceType::Accessory,
            18 => DeviceType::Keypad,
            19 => DeviceType::Laptop,
            20 => DeviceType::Monitor,
            _ => DeviceType::Unknown,
        }
    }
}

impl std::fmt::Display for DeviceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            DeviceType::Motherboard => "motherboard",
            DeviceType::Dram => "dram",
            DeviceType::Gpu => "gpu",
            DeviceType::Cooler => "cooler",
            DeviceType::LedStrip => "led-strip",
            DeviceType::Keyboard => "keyboard",
            DeviceType::Mouse => "mouse",
            DeviceType::MouseMat => "mouse-mat",
            DeviceType::Headset => "headset",
            DeviceType::HeadsetStand => "headset-stand",
            DeviceType::Gamepad => "gamepad",
            DeviceType::Light => "light",
            DeviceType::Speaker => "speaker",
            DeviceType::Virtual => "virtual",
            DeviceType::Storage => "storage",
            DeviceType::Case => "case",
            DeviceType::Microphone => "microphone",
            DeviceType::Accessory => "accessory",
            DeviceType::Keypad => "keypad",
            DeviceType::Laptop => "laptop",
            DeviceType::Monitor => "monitor",
            DeviceType::Unknown => "unknown",
        };
        f.write_str(name)
    }
}

/// Packs a color the way OpenRGB does: red in the low byte, then green, then blue.
#[must_use]
pub const fn color_to_wire(color: Rgb) -> u32 {
    u32::from_le_bytes([color.r, color.g, color.b, 0])
}

/// Unpacks an OpenRGB color.
#[must_use]
pub const fn color_from_wire(value: u32) -> Rgb {
    let [r, g, b, _] = value.to_le_bytes();
    Rgb::new(r, g, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trips() {
        let header = Header {
            device_index: 3,
            packet_id: PacketId::UpdateLeds,
            payload_len: 42,
        };
        let bytes = header.encode();
        assert_eq!(&bytes[0..4], b"ORGB");
        assert_eq!(Header::decode(&bytes), Ok(header));
    }

    #[test]
    fn decode_rejects_bad_magic_and_unknown_ids() {
        let mut bytes = Header {
            device_index: 0,
            packet_id: PacketId::SaveMode,
            payload_len: 0,
        }
        .encode();
        bytes[0] = b'X';
        assert_eq!(Header::decode(&bytes), Err(ProtocolError::BadMagic));

        let mut bytes = Header {
            device_index: 0,
            packet_id: PacketId::SaveMode,
            payload_len: 0,
        }
        .encode();
        bytes[8..12].copy_from_slice(&9999u32.to_le_bytes());
        assert_eq!(
            Header::decode(&bytes),
            Err(ProtocolError::UnknownPacketId(9999))
        );
    }

    #[test]
    fn colors_pack_red_in_the_low_byte() {
        let color = Rgb::new(0x11, 0x22, 0x33);
        assert_eq!(color_to_wire(color), 0x0033_2211);
        assert_eq!(color_from_wire(0x0033_2211), color);
    }

    #[test]
    fn unsolicited_packets_are_classified() {
        assert!(PacketId::DeviceListUpdated.is_unsolicited());
        assert!(PacketId::Ack.is_unsolicited());
        assert!(!PacketId::RequestControllerData.is_unsolicited());
    }

    #[test]
    fn device_type_maps_unknown_values() {
        assert_eq!(DeviceType::from_raw(1), DeviceType::Dram);
        assert_eq!(DeviceType::from_raw(99), DeviceType::Unknown);
    }
}
