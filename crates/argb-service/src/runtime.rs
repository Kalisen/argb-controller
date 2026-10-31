//! The service body: keeps an engine connected to OpenRGB, follows the
//! configuration file, and applies the requested power state.

use std::time::{Duration, Instant, SystemTime};

use anyhow::Context;
use argb_backend_openrgb::{OpenRgbClient, OpenRgbDevice, open_devices};
use argb_config::{ConfigFile, ConfigStore};
use argb_domain::{DeviceAlias, LightingProfile, PowerState};
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

type OpenRgbEngine = Engine<OpenRgbDevice<OpenRgbClient>>;

/// Runtime state of the service between commands.
pub struct Runtime {
    store: ConfigStore,
    config: ConfigFile,
    config_modified: Option<SystemTime>,
    last_config_poll: Instant,
    engine: Option<OpenRgbEngine>,
    last_connect_attempt: Option<Instant>,
    desired_state: PowerState,
    /// When the post-resume re-detection is due, if one is pending.
    rescan_due: Option<Instant>,
    clock: Instant,
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
        self.engine = None;
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

    /// Housekeeping: reconnects when disconnected, and after a configuration
    /// change drops the engine so the next call rebuilds it from the new
    /// configuration and the server's current device list. Call before each
    /// wait.
    pub fn poll(&mut self) {
        if self.last_config_poll.elapsed() >= CONFIG_POLL_INTERVAL {
            self.last_config_poll = Instant::now();
            self.reload_if_changed();
        }
        if self.engine.is_none() && self.reconnect_due() {
            self.connect();
        }
        if self.rescan_due.is_some_and(|due| due <= Instant::now()) {
            self.rescan_due = None;
            self.rescan_after_resume();
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
                self.engine = None;
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
            Ok(engine) => {
                self.engine = Some(engine);
                self.apply();
            }
            Err(error) => {
                warn!(%error, retry_in = ?RECONNECT_INTERVAL, "OpenRGB unavailable");
            }
        }
    }

    fn build_engine(&self) -> anyhow::Result<OpenRgbEngine> {
        let settings = &self.config.openrgb;
        let profile = self.config.to_profile();
        let devices = open_devices(&settings.host, settings.port, CLIENT_NAME, &profile)
            .with_context(|| {
                format!(
                    "connecting to OpenRGB at {}:{}",
                    settings.host, settings.port
                )
            })?;
        if !any_configured(&profile, devices.iter().map(LightingDevice::alias)) {
            anyhow::bail!(
                "OpenRGB lists {} controller(s), none matching the {} configured device(s)",
                devices.len(),
                profile.devices.len()
            );
        }
        Ok(Engine::new(profile, devices))
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

    /// Drops the connection on backend failures so that the next poll
    /// reconnects; capability errors are already logged by the engine.
    fn handle_errors(&mut self, errors: &[DeviceError]) {
        if errors
            .iter()
            .any(|error| matches!(error, DeviceError::Backend { .. }))
        {
            error!("OpenRGB connection lost; reconnecting");
            self.engine = None;
        }
    }
}

/// Whether the engine would drive anything the user configured. An empty
/// profile always passes. A controller list that matches none of the
/// configured devices is what OpenRGB serves while it is still detecting; an
/// engine built on it would sit idle, so the caller retries instead.
fn any_configured<'a>(
    profile: &LightingProfile,
    aliases: impl IntoIterator<Item = &'a DeviceAlias>,
) -> bool {
    profile.devices.is_empty()
        || aliases
            .into_iter()
            .any(|alias| profile.devices.contains_key(alias))
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
}
