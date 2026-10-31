//! Phase 2 spike: can this machine reach the Govee lights over Bluetooth LE,
//! both from a console and from a Windows service running in session 0 as
//! `LocalService`? The answer decides how the Govee backend is hosted.
//!
//! Console: `argb-ble-probe scan`, `argb-ble-probe connect --name H6061 --layout hexa --color 40e0d0`.
//! Service: `argb-ble-probe service install --name H6061 --layout hexa --color 40e0d0` from an
//! elevated prompt, then `argb-ble-probe service start`; the result lands in
//! `ble-probe.log` beside the service logs.

mod crypto;
mod govee;
mod probe;
#[cfg(windows)]
mod service;

use std::path::PathBuf;
use std::time::Duration;

use argb_config::paths;
use clap::{Args, Parser, Subcommand};
use tracing::error;

use crate::govee::Layout;
use crate::probe::Actions;

/// Default scan duration in seconds.
const DEFAULT_SCAN_SECONDS: u64 = 8;
/// Name of the probe's log file in the service log directory.
const LOG_FILE_NAME: &str = "ble-probe.log";

#[derive(Parser)]
#[command(
    name = "argb-ble-probe",
    about = "Bluetooth LE probe for the Govee lights (Phase 2 spike)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List the Bluetooth LE peripherals in range.
    Scan {
        /// How long to scan.
        #[arg(long, default_value_t = DEFAULT_SCAN_SECONDS)]
        seconds: u64,
    },
    /// Scan, connect to one light, list its services, send a keep-alive and
    /// optionally a color, and print the replies.
    Connect(Target),
    /// Register, run or remove the probe as a Windows service, to test
    /// Bluetooth access from session 0 as LocalService.
    #[cfg(windows)]
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Entry point used by the Service Control Manager; not for the console.
    #[cfg(windows)]
    #[command(hide = true)]
    RunService(Target),
}

/// Which light to connect to and what to do with it.
#[derive(Args, Clone)]
struct Target {
    /// Case-insensitive substring of the advertised name, e.g. `H6061`.
    #[arg(long)]
    name: String,
    /// How long to scan before connecting.
    #[arg(long, default_value_t = DEFAULT_SCAN_SECONDS)]
    seconds: u64,
    /// Color to show, as six hex digits (e.g. `40e0d0`).
    #[arg(long)]
    color: Option<String>,
    /// Frame format of the target model; required with `--color`.
    #[arg(long, value_enum)]
    layout: Option<Layout>,
    /// Whole-device brightness in percent.
    #[arg(long)]
    brightness: Option<u8>,
    /// Power the light off at the end.
    #[arg(long)]
    off: bool,
}

#[cfg(windows)]
#[derive(Subcommand)]
enum ServiceCommand {
    /// Copy this binary beside the argb-controller binaries and register the
    /// probe service (manual start, LocalService) with the given target.
    Install(Target),
    /// Start the registered probe service once; read the log afterwards.
    Start,
    /// Stop and unregister the probe service.
    Uninstall,
    /// Print the log file path.
    Log,
}

impl Target {
    fn actions(&self) -> anyhow::Result<Actions> {
        Ok(Actions {
            color: self.color.as_deref().map(parse_color).transpose()?,
            layout: self.layout,
            brightness: self.brightness,
            power_off: self.off,
        })
    }
}

/// Parses `rrggbb` (an optional leading `#` is accepted).
fn parse_color(text: &str) -> anyhow::Result<(u8, u8, u8)> {
    let digits = text.trim_start_matches('#');
    anyhow::ensure!(
        digits.len() == 6,
        "color must be six hex digits, got {text:?}"
    );
    let channel = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16);
    Ok((channel(0)?, channel(2)?, channel(4)?))
}

/// Where the service writes its log: beside the argb-controller service logs,
/// which the service account may write to.
fn log_path() -> PathBuf {
    paths::log_dir_for(&paths::default_config_path()).join(LOG_FILE_NAME)
}

fn init_console_logging() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
}

fn run_probe(seconds: u64, target: Option<&Target>) -> anyhow::Result<()> {
    let actions = target.map(Target::actions).transpose()?.unwrap_or_default();
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(probe::run(
        Duration::from_secs(seconds),
        target.map(|t| t.name.as_str()),
        actions,
    ))
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Scan { seconds } => {
            init_console_logging();
            run_probe(seconds, None)
        }
        Command::Connect(target) => {
            init_console_logging();
            run_probe(target.seconds, Some(&target))
        }
        #[cfg(windows)]
        Command::Service { command } => {
            init_console_logging();
            match command {
                ServiceCommand::Install(target) => service::install(&target),
                ServiceCommand::Start => service::start(),
                ServiceCommand::Uninstall => service::uninstall(),
                ServiceCommand::Log => {
                    println!("{}", log_path().display());
                    Ok(())
                }
            }
        }
        #[cfg(windows)]
        Command::RunService(target) => {
            let result = service::run(target);
            if let Err(error) = &result {
                error!(%error, "probe service failed");
            }
            result
        }
    }
}
