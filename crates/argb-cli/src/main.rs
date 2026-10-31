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
    ControllerDescription, ModeMap, OpenRgbClient, assign_aliases, new_devices,
};
use argb_config::{ConfigError, ConfigFile, ConfigStore, DeviceSchema, OpenRgbSettings, paths};
use argb_domain::{Backend, Brightness, DeviceAlias, LightingMode, PowerState, PulsePeriod, Rgb};
use clap::{Args, Parser, Subcommand, ValueEnum};

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
}

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
    let store = ConfigStore::new(cli.config.unwrap_or_else(paths::default_config_path));
    match cli.command {
        TopCommand::Config { command } => run_config(&store, command),
        TopCommand::Devices { command } => run_devices(&store, &command),
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
