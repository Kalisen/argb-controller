//! The service body: keeps an engine connected to OpenRGB, follows the
//! configuration file, and applies the requested power state.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Context;
use argb_backend_openrgb::{OpenRgbClient, OpenRgbDevice, assign_aliases};
use argb_config::{ConfigFile, ConfigStore};
use argb_domain::PowerState;
use argb_engine::{DEFAULT_TICK_RATE_HZ, DeviceError, Engine, Persist};
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
            clock: Instant::now(),
        }
    }

    /// Moves every device to `state`.
    pub fn enter_state(&mut self, state: PowerState) {
        info!(%state, "entering state");
        self.desired_state = state;
        self.apply();
    }

    /// Housekeeping: reloads a changed configuration and reconnects when
    /// disconnected. Call before each wait.
    pub fn poll(&mut self) {
        if self.last_config_poll.elapsed() >= CONFIG_POLL_INTERVAL {
            self.last_config_poll = Instant::now();
            self.reload_if_changed();
        }
        if self.engine.is_none() && self.reconnect_due() {
            self.connect();
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
                info!(path = %self.store.path().display(), "configuration changed; reapplying");
                let connection_changed = config.openrgb != self.config.openrgb;
                self.config = config;
                if connection_changed {
                    self.engine = None;
                    self.last_connect_attempt = None;
                } else if let Some(engine) = self.engine.as_mut() {
                    engine.set_profile(self.config.to_profile());
                    self.apply();
                }
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
        let mut client = OpenRgbClient::connect(&settings.host, settings.port, CLIENT_NAME)
            .with_context(|| {
                format!(
                    "connecting to OpenRGB at {}:{}",
                    settings.host, settings.port
                )
            })?;
        let controllers = client
            .controllers()
            .context("listing OpenRGB controllers")?;
        let profile = self.config.to_profile();
        let client = Arc::new(Mutex::new(client));
        let devices = assign_aliases(&profile, &controllers)
            .into_iter()
            .map(|assignment| {
                let description = controllers[assignment.index as usize].clone();
                info!(
                    index = assignment.index,
                    alias = %assignment.alias,
                    name = %description.name,
                    configured = assignment.configured,
                    "controller mapped"
                );
                OpenRgbDevice::new(
                    assignment.alias,
                    assignment.index,
                    description,
                    Arc::clone(&client),
                )
            })
            .collect();
        Ok(Engine::new(profile, devices))
    }

    fn apply(&mut self) {
        let Some(engine) = self.engine.as_mut() else {
            return;
        };
        let persist = Persist::for_state(self.desired_state, self.config.boot.follow);
        let errors = engine.apply_state(self.desired_state, persist);
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
