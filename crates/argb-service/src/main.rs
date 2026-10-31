//! The argb-controller Windows service.
//!
//! Runs as a service by default. Pass `--console` to run the same logic in
//! the foreground for development.

mod runtime;

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::time::Duration;

use argb_config::{ConfigStore, paths};
use argb_domain::PowerState;
use tracing::{error, info};

use crate::runtime::Runtime;

/// Service name registered with the Service Control Manager.
pub const SERVICE_NAME: &str = "argb-controller";
/// Name shown in the Services console.
pub const SERVICE_DISPLAY_NAME: &str = "ARGB Controller";
/// Flag that runs the service logic in the foreground.
const CONSOLE_FLAG: &str = "--console";
/// Flag naming the configuration file; defaults to the machine-wide file.
const CONFIG_FLAG: &str = "--config";

/// Longest time the suspend handler blocks while the sleep lighting is
/// written. Windows expects power-event handlers to return promptly; two
/// seconds is the documented budget for applications.
const SUSPEND_APPLY_BUDGET: Duration = Duration::from_millis(1800);

/// Commands delivered to the run loop.
#[derive(Debug)]
enum Command {
    /// Move devices to the lighting for `PowerState`. When a sender is
    /// given, the loop signals it once the devices are updated, so that a
    /// control handler can block until the sleep lighting is on.
    EnterState(PowerState, Option<SyncSender<()>>),
    /// Write the boot lighting to device memory, then stop.
    Shutdown,
    /// Shut the loop down without touching the devices.
    Stop,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let config_path = config_path_from_args(&args)?;
    if args.iter().any(|arg| arg == CONSOLE_FLAG) {
        return run_console(config_path);
    }
    run_as_service(config_path)
}

/// The value following `--config`, or the default path.
fn config_path_from_args(args: &[String]) -> anyhow::Result<PathBuf> {
    match args.iter().position(|arg| arg == CONFIG_FLAG) {
        None => Ok(paths::default_config_path()),
        Some(at) => args
            .get(at + 1)
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("{CONFIG_FLAG} needs a file path")),
    }
}

fn run_console(config_path: PathBuf) -> anyhow::Result<()> {
    info!("running in console mode; press Ctrl+C to stop");
    let (tx, rx) = mpsc::channel();
    let stop_tx: Sender<Command> = tx;
    install_ctrlc(move || {
        let _ = stop_tx.send(Command::Stop);
    });
    run_loop(&rx, config_path)
}

/// Minimal Ctrl+C handling without an extra dependency: a thread that waits
/// for end-of-input on stdin, which is what the console delivers on Ctrl+C.
fn install_ctrlc(on_stop: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || {
        let mut sink = String::new();
        let _ = std::io::stdin().read_line(&mut sink);
        on_stop();
    });
}

/// The service body shared by console and service modes.
///
/// Connects to OpenRGB (retrying until it is up), applies the active state,
/// then serves commands from the control handler while ticking the software
/// pulse and following configuration changes.
fn run_loop(rx: &Receiver<Command>, config_path: PathBuf) -> anyhow::Result<()> {
    let store = ConfigStore::new(config_path);
    let mut runtime = Runtime::new(store, PowerState::Active);

    loop {
        runtime.poll();
        match rx.recv_timeout(runtime.wait_interval()) {
            Ok(Command::EnterState(state, done)) => {
                runtime.enter_state(state);
                if let Some(done) = done {
                    let _ = done.try_send(());
                }
            }
            Ok(Command::Shutdown) => {
                runtime.prepare_for_shutdown();
                info!("stopping after shutdown notification");
                return Ok(());
            }
            Ok(Command::Stop) => {
                info!("stopping");
                return Ok(());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => runtime.tick(),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                error!("command channel closed unexpectedly");
                return Ok(());
            }
        }
    }
}

#[cfg(windows)]
fn run_as_service(config_path: PathBuf) -> anyhow::Result<()> {
    service::CONFIG_PATH
        .set(config_path)
        .map_err(|_| anyhow::anyhow!("service started twice in one process"))?;
    windows_service::service_dispatcher::start(SERVICE_NAME, ffi_service_main)?;
    Ok(())
}

#[cfg(not(windows))]
fn run_as_service(_config_path: PathBuf) -> anyhow::Result<()> {
    anyhow::bail!("service mode is only available on Windows; use {CONSOLE_FLAG}")
}

#[cfg(windows)]
windows_service::define_windows_service!(ffi_service_main, service_main);

#[cfg(windows)]
fn service_main(_arguments: Vec<std::ffi::OsString>) {
    if let Err(error) = service::run() {
        error!(%error, "service failed");
    }
}

#[cfg(windows)]
mod service {
    use std::path::PathBuf;
    use std::sync::{OnceLock, mpsc};
    use std::time::Duration;

    use argb_domain::PowerState;
    use windows_service::service::{
        PowerEventParam, ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState,
        ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};

    use super::{Command, SERVICE_NAME, SUSPEND_APPLY_BUDGET, run_loop};

    /// Configuration path handed over from `main`, since the dispatcher
    /// calls `service_main` without our arguments.
    pub static CONFIG_PATH: OnceLock<PathBuf> = OnceLock::new();

    pub fn run() -> anyhow::Result<()> {
        let config_path = CONFIG_PATH
            .get()
            .cloned()
            .unwrap_or_else(argb_config::paths::default_config_path);
        let (tx, rx) = mpsc::channel::<Command>();

        let handler = move |control: ServiceControl| -> ServiceControlHandlerResult {
            match control {
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                ServiceControl::Stop => {
                    let _ = tx.send(Command::Stop);
                    ServiceControlHandlerResult::NoError
                }
                // Preshutdown arrives first and with a generous budget; a
                // plain Shutdown is the fallback when preshutdown is not
                // delivered. Both write the boot lighting, then stop.
                ServiceControl::Preshutdown | ServiceControl::Shutdown => {
                    let _ = tx.send(Command::Shutdown);
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::PowerEvent(event) => {
                    match event {
                        PowerEventParam::Suspend => {
                            // Block until the sleep lighting is on the
                            // devices (or the budget runs out): once this
                            // handler returns, Windows may cut power.
                            let (done_tx, done_rx) = mpsc::sync_channel(1);
                            let _ = tx.send(Command::EnterState(PowerState::Sleep, Some(done_tx)));
                            let _ = done_rx.recv_timeout(SUSPEND_APPLY_BUDGET);
                        }
                        PowerEventParam::ResumeAutomatic | PowerEventParam::ResumeSuspend => {
                            let _ = tx.send(Command::EnterState(PowerState::Active, None));
                        }
                        _ => {}
                    }
                    ServiceControlHandlerResult::NoError
                }
                _ => ServiceControlHandlerResult::NotImplemented,
            }
        };

        let status_handle = service_control_handler::register(SERVICE_NAME, handler)?;
        let report = |state: ServiceState, accept: ServiceControlAccept| ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        };

        status_handle.set_service_status(report(
            ServiceState::Running,
            ServiceControlAccept::STOP
                | ServiceControlAccept::PRESHUTDOWN
                | ServiceControlAccept::SHUTDOWN
                | ServiceControlAccept::POWER_EVENT,
        ))?;

        let result = run_loop(&rx, config_path);

        status_handle
            .set_service_status(report(ServiceState::Stopped, ServiceControlAccept::empty()))?;
        result
    }
}
