//! Blocking TCP client for the OpenRGB SDK server.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use thiserror::Error;
use tracing::{debug, info};

use crate::protocol::{
    HEADER_LEN, Header, MAX_PROTOCOL_VERSION, MIN_PROTOCOL_VERSION, PacketId, ProtocolError,
    SERVER_DEVICE_INDEX,
};

/// Time allowed for the TCP connection and for each reply.
pub const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Errors talking to the server.
#[derive(Debug, Error)]
pub enum OpenRgbError {
    /// Socket failure.
    #[error("OpenRGB connection error: {0}")]
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
        debug!(?header, "sending");
        self.stream.write_all(&header.encode())?;
        self.stream.write_all(payload)?;
        self.stream.flush()?;
        Ok(())
    }

    /// Reads packets until one of kind `expected` arrives, skipping unsolicited
    /// device-list notifications.
    fn receive(&mut self, expected: PacketId) -> Result<Vec<u8>, OpenRgbError> {
        loop {
            let mut header_bytes = [0u8; HEADER_LEN];
            self.stream.read_exact(&mut header_bytes)?;
            let header = Header::decode(&header_bytes)?;
            let mut payload = vec![0u8; header.payload_len as usize];
            self.stream.read_exact(&mut payload)?;
            debug!(?header, "received");
            if header.packet_id == expected {
                return Ok(payload);
            }
            if header.packet_id == PacketId::DeviceListUpdated {
                continue;
            }
            return Err(OpenRgbError::UnexpectedPacket {
                expected,
                got: header.packet_id,
            });
        }
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

    fn read_packet(stream: &mut TcpStream) -> (Header, Vec<u8>) {
        let mut header = [0u8; HEADER_LEN];
        stream.read_exact(&mut header).unwrap();
        let header = Header::decode(&header).unwrap();
        let mut payload = vec![0u8; header.payload_len as usize];
        stream.read_exact(&mut payload).unwrap();
        (header, payload)
    }

    fn reply(stream: &mut TcpStream, id: PacketId, payload: &[u8]) {
        let header = Header {
            device_index: 0,
            packet_id: id,
            payload_len: u32::try_from(payload.len()).unwrap(),
        };
        stream.write_all(&header.encode()).unwrap();
        stream.write_all(payload).unwrap();
    }

    /// A minimal fake server: answers version negotiation, records the client
    /// name, then reports `count` controllers after an unsolicited notification.
    fn fake_server(server_version: u32, count: u32) -> (SocketAddr, thread::JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();

            let (header, _) = read_packet(&mut stream);
            assert_eq!(header.packet_id, PacketId::RequestProtocolVersion);
            reply(
                &mut stream,
                PacketId::RequestProtocolVersion,
                &server_version.to_le_bytes(),
            );

            if server_version < MIN_PROTOCOL_VERSION {
                return Vec::new();
            }

            let (header, name) = read_packet(&mut stream);
            assert_eq!(header.packet_id, PacketId::SetClientName);

            let (header, _) = read_packet(&mut stream);
            assert_eq!(header.packet_id, PacketId::RequestControllerCount);
            reply(&mut stream, PacketId::DeviceListUpdated, &[]);
            reply(
                &mut stream,
                PacketId::RequestControllerCount,
                &count.to_le_bytes(),
            );
            name
        });
        (addr, handle)
    }

    #[test]
    fn connects_negotiates_and_counts_controllers() {
        let (addr, server) = fake_server(MAX_PROTOCOL_VERSION + 1, 3);
        let mut client =
            OpenRgbClient::connect(&addr.ip().to_string(), addr.port(), "test").unwrap();
        assert_eq!(client.protocol_version(), MAX_PROTOCOL_VERSION);
        assert_eq!(client.controller_count().unwrap(), 3);
        assert_eq!(server.join().unwrap(), b"test\0");
    }

    #[test]
    fn refuses_servers_older_than_required() {
        let (addr, server) = fake_server(MIN_PROTOCOL_VERSION - 1, 0);
        let result = OpenRgbClient::connect(&addr.ip().to_string(), addr.port(), "test");
        assert!(matches!(result, Err(OpenRgbError::ProtocolTooOld { .. })));
        server.join().unwrap();
    }
}
