//! `argb`: inspect and edit the argb-controller configuration from scripts.
//!
//! Every mutation goes through [`ConfigStore::update`], which holds a lock and
//! replaces the file atomically, so concurrent invocations cannot corrupt it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, bail};
use argb_config::{ConfigError, ConfigFile, ConfigStore, DeviceSchema, paths};
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

#[derive(Subcommand)]
enum TopCommand {
    /// Inspect or change the configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
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
    let TopCommand::Config { command } = cli.command;
    run_config(&store, command)
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
