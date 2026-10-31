//! Running the probe as a Windows service, and registering it.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::Context;
use clap::ValueEnum;
use tracing::info;
use windows_service::service::{
    ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

use crate::{Target, log_path, run_probe};

/// Service name registered with the Service Control Manager.
const SERVICE_NAME: &str = "argb-ble-probe";
/// Name shown in the Services console.
const SERVICE_DISPLAY_NAME: &str = "ARGB BLE Probe (spike)";
/// Account the probe runs under: the same as the real service.
const SERVICE_ACCOUNT: &str = r"NT AUTHORITY\LocalService";
/// Directory the argb-controller binaries live in, under the program files folder.
const INSTALL_DIR_NAME: &str = "argb-controller";
/// Environment variable Windows uses for the program files folder.
const PROGRAM_FILES_ENV: &str = "ProgramFiles";
/// File name of this binary once copied.
const BINARY_NAME: &str = "argb-ble-probe.exe";
/// Subcommand the service is launched with.
const RUN_SUBCOMMAND: &str = "run-service";

/// Arguments handed over from `main`, since the dispatcher calls
/// `service_main` without them.
static TARGET: OnceLock<Target> = OnceLock::new();

/// Runs the probe under the Service Control Manager.
///
/// # Errors
/// Dispatcher failures; probe failures are logged and reported as a stopped
/// service with a non-zero exit code.
pub fn run(target: Target) -> anyhow::Result<()> {
    TARGET
        .set(target)
        .map_err(|_| anyhow::anyhow!("service started twice in one process"))?;
    windows_service::service_dispatcher::start(SERVICE_NAME, ffi_service_main)?;
    Ok(())
}

windows_service::define_windows_service!(ffi_service_main, service_main);

fn service_main(_arguments: Vec<OsString>) {
    let _ = body();
}

fn body() -> anyhow::Result<()> {
    let appender = tracing_appender::rolling::never(
        log_path().parent().map(PathBuf::from).unwrap_or_default(),
        crate::LOG_FILE_NAME,
    );
    let (writer, _guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(writer)
        .init();

    let handler = |control: ServiceControl| -> ServiceControlHandlerResult {
        match control {
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };
    let status_handle = service_control_handler::register(SERVICE_NAME, handler)?;
    let report = |state: ServiceState, code: u32| ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(code),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    };
    status_handle.set_service_status(report(ServiceState::Running, 0))?;

    info!("probe service running in session 0");
    let target = TARGET.get().cloned();
    let result = run_probe(
        target
            .as_ref()
            .map_or(crate::DEFAULT_SCAN_SECONDS, |t| t.seconds),
        target.as_ref(),
    );
    match &result {
        Ok(()) => info!("probe finished"),
        Err(error) => tracing::error!(%error, "probe failed"),
    }
    let code = u32::from(result.is_err());
    status_handle.set_service_status(report(ServiceState::Stopped, code))?;
    result
}

fn install_dir() -> anyhow::Result<PathBuf> {
    let base = std::env::var_os(PROGRAM_FILES_ENV).context("ProgramFiles is not set")?;
    Ok(PathBuf::from(base).join(INSTALL_DIR_NAME))
}

fn manager(access: ServiceManagerAccess) -> anyhow::Result<ServiceManager> {
    ServiceManager::local_computer(None::<&str>, access)
        .context("opening the Service Control Manager (elevated prompt needed)")
}

/// Copies this binary beside the argb-controller binaries and registers the
/// probe service with `target` as its launch arguments.
///
/// # Errors
/// Copy or registration failures (an elevated prompt is required).
pub fn install(target: &Target) -> anyhow::Result<()> {
    let dir = install_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let binary = dir.join(BINARY_NAME);
    let current = std::env::current_exe()?;
    std::fs::copy(&current, &binary)
        .with_context(|| format!("copying {} to {}", current.display(), binary.display()))?;

    let mut launch_arguments = vec![
        OsString::from(RUN_SUBCOMMAND),
        OsString::from("--name"),
        OsString::from(&target.name),
        OsString::from("--seconds"),
        OsString::from(target.seconds.to_string()),
    ];
    if let Some(color) = &target.color {
        launch_arguments.push(OsString::from("--color"));
        launch_arguments.push(OsString::from(color));
    }
    if let Some(layout) = target.layout {
        launch_arguments.push(OsString::from("--layout"));
        launch_arguments.push(OsString::from(
            layout
                .to_possible_value()
                .map_or_else(String::new, |v| v.get_name().to_owned()),
        ));
    }
    if let Some(brightness) = target.brightness {
        launch_arguments.push(OsString::from("--brightness"));
        launch_arguments.push(OsString::from(brightness.to_string()));
    }
    if target.off {
        launch_arguments.push(OsString::from("--off"));
    }
    let info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(SERVICE_DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::OnDemand,
        error_control: ServiceErrorControl::Normal,
        executable_path: binary.clone(),
        launch_arguments,
        dependencies: vec![],
        account_name: Some(OsString::from(SERVICE_ACCOUNT)),
        account_password: None,
    };

    let manager = manager(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
    if let Ok(existing) = manager.open_service(SERVICE_NAME, ServiceAccess::CHANGE_CONFIG) {
        existing
            .change_config(&info)
            .context("updating the service")?;
        info!(binary = %binary.display(), "probe service updated");
    } else {
        manager
            .create_service(&info, ServiceAccess::QUERY_STATUS)
            .context("registering the service")?;
        info!(binary = %binary.display(), "probe service registered");
    }
    println!(
        "Registered {SERVICE_NAME} as {SERVICE_ACCOUNT}. Run `argb-ble-probe service start`, then read {}",
        log_path().display()
    );
    Ok(())
}

/// Starts the probe service once.
///
/// # Errors
/// When the service is not registered or cannot be started.
pub fn start() -> anyhow::Result<()> {
    let manager = manager(ServiceManagerAccess::CONNECT)?;
    let service = manager
        .open_service(SERVICE_NAME, ServiceAccess::START)
        .context("opening the probe service (is it installed?)")?;
    service
        .start::<OsString>(&[])
        .context("starting the probe service")?;
    println!(
        "Started {SERVICE_NAME}; wait for the scan to finish, then read {}",
        log_path().display()
    );
    Ok(())
}

/// Unregisters the probe service.
///
/// # Errors
/// When the service cannot be opened or deleted.
pub fn uninstall() -> anyhow::Result<()> {
    let manager = manager(ServiceManagerAccess::CONNECT)?;
    let service = manager
        .open_service(SERVICE_NAME, ServiceAccess::DELETE)
        .context("opening the probe service")?;
    service.delete().context("deleting the probe service")?;
    println!("Unregistered {SERVICE_NAME}; the binary stays in the install directory.");
    Ok(())
}
