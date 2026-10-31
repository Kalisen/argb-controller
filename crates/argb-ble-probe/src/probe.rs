//! The probe itself: scan, connect, exchange a few frames, report.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::pin::Pin;
use std::time::Duration;

use anyhow::Context;
use btleplug::api::{
    Central, CharPropFlags, Characteristic, Manager as _, Peripheral as _, ScanFilter,
    ValueNotification, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::{Stream, StreamExt};
use tracing::{info, warn};

use crate::crypto::{FrameCipher, KEY_LEN, PRE_SHARED_KEY};
use crate::govee::{self, HandshakeStep, Layout};

/// Prefix of the advertised name of every Govee light seen so far.
const GOVEE_NAME_PREFIX: &str = "Govee_";
/// Older Govee firmware advertises under this prefix.
const IHOMENT_NAME_PREFIX: &str = "ihoment_";
/// High byte of the manufacturer company id Govee advertises under; the low
/// byte holds the broadcast version in its low nibble and flags above it.
const GOVEE_COMPANY_ID_HIGH: u16 = 0x88;
/// Bit of the company id's low byte that announces the encrypted channel.
const ENCRYPTED_FLAG: u16 = 0x40;
/// Pause between consecutive frames, so the light processes each one.
const INTER_FRAME_PAUSE: Duration = Duration::from_millis(100);
/// How long replies are collected after the last frame.
const REPLY_WINDOW: Duration = Duration::from_secs(2);
/// How long the light may take to answer the session-key request.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(6);
/// How long the light may take to acknowledge the confirmation; the
/// reference implementation continues without it.
const HANDSHAKE_CONFIRM_TIMEOUT: Duration = Duration::from_secs(1);

/// What the probe does once connected, beyond listing services and sending
/// the keep-alive.
#[derive(Debug, Clone, Copy, Default)]
pub struct Actions {
    /// Color to show, if any.
    pub color: Option<(u8, u8, u8)>,
    /// Frame layout of the target model.
    pub layout: Option<Layout>,
    /// Whole-device brightness in percent, if any.
    pub brightness: Option<u8>,
    /// Power the light off at the end.
    pub power_off: bool,
}

/// A peripheral seen during the scan, with what its advertisement says.
#[derive(Clone)]
struct Seen {
    peripheral: Peripheral,
    name: String,
    encrypted: bool,
}

/// Runs the whole probe: scan for `scan_for`, report what was seen, then
/// connect to the first light whose name contains `target` (if given) and
/// carry out `actions`.
///
/// # Errors
/// Any Bluetooth failure; the log holds the step reached.
pub async fn run(scan_for: Duration, target: Option<&str>, actions: Actions) -> anyhow::Result<()> {
    let adapter = first_adapter().await?;
    let seen = scan(&adapter, scan_for).await?;
    let Some(target) = target else {
        info!("no target given; scan only");
        return Ok(());
    };
    let wanted = target.to_ascii_lowercase();
    let Some(found) = seen
        .into_iter()
        .find(|s| s.name.to_ascii_lowercase().contains(&wanted))
    else {
        anyhow::bail!("no peripheral whose name contains {target:?}");
    };
    exercise(&found, actions).await
}

async fn first_adapter() -> anyhow::Result<Adapter> {
    let manager = Manager::new().await.context("Bluetooth manager")?;
    let adapters = manager.adapters().await.context("listing adapters")?;
    info!(count = adapters.len(), "Bluetooth adapters");
    let adapter = adapters
        .into_iter()
        .next()
        .context("no Bluetooth adapter available in this session")?;
    let description = adapter.adapter_info().await.unwrap_or_default();
    info!(adapter = %description, "using adapter");
    Ok(adapter)
}

async fn scan(adapter: &Adapter, scan_for: Duration) -> anyhow::Result<Vec<Seen>> {
    adapter
        .start_scan(ScanFilter::default())
        .await
        .context("starting the scan")?;
    info!(seconds = scan_for.as_secs(), "scanning");
    tokio::time::sleep(scan_for).await;
    let peripherals = adapter.peripherals().await.context("listing peripherals")?;
    if let Err(error) = adapter.stop_scan().await {
        warn!(%error, "stopping the scan");
    }
    info!(count = peripherals.len(), "peripherals seen");
    let mut seen = Vec::with_capacity(peripherals.len());
    for peripheral in peripherals {
        let properties = peripheral.properties().await?;
        let name = properties
            .as_ref()
            .and_then(|p| p.local_name.clone())
            .unwrap_or_default();
        let rssi = properties.as_ref().and_then(|p| p.rssi);
        let manufacturer: Vec<String> = properties
            .as_ref()
            .map(|p| {
                p.manufacturer_data
                    .iter()
                    .map(|(company, data)| format!("{company:04x}: {}", govee::hex(data)))
                    .collect()
            })
            .unwrap_or_default();
        let govee = name.starts_with(GOVEE_NAME_PREFIX) || name.starts_with(IHOMENT_NAME_PREFIX);
        let encrypted = properties.as_ref().is_some_and(|p| {
            p.manufacturer_data
                .keys()
                .any(|id| advertises_encryption(*id))
        });
        info!(
            address = %peripheral.address(),
            name,
            rssi,
            govee,
            encrypted,
            manufacturer = ?manufacturer,
            "peripheral"
        );
        seen.push(Seen {
            peripheral,
            name,
            encrypted,
        });
    }
    Ok(seen)
}

/// Whether a manufacturer company id is Govee's with the encrypt flag set.
fn advertises_encryption(company_id: u16) -> bool {
    company_id >> 8 == GOVEE_COMPANY_ID_HIGH && company_id & ENCRYPTED_FLAG != 0
}

async fn exercise(found: &Seen, actions: Actions) -> anyhow::Result<()> {
    let peripheral = &found.peripheral;
    let address = peripheral.address();
    info!(%address, name = found.name, encrypted = found.encrypted, "connecting");
    peripheral.connect().await.context("connecting")?;
    let result = exchange(found, actions).await;
    if let Err(error) = peripheral.disconnect().await {
        warn!(%error, "disconnecting");
    } else {
        info!(%address, "disconnected");
    }
    result
}

/// The two characteristics the exchange needs.
struct Endpoints {
    write: Characteristic,
    notify: Option<Characteristic>,
}

fn endpoints(peripheral: &Peripheral) -> anyhow::Result<Endpoints> {
    let mut write = None;
    let mut notify = None;
    for service in peripheral.services() {
        info!(uuid = %service.uuid, primary = service.primary, "service");
        for characteristic in service.characteristics {
            info!(
                uuid = %characteristic.uuid,
                properties = ?characteristic.properties,
                "  characteristic"
            );
            if characteristic.uuid == govee::WRITE_CHARACTERISTIC {
                write = Some(characteristic);
            } else if characteristic.uuid == govee::NOTIFY_CHARACTERISTIC {
                notify = Some(characteristic);
            }
        }
    }
    let write = write.with_context(|| {
        format!(
            "write characteristic {} (service {}) not found",
            govee::WRITE_CHARACTERISTIC,
            govee::CONTROL_SERVICE
        )
    })?;
    Ok(Endpoints { write, notify })
}

/// The command channel of one connected light: frames go out through the
/// write characteristic and come back as notifications, both through the
/// session cipher once the handshake has produced one.
struct Channel<'a> {
    peripheral: &'a Peripheral,
    write: Characteristic,
    write_type: WriteType,
    notifications: Pin<Box<dyn Stream<Item = ValueNotification> + Send>>,
    cipher: Option<FrameCipher>,
    /// Wire bytes of the handshake, so late retransmits are not mistaken
    /// for replies.
    echoes: Vec<Vec<u8>>,
}

impl Channel<'_> {
    /// Writes `plain`, encrypted when the channel has a cipher.
    async fn send(&mut self, label: &str, plain: &[u8]) -> anyhow::Result<()> {
        let wire = self
            .cipher
            .as_ref()
            .map_or_else(|| plain.to_vec(), |cipher| cipher.encrypt(plain));
        info!(label, frame = %govee::hex(plain), "writing");
        self.peripheral
            .write(&self.write, &wire, self.write_type)
            .await
            .with_context(|| format!("writing the {label} frame"))
    }

    /// Writes `wire` as is (handshake frames are already encrypted).
    async fn send_raw(&mut self, label: &str, wire: &[u8]) -> anyhow::Result<()> {
        info!(label, "writing");
        self.echoes.push(wire.to_vec());
        self.peripheral
            .write(&self.write, wire, self.write_type)
            .await
            .with_context(|| format!("writing the {label} frame"))
    }

    /// The next notification's raw bytes before `deadline`, skipping
    /// handshake echoes.
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

    /// The next reply, decrypted when the channel has a cipher.
    async fn next_plain(&mut self, deadline: tokio::time::Instant) -> Option<Vec<u8>> {
        let raw = self.next_raw(deadline).await?;
        Some(
            self.cipher
                .as_ref()
                .map_or(raw.clone(), |cipher| cipher.decrypt(&raw)),
        )
    }

    /// Opens the encrypted channel: asks for a session key under the
    /// pre-shared key, confirms, then switches the channel to that key.
    async fn handshake(&mut self) -> anyhow::Result<()> {
        let psk = FrameCipher::new(&PRE_SHARED_KEY);
        let request = psk.encrypt(&govee::handshake(HandshakeStep::SessionKey, &random_fill()));
        self.send_raw("handshake: session key request", &request)
            .await?;
        let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
        let key = loop {
            let raw = self
                .next_raw(deadline)
                .await
                .context("no session-key reply within the handshake timeout")?;
            self.echoes.push(raw.clone());
            let plain = psk.decrypt(&raw);
            if govee::is_handshake_reply(&plain, HandshakeStep::SessionKey) {
                let mut key = [0u8; KEY_LEN];
                key.copy_from_slice(&plain[govee::SESSION_KEY_RANGE]);
                break key;
            }
            info!(frame = %govee::hex(&plain), "skipping a non-handshake frame during the handshake");
        };
        info!("session key received");

        let confirm = psk.encrypt(&govee::handshake(HandshakeStep::Confirm, &random_fill()));
        self.send_raw("handshake: confirm", &confirm).await?;
        let deadline = tokio::time::Instant::now() + HANDSHAKE_CONFIRM_TIMEOUT;
        let mut acknowledged = false;
        while let Some(raw) = self.next_raw(deadline).await {
            self.echoes.push(raw.clone());
            if govee::is_handshake_reply(&psk.decrypt(&raw), HandshakeStep::Confirm) {
                acknowledged = true;
                break;
            }
        }
        info!(acknowledged, "handshake complete");
        self.cipher = Some(FrameCipher::new(&key));
        Ok(())
    }
}

/// Fill bytes for the handshake frames; the light ignores their value.
fn random_fill() -> [u8; govee::FRAME_LEN - 3] {
    let mut hasher = RandomState::new().build_hasher();
    let mut fill = [0u8; govee::FRAME_LEN - 3];
    for chunk in fill.chunks_mut(8) {
        hasher.write_u8(0);
        let word = hasher.finish().to_le_bytes();
        chunk.copy_from_slice(&word[..chunk.len()]);
    }
    fill
}

async fn exchange(found: &Seen, actions: Actions) -> anyhow::Result<()> {
    let peripheral = &found.peripheral;
    peripheral
        .discover_services()
        .await
        .context("discovering services")?;
    let Endpoints { write, notify } = endpoints(peripheral)?;
    let write_type = if write
        .properties
        .contains(CharPropFlags::WRITE_WITHOUT_RESPONSE)
    {
        WriteType::WithoutResponse
    } else {
        WriteType::WithResponse
    };
    info!(?write_type, "write characteristic found");

    match &notify {
        Some(characteristic) if characteristic.properties.contains(CharPropFlags::NOTIFY) => {
            peripheral
                .subscribe(characteristic)
                .await
                .context("subscribing to notifications")?;
            info!("subscribed to the notify characteristic");
        }
        _ => warn!("no notify characteristic; replies will not be seen"),
    }
    let notifications = peripheral.notifications().await?;
    let mut channel = Channel {
        peripheral,
        write,
        write_type,
        notifications,
        cipher: None,
        echoes: Vec::new(),
    };

    if found.encrypted {
        channel.handshake().await?;
    }

    let mut frames: Vec<(&str, [u8; govee::FRAME_LEN])> = vec![("keep-alive", govee::keep_alive())];
    if let Some((red, green, blue)) = actions.color {
        let layout = actions
            .layout
            .context("a color needs --layout to pick the frame format")?;
        frames.push(("power on", govee::power(true)));
        frames.push(("color", govee::color(layout, red, green, blue)));
    }
    if let Some(percent) = actions.brightness {
        frames.push(("brightness", govee::brightness(percent)));
    }
    if actions.power_off {
        frames.push(("power off", govee::power(false)));
    }

    for (label, frame) in frames {
        channel.send(label, &frame).await?;
        tokio::time::sleep(INTER_FRAME_PAUSE).await;
    }

    let deadline = tokio::time::Instant::now() + REPLY_WINDOW;
    while let Some(plain) = channel.next_plain(deadline).await {
        info!(value = %govee::hex(&plain), "reply");
    }
    Ok(())
}
