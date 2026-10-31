//! Wire format of the OpenRGB SDK protocol.
//!
//! Every message is a 16-byte little-endian header followed by a payload:
//! magic `ORGB`, device index, packet id, payload length.

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
/// Highest protocol version this client understands.
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
    /// Negotiate protocol version. Payload and reply: `u32`.
    RequestProtocolVersion = 40,
    /// Name this client in the server UI. Payload: NUL-terminated string.
    SetClientName = 50,
    /// Server notification that the device list changed. No payload.
    DeviceListUpdated = 100,
    /// Write all LEDs of a controller. Payload: `u32` length, `u16` count, colors.
    UpdateLeds = 1050,
    /// Write all LEDs of one zone.
    UpdateZoneLeds = 1051,
    /// Write one LED.
    UpdateSingleLed = 1052,
    /// Switch the controller to its direct/custom mode.
    SetCustomMode = 1100,
    /// Set the active mode and its parameters.
    UpdateMode = 1101,
    /// Set the active mode and ask the controller to store it.
    SaveMode = 1102,
}

/// Errors in decoding a header.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtocolError {
    /// The header did not start with [`MAGIC`].
    #[error("packet does not start with the ORGB magic")]
    BadMagic,
    /// The packet id is not one this client knows.
    #[error("unknown packet id {0}")]
    UnknownPacketId(u32),
}

impl TryFrom<u32> for PacketId {
    type Error = ProtocolError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => PacketId::RequestControllerCount,
            1 => PacketId::RequestControllerData,
            40 => PacketId::RequestProtocolVersion,
            50 => PacketId::SetClientName,
            100 => PacketId::DeviceListUpdated,
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
}
