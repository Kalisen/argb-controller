//! The Govee Bluetooth LE command frames.
//!
//! Facts come from community reverse engineering (see
//! `docs/phase2-govee-ble.md`): every frame is 20 bytes, the first byte names
//! the frame type, the second the operation, the payload follows zero-padded,
//! and the last byte is the XOR of the nineteen before it. Commands go to one
//! characteristic, replies arrive as notifications on another.

use uuid::Uuid;

/// GATT service every known Govee BLE light exposes.
pub const CONTROL_SERVICE: Uuid = Uuid::from_u128(0x0001_0203_0405_0607_0809_0a0b_0c0d_1910);
/// Characteristic that carries replies and status as notifications.
pub const NOTIFY_CHARACTERISTIC: Uuid = Uuid::from_u128(0x0001_0203_0405_0607_0809_0a0b_0c0d_2b10);
/// Characteristic that takes command frames.
pub const WRITE_CHARACTERISTIC: Uuid = Uuid::from_u128(0x0001_0203_0405_0607_0809_0a0b_0c0d_2b11);

/// Length of every frame.
pub const FRAME_LEN: usize = 20;

/// Frame types (byte 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameType {
    /// A command the light carries out.
    Command = 0x33,
    /// A status read; the light answers with a notification. Also the keep-alive.
    Status = 0xaa,
    /// The encrypted-channel handshake; byte 1 is a [`HandshakeStep`].
    Handshake = 0xe7,
}

/// Steps of the handshake that opens the encrypted channel (byte 1 of a
/// [`FrameType::Handshake`] frame).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum HandshakeStep {
    /// Host sends random fill; the light answers with the 16-byte session key
    /// in bytes 2-17.
    SessionKey = 0x01,
    /// Host confirms; the light echoes an acknowledgement.
    Confirm = 0x02,
}

/// Where the session key sits in a [`HandshakeStep::SessionKey`] reply.
pub const SESSION_KEY_RANGE: std::ops::Range<usize> = 2..18;

/// Operations (byte 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Operation {
    /// Power on or off; also the keep-alive when sent as a status read.
    Power = 0x01,
    /// Whole-device brightness.
    Brightness = 0x04,
    /// Lighting mode: manual color, scene, segments.
    Mode = 0x05,
}

/// Sub-modes of [`Operation::Mode`] (byte 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Mode {
    /// One color for the whole device on the older family: bytes 3-5 are
    /// red, green, blue.
    ManualColor = 0x02,
    /// Segment addressing used by the newer family (Glide Hexa and the
    /// floor lamp among them).
    Segment = 0x15,
}

/// Sub-commands of [`Mode::Segment`] (byte 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SegmentCommand {
    /// Color for the segments selected by the mask: bytes 4-6 are red,
    /// green, blue; the mask position depends on the [`Layout`].
    Color = 0x01,
}

/// How a model lays out its segment-color frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// Older family: the manual-color sub-mode with no segments.
    Manual,
    /// Glide Hexa (H6061): ten panels, mask at bytes 9-10 (panels 1-8 in
    /// byte 9, panels 9-10 in byte 10).
    Hexa,
    /// Lyra floor lamp (H6076): seven segments, a Kelvin word and a preview
    /// color before the little-endian mask at bytes 12-13.
    Lamp,
}

/// Segment mask selecting all ten Glide Hexa panels, as it sits in the frame.
pub const HEXA_ALL_PANELS_MASK: [u8; 2] = [0xff, 0x03];
/// Segment mask selecting all seven floor lamp segments, little-endian.
pub const LAMP_ALL_SEGMENTS_MASK: [u8; 2] = [0x7f, 0x00];
/// Byte offset of the mask in a Glide Hexa segment-color frame.
const HEXA_MASK_OFFSET: usize = 9;
/// Byte offset of the mask in a floor lamp segment-color frame.
const LAMP_MASK_OFFSET: usize = 12;

/// Value meaning "on" for [`Operation::Power`].
const POWER_ON: u8 = 0x01;
/// Value meaning "off" for [`Operation::Power`].
const POWER_OFF: u8 = 0x00;
/// Highest whole-device brightness (percent) on both models.
pub const BRIGHTNESS_MAX: u8 = 100;

/// Builds a frame from its type, operation and payload (bytes 2 onwards).
#[must_use]
pub fn frame(kind: FrameType, operation: Operation, payload: &[u8]) -> [u8; FRAME_LEN] {
    let mut bytes = [0u8; FRAME_LEN];
    bytes[0] = kind as u8;
    bytes[1] = operation as u8;
    let body = &mut bytes[2..FRAME_LEN - 1];
    let copied = payload.len().min(body.len());
    body[..copied].copy_from_slice(&payload[..copied]);
    bytes[FRAME_LEN - 1] = checksum(&bytes[..FRAME_LEN - 1]);
    bytes
}

/// A handshake frame: the step and a fill the light ignores, framed like
/// every other frame. The caller encrypts it with the pre-shared key.
#[must_use]
pub fn handshake(step: HandshakeStep, fill: &[u8]) -> [u8; FRAME_LEN] {
    let mut bytes = [0u8; FRAME_LEN];
    bytes[0] = FrameType::Handshake as u8;
    bytes[1] = step as u8;
    let body = &mut bytes[2..FRAME_LEN - 1];
    let copied = fill.len().min(body.len());
    body[..copied].copy_from_slice(&fill[..copied]);
    bytes[FRAME_LEN - 1] = checksum(&bytes[..FRAME_LEN - 1]);
    bytes
}

/// Whether a plaintext frame is the handshake reply for `step`.
#[must_use]
pub fn is_handshake_reply(plain: &[u8], step: HandshakeStep) -> bool {
    plain.len() == FRAME_LEN && plain[0] == FrameType::Handshake as u8 && plain[1] == step as u8
}

/// XOR of every byte.
fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |acc, byte| acc ^ byte)
}

/// The power status read the app sends every two seconds as a keep-alive.
#[must_use]
pub fn keep_alive() -> [u8; FRAME_LEN] {
    frame(FrameType::Status, Operation::Power, &[])
}

/// Power on or off.
#[must_use]
pub fn power(on: bool) -> [u8; FRAME_LEN] {
    frame(
        FrameType::Command,
        Operation::Power,
        &[if on { POWER_ON } else { POWER_OFF }],
    )
}

/// Whole-device brightness in percent (clamped to [`BRIGHTNESS_MAX`]).
#[must_use]
pub fn brightness(percent: u8) -> [u8; FRAME_LEN] {
    frame(
        FrameType::Command,
        Operation::Brightness,
        &[percent.min(BRIGHTNESS_MAX)],
    )
}

/// One color for the whole device, in the frame `layout` expects.
#[must_use]
pub fn color(layout: Layout, red: u8, green: u8, blue: u8) -> [u8; FRAME_LEN] {
    match layout {
        Layout::Manual => frame(
            FrameType::Command,
            Operation::Mode,
            &[Mode::ManualColor as u8, red, green, blue],
        ),
        Layout::Hexa => segment_color(red, green, blue, HEXA_MASK_OFFSET, HEXA_ALL_PANELS_MASK),
        Layout::Lamp => segment_color(red, green, blue, LAMP_MASK_OFFSET, LAMP_ALL_SEGMENTS_MASK),
    }
}

/// One color for the segments selected by `mask`, placed at `mask_offset`
/// in the frame.
fn segment_color(
    red: u8,
    green: u8,
    blue: u8,
    mask_offset: usize,
    mask: [u8; 2],
) -> [u8; FRAME_LEN] {
    let mut payload = [0u8; FRAME_LEN - 3];
    payload[0] = Mode::Segment as u8;
    payload[1] = SegmentCommand::Color as u8;
    payload[2] = red;
    payload[3] = green;
    payload[4] = blue;
    payload[mask_offset - 2] = mask[0];
    payload[mask_offset - 1] = mask[1];
    frame(FrameType::Command, Operation::Mode, &payload)
}

/// Hex rendering of a frame or reply for the log.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(hex_digits: &str) -> [u8; FRAME_LEN] {
        let mut bytes = [0u8; FRAME_LEN];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex_digits[i * 2..i * 2 + 2], 16).unwrap();
        }
        bytes
    }

    #[test]
    fn keep_alive_matches_the_documented_frame() {
        assert_eq!(
            keep_alive(),
            parse("aa010000000000000000000000000000000000ab")
        );
    }

    #[test]
    fn hexa_single_panel_matches_the_documented_sample() {
        // Panel 1 red, from the govee-ble-segments documentation.
        let bytes = segment_color(0xff, 0, 0, HEXA_MASK_OFFSET, [0x01, 0x00]);
        assert_eq!(bytes, parse("33051501ff0000000001000000000000000000dc"));
    }

    #[test]
    fn hexa_whole_device_red() {
        assert_eq!(
            color(Layout::Hexa, 0xff, 0, 0),
            parse("33051501ff00000000ff03000000000000000021")
        );
    }

    #[test]
    fn lamp_whole_device_red_matches_the_home_assistant_test_vector() {
        assert_eq!(
            color(Layout::Lamp, 0xff, 0, 0),
            parse("33051501ff000000000000007f000000000000a2")
        );
    }

    #[test]
    fn manual_color_carries_rgb_after_the_sub_mode() {
        let bytes = color(Layout::Manual, 0x12, 0x34, 0x56);
        assert_eq!(&bytes[..6], &[0x33, 0x05, 0x02, 0x12, 0x34, 0x56]);
    }

    #[test]
    fn power_frames() {
        assert_eq!(
            power(true),
            parse("3301010000000000000000000000000000000033")
        );
        assert_eq!(
            power(false),
            parse("3301000000000000000000000000000000000032")
        );
    }

    #[test]
    fn brightness_is_clamped_to_the_scale() {
        assert_eq!(brightness(250)[2], BRIGHTNESS_MAX);
        assert_eq!(
            brightness(50),
            parse("3304320000000000000000000000000000000005")
        );
    }

    #[test]
    fn handshake_frame_carries_the_step_and_a_checksum() {
        let fill = [0x11u8; 17];
        let bytes = handshake(HandshakeStep::SessionKey, &fill);
        assert_eq!(&bytes[..2], &[0xe7, 0x01]);
        assert_eq!(&bytes[2..19], &fill);
        assert_eq!(bytes[19], 0xe7 ^ 0x01 ^ 0x11);
        assert!(is_handshake_reply(&bytes, HandshakeStep::SessionKey));
        assert!(!is_handshake_reply(&bytes, HandshakeStep::Confirm));
    }

    #[test]
    fn hex_renders_bytes_with_spaces() {
        assert_eq!(hex(&[0xaa, 0x01, 0xab]), "aa 01 ab");
    }
}
