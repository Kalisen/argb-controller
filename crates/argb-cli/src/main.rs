//! `argb`: inspect and edit the argb-controller configuration from scripts.
//!
//! Every mutation goes through [`ConfigStore::update`], which holds a lock and
//! replaces the file atomically, so concurrent invocations cannot corrupt it.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, bail};
use argb_backend_openrgb::protocol::{ColorMode, mode_flags};
use argb_backend_openrgb::{
    ControllerDescription, ModeMap, OpenRgbClient, assign_aliases, new_devices, open_devices,
};
use argb_config::{ConfigError, ConfigFile, ConfigStore, DeviceSchema, OpenRgbSettings, paths};
use argb_domain::{Backend, Brightness, DeviceAlias, LightingMode, PowerState, PulsePeriod, Rgb};
use argb_engine::{Attendance, Engine, LightingDevice, Persist};
use clap::{Args, Parser, Subcommand, ValueEnum};

mod service_admin;

#[derive(Parser)]
#[command(
    name = "argb",
    version,
    about = "Configure the ARGB Controller service"
)]
struct Cli {
    /// Configuration file to operate on. Defaults to the service's machine-wide file.
    #[arg(long, global = true, env = "ARGB_CONFIG")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: TopCommand,
}

/// Name this tool registers with the OpenRGB server.
const CLIENT_NAME: &str = "argb-cli";

#[derive(Subcommand)]
enum TopCommand {
    /// Inspect or change the configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Inspect the devices the backends expose.
    Devices {
        #[command(subcommand)]
        command: DevicesCommand,
    },
    /// Put every device into its lighting for a power state, the way the
    /// service does when no host will keep driving them (hardware effects
    /// only). Useful to preview the sleep or boot lighting.
    Apply {
        /// Power state to show.
        #[arg(value_enum)]
        state: StateArg,
        /// Store the modes in device memory regardless of the `[boot] follow`
        /// policy.
        #[arg(long)]
        save: bool,
    },
    /// Write a distinct color per device to device memory, so that a cold
    /// boot reveals which devices keep their lighting.
    ProbePersistence,
    /// Install, control and check the Windows service (needs an elevated prompt
    /// for everything but status and check).
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
}

#[derive(Subcommand)]
enum ServiceCommand {
    /// Copy the binaries to the program directory, restrict the configuration
    /// directory to administrators, register the service to start at boot as
    /// LocalService with automatic restart, and start it.
    Install {
        /// Directory that receives the binaries. Defaults to the program files folder.
        #[arg(long)]
        install_dir: Option<PathBuf>,
        /// Service executable to install. Defaults to argb-service.exe next to this tool.
        #[arg(long)]
        service_binary: Option<PathBuf>,
        /// Register without starting.
        #[arg(long)]
        no_start: bool,
    },
    /// Stop and unregister the service, leaving binaries and configuration in place.
    Uninstall,
    /// Start the service.
    Start,
    /// Stop the service.
    Stop,
    /// Print the service registration and state.
    Status,
    /// Report on the configuration, the OpenRGB SDK server and the service.
    Check,
}

/// Colors used by `probe-persistence`, assigned to devices in list order.
const PROBE_PALETTE: [(&str, Rgb); 8] = [
    ("red", Rgb::new(255, 0, 0)),
    ("green", Rgb::new(0, 255, 0)),
    ("blue", Rgb::new(0, 0, 255)),
    ("yellow", Rgb::new(255, 255, 0)),
    ("magenta", Rgb::new(255, 0, 255)),
    ("cyan", Rgb::new(0, 255, 255)),
    ("white", Rgb::WHITE),
    ("orange", Rgb::new(255, 106, 0)),
];

#[derive(Subcommand)]
enum DevicesCommand {
    /// List the controllers OpenRGB reports and the alias each one maps to.
    List,
    /// Print everything OpenRGB reports about one controller: modes with
    /// their flags and ranges, zones and LED count.
    Show {
        /// Controller index as printed by `devices list`.
        index: u32,
    },
    /// Add a configuration entry for every controller OpenRGB reports that
    /// no configured device matches. New entries have no overrides, so they
    /// follow the state defaults; the running service picks the file up.
    Scan {
        /// Ask OpenRGB to detect hardware again before scanning.
        #[arg(long)]
        rescan: bool,
        /// Print what would be added without writing the configuration.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Print the configuration file path.
    Path,
    /// Create a starter configuration.
    Init {
        /// Overwrite an existing file.
        #[arg(long)]
        force: bool,
    },
    /// Print the configuration as TOML.
    Show,
    /// Check that the configuration parses and is valid.
    Validate,
    /// Set the default mode for a power state.
    SetDefault {
        /// Power state to change.
        #[arg(long, value_enum)]
        state: StateArg,
        #[command(flatten)]
        mode: ModeArgs,
    },
    /// Add a device or update how it is matched.
    SetDevice {
        /// Alias of the device (lower-case letters, digits, '-' or '_').
        alias: String,
        /// Backend that owns the device.
        #[arg(long, value_enum)]
        backend: BackendArg,
        /// Case-insensitive substring of the name the backend reports.
        #[arg(long)]
        match_name: String,
    },
    /// Override the mode a device shows in one power state.
    SetDeviceMode {
        /// Alias of an existing device.
        alias: String,
        /// Power state to override.
        #[arg(long, value_enum)]
        state: StateArg,
        #[command(flatten)]
        mode: ModeArgs,
    },
    /// Remove a device's override for one state, so it follows the default again.
    ClearDeviceMode {
        /// Alias of an existing device.
        alias: String,
        /// Power state whose override is removed.
        #[arg(long, value_enum)]
        state: StateArg,
    },
    /// Remove a device entirely.
    RemoveDevice {
        /// Alias of the device.
        alias: String,
    },
    /// Give a device a new alias, keeping its selector and overrides.
    RenameDevice {
        /// Current alias.
        alias: String,
        /// New alias (lower-case letters, digits, '-' or '_').
        new_alias: String,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum StateArg {
    Active,
    Sleep,
}

impl From<StateArg> for PowerState {
    fn from(value: StateArg) -> Self {
        match value {
            StateArg::Active => PowerState::Active,
            StateArg::Sleep => PowerState::Sleep,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum BackendArg {
    Openrgb,
    GoveeBle,
}

impl From<BackendArg> for Backend {
    fn from(value: BackendArg) -> Self {
        match value {
            BackendArg::Openrgb => Backend::OpenRgb,
            BackendArg::GoveeBle => Backend::GoveeBle,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum ModeKind {
    Static,
    Pulse,
}

#[derive(Args)]
struct ModeArgs {
    /// Lighting mode.
    #[arg(long, value_enum)]
    mode: ModeKind,
    /// Color as #RRGGBB.
    #[arg(long)]
    color: String,
    /// Brightness percent, 0 to 100. Static mode only; defaults to 100.
    #[arg(long)]
    brightness: Option<u8>,
    /// Full pulse cycle in milliseconds. Pulse mode only.
    #[arg(long)]
    period_ms: Option<u64>,
}

impl ModeArgs {
    fn into_mode(self) -> anyhow::Result<LightingMode> {
        let color = Rgb::from_hex(&self.color)?;
        match self.mode {
            ModeKind::Static => {
                if self.period_ms.is_some() {
                    bail!("--period-ms applies to pulse mode only");
                }
                let brightness =
                    Brightness::new(self.brightness.unwrap_or(Brightness::MAX.percent()))?;
                Ok(LightingMode::Static { color, brightness })
            }
            ModeKind::Pulse => {
                if self.brightness.is_some() {
                    bail!("--brightness applies to static mode only");
                }
                let period_ms = self
                    .period_ms
                    .context("--period-ms is required for pulse mode")?;
                Ok(LightingMode::Pulse {
                    color,
                    period: PulsePeriod::from_millis(period_ms)?,
                })
            }
        }
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let explicit_config = cli.config.clone();
    let store = ConfigStore::new(cli.config.unwrap_or_else(paths::default_config_path));
    match cli.command {
        TopCommand::Service { command } => {
            run_service(&store, explicit_config.as_deref(), &command)
        }
        TopCommand::Config { command } => run_config(&store, command),
        TopCommand::Devices { command } => run_devices(&store, &command),
        TopCommand::Apply { state, save } => apply_state(&store, state.into(), save),
        TopCommand::ProbePersistence => probe_persistence(&store),
    }
}

fn run_service(
    store: &ConfigStore,
    explicit_config: Option<&std::path::Path>,
    command: &ServiceCommand,
) -> anyhow::Result<()> {
    match command {
        ServiceCommand::Install {
            install_dir,
            service_binary,
            no_start,
        } => {
            let binary = match service_binary {
                Some(path) => path.clone(),
                None => service_admin::sibling_service_binary()?,
            };
            let dir = install_dir
                .clone()
                .unwrap_or_else(service_admin::default_install_dir);
            service_admin::platform::install(&binary, &dir, store, explicit_config, !no_start)?;
            println!();
            service_admin::check(store);
            Ok(())
        }
        ServiceCommand::Uninstall => service_admin::platform::uninstall(),
        ServiceCommand::Start => service_admin::platform::start(),
        ServiceCommand::Stop => service_admin::platform::stop(),
        ServiceCommand::Status => {
            let (_, line) = service_admin::platform::status_line()?;
            println!("{line}");
            Ok(())
        }
        ServiceCommand::Check => {
            if service_admin::check(store) {
                Ok(())
            } else {
                bail!("some checks failed")
            }
        }
    }
}

fn load_or_starter(store: &ConfigStore) -> anyhow::Result<ConfigFile> {
    if store.exists() {
        Ok(store.load()?)
    } else {
        Ok(ConfigFile::starter())
    }
}

fn apply_state(store: &ConfigStore, state: PowerState, save: bool) -> anyhow::Result<()> {
    let config = load_or_starter(store)?;
    let profile = config.to_profile();
    let devices = open_devices(
        &config.openrgb.host,
        config.openrgb.port,
        CLIENT_NAME,
        &profile,
    )
    .with_context(|| {
        format!(
            "connecting to OpenRGB at {}:{}",
            config.openrgb.host, config.openrgb.port
        )
    })?;
    let persist = if save {
        Persist::SaveToDevice
    } else {
        Persist::for_state(state, config.boot.follow)
    };
    let mut engine = Engine::new(profile, devices);
    let errors = engine.apply_state(state, persist, Attendance::Unattended);
    for error in &errors {
        eprintln!("{error}");
    }
    println!(
        "applied {state} lighting ({persist:?}) to {} device(s), {} failed",
        engine.devices().len(),
        errors.len()
    );
    if errors.is_empty() {
        Ok(())
    } else {
        bail!("some devices did not take the state")
    }
}

fn probe_persistence(store: &ConfigStore) -> anyhow::Result<()> {
    let config = load_or_starter(store)?;
    let profile = config.to_profile();
    let mut devices = open_devices(
        &config.openrgb.host,
        config.openrgb.port,
        CLIENT_NAME,
        &profile,
    )
    .with_context(|| {
        format!(
            "connecting to OpenRGB at {}:{}",
            config.openrgb.host, config.openrgb.port
        )
    })?;
    println!("{:<34}{:<10}hex", "alias", "color");
    let mut failures = 0;
    for (i, device) in devices.iter_mut().enumerate() {
        let (name, color) = PROBE_PALETTE[i % PROBE_PALETTE.len()];
        match device.apply_static(color, Brightness::MAX, Persist::SaveToDevice) {
            Ok(()) => println!("{:<34}{:<10}{}", device.alias(), name, color.to_hex()),
            Err(error) => {
                failures += 1;
                println!("{:<34}{:<10}{error}", device.alias(), "FAILED");
            }
        }
    }
    println!();
    println!("Now shut Windows down fully and power the machine back on. With Fast Startup");
    println!("enabled, use 'shutdown /s /t 0' or hold Shift while clicking Shut down, since a");
    println!("regular shutdown hibernates the kernel and keeps the lighting state in RAM.");
    println!("During POST, note which device shows its color above: those keep their lighting");
    println!("in their own memory and are safe to rely on for boot lighting.");
    if failures == 0 {
        Ok(())
    } else {
        bail!("{failures} device(s) failed to take the probe color")
    }
}

fn run_devices(store: &ConfigStore, command: &DevicesCommand) -> anyhow::Result<()> {
    let config = if store.exists() {
        store.load()?
    } else {
        ConfigFile::starter()
    };
    match command {
        DevicesCommand::List => list_devices(&config),
        DevicesCommand::Show { index } => show_device(&config, *index),
        DevicesCommand::Scan { rescan, dry_run } => scan_devices(store, &config, *rescan, *dry_run),
    }
}

/// Longest wait for OpenRGB to finish a requested hardware detection.
const RESCAN_TIMEOUT: Duration = Duration::from_secs(60);

fn scan_devices(
    store: &ConfigStore,
    config: &ConfigFile,
    rescan: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    let mut client = connect(config)?;
    if rescan {
        println!("asking OpenRGB to detect hardware again...");
        if !client.rescan(RESCAN_TIMEOUT)? {
            println!(
                "no device-list notification within {RESCAN_TIMEOUT:?}; scanning what is listed now"
            );
        }
    }
    let controllers = client.controllers()?;
    let found = new_devices(&config.to_profile(), &controllers);
    if found.is_empty() {
        println!("every controller OpenRGB reports is already configured");
        return Ok(());
    }
    for device in &found {
        println!(
            "{:>5}  {:<34}{}",
            device.index, device.alias, device.match_name
        );
    }
    if dry_run {
        println!("{} new device(s); nothing written (--dry-run)", found.len());
        return Ok(());
    }
    if !store.exists() {
        store.save(&ConfigFile::starter())?;
    }
    store.update(|config| {
        for device in &found {
            config.devices.insert(
                device.alias.clone(),
                DeviceSchema {
                    backend: Backend::OpenRgb,
                    match_name: device.match_name.clone(),
                    states: BTreeMap::new(),
                },
            );
        }
        Ok(())
    })?;
    println!(
        "added {} device(s) to {}; a running service applies them within a second",
        found.len(),
        store.path().display()
    );
    Ok(())
}

fn connect(config: &ConfigFile) -> anyhow::Result<OpenRgbClient> {
    let OpenRgbSettings { host, port } = &config.openrgb;
    OpenRgbClient::connect(host, *port, CLIENT_NAME).with_context(|| {
        format!("connecting to OpenRGB at {host}:{port}; is the SDK server enabled?")
    })
}

fn show_device(config: &ConfigFile, index: u32) -> anyhow::Result<()> {
    let mut client = connect(config)?;
    let count = client.controller_count()?;
    if index >= count {
        bail!("controller index {index} is out of range; OpenRGB reports {count} controllers");
    }
    let controller = client.controller(index)?;
    print_controller(index, &controller);
    Ok(())
}

fn print_controller(index: u32, controller: &ControllerDescription) {
    let modes = ModeMap::analyze(controller);
    let caps = modes.capabilities(controller);
    println!("controller {index}: {}", controller.name);
    println!(
        "  type: {}  vendor: {}  version: {}",
        controller.device_type, controller.vendor, controller.version
    );
    println!("  description: {}", controller.description);
    println!(
        "  location: {}  serial: {}",
        controller.location, controller.serial
    );
    println!(
        "  leds: {}  active mode: {}",
        controller.led_count(),
        controller.active_mode
    );
    println!("  capabilities: {caps:?}");
    println!(
        "  roles: direct={:?} static={:?} pulse={:?}",
        modes.direct, modes.static_mode, modes.pulse
    );
    println!("  modes:");
    for (i, mode) in controller.modes.iter().enumerate() {
        let color_mode = ColorMode::from_raw(mode.color_mode)
            .map_or_else(|| mode.color_mode.to_string(), |m| format!("{m:?}"));
        println!(
            "    [{i}] {:<16} value={} flags=[{}] speed={}..{} (now {}) brightness={}..{} (now {}) colors={}..{} color_mode={color_mode}",
            mode.name,
            mode.value,
            mode_flags::describe(mode.flags),
            mode.speed_min,
            mode.speed_max,
            mode.speed,
            mode.brightness_min,
            mode.brightness_max,
            mode.brightness,
            mode.colors_min,
            mode.colors_max,
        );
    }
    println!("  zones:");
    for (i, zone) in controller.zones.iter().enumerate() {
        println!(
            "    [{i}] {:<16} type={} leds={} ({}..{}) segments={}",
            zone.name,
            zone.zone_type,
            zone.leds_count,
            zone.leds_min,
            zone.leds_max,
            zone.segments.len()
        );
    }
}

/// Prints one line per controller: index, alias (marked `*` when it comes
/// from the configuration), device type, capabilities and reported name.
fn list_devices(config: &ConfigFile) -> anyhow::Result<()> {
    let mut client = connect(config)?;
    let controllers = client.controllers()?;
    let profile = config.to_profile();

    println!(
        "{:>5}  {:<34}{:<13}{:<9}name",
        "index", "alias", "type", "caps"
    );
    for assignment in assign_aliases(&profile, &controllers) {
        let controller = &controllers[assignment.index as usize];
        let caps = ModeMap::analyze(controller).capabilities(controller);
        let flags = format!(
            "{}{}{}{}",
            if caps.direct { 'D' } else { '-' },
            if caps.hardware_brightness { 'B' } else { '-' },
            if caps.hardware_pulse { 'P' } else { '-' },
            if caps.persistent_memory { 'S' } else { '-' },
        );
        let alias = format!(
            "{}{}",
            assignment.alias,
            if assignment.configured { " *" } else { "" }
        );
        println!(
            "{:>5}  {:<34}{:<13}{:<9}{}",
            assignment.index, alias, controller.device_type, flags, controller.name
        );
    }
    println!();
    println!("caps: D direct, B hardware brightness, P hardware pulse, S persistent memory");
    println!("*: alias taken from the configuration; others are derived from the name");
    Ok(())
}

fn run_config(store: &ConfigStore, command: ConfigCommand) -> anyhow::Result<()> {
    match command {
        ConfigCommand::Path => println!("{}", store.path().display()),
        ConfigCommand::Init { force } => {
            if store.exists() && !force {
                bail!(
                    "{} already exists; pass --force to overwrite",
                    store.path().display()
                );
            }
            store.save(&ConfigFile::starter())?;
            println!("wrote {}", store.path().display());
        }
        ConfigCommand::Show => print!("{}", store.load()?.to_toml()?),
        ConfigCommand::Validate => {
            store.load()?;
            println!("{} is valid", store.path().display());
        }
        ConfigCommand::SetDefault { state, mode } => {
            let mode = mode.into_mode()?;
            store.update(|config| {
                match PowerState::from(state) {
                    PowerState::Active => config.states.active = mode,
                    PowerState::Sleep => config.states.sleep = mode,
                }
                Ok(())
            })?;
        }
        ConfigCommand::SetDevice {
            alias,
            backend,
            match_name,
        } => {
            let alias = DeviceAlias::new(alias)?;
            store.update(|config| {
                let entry = config.devices.entry(alias).or_insert_with(|| DeviceSchema {
                    backend: backend.into(),
                    match_name: match_name.clone(),
                    states: BTreeMap::new(),
                });
                entry.backend = backend.into();
                entry.match_name.clone_from(&match_name);
                Ok(())
            })?;
        }
        ConfigCommand::SetDeviceMode { alias, state, mode } => {
            let alias = DeviceAlias::new(alias)?;
            let mode = mode.into_mode()?;
            store.update(|config| {
                existing_device(config, &alias)?
                    .states
                    .insert(state.into(), mode);
                Ok(())
            })?;
        }
        ConfigCommand::ClearDeviceMode { alias, state } => {
            let alias = DeviceAlias::new(alias)?;
            store.update(|config| {
                existing_device(config, &alias)?
                    .states
                    .remove(&state.into());
                Ok(())
            })?;
        }
        ConfigCommand::RenameDevice { alias, new_alias } => {
            let alias = DeviceAlias::new(alias)?;
            let new_alias = DeviceAlias::new(new_alias)?;
            store.update(|config| {
                if config.devices.contains_key(&new_alias) {
                    return Err(ConfigError::Invalid(format!(
                        "a device with alias '{new_alias}' already exists"
                    )));
                }
                let device = config
                    .devices
                    .remove(&alias)
                    .ok_or_else(|| unknown_device(&alias))?;
                config.devices.insert(new_alias.clone(), device);
                Ok(())
            })?;
        }
        ConfigCommand::RemoveDevice { alias } => {
            let alias = DeviceAlias::new(alias)?;
            store.update(|config| {
                if config.devices.remove(&alias).is_none() {
                    return Err(unknown_device(&alias));
                }
                Ok(())
            })?;
        }
    }
    Ok(())
}

fn existing_device<'a>(
    config: &'a mut ConfigFile,
    alias: &DeviceAlias,
) -> Result<&'a mut DeviceSchema, ConfigError> {
    config
        .devices
        .get_mut(alias)
        .ok_or_else(|| unknown_device(alias))
}

fn unknown_device(alias: &DeviceAlias) -> ConfigError {
    ConfigError::Invalid(format!("no device with alias '{alias}'"))
}
