//! `argb service ...`: install, control and check the Windows service.
//!
//! Installation copies the two binaries into a program directory, prepares
//! the machine-wide configuration directory with an administrators-only
//! ACL, registers the service to run as `LocalService` (it only needs the
//! loopback network and read access to the configuration) and starts it.

use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, bail};
use argb_config::ConfigStore;
use argb_service::SERVICE_BINARY_NAME;

/// Directory name under the program files folder that receives the binaries.
pub const INSTALL_DIR_NAME: &str = "argb-controller";
/// Environment variable Windows uses for the program files folder.
const PROGRAM_FILES_ENV: &str = "ProgramFiles";
/// Fallback when the environment variable is missing.
const PROGRAM_FILES_FALLBACK: &str = r"C:\Program Files";
/// Account the service runs as. Least privilege: loopback network access and
/// read access to the configuration are all it needs.
pub const SERVICE_ACCOUNT: &str = r"NT AUTHORITY\LocalService";
/// Time allowed for the service to reach the requested state.
pub const STATE_CHANGE_TIMEOUT: Duration = Duration::from_secs(20);
/// Delay before the Service Control Manager restarts a crashed service.
pub const RESTART_DELAY: Duration = Duration::from_secs(5);
/// Failures are counted afresh after this long without one.
pub const FAILURE_RESET_PERIOD: Duration = Duration::from_secs(24 * 60 * 60);
/// Time allowed for the OpenRGB SDK port to answer in `check`.
const OPENRGB_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Well-known security identifiers used in the configuration directory ACL,
/// so the rule works on any Windows language.
mod sid {
    /// BUILTIN\Administrators.
    pub const ADMINISTRATORS: &str = "*S-1-5-32-544";
    /// NT AUTHORITY\SYSTEM.
    pub const SYSTEM: &str = "*S-1-5-18";
    /// NT AUTHORITY\LocalService, the service account.
    pub const LOCAL_SERVICE: &str = "*S-1-5-19";
    /// BUILTIN\Users.
    pub const USERS: &str = "*S-1-5-32-545";
}

/// `icacls` arguments that make `dir` writable by administrators and SYSTEM
/// only, readable by the service account and local users, with inheritance
/// cut so parent permissions cannot widen it.
#[must_use]
pub fn acl_arguments(dir: &Path) -> Vec<String> {
    vec![
        dir.display().to_string(),
        "/inheritance:r".into(),
        "/grant:r".into(),
        format!("{}:(OI)(CI)F", sid::ADMINISTRATORS),
        format!("{}:(OI)(CI)F", sid::SYSTEM),
        format!("{}:(OI)(CI)RX", sid::LOCAL_SERVICE),
        format!("{}:(OI)(CI)RX", sid::USERS),
        "/Q".into(),
    ]
}

/// The default program directory, e.g. `C:\Program Files\argb-controller`.
#[must_use]
pub fn default_install_dir() -> PathBuf {
    let base = std::env::var_os(PROGRAM_FILES_ENV)
        .map_or_else(|| PathBuf::from(PROGRAM_FILES_FALLBACK), PathBuf::from);
    base.join(INSTALL_DIR_NAME)
}

/// The service binary next to the running `argb` executable.
///
/// # Errors
/// When the running executable's location is unknown or the service binary
/// is not beside it.
pub fn sibling_service_binary() -> anyhow::Result<PathBuf> {
    let me = std::env::current_exe().context("locating the argb executable")?;
    let dir = me
        .parent()
        .context("the argb executable has no parent directory")?;
    let candidate = dir.join(SERVICE_BINARY_NAME);
    if !candidate.is_file() {
        bail!(
            "{} not found next to {}; build the workspace or pass --service-binary",
            SERVICE_BINARY_NAME,
            me.display()
        );
    }
    Ok(candidate)
}

/// Creates the configuration directory with a starter file when missing and
/// applies the restrictive ACL.
///
/// # Errors
/// I/O or `icacls` failure.
pub fn prepare_config_dir(store: &ConfigStore) -> anyhow::Result<()> {
    if !store.exists() {
        store.save(&argb_config::ConfigFile::starter())?;
        println!("wrote starter configuration to {}", store.path().display());
    }
    let dir = store
        .path()
        .parent()
        .context("configuration path has no directory")?;
    let status = Command::new("icacls")
        .args(acl_arguments(dir))
        .status()
        .context("running icacls")?;
    if !status.success() {
        bail!(
            "icacls failed with {status} while restricting {}",
            dir.display()
        );
    }
    println!(
        "restricted {} to administrators (service account reads)",
        dir.display()
    );
    Ok(())
}

/// Whether the OpenRGB SDK port answers.
#[must_use]
pub fn openrgb_reachable(host: &str, port: u16) -> bool {
    (host, port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut addrs| addrs.next())
        .is_some_and(|addr| TcpStream::connect_timeout(&addr, OPENRGB_PROBE_TIMEOUT).is_ok())
}

/// Prints a health report: service registration and state, OpenRGB
/// reachability, configuration validity. Returns whether everything passed.
pub fn check(store: &ConfigStore) -> bool {
    let mut ok = true;
    let mut report = |good: bool, what: &str, detail: String| {
        println!("{}  {what}: {detail}", if good { "OK  " } else { "WARN" });
        ok &= good;
    };

    match store.load() {
        Ok(config) => {
            report(
                true,
                "configuration",
                format!(
                    "{} ({} device(s))",
                    store.path().display(),
                    config.devices.len()
                ),
            );
            let reachable = openrgb_reachable(&config.openrgb.host, config.openrgb.port);
            report(
                reachable,
                "OpenRGB SDK",
                if reachable {
                    format!(
                        "listening on {}:{}",
                        config.openrgb.host, config.openrgb.port
                    )
                } else {
                    format!(
                        "nothing on {}:{}; start OpenRGB with its SDK server enabled on 127.0.0.1 \
                         (Settings > General: Start at login, Start minimized, Start server)",
                        config.openrgb.host, config.openrgb.port
                    )
                },
            );
        }
        Err(error) => report(false, "configuration", error.to_string()),
    }

    match platform::status_line() {
        Ok((running, detail)) => report(running, "service", detail),
        Err(error) => report(false, "service", error.to_string()),
    }
    ok
}

/// Platform-specific service control.
#[cfg(windows)]
pub mod platform {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use anyhow::{Context, bail};
    use argb_config::ConfigStore;
    use argb_service::{
        CONFIG_FLAG, SERVICE_BINARY_NAME, SERVICE_DESCRIPTION, SERVICE_DISPLAY_NAME, SERVICE_NAME,
    };
    use windows_service::service::{
        ServiceAccess, ServiceAction, ServiceActionType, ServiceErrorControl,
        ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceStartType,
        ServiceState, ServiceType,
    };
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    use super::{
        FAILURE_RESET_PERIOD, RESTART_DELAY, SERVICE_ACCOUNT, STATE_CHANGE_TIMEOUT,
        prepare_config_dir,
    };

    /// Windows error: access denied.
    const ERROR_ACCESS_DENIED: i32 = 5;
    /// Windows error: the specified service does not exist.
    const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
    /// Windows error: the service has not been started.
    const ERROR_SERVICE_NOT_ACTIVE: i32 = 1062;
    /// Windows error: an instance of the service is already running.
    const ERROR_SERVICE_ALREADY_RUNNING: i32 = 1056;
    /// Poll interval while waiting for a state change.
    const POLL_INTERVAL: Duration = Duration::from_millis(250);
    /// Name of the CLI binary copied beside the service.
    const CLI_BINARY_NAME: &str = "argb.exe";

    fn os_error(error: &windows_service::Error) -> Option<i32> {
        match error {
            windows_service::Error::Winapi(io) => io.raw_os_error(),
            _ => None,
        }
    }

    fn manager(access: ServiceManagerAccess) -> anyhow::Result<ServiceManager> {
        ServiceManager::local_computer(None::<&str>, access).map_err(|error| {
            if os_error(&error) == Some(ERROR_ACCESS_DENIED) {
                anyhow::anyhow!("access denied by the Service Control Manager; run this from an elevated (administrator) prompt")
            } else {
                anyhow::anyhow!("opening the Service Control Manager: {error}")
            }
        })
    }

    fn open(
        manager: &ServiceManager,
        access: ServiceAccess,
    ) -> anyhow::Result<Option<windows_service::service::Service>> {
        match manager.open_service(SERVICE_NAME, access) {
            Ok(service) => Ok(Some(service)),
            Err(error) if os_error(&error) == Some(ERROR_SERVICE_DOES_NOT_EXIST) => Ok(None),
            Err(error) if os_error(&error) == Some(ERROR_ACCESS_DENIED) => {
                bail!("access denied opening service {SERVICE_NAME}; run from an elevated prompt")
            }
            Err(error) => Err(anyhow::anyhow!("opening service {SERVICE_NAME}: {error}")),
        }
    }

    fn wait_for(
        service: &windows_service::service::Service,
        wanted: ServiceState,
    ) -> anyhow::Result<()> {
        let deadline = Instant::now() + STATE_CHANGE_TIMEOUT;
        loop {
            let status = service.query_status().context("querying service status")?;
            if status.current_state == wanted {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!(
                    "service did not reach {wanted:?} within {STATE_CHANGE_TIMEOUT:?} (now {:?})",
                    status.current_state
                );
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    fn service_info(binary: &Path, config_path: Option<&Path>) -> ServiceInfo {
        let launch_arguments = config_path
            .map(|path| vec![OsString::from(CONFIG_FLAG), path.as_os_str().to_owned()])
            .unwrap_or_default();
        ServiceInfo {
            name: OsString::from(SERVICE_NAME),
            display_name: OsString::from(SERVICE_DISPLAY_NAME),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: binary.to_path_buf(),
            launch_arguments,
            dependencies: vec![],
            account_name: Some(OsString::from(SERVICE_ACCOUNT)),
            account_password: None,
        }
    }

    /// Copies the binaries to `install_dir`, prepares the configuration
    /// directory, registers (or updates) the service and starts it unless
    /// `start` is false. `config_path` is passed to the service when it is
    /// not the default.
    ///
    /// # Errors
    /// Any step failing; the message says which.
    pub fn install(
        service_binary: &Path,
        install_dir: &Path,
        store: &ConfigStore,
        explicit_config: Option<&Path>,
        start: bool,
    ) -> anyhow::Result<()> {
        let manager =
            manager(ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;

        // Stop a running instance first so the binary can be replaced.
        if let Some(existing) = open(&manager, ServiceAccess::QUERY_STATUS | ServiceAccess::STOP)? {
            let status = existing.query_status().context("querying service status")?;
            if status.current_state != ServiceState::Stopped {
                println!("stopping the running service");
                let _ = existing.stop();
                wait_for(&existing, ServiceState::Stopped)?;
            }
        }

        std::fs::create_dir_all(install_dir)
            .with_context(|| format!("creating {}", install_dir.display()))?;
        let target_service = install_dir.join(SERVICE_BINARY_NAME);
        std::fs::copy(service_binary, &target_service).with_context(|| {
            format!(
                "copying {} to {}",
                service_binary.display(),
                target_service.display()
            )
        })?;
        let me = std::env::current_exe().context("locating the argb executable")?;
        let target_cli = install_dir.join(CLI_BINARY_NAME);
        if me != target_cli {
            std::fs::copy(&me, &target_cli)
                .with_context(|| format!("copying {} to {}", me.display(), target_cli.display()))?;
        }
        println!("installed binaries in {}", install_dir.display());

        prepare_config_dir(store)?;

        let info = service_info(&target_service, explicit_config);
        let access =
            ServiceAccess::QUERY_STATUS | ServiceAccess::START | ServiceAccess::CHANGE_CONFIG;
        let service = if let Some(existing) = open(&manager, access)? {
            existing
                .change_config(&info)
                .context("updating the service registration")?;
            println!("updated service {SERVICE_NAME}");
            existing
        } else {
            let created = manager
                .create_service(&info, access)
                .context("registering the service")?;
            println!("registered service {SERVICE_NAME} ({SERVICE_DISPLAY_NAME})");
            created
        };
        service
            .set_description(SERVICE_DESCRIPTION)
            .context("setting the service description")?;
        service
            .update_failure_actions(ServiceFailureActions {
                reset_period: ServiceFailureResetPeriod::After(FAILURE_RESET_PERIOD),
                reboot_msg: None,
                command: None,
                actions: Some(vec![
                    ServiceAction {
                        action_type: ServiceActionType::Restart,
                        delay: RESTART_DELAY
                    };
                    3
                ]),
            })
            .context("setting the restart policy")?;

        if start {
            service.start::<&str>(&[]).context("starting the service")?;
            wait_for(&service, ServiceState::Running)?;
            println!("service running as {SERVICE_ACCOUNT}");
        }
        Ok(())
    }

    /// Stops and unregisters the service. The binaries and the configuration
    /// stay in place.
    ///
    /// # Errors
    /// When the service cannot be opened, stopped or deleted.
    pub fn uninstall() -> anyhow::Result<()> {
        let manager = manager(ServiceManagerAccess::CONNECT)?;
        let Some(service) = open(
            &manager,
            ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
        )?
        else {
            println!("service {SERVICE_NAME} is not registered");
            return Ok(());
        };
        let status = service.query_status().context("querying service status")?;
        if status.current_state != ServiceState::Stopped {
            let _ = service.stop();
            wait_for(&service, ServiceState::Stopped)?;
        }
        service.delete().context("deleting the service")?;
        println!("service {SERVICE_NAME} removed; binaries and configuration left in place");
        Ok(())
    }

    /// Starts the service and waits until it runs.
    ///
    /// # Errors
    /// When the service is not registered or does not start.
    pub fn start() -> anyhow::Result<()> {
        let manager = manager(ServiceManagerAccess::CONNECT)?;
        let Some(service) = open(&manager, ServiceAccess::QUERY_STATUS | ServiceAccess::START)?
        else {
            bail!("service {SERVICE_NAME} is not registered; run 'argb service install'");
        };
        match service.start::<&str>(&[]) {
            Ok(()) => {}
            Err(error) if os_error(&error) == Some(ERROR_SERVICE_ALREADY_RUNNING) => {}
            Err(error) => return Err(anyhow::anyhow!("starting the service: {error}")),
        }
        wait_for(&service, ServiceState::Running)?;
        println!("service {SERVICE_NAME} running");
        Ok(())
    }

    /// Stops the service and waits until it has stopped.
    ///
    /// # Errors
    /// When the service is not registered or does not stop.
    pub fn stop() -> anyhow::Result<()> {
        let manager = manager(ServiceManagerAccess::CONNECT)?;
        let Some(service) = open(&manager, ServiceAccess::QUERY_STATUS | ServiceAccess::STOP)?
        else {
            bail!("service {SERVICE_NAME} is not registered");
        };
        match service.stop() {
            Ok(_) => {}
            Err(error) if os_error(&error) == Some(ERROR_SERVICE_NOT_ACTIVE) => {}
            Err(error) => return Err(anyhow::anyhow!("stopping the service: {error}")),
        }
        wait_for(&service, ServiceState::Stopped)?;
        println!("service {SERVICE_NAME} stopped");
        Ok(())
    }

    /// One line describing the registration and state, plus whether it is running.
    ///
    /// # Errors
    /// When the Service Control Manager cannot be reached.
    pub fn status_line() -> anyhow::Result<(bool, String)> {
        let manager = manager(ServiceManagerAccess::CONNECT)?;
        let Some(service) = open(
            &manager,
            ServiceAccess::QUERY_STATUS | ServiceAccess::QUERY_CONFIG,
        )?
        else {
            return Ok((
                false,
                format!("{SERVICE_NAME} is not registered; run 'argb service install'"),
            ));
        };
        let status = service.query_status().context("querying service status")?;
        let config = service
            .query_config()
            .context("querying service configuration")?;
        let account = config.account_name.map_or_else(
            || "LocalSystem".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        let binary: PathBuf = config.executable_path;
        Ok((
            status.current_state == ServiceState::Running,
            format!(
                "{SERVICE_NAME} {:?}, start {:?}, account {account}, binary {}",
                status.current_state,
                config.start_type,
                binary.display()
            ),
        ))
    }
}

/// Stubs for other platforms, so the CLI builds everywhere.
#[cfg(not(windows))]
pub mod platform {
    use std::path::Path;

    use anyhow::bail;
    use argb_config::ConfigStore;

    const NOT_WINDOWS: &str = "service management is only available on Windows";

    /// Not available.
    ///
    /// # Errors
    /// Always.
    pub fn install(
        _service_binary: &Path,
        _install_dir: &Path,
        _store: &ConfigStore,
        _explicit_config: Option<&Path>,
        _start: bool,
    ) -> anyhow::Result<()> {
        bail!(NOT_WINDOWS)
    }

    /// Not available.
    ///
    /// # Errors
    /// Always.
    pub fn uninstall() -> anyhow::Result<()> {
        bail!(NOT_WINDOWS)
    }

    /// Not available.
    ///
    /// # Errors
    /// Always.
    pub fn start() -> anyhow::Result<()> {
        bail!(NOT_WINDOWS)
    }

    /// Not available.
    ///
    /// # Errors
    /// Always.
    pub fn stop() -> anyhow::Result<()> {
        bail!(NOT_WINDOWS)
    }

    /// Not available.
    ///
    /// # Errors
    /// Always.
    pub fn status_line() -> anyhow::Result<(bool, String)> {
        bail!(NOT_WINDOWS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acl_cuts_inheritance_and_grants_admins_system_service_and_users() {
        let args = acl_arguments(Path::new(r"C:\ProgramData\argb-controller"));
        assert_eq!(args[0], r"C:\ProgramData\argb-controller");
        assert_eq!(args[1], "/inheritance:r");
        assert_eq!(args[2], "/grant:r");
        assert!(args.contains(&"*S-1-5-32-544:(OI)(CI)F".to_owned()));
        assert!(args.contains(&"*S-1-5-18:(OI)(CI)F".to_owned()));
        assert!(args.contains(&"*S-1-5-19:(OI)(CI)RX".to_owned()));
        assert!(args.contains(&"*S-1-5-32-545:(OI)(CI)RX".to_owned()));
        assert!(!args.iter().any(|a| a.contains("S-1-5-32-545:(OI)(CI)F")));
    }

    #[test]
    fn install_dir_is_under_program_files() {
        assert!(default_install_dir().ends_with(INSTALL_DIR_NAME));
    }

    #[test]
    fn unreachable_port_is_reported_as_such() {
        // Port 1 is reserved and never an SDK server.
        assert!(!openrgb_reachable("127.0.0.1", 1));
    }
}
