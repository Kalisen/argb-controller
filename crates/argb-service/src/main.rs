//! The argb-controller Windows service.
//!
//! Runs as a service by default. Pass `--console` to run the same logic in
//! the foreground for development.

mod runtime;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::time::Duration;

use argb_config::{ConfigStore, paths};
use argb_domain::PowerState;
use tracing::{error, info};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::Rotation;

use crate::runtime::Runtime;

use argb_service::{CONFIG_FLAG, CONSOLE_FLAG, SERVICE_NAME};

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

/// File name prefix of the service log; the date and `.log` follow.
const LOG_FILE_PREFIX: &str = "argb-service";
/// File name suffix of the service log.
const LOG_FILE_SUFFIX: &str = "log";
/// Daily log files kept before the oldest is deleted.
const LOG_FILES_KEPT: usize = 7;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config_path = config_path_from_args(&args)?;
    let console = args.iter().any(|arg| arg == CONSOLE_FLAG);
    let _log_guard = init_logging(console, &config_path);
    if console {
        return run_console(config_path);
    }
    run_as_service(config_path)
}

/// Console mode logs to stdout. Service mode has no console, so it logs to
/// a daily file under the `logs` directory beside the configuration, keeping
/// [`LOG_FILES_KEPT`] days; the returned guard flushes the file on exit.
/// When that directory cannot be written the service runs without a log.
fn init_logging(console: bool, config_path: &Path) -> Option<WorkerGuard> {
    let filter = || {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
    };
    if console {
        tracing_subscriber::fmt().with_env_filter(filter()).init();
        return None;
    }
    let dir = paths::log_dir_for(config_path);
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix(LOG_FILE_PREFIX)
        .filename_suffix(LOG_FILE_SUFFIX)
        .max_log_files(LOG_FILES_KEPT)
        .build(&dir)
        .ok()?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_env_filter(filter())
        .with_ansi(false)
        .with_writer(writer)
        .init();
    info!(dir = %dir.display(), "logging to file");
    Some(guard)
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
