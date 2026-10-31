//! The argb-controller Windows service.
//!
//! Runs as a service by default. Pass `--console` to run the same logic in
//! the foreground for development.

mod runtime;

use std::sync::mpsc::{self, Receiver, Sender};

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

/// Commands delivered to the run loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    /// Move devices to the lighting for `PowerState`.
    EnterState(PowerState),
    /// Shut the loop down.
    Stop,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    if std::env::args().any(|arg| arg == CONSOLE_FLAG) {
        return run_console();
    }
    run_as_service()
}

fn run_console() -> anyhow::Result<()> {
    info!("running in console mode; press Ctrl+C to stop");
    let (tx, rx) = mpsc::channel();
    tx.send(Command::EnterState(PowerState::Active))?;
    let stop_tx: Sender<Command> = tx.clone();
    install_ctrlc(move || {
        let _ = stop_tx.send(Command::Stop);
    });
    run_loop(&rx)
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
fn run_loop(rx: &Receiver<Command>) -> anyhow::Result<()> {
    let store = ConfigStore::new(paths::default_config_path());
    let mut runtime = Runtime::new(store, PowerState::Active);

    loop {
        runtime.poll();
        match rx.recv_timeout(runtime.wait_interval()) {
            Ok(Command::EnterState(state)) => runtime.enter_state(state),
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
fn run_as_service() -> anyhow::Result<()> {
    windows_service::service_dispatcher::start(SERVICE_NAME, ffi_service_main)?;
    Ok(())
}

#[cfg(not(windows))]
fn run_as_service() -> anyhow::Result<()> {
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
    use std::sync::mpsc;
    use std::time::Duration;

    use argb_domain::PowerState;
    use windows_service::service::{
        PowerEventParam, ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState,
        ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};

    use super::{Command, SERVICE_NAME, run_loop};

    pub fn run() -> anyhow::Result<()> {
        let (tx, rx) = mpsc::channel::<Command>();

        let handler = move |control: ServiceControl| -> ServiceControlHandlerResult {
            match control {
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    let _ = tx.send(Command::Stop);
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::PowerEvent(event) => {
                    match event {
                        PowerEventParam::Suspend => {
                            let _ = tx.send(Command::EnterState(PowerState::Sleep));
                        }
                        PowerEventParam::ResumeAutomatic | PowerEventParam::ResumeSuspend => {
                            let _ = tx.send(Command::EnterState(PowerState::Active));
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
                | ServiceControlAccept::SHUTDOWN
                | ServiceControlAccept::POWER_EVENT,
        ))?;

        let result = run_loop(&rx);

        status_handle
            .set_service_status(report(ServiceState::Stopped, ServiceControlAccept::empty()))?;
        result
    }
}
