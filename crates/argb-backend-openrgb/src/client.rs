//! Blocking TCP client for the OpenRGB SDK server.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use argb_domain::Rgb;
use thiserror::Error;
use tracing::{debug, info, trace};

use crate::description::{
    ControllerDescription, ModeDescription, encode_led_update, encode_mode_update,
    strip_size_prefix,
};
use crate::protocol::{
    HEADER_LEN, Header, MAX_PROTOCOL_VERSION, MIN_PROTOCOL_VERSION, PacketId, ProtocolError,
    SERVER_DEVICE_INDEX,
};

/// Time allowed for the TCP connection and for each reply.
pub const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// How long device-list announcements must stop after a rescan before
/// detection is considered finished. Detection takes about a second on the
/// reference machine; USB and bus probes can leave gaps between announcements.
pub const RESCAN_QUIET_PERIOD: Duration = Duration::from_secs(3);

/// Errors talking to the server.
#[derive(Debug, Error)]
pub enum OpenRgbError {
    /// Socket failure.
    #[error("OpenRGB connection failed")]
    Io(#[from] std::io::Error),
    /// The host name resolved to nothing.
    #[error("OpenRGB host {0:?} did not resolve to an address")]
    Unresolvable(String),
    /// Malformed packet.
    #[error("OpenRGB protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    /// A reply of a different kind than requested.
    #[error("expected OpenRGB packet {expected:?}, got {got:?}")]
    UnexpectedPacket {
        /// Packet asked for.
        expected: PacketId,
        /// Packet received.
        got: PacketId,
    },
    /// A reply shorter than its type requires.
    #[error("OpenRGB reply to {0:?} was too short")]
    ShortPayload(PacketId),
    /// The server speaks a protocol older than this client requires.
    #[error("OpenRGB server protocol version {server} is older than the required {required}")]
    ProtocolTooOld {
        /// Version the server offered.
        server: u32,
        /// Version this client requires.
        required: u32,
    },
}

/// The subset of controller operations the device adapter needs. Implemented
/// by [`OpenRgbClient`]; tests implement it with a recorder.
pub trait ControllerCommands {
    /// Sets `mode_index` active with the parameters in `mode`.
    ///
    /// # Errors
    /// [`OpenRgbError`] on I/O failure.
    fn update_mode(
        &mut self,
        controller: u32,
        mode_index: u32,
        mode: &ModeDescription,
    ) -> Result<(), OpenRgbError>;

    /// Like [`Self::update_mode`], and asks the controller to store the mode.
    ///
    /// # Errors
    /// [`OpenRgbError`] on I/O failure.
    fn save_mode(
        &mut self,
        controller: u32,
        mode_index: u32,
        mode: &ModeDescription,
    ) -> Result<(), OpenRgbError>;

    /// Writes every LED color of the controller.
    ///
    /// # Errors
    /// [`OpenRgbError`] on I/O failure.
    fn update_leds(&mut self, controller: u32, colors: &[Rgb]) -> Result<(), OpenRgbError>;
}

/// A connected SDK client.
pub struct OpenRgbClient {
    stream: TcpStream,
    protocol_version: u32,
}

impl OpenRgbClient {
    /// Connects, negotiates the protocol version and registers `client_name`.
    ///
    /// # Errors
    /// [`OpenRgbError`] on connection failure, version mismatch or protocol error.
    pub fn connect(host: &str, port: u16, client_name: &str) -> Result<Self, OpenRgbError> {
        let addr = resolve(host, port)?;
        let stream = TcpStream::connect_timeout(&addr, IO_TIMEOUT)?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        stream.set_nodelay(true)?;
        let mut client = Self {
            stream,
            protocol_version: 0,
        };
        client.negotiate_protocol()?;
        client.set_client_name(client_name)?;
        info!(%addr, version = client.protocol_version, "connected to OpenRGB");
        Ok(client)
    }

    /// Protocol version agreed with the server.
    #[must_use]
    pub const fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    /// Number of controllers the server exposes.
    ///
    /// # Errors
    /// [`OpenRgbError`] on I/O or protocol failure.
    pub fn controller_count(&mut self) -> Result<u32, OpenRgbError> {
        self.send(SERVER_DEVICE_INDEX, PacketId::RequestControllerCount, &[])?;
        let payload = self.receive(PacketId::RequestControllerCount)?;
        read_u32(&payload, PacketId::RequestControllerCount)
    }

    /// The description of controller `index`.
    ///
    /// # Errors
    /// [`OpenRgbError`] on I/O or protocol failure.
    pub fn controller(&mut self, index: u32) -> Result<ControllerDescription, OpenRgbError> {
        self.send(
            index,
            PacketId::RequestControllerData,
            &self.protocol_version.to_le_bytes(),
        )?;
        let payload = self.receive(PacketId::RequestControllerData)?;
        let body = strip_size_prefix(&payload)?;
        Ok(ControllerDescription::decode(body, self.protocol_version)?)
    }

    /// Descriptions of every controller, in index order.
    ///
    /// # Errors
    /// [`OpenRgbError`] on I/O or protocol failure.
    pub fn controllers(&mut self) -> Result<Vec<ControllerDescription>, OpenRgbError> {
        let count = self.controller_count()?;
        (0..count).map(|index| self.controller(index)).collect()
    }

    /// Asks the server to detect hardware again and waits up to `timeout`
    /// for the detection to finish. Returns whether it did, with at least one
    /// controller listed; the device list may have changed either way.
    ///
    /// The server announces a changed device list when it clears the list at
    /// the start of detection and again as controllers register, so the first
    /// announcement does not mean detection is over. At protocol version 4 the
    /// server sends no detection-complete packet (version 6 and later do), so
    /// detection counts as done once announcements stop for
    /// [`RESCAN_QUIET_PERIOD`] and the list is not empty.
    ///
    /// # Errors
    /// [`OpenRgbError`] on I/O or protocol failure.
    pub fn rescan(&mut self, timeout: Duration) -> Result<bool, OpenRgbError> {
        self.rescan_with_quiet_period(timeout, RESCAN_QUIET_PERIOD)
    }

    fn rescan_with_quiet_period(
        &mut self,
        timeout: Duration,
        quiet_period: Duration,
    ) -> Result<bool, OpenRgbError> {
        self.send(SERVER_DEVICE_INDEX, PacketId::RequestRescanDevices, &[])?;
        let deadline = Instant::now() + timeout;
        let mut announced = false;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            let wait = if announced {
                quiet_period.min(remaining)
            } else {
                remaining
            };
            match self.read_notification(wait)? {
                Some(PacketId::DetectionComplete) => return Ok(true),
                Some(PacketId::DeviceListUpdated) => announced = true,
                Some(_) => {}
                None if !announced => return Ok(false),
                None => {
                    if self.controller_count()? > 0 {
                        return Ok(true);
                    }
                    debug!("rescan went quiet with an empty device list; still waiting");
                }
            }
        }
    }

    /// Switches a controller to its direct or custom mode.
    ///
    /// # Errors
    /// [`OpenRgbError`] on I/O failure.
    pub fn set_custom_mode(&mut self, controller: u32) -> Result<(), OpenRgbError> {
        self.send(controller, PacketId::SetCustomMode, &[])
    }

    fn negotiate_protocol(&mut self) -> Result<(), OpenRgbError> {
        self.send(
            SERVER_DEVICE_INDEX,
            PacketId::RequestProtocolVersion,
            &MAX_PROTOCOL_VERSION.to_le_bytes(),
        )?;
        let payload = self.receive(PacketId::RequestProtocolVersion)?;
        let server = read_u32(&payload, PacketId::RequestProtocolVersion)?;
        if server < MIN_PROTOCOL_VERSION {
            return Err(OpenRgbError::ProtocolTooOld {
                server,
                required: MIN_PROTOCOL_VERSION,
            });
        }
        self.protocol_version = server.min(MAX_PROTOCOL_VERSION);
        Ok(())
    }

    fn set_client_name(&mut self, name: &str) -> Result<(), OpenRgbError> {
        let mut payload = name.as_bytes().to_vec();
        payload.push(0);
        self.send(SERVER_DEVICE_INDEX, PacketId::SetClientName, &payload)
    }

    fn send(
        &mut self,
        device_index: u32,
        packet_id: PacketId,
        payload: &[u8],
    ) -> Result<(), OpenRgbError> {
        let header = Header {
            device_index,
            packet_id,
            payload_len: u32::try_from(payload.len()).expect("payload fits in u32"),
        };
        trace!(?header, "sending");
        self.stream.write_all(&header.encode())?;
        self.stream.write_all(payload)?;
        self.stream.flush()?;
        Ok(())
    }

    /// Reads server notifications until `wanted` arrives or `timeout` passes.
    /// Any request reply arriving here is a protocol error, since nothing
    /// was asked.
    #[cfg(test)]
    fn wait_for_notification(
        &mut self,
        wanted: PacketId,
        timeout: Duration,
    ) -> Result<bool, OpenRgbError> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.read_notification(remaining)? {
                Some(id) if id == wanted => return Ok(true),
                Some(_) => {}
                None => return Ok(false),
            }
        }
    }

    /// Reads one server notification, waiting at most `timeout`. Returns
    /// `None` when none arrived. Any request reply arriving here is a
    /// protocol error, since nothing was asked.
    fn read_notification(&mut self, timeout: Duration) -> Result<Option<PacketId>, OpenRgbError> {
        if timeout.is_zero() {
            return Ok(None);
        }
        self.stream.set_read_timeout(Some(timeout))?;
        let mut header_bytes = [0u8; HEADER_LEN];
        let read = self.stream.read_exact(&mut header_bytes);
        self.stream.set_read_timeout(Some(IO_TIMEOUT))?;
        match read {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        }
        let header = Header::decode(&header_bytes)?;
        let mut payload = vec![0u8; header.payload_len as usize];
        self.stream.read_exact(&mut payload)?;
        trace!(?header, "received");
        if header.packet_id.is_unsolicited() {
            Ok(Some(header.packet_id))
        } else {
            Err(OpenRgbError::UnexpectedPacket {
                expected: PacketId::DeviceListUpdated,
                got: header.packet_id,
            })
        }
    }

    /// Reads packets until one of kind `expected` arrives, skipping anything
    /// the server sends on its own.
    fn receive(&mut self, expected: PacketId) -> Result<Vec<u8>, OpenRgbError> {
        loop {
            let mut header_bytes = [0u8; HEADER_LEN];
            self.stream.read_exact(&mut header_bytes)?;
            let header = Header::decode(&header_bytes)?;
            let mut payload = vec![0u8; header.payload_len as usize];
            self.stream.read_exact(&mut payload)?;
            trace!(?header, "received");
            if header.packet_id == expected {
                return Ok(payload);
            }
            if header.packet_id.is_unsolicited() {
                debug!(packet = ?header.packet_id, "ignoring unsolicited packet");
                continue;
            }
            return Err(OpenRgbError::UnexpectedPacket {
                expected,
                got: header.packet_id,
            });
        }
    }
}

impl ControllerCommands for OpenRgbClient {
    fn update_mode(
        &mut self,
        controller: u32,
        mode_index: u32,
        mode: &ModeDescription,
    ) -> Result<(), OpenRgbError> {
        let payload = encode_mode_update(mode_index, mode, self.protocol_version);
        self.send(controller, PacketId::UpdateMode, &payload)
    }

    fn save_mode(
        &mut self,
        controller: u32,
        mode_index: u32,
        mode: &ModeDescription,
    ) -> Result<(), OpenRgbError> {
        let payload = encode_mode_update(mode_index, mode, self.protocol_version);
        self.send(controller, PacketId::SaveMode, &payload)
    }

    fn update_leds(&mut self, controller: u32, colors: &[Rgb]) -> Result<(), OpenRgbError> {
        self.send(controller, PacketId::UpdateLeds, &encode_led_update(colors))
    }
}

fn resolve(host: &str, port: u16) -> Result<SocketAddr, OpenRgbError> {
    (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| OpenRgbError::Unresolvable(host.to_owned()))
}

fn read_u32(payload: &[u8], packet: PacketId) -> Result<u32, OpenRgbError> {
    let bytes: [u8; 4] = payload
        .get(0..4)
        .and_then(|b| b.try_into().ok())
        .ok_or(OpenRgbError::ShortPayload(packet))?;
    Ok(u32::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::thread;

    use super::*;
    use crate::description::tests::sample_controller;

    /// What the fake server saw from the client.
    #[derive(Debug, Default)]
    struct Observed {
        client_name: Vec<u8>,
        packets: Vec<(Header, Vec<u8>)>,
    }

    fn read_packet(stream: &mut TcpStream) -> (Header, Vec<u8>) {
        let mut header = [0u8; HEADER_LEN];
        stream.read_exact(&mut header).unwrap();
        let header = Header::decode(&header).unwrap();
        let mut payload = vec![0u8; header.payload_len as usize];
        stream.read_exact(&mut payload).unwrap();
        (header, payload)
    }

    fn reply(stream: &mut TcpStream, device_index: u32, id: PacketId, payload: &[u8]) {
        let header = Header {
            device_index,
            packet_id: id,
            payload_len: u32::try_from(payload.len()).unwrap(),
        };
        stream.write_all(&header.encode()).unwrap();
        stream.write_all(payload).unwrap();
    }

    /// A fake server speaking `server_version` that exposes `controllers` and
    /// records every packet after the handshake until the client disconnects.
    fn fake_server(
        server_version: u32,
        controllers: Vec<ControllerDescription>,
    ) -> (SocketAddr, thread::JoinHandle<Observed>) {
        let script = vec![
            (50, PacketId::DetectionStarted),
            (0, PacketId::DeviceListUpdated),
        ];
        fake_server_scripted(server_version, controllers, script)
    }

    /// Like [`fake_server`]; answers a rescan request by sending each packet
    /// of `rescan_script` after its delay in milliseconds.
    fn fake_server_scripted(
        server_version: u32,
        controllers: Vec<ControllerDescription>,
        rescan_script: Vec<(u64, PacketId)>,
    ) -> (SocketAddr, thread::JoinHandle<Observed>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let mut observed = Observed::default();
            let (mut stream, _) = listener.accept().unwrap();
            let serialize_version = server_version.min(MAX_PROTOCOL_VERSION);

            let (header, _) = read_packet(&mut stream);
            assert_eq!(header.packet_id, PacketId::RequestProtocolVersion);
            reply(
                &mut stream,
                0,
                PacketId::RequestProtocolVersion,
                &server_version.to_le_bytes(),
            );
            if server_version < MIN_PROTOCOL_VERSION {
                return observed;
            }

            let (header, name) = read_packet(&mut stream);
            assert_eq!(header.packet_id, PacketId::SetClientName);
            observed.client_name = name;

            loop {
                let mut header_bytes = [0u8; HEADER_LEN];
                if stream.read_exact(&mut header_bytes).is_err() {
                    return observed;
                }
                let header = Header::decode(&header_bytes).unwrap();
                let mut payload = vec![0u8; header.payload_len as usize];
                stream.read_exact(&mut payload).unwrap();
                match header.packet_id {
                    PacketId::RequestControllerCount => {
                        // Interleave an unsolicited notification to prove the client skips it.
                        reply(&mut stream, 0, PacketId::DeviceListUpdated, &[]);
                        let count = u32::try_from(controllers.len()).unwrap();
                        reply(
                            &mut stream,
                            0,
                            PacketId::RequestControllerCount,
                            &count.to_le_bytes(),
                        );
                    }
                    PacketId::RequestRescanDevices => {
                        for (delay_ms, id) in &rescan_script {
                            thread::sleep(Duration::from_millis(*delay_ms));
                            reply(&mut stream, 0, *id, &[]);
                        }
                    }
                    PacketId::RequestControllerData => {
                        let requested = u32::from_le_bytes(payload[0..4].try_into().unwrap());
                        assert_eq!(requested, serialize_version);
                        let body =
                            controllers[header.device_index as usize].encode(serialize_version);
                        let mut reply_payload = Vec::new();
                        let total = u32::try_from(body.len() + 4).unwrap();
                        reply_payload.extend_from_slice(&total.to_le_bytes());
                        reply_payload.extend_from_slice(&body);
                        reply(
                            &mut stream,
                            header.device_index,
                            PacketId::RequestControllerData,
                            &reply_payload,
                        );
                    }
                    _ => observed.packets.push((header, payload)),
                }
            }
        });
        (addr, handle)
    }

    fn connect(addr: SocketAddr) -> OpenRgbClient {
        OpenRgbClient::connect(&addr.ip().to_string(), addr.port(), "test").unwrap()
    }

    #[test]
    fn connects_negotiates_and_counts_controllers() {
        let (addr, server) = fake_server(MAX_PROTOCOL_VERSION + 2, vec![sample_controller(); 3]);
        let mut client = connect(addr);
        assert_eq!(client.protocol_version(), MAX_PROTOCOL_VERSION);
        assert_eq!(client.controller_count().unwrap(), 3);
        drop(client);
        assert_eq!(server.join().unwrap().client_name, b"test\0");
    }

    #[test]
    fn refuses_servers_older_than_required() {
        let (addr, server) = fake_server(MIN_PROTOCOL_VERSION - 1, Vec::new());
        let result = OpenRgbClient::connect(&addr.ip().to_string(), addr.port(), "test");
        assert!(matches!(result, Err(OpenRgbError::ProtocolTooOld { .. })));
        server.join().unwrap();
    }

    #[test]
    fn fetches_controller_descriptions() {
        let expected = sample_controller();
        let (addr, server) = fake_server(MAX_PROTOCOL_VERSION, vec![expected.clone()]);
        let mut client = connect(addr);
        assert_eq!(client.controllers().unwrap(), vec![expected]);
        drop(client);
        server.join().unwrap();
    }

    const TEST_QUIET: Duration = Duration::from_millis(200);

    #[test]
    fn rescan_outlasts_the_announcement_sent_when_the_list_is_cleared() {
        let script = vec![
            (0, PacketId::DeviceListUpdated),
            (100, PacketId::DeviceListUpdated),
            (100, PacketId::DeviceListUpdated),
        ];
        let (addr, server) =
            fake_server_scripted(MAX_PROTOCOL_VERSION, vec![sample_controller()], script);
        let mut client = connect(addr);
        let started = Instant::now();
        assert!(
            client
                .rescan_with_quiet_period(Duration::from_secs(5), TEST_QUIET)
                .unwrap()
        );
        // Last announcement at ~200 ms, then a full quiet period.
        assert!(started.elapsed() >= Duration::from_millis(200) + TEST_QUIET);
        assert_eq!(client.controller_count().unwrap(), 1);
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn rescan_returns_on_the_detection_complete_packet() {
        let script = vec![
            (0, PacketId::DeviceListUpdated),
            (50, PacketId::DetectionComplete),
        ];
        let (addr, server) =
            fake_server_scripted(MAX_PROTOCOL_VERSION, vec![sample_controller()], script);
        let mut client = connect(addr);
        let started = Instant::now();
        assert!(
            client
                .rescan_with_quiet_period(Duration::from_secs(5), Duration::from_secs(5))
                .unwrap()
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn rescan_reports_failure_while_the_device_list_stays_empty() {
        let (addr, server) = fake_server(MAX_PROTOCOL_VERSION, Vec::new());
        let mut client = connect(addr);
        assert!(
            !client
                .rescan_with_quiet_period(Duration::from_millis(600), TEST_QUIET)
                .unwrap()
        );
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn rescan_reports_failure_when_the_server_announces_nothing() {
        let (addr, server) = fake_server_scripted(MAX_PROTOCOL_VERSION, Vec::new(), Vec::new());
        let mut client = connect(addr);
        assert!(
            !client
                .rescan_with_quiet_period(Duration::from_millis(100), TEST_QUIET)
                .unwrap()
        );
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn wait_for_notification_times_out_cleanly_and_the_connection_stays_usable() {
        let (addr, server) = fake_server(MAX_PROTOCOL_VERSION, vec![sample_controller()]);
        let mut client = connect(addr);
        assert!(
            !client
                .wait_for_notification(PacketId::DeviceListUpdated, Duration::from_millis(50))
                .unwrap()
        );
        assert_eq!(client.controller_count().unwrap(), 1);
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn sends_mode_and_led_updates_with_the_negotiated_layout() {
        let (addr, server) = fake_server(MIN_PROTOCOL_VERSION, Vec::new());
        let mut client = connect(addr);
        let mode = sample_controller().modes[1].clone();
        client.update_mode(2, 1, &mode).unwrap();
        client.save_mode(2, 1, &mode).unwrap();
        client.update_leds(2, &[Rgb::WHITE]).unwrap();
        client.set_custom_mode(2).unwrap();
        drop(client);

        let observed = server.join().unwrap();
        let kinds: Vec<_> = observed
            .packets
            .iter()
            .map(|(h, _)| (h.device_index, h.packet_id))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (2, PacketId::UpdateMode),
                (2, PacketId::SaveMode),
                (2, PacketId::UpdateLeds),
                (2, PacketId::SetCustomMode)
            ]
        );
        assert_eq!(
            observed.packets[0].1,
            encode_mode_update(1, &mode, MIN_PROTOCOL_VERSION)
        );
        assert_eq!(observed.packets[2].1, encode_led_update(&[Rgb::WHITE]));
        assert!(observed.packets[3].1.is_empty());
    }
}
