//! The service body: keeps an engine connected to OpenRGB and the Govee
//! lights, follows the configuration file, and applies the requested power
//! state.

use std::time::{Duration, Instant, SystemTime};

use anyhow::Context;
use argb_backend_govee::{SharedRuntime as BleRuntimeSlot, configured_devices};
use argb_backend_openrgb::client::RESCAN_QUIET_PERIOD;
use argb_backend_openrgb::{OpenRgbClient, SharedClient, open_session};
use argb_config::{ConfigFile, ConfigStore};
use argb_domain::{Backend, DeviceAlias, LightingProfile, PowerState};
use argb_engine::{Attendance, DEFAULT_TICK_RATE_HZ, DeviceError, Engine, LightingDevice, Persist};
use tracing::{error, info, warn};

/// Name the service registers with the OpenRGB server.
pub const CLIENT_NAME: &str = "argb-controller";
/// Delay between attempts to reach the OpenRGB server.
pub const RECONNECT_INTERVAL: Duration = Duration::from_secs(5);
/// How often the configuration file's timestamp is checked.
pub const CONFIG_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// Time between software pulse frames.
pub const TICK_INTERVAL: Duration = Duration::from_millis(1000 / DEFAULT_TICK_RATE_HZ as u64);
/// Longest wait between runtime checks when nothing is pulsing.
pub const IDLE_INTERVAL: Duration = Duration::from_millis(500);
/// Pause after the host wakes before devices are driven again, so that
/// OpenRGB and the buses underneath it have re-initialized.
pub const RESUME_SETTLE: Duration = Duration::from_secs(2);
/// Delay after a resume before OpenRGB is asked to detect its hardware
/// again. A re-detection three seconds after waking left every device
/// unresponsive; the buses need longer than the first write does.
pub const RESUME_RESCAN_DELAY: Duration = Duration::from_secs(15);
/// Longest wait for OpenRGB to finish detecting hardware again after a resume.
pub const RESUME_RESCAN_TIMEOUT: Duration = Duration::from_secs(30);
/// How long OpenRGB's device-list announcements must stop before the engine
/// is rebuilt on the new list. OpenRGB announces once per controller while
/// detecting, so rebuilding on the first announcement would catch the list
/// half full; the same quiet period closes a rescan.
pub const DEVICE_LIST_SETTLE: Duration = RESCAN_QUIET_PERIOD;
/// Delay before a Govee light that failed its last apply (off, out of
/// range, Bluetooth busy) is tried again. OpenRGB failures reconnect
/// instead, since they mean the whole server connection is gone.
pub const GOVEE_RETRY_INTERVAL: Duration = Duration::from_secs(10);

/// The engine drives OpenRGB controllers and Govee lights alike.
type ServiceEngine = Engine<Box<dyn LightingDevice>>;

/// Runtime state of the service between commands.
pub struct Runtime {
    store: ConfigStore,
    config: ConfigFile,
    config_modified: Option<SystemTime>,
    last_config_poll: Instant,
    engine: Option<ServiceEngine>,
    /// The engine's OpenRGB connection, read for server notifications
    /// between writes.
    client: Option<SharedClient<OpenRgbClient>>,
    /// The Bluetooth runtime the Govee devices share, created on first use.
    ble: BleRuntimeSlot,
    /// When the devices that failed their last apply are retried, if any did.
    retry_due: Option<Instant>,
    device_list: DeviceListWatch,
    last_connect_attempt: Option<Instant>,
    desired_state: PowerState,
    /// When the post-resume re-detection is due, if one is pending.
    rescan_due: Option<Instant>,
    clock: Instant,
}

/// Debounces OpenRGB's device-list announcements: the engine is rebuilt once
/// they have stopped for a quiet period, not on each one.
#[derive(Debug, Default)]
struct DeviceListWatch {
    /// When the latest announcement arrived, if a rebuild is pending.
    changed_at: Option<Instant>,
}

impl DeviceListWatch {
    /// Records an announcement at `now`. Returns whether it opened a burst
    /// (so the caller logs the first one only).
    fn announce(&mut self, now: Instant) -> bool {
        let first = self.changed_at.is_none();
        self.changed_at = Some(now);
        first
    }

    /// Whether announcements have stopped for `quiet` as of `now`; clears
    /// the pending rebuild when they have.
    fn take_settled(&mut self, now: Instant, quiet: Duration) -> bool {
        let settled = self
            .changed_at
            .is_some_and(|at| now.duration_since(at) >= quiet);
        if settled {
            self.changed_at = None;
        }
        settled
    }
}

impl Runtime {
    /// Loads the configuration (falling back to the starter document when the
    /// file is missing or invalid) and prepares to connect.
    #[must_use]
    pub fn new(store: ConfigStore, initial_state: PowerState) -> Self {
        let config_modified = modified_time(&store);
        let config = load_or_starter(&store);
        Self {
            store,
            config,
            config_modified,
            last_config_poll: Instant::now(),
            engine: None,
            client: None,
            ble: BleRuntimeSlot::default(),
            retry_due: None,
            device_list: DeviceListWatch::default(),
            last_connect_attempt: None,
            desired_state: initial_state,
            rescan_due: None,
            clock: Instant::now(),
        }
    }

    /// Moves every device to `state`. Entering the active state from sleep
    /// waits [`RESUME_SETTLE`] first and schedules the re-detection of
    /// [`RESUME_RESCAN_DELAY`].
    pub fn enter_state(&mut self, state: PowerState) {
        info!(%state, "entering state");
        let resuming = state == PowerState::Active && self.desired_state == PowerState::Sleep;
        self.desired_state = state;
        if resuming {
            std::thread::sleep(RESUME_SETTLE);
        }
        self.apply();
        // Windows reports a resume twice (automatic, then user-triggered);
        // the second report must not cancel the re-detection the first
        // scheduled, so only a change of state touches it.
        if resuming {
            self.rescan_due = Some(Instant::now() + RESUME_RESCAN_DELAY);
        } else if state == PowerState::Sleep {
            self.rescan_due = None;
        }
    }

    /// Whether a post-resume re-detection is scheduled.
    #[cfg(test)]
    fn rescan_pending(&self) -> bool {
        self.rescan_due.is_some()
    }

    /// Makes OpenRGB detect its hardware again, then reconnects so the
    /// engine is rebuilt on the fresh controllers and the state applied.
    ///
    /// OpenRGB keeps the device handles it opened before the host slept.
    /// The Gigabyte board's USB controller no longer answers on them after a
    /// resume: writes report success and change nothing, and the board shows
    /// its firmware rainbow until a re-detection.
    fn rescan_after_resume(&mut self) {
        self.disconnect();
        self.last_connect_attempt = None;
        let settings = &self.config.openrgb;
        let mut client = match OpenRgbClient::connect(&settings.host, settings.port, CLIENT_NAME) {
            Ok(client) => client,
            Err(error) => {
                warn!(%error, retry_in = ?RECONNECT_INTERVAL, "OpenRGB unavailable after resume");
                return;
            }
        };
        match client.rescan(RESUME_RESCAN_TIMEOUT) {
            Ok(true) => info!("OpenRGB detected its hardware again"),
            Ok(false) => warn!(
                timeout = ?RESUME_RESCAN_TIMEOUT,
                "no device-list notification; using what OpenRGB lists now"
            ),
            Err(error) => warn!(%error, "rescan request failed; using what OpenRGB lists now"),
        }
        drop(client);
        self.connect();
    }

    /// Writes the boot lighting to device memory: the state `[boot] follow`
    /// names, realized with hardware effects only, since no host will be
    /// running. Call on shutdown before stopping.
    pub fn prepare_for_shutdown(&mut self) {
        let state = self.config.boot.follow;
        info!(%state, "writing boot lighting to device memory");
        self.desired_state = state;
        let Some(engine) = self.engine.as_mut() else {
            warn!("OpenRGB not connected; boot lighting left as last saved");
            return;
        };
        let errors = engine.apply_state(state, Persist::SaveToDevice, Attendance::Unattended);
        self.handle_errors(&errors);
    }

    /// Housekeeping: reconnects when disconnected, rebuilds the engine after
    /// a configuration change or once OpenRGB's device list has changed and
    /// settled, and runs the post-resume re-detection when due. Call before
    /// each wait.
    pub fn poll(&mut self) {
        if self.last_config_poll.elapsed() >= CONFIG_POLL_INTERVAL {
            self.last_config_poll = Instant::now();
            self.reload_if_changed();
        }
        self.watch_device_list();
        if self
            .device_list
            .take_settled(Instant::now(), DEVICE_LIST_SETTLE)
        {
            info!("OpenRGB's device list changed; rebuilding on the new list");
            self.disconnect();
            self.last_connect_attempt = None;
        }
        if self.engine.is_none() && self.reconnect_due() {
            self.connect();
        }
        if self.rescan_due.is_some_and(|due| due <= Instant::now()) {
            self.rescan_due = None;
            self.rescan_after_resume();
        }
        if self.retry_due.is_some_and(|due| due <= Instant::now()) {
            self.retry_due = None;
            self.retry_failed();
        }
    }

    /// Applies the state again to the devices whose last apply failed.
    fn retry_failed(&mut self) {
        let Some(engine) = self.engine.as_mut() else {
            return;
        };
        if !engine.has_failed_devices() {
            return;
        }
        info!("retrying the devices that failed their last update");
        let persist = Persist::for_state(self.desired_state, self.config.boot.follow);
        let attendance = Attendance::for_state(self.desired_state);
        let errors = engine.retry_failed(persist, attendance);
        self.handle_errors(&errors);
    }

    /// Reads the notifications OpenRGB sent on the engine's connection and
    /// notes a device-list change. The server announces one whenever its
    /// list changes: it is still detecting when the service connects at
    /// boot (the two start together, and the first connection saw two of
    /// five controllers), the user rescans from its UI, or a controller is
    /// plugged in. An engine built on the old list would drive only part of
    /// the hardware, so it is rebuilt once the announcements settle.
    fn watch_device_list(&mut self) {
        let changed = {
            let Some(client) = self.client.as_ref() else {
                return;
            };
            match client.lock() {
                Ok(mut client) => client.device_list_changed().map_err(|e| e.to_string()),
                Err(_) => Err("OpenRGB client lock poisoned".to_owned()),
            }
        };
        match changed {
            Ok(true) => {
                if self.device_list.announce(Instant::now()) {
                    info!(settle = ?DEVICE_LIST_SETTLE, "OpenRGB announced a device-list change");
                }
            }
            Ok(false) => {}
            Err(error) => {
                error!(%error, "OpenRGB connection lost; reconnecting");
                self.disconnect();
            }
        }
    }

    /// Writes the next software pulse frame, if any device is pulsing.
    pub fn tick(&mut self) {
        let Some(engine) = self.engine.as_mut() else {
            return;
        };
        if !engine.has_software_pulse() {
            return;
        }
        let errors = engine.tick(self.clock.elapsed());
        self.handle_errors(&errors);
    }

    /// How long the caller may wait before the next [`Self::poll`] or
    /// [`Self::tick`].
    #[must_use]
    pub fn wait_interval(&self) -> Duration {
        match &self.engine {
            Some(engine) if engine.has_software_pulse() => TICK_INTERVAL,
            _ => IDLE_INTERVAL,
        }
    }

    fn reconnect_due(&self) -> bool {
        self.last_connect_attempt
            .is_none_or(|at| at.elapsed() >= RECONNECT_INTERVAL)
    }

    fn reload_if_changed(&mut self) {
        let modified = modified_time(&self.store);
        if modified == self.config_modified {
            return;
        }
        self.config_modified = modified;
        match self.store.load() {
            Ok(config) => {
                info!(path = %self.store.path().display(), "configuration changed; reconnecting");
                self.config = config;
                // Rebuild the engine rather than swap the profile: the device
                // set depends on the configuration (aliases, selectors) and
                // on what the server reports now, which `argb devices scan`
                // may just have changed. Reconnecting over loopback is cheap.
                self.disconnect();
                self.last_connect_attempt = None;
            }
            Err(error) => {
                warn!(path = %self.store.path().display(), %error, "configuration unreadable; keeping the previous one");
            }
        }
    }

    fn connect(&mut self) {
        self.last_connect_attempt = Some(Instant::now());
        match self.build_engine() {
            Ok((engine, client)) => {
                self.engine = Some(engine);
                self.client = Some(client);
                self.apply();
            }
            Err(error) => {
                warn!(%error, retry_in = ?RECONNECT_INTERVAL, "OpenRGB unavailable");
            }
        }
    }

    /// Drops the engine and its connection; the next poll reconnects when
    /// due. Any pending device-list rebuild goes with it, since the new
    /// connection starts from the server's current list anyway.
    fn disconnect(&mut self) {
        self.engine = None;
        self.client = None;
        self.device_list = DeviceListWatch::default();
        self.retry_due = None;
    }

    /// Opens the OpenRGB session and builds the engine on its controllers
    /// plus the configured Govee lights. The lights are not contacted here:
    /// each one connects on its first apply.
    fn build_engine(&self) -> anyhow::Result<(ServiceEngine, SharedClient<OpenRgbClient>)> {
        let settings = &self.config.openrgb;
        let profile = self.config.to_profile();
        let session = open_session(&settings.host, settings.port, CLIENT_NAME, &profile)
            .with_context(|| {
                format!(
                    "connecting to OpenRGB at {}:{}",
                    settings.host, settings.port
                )
            })?;
        if !any_configured(&profile, session.devices.iter().map(LightingDevice::alias)) {
            anyhow::bail!(
                "OpenRGB lists {} controller(s), none matching the {} configured device(s)",
                session.devices.len(),
                profile.devices.len()
            );
        }
        let mut devices: Vec<Box<dyn LightingDevice>> = session
            .devices
            .into_iter()
            .map(|device| Box::new(device) as Box<dyn LightingDevice>)
            .collect();
        for device in configured_devices(&profile, &self.ble) {
            info!(alias = %device.alias(), "Govee light configured");
            devices.push(Box::new(device));
        }
        Ok((Engine::new(profile, devices), session.client))
    }

    fn apply(&mut self) {
        let Some(engine) = self.engine.as_mut() else {
            return;
        };
        let persist = Persist::for_state(self.desired_state, self.config.boot.follow);
        let attendance = Attendance::for_state(self.desired_state);
        let errors = engine.apply_state(self.desired_state, persist, attendance);
        self.handle_errors(&errors);
    }

    /// Sorts backend failures by origin: a Govee light that failed is
    /// retried after [`GOVEE_RETRY_INTERVAL`], an OpenRGB failure drops the
    /// connection so that the next poll reconnects. Capability errors are
    /// already logged by the engine.
    fn handle_errors(&mut self, errors: &[DeviceError]) {
        let mut openrgb_failed = false;
        let mut govee_failed = false;
        for error in errors {
            if let DeviceError::Backend { alias, .. } = error {
                match backend_of(&self.config, alias) {
                    Backend::GoveeBle => govee_failed = true,
                    Backend::OpenRgb => openrgb_failed = true,
                }
            }
        }
        if openrgb_failed {
            error!("OpenRGB connection lost; reconnecting");
            self.disconnect();
        } else if govee_failed {
            warn!(retry_in = ?GOVEE_RETRY_INTERVAL, "a Govee light did not take the update");
            self.retry_due = Some(Instant::now() + GOVEE_RETRY_INTERVAL);
        }
    }
}

/// The backend a device alias belongs to. Aliases the configuration does
/// not name are OpenRGB controllers the service derived an alias for.
fn backend_of(config: &ConfigFile, alias: &DeviceAlias) -> Backend {
    config
        .devices
        .get(alias)
        .map_or(Backend::OpenRgb, |device| device.backend)
}

/// Whether the engine would drive any OpenRGB controller the user
/// configured. A profile with no OpenRGB devices always passes. A controller
/// list that matches none of the configured devices is what OpenRGB serves
/// while it is still detecting; an engine built on it would sit idle, so the
/// caller retries instead.
fn any_configured<'a>(
    profile: &LightingProfile,
    aliases: impl IntoIterator<Item = &'a DeviceAlias>,
) -> bool {
    let configured: Vec<&DeviceAlias> = profile
        .devices
        .iter()
        .filter(|(_, settings)| settings.selector.backend == Backend::OpenRgb)
        .map(|(alias, _)| alias)
        .collect();
    configured.is_empty() || aliases.into_iter().any(|alias| configured.contains(&alias))
}

fn modified_time(store: &ConfigStore) -> Option<SystemTime> {
    std::fs::metadata(store.path())
        .and_then(|meta| meta.modified())
        .ok()
}

fn load_or_starter(store: &ConfigStore) -> ConfigFile {
    match store.load() {
        Ok(config) => {
            info!(path = %store.path().display(), devices = config.devices.len(), "configuration loaded");
            config
        }
        Err(error) => {
            warn!(path = %store.path().display(), %error, "configuration unavailable; using the starter defaults");
            ConfigFile::starter()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use argb_domain::{Backend, DeviceSelector, DeviceSettings};

    use super::*;

    fn runtime_in(state: PowerState) -> Runtime {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(dir.path().join("missing.toml"));
        Runtime::new(store, state)
    }

    #[test]
    fn repeated_resume_reports_keep_the_scheduled_rescan() {
        let mut runtime = runtime_in(PowerState::Sleep);
        runtime.enter_state(PowerState::Active);
        assert!(runtime.rescan_pending());
        runtime.enter_state(PowerState::Active);
        assert!(runtime.rescan_pending());
    }

    #[test]
    fn device_list_announcements_settle_after_a_quiet_period() {
        let quiet = Duration::from_secs(3);
        let t0 = Instant::now();
        let mut watch = DeviceListWatch::default();
        assert!(!watch.take_settled(t0, quiet));

        assert!(watch.announce(t0));
        assert!(!watch.take_settled(t0 + Duration::from_secs(2), quiet));
        // A second announcement inside the quiet period restarts it.
        assert!(!watch.announce(t0 + Duration::from_secs(2)));
        assert!(!watch.take_settled(t0 + Duration::from_secs(4), quiet));
        assert!(watch.take_settled(t0 + Duration::from_secs(5), quiet));
        // Settling clears the pending rebuild.
        assert!(!watch.take_settled(t0 + Duration::from_secs(60), quiet));
        assert!(watch.announce(t0 + Duration::from_secs(60)));
    }

    #[test]
    fn going_back_to_sleep_cancels_the_rescan() {
        let mut runtime = runtime_in(PowerState::Sleep);
        runtime.enter_state(PowerState::Active);
        runtime.enter_state(PowerState::Sleep);
        assert!(!runtime.rescan_pending());
    }

    fn settings() -> DeviceSettings {
        DeviceSettings {
            selector: DeviceSelector {
                backend: Backend::OpenRgb,
                match_name: "ram".to_owned(),
            },
            overrides: BTreeMap::new(),
        }
    }

    #[test]
    fn an_engine_needs_at_least_one_configured_controller() {
        let mut profile = ConfigFile::starter().to_profile();
        let ram = DeviceAlias::new("ram").unwrap();
        let other = DeviceAlias::new("other").unwrap();
        assert!(any_configured(&profile, [&other]));
        profile.devices.insert(ram.clone(), settings());
        assert!(any_configured(&profile, [&other, &ram]));
        assert!(!any_configured(&profile, [&other]));
        assert!(!any_configured(&profile, []));
    }

    #[test]
    fn govee_only_profiles_accept_any_controller_list() {
        let mut profile = ConfigFile::starter().to_profile();
        let mut lamp = settings();
        lamp.selector.backend = Backend::GoveeBle;
        profile
            .devices
            .insert(DeviceAlias::new("lamp").unwrap(), lamp);
        assert!(any_configured(&profile, []));
    }

    #[test]
    fn unknown_aliases_belong_to_openrgb() {
        let mut config = ConfigFile::starter();
        config.devices.insert(
            DeviceAlias::new("lamp").unwrap(),
            argb_config::DeviceSchema {
                backend: Backend::GoveeBle,
                match_name: "H6076".into(),
                states: BTreeMap::new(),
            },
        );
        assert_eq!(
            backend_of(&config, &DeviceAlias::new("lamp").unwrap()),
            Backend::GoveeBle
        );
        assert_eq!(
            backend_of(&config, &DeviceAlias::new("derived").unwrap()),
            Backend::OpenRgb
        );
    }
}
