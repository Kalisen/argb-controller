//! The Bluetooth LE link to one light, and the shared runtime behind it.
//!
//! The engine drives devices synchronously, so the backend owns a small Tokio
//! runtime and blocks on it per operation. One runtime and one adapter serve
//! every light; each light keeps its own connection, cipher and model.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use btleplug::api::{
    Central, CharPropFlags, Characteristic, Manager as _, Peripheral as _, ScanFilter,
    ValueNotification, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::{FutureExt, Stream, StreamExt};
use tracing::{debug, info, warn};

use crate::crypto::{FrameCipher, KEY_LEN, PRE_SHARED_KEY};
use crate::error::GoveeError;
use crate::model::{Model, is_govee_name};
use crate::protocol::{self, FRAME_LEN, HandshakeStep};

/// High byte of the manufacturer company id Govee advertises under; the low
/// byte holds the broadcast version in its low nibble and flags above it.
const GOVEE_COMPANY_ID_HIGH: u16 = 0x88;
/// Bit of the company id's low byte that announces the encrypted channel.
const ENCRYPTED_FLAG: u16 = 0x40;
/// How long a scan runs when a configured light is not in the adapter's
/// cache yet.
pub const SCAN_DURATION: Duration = Duration::from_secs(5);
/// How long the light may take to answer the session-key request.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(6);
/// How long the light may take to acknowledge the confirmation; the
/// reference implementation continues without it.
const HANDSHAKE_CONFIRM_TIMEOUT: Duration = Duration::from_secs(1);
/// Pause after a connection before the first frame, so the subscription is
/// in place and the light has settled.
const CONNECT_SETTLE: Duration = Duration::from_millis(100);

/// What the backend needs from a connection, so devices can be tested
/// against a fake.
pub trait Link {
    /// Connects (scanning first when needed) and opens the encrypted
    /// channel when the light advertises one. Idempotent while connected.
    ///
    /// # Errors
    /// [`GoveeError`] naming the step that failed.
    fn connect(&mut self) -> Result<(), GoveeError>;

    /// Writes one frame on the open connection.
    ///
    /// # Errors
    /// [`GoveeError`] when not connected or the write fails.
    fn send(&mut self, frame: &[u8; FRAME_LEN]) -> Result<(), GoveeError>;

    /// Closes the connection; the light keeps its state.
    fn disconnect(&mut self);

    /// Whether a connection is open.
    fn is_connected(&self) -> bool;

    /// The model learned from the advertised name, once connected or scanned.
    fn model(&self) -> Option<Model>;
}

/// A light seen advertising.
#[derive(Debug, Clone)]
pub struct Advertisement {
    /// Advertised local name.
    pub name: String,
    /// Whether the manufacturer data announces the encrypted channel.
    pub encrypted: bool,
    /// Signal strength, when reported.
    pub rssi: Option<i16>,
}

/// The Tokio runtime and Bluetooth adapter every link shares.
pub struct BleRuntime {
    runtime: tokio::runtime::Runtime,
    adapter: Adapter,
    /// When the last scan finished, so links do not scan back to back.
    last_scan: Mutex<Option<Instant>>,
}

/// A runtime created on first use and shared by every link, so a missing
/// adapter is reported per device instead of failing the whole backend.
pub type SharedRuntime = Arc<Mutex<Option<Arc<BleRuntime>>>>;

impl BleRuntime {
    /// Starts the runtime and opens the first adapter.
    ///
    /// # Errors
    /// [`GoveeError::NoAdapter`] when the runtime cannot start or no adapter
    /// is usable from this process.
    pub fn new() -> Result<Self, GoveeError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(|e| GoveeError::NoAdapter(e.to_string()))?;
        let adapter = runtime.block_on(async {
            let manager = Manager::new()
                .await
                .map_err(|e| GoveeError::NoAdapter(e.to_string()))?;
            manager
                .adapters()
                .await
                .map_err(|e| GoveeError::NoAdapter(e.to_string()))?
                .into_iter()
                .next()
                .ok_or_else(|| GoveeError::NoAdapter("no adapter in this session".into()))
        })?;
        Ok(Self {
            runtime,
            adapter,
            last_scan: Mutex::new(None),
        })
    }

    /// The runtime from a shared slot, created on first use.
    ///
    /// # Errors
    /// [`GoveeError::NoAdapter`] as [`Self::new`]; the slot stays empty so
    /// the next call tries again.
    pub fn shared(slot: &SharedRuntime) -> Result<Arc<BleRuntime>, GoveeError> {
        let mut guard = slot
            .lock()
            .map_err(|_| GoveeError::NoAdapter("runtime lock poisoned".into()))?;
        if let Some(runtime) = guard.as_ref() {
            return Ok(Arc::clone(runtime));
        }
        let runtime = Arc::new(Self::new()?);
        *guard = Some(Arc::clone(&runtime));
        Ok(runtime)
    }

    /// Scans for `duration` and returns every peripheral the adapter knows
    /// afterwards, with what its advertisement says.
    ///
    /// # Errors
    /// [`GoveeError::Bluetooth`] when the adapter refuses to scan.
    pub fn scan(&self, duration: Duration) -> Result<Vec<(Peripheral, Advertisement)>, GoveeError> {
        self.runtime.block_on(async {
            self.adapter
                .start_scan(ScanFilter::default())
                .await
                .map_err(|e| GoveeError::at("starting the scan", e))?;
            tokio::time::sleep(duration).await;
            let result = self.known().await;
            if let Err(error) = self.adapter.stop_scan().await {
                warn!(%error, "stopping the scan");
            }
            if let Ok(mut last) = self.last_scan.lock() {
                *last = Some(Instant::now());
            }
            result
        })
    }

    /// Every peripheral the adapter has heard, without scanning.
    async fn known(&self) -> Result<Vec<(Peripheral, Advertisement)>, GoveeError> {
        let peripherals = self
            .adapter
            .peripherals()
            .await
            .map_err(|e| GoveeError::at("listing peripherals", e))?;
        let mut seen = Vec::with_capacity(peripherals.len());
        for peripheral in peripherals {
            let Ok(Some(properties)) = peripheral.properties().await else {
                continue;
            };
            let Some(name) = properties.local_name else {
                continue;
            };
            let encrypted = properties
                .manufacturer_data
                .keys()
                .any(|id| advertises_encryption(*id));
            seen.push((
                peripheral,
                Advertisement {
                    name,
                    encrypted,
                    rssi: properties.rssi,
                },
            ));
        }
        Ok(seen)
    }

    /// The first Govee light whose name contains `match_name`
    /// (case-insensitive), from the adapter's cache or after a scan.
    fn find(&self, match_name: &str) -> Result<(Peripheral, Advertisement), GoveeError> {
        let wanted = match_name.to_ascii_lowercase();
        let matches = |(_, advert): &(Peripheral, Advertisement)| {
            is_govee_name(&advert.name) && advert.name.to_ascii_lowercase().contains(&wanted)
        };
        let cached = self.runtime.block_on(self.known())?;
        if let Some(found) = cached.into_iter().find(matches) {
            return Ok(found);
        }
        self.scan(SCAN_DURATION)?
            .into_iter()
            .find(matches)
            .ok_or_else(|| GoveeError::NotFound {
                match_name: match_name.to_owned(),
                scan_secs: SCAN_DURATION.as_secs(),
            })
    }
}

/// Whether a manufacturer company id is Govee's with the encrypt flag set.
fn advertises_encryption(company_id: u16) -> bool {
    company_id >> 8 == GOVEE_COMPANY_ID_HIGH && company_id & ENCRYPTED_FLAG != 0
}

/// An open connection's pieces.
struct Session {
    peripheral: Peripheral,
    write: Characteristic,
    write_type: WriteType,
    notifications: Pin<Box<dyn Stream<Item = ValueNotification> + Send>>,
    cipher: Option<FrameCipher>,
    /// Wire bytes of the handshake, so late retransmits are ignored.
    echoes: Vec<Vec<u8>>,
}

/// The link to one light over the shared runtime.
pub struct BleLink {
    runtime: SharedRuntime,
    match_name: String,
    model: Option<Model>,
    name: Option<String>,
    session: Option<Session>,
}

impl BleLink {
    /// A link to the light whose advertised name contains `match_name`;
    /// nothing happens until [`Link::connect`].
    #[must_use]
    pub fn new(runtime: SharedRuntime, match_name: &str, model: Option<Model>) -> Self {
        Self {
            runtime,
            match_name: match_name.to_owned(),
            model,
            name: None,
            session: None,
        }
    }

    /// The advertised name, once the light has been found.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    fn open(&mut self, runtime: &BleRuntime) -> Result<Session, GoveeError> {
        let (peripheral, advert) = runtime.find(&self.match_name)?;
        self.name = Some(advert.name.clone());
        if self.model.is_none() {
            self.model = Model::from_text(&advert.name);
        }
        info!(
            name = advert.name,
            encrypted = advert.encrypted,
            "connecting to Govee light"
        );
        runtime.runtime.block_on(async {
            peripheral
                .connect()
                .await
                .map_err(|e| GoveeError::at("connecting", e))?;
            let result = Self::open_session(peripheral.clone(), advert.encrypted).await;
            if result.is_err() {
                let _ = peripheral.disconnect().await;
            }
            result
        })
    }

    async fn open_session(peripheral: Peripheral, encrypted: bool) -> Result<Session, GoveeError> {
        peripheral
            .discover_services()
            .await
            .map_err(|e| GoveeError::at("discovering services", e))?;
        let mut write = None;
        let mut notify = None;
        for characteristic in peripheral.characteristics() {
            if characteristic.uuid == protocol::WRITE_CHARACTERISTIC {
                write = Some(characteristic);
            } else if characteristic.uuid == protocol::NOTIFY_CHARACTERISTIC {
                notify = Some(characteristic);
            }
        }
        let write = write.ok_or_else(|| {
            GoveeError::at("discovering services", "no Govee write characteristic")
        })?;
        let write_type = if write
            .properties
            .contains(CharPropFlags::WRITE_WITHOUT_RESPONSE)
        {
            WriteType::WithoutResponse
        } else {
            WriteType::WithResponse
        };
        if let Some(notify) = &notify {
            peripheral
                .subscribe(notify)
                .await
                .map_err(|e| GoveeError::at("subscribing", e))?;
        }
        let notifications = peripheral
            .notifications()
            .await
            .map_err(|e| GoveeError::at("subscribing", e))?;
        tokio::time::sleep(CONNECT_SETTLE).await;
        let mut session = Session {
            peripheral,
            write,
            write_type,
            notifications,
            cipher: None,
            echoes: Vec::new(),
        };
        if encrypted {
            session.handshake().await?;
        }
        Ok(session)
    }
}

impl Session {
    async fn write_raw(&mut self, wire: &[u8]) -> Result<(), GoveeError> {
        self.peripheral
            .write(&self.write, wire, self.write_type)
            .await
            .map_err(|e| GoveeError::at("writing", e))
    }

    /// The next notification before `deadline` that is not a handshake echo.
    async fn next_raw(&mut self, deadline: tokio::time::Instant) -> Option<Vec<u8>> {
        loop {
            let notification = tokio::time::timeout_at(deadline, self.notifications.next())
                .await
                .ok()??;
            if self.echoes.contains(&notification.value) {
                continue;
            }
            return Some(notification.value);
        }
    }

    /// Drops the notifications already received, so the stream's buffer
    /// does not grow while nobody reads replies.
    fn drain(&mut self) {
        while self.notifications.next().now_or_never().flatten().is_some() {}
    }

    async fn handshake(&mut self) -> Result<(), GoveeError> {
        let psk = FrameCipher::new(&PRE_SHARED_KEY);
        let request = psk.encrypt(&protocol::handshake(
            HandshakeStep::SessionKey,
            &random_fill(),
        ));
        self.echoes.push(request.clone());
        self.write_raw(&request).await?;
        let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
        let key = loop {
            let raw = self
                .next_raw(deadline)
                .await
                .ok_or_else(|| GoveeError::Handshake("no session-key reply".into()))?;
            self.echoes.push(raw.clone());
            let plain = psk.decrypt(&raw);
            if protocol::is_handshake_reply(&plain, HandshakeStep::SessionKey) {
                let mut key = [0u8; KEY_LEN];
                key.copy_from_slice(&plain[protocol::SESSION_KEY_RANGE]);
                break key;
            }
            debug!("skipping a non-handshake frame during the handshake");
        };
        let confirm = psk.encrypt(&protocol::handshake(HandshakeStep::Confirm, &random_fill()));
        self.echoes.push(confirm.clone());
        self.write_raw(&confirm).await?;
        let deadline = tokio::time::Instant::now() + HANDSHAKE_CONFIRM_TIMEOUT;
        while let Some(raw) = self.next_raw(deadline).await {
            self.echoes.push(raw.clone());
            if protocol::is_handshake_reply(&psk.decrypt(&raw), HandshakeStep::Confirm) {
                break;
            }
        }
        self.cipher = Some(FrameCipher::new(&key));
        debug!("encrypted channel open");
        Ok(())
    }
}

/// Fill bytes for the handshake frames; the light ignores their value.
fn random_fill() -> [u8; FRAME_LEN - 3] {
    let mut hasher = RandomState::new().build_hasher();
    let mut fill = [0u8; FRAME_LEN - 3];
    for chunk in fill.chunks_mut(8) {
        hasher.write_u8(0);
        let word = hasher.finish().to_le_bytes();
        chunk.copy_from_slice(&word[..chunk.len()]);
    }
    fill
}

impl Link for BleLink {
    fn connect(&mut self) -> Result<(), GoveeError> {
        if self.session.is_some() {
            return Ok(());
        }
        let runtime = BleRuntime::shared(&self.runtime)?;
        let session = self.open(&runtime)?;
        self.session = Some(session);
        Ok(())
    }

    fn send(&mut self, frame: &[u8; FRAME_LEN]) -> Result<(), GoveeError> {
        let runtime = BleRuntime::shared(&self.runtime)?;
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| GoveeError::at("writing", "not connected"))?;
        let wire = session
            .cipher
            .as_ref()
            .map_or_else(|| frame.to_vec(), |cipher| cipher.encrypt(frame));
        let result = runtime.runtime.block_on(session.write_raw(&wire));
        session.drain();
        if result.is_err() {
            self.disconnect();
        }
        result
    }

    fn disconnect(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        if let Ok(runtime) = BleRuntime::shared(&self.runtime) {
            runtime.runtime.block_on(async {
                if let Err(error) = session.peripheral.disconnect().await {
                    debug!(%error, "disconnecting");
                }
            });
        }
    }

    fn is_connected(&self) -> bool {
        self.session.is_some()
    }

    fn model(&self) -> Option<Model> {
        self.model
    }
}

impl Drop for BleLink {
    fn drop(&mut self) {
        self.disconnect();
    }
}
