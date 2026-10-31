//! Identity of the argb-controller Windows service, shared with the CLI that
//! installs and controls it.

/// Service name registered with the Service Control Manager.
pub const SERVICE_NAME: &str = "argb-controller";
/// Name shown in the Services console.
pub const SERVICE_DISPLAY_NAME: &str = "ARGB Controller";
/// Description shown in the Services console.
pub const SERVICE_DESCRIPTION: &str =
    "Drives RGB lighting through OpenRGB according to the machine's power state.";
/// File name of the service executable.
pub const SERVICE_BINARY_NAME: &str = "argb-service.exe";
/// Flag that runs the service logic in the foreground.
pub const CONSOLE_FLAG: &str = "--console";
/// Flag naming the configuration file; defaults to the machine-wide file.
pub const CONFIG_FLAG: &str = "--config";
