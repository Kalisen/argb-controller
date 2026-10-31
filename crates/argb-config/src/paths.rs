//! Well-known file locations.

use std::path::PathBuf;

/// Directory name under the machine-wide program data folder.
pub const SERVICE_DIR_NAME: &str = "argb-controller";
/// Configuration file name.
pub const CONFIG_FILE_NAME: &str = "config.toml";
/// Sidecar file used for the advisory lock around configuration writes.
pub const LOCK_FILE_NAME: &str = "config.toml.lock";
/// Environment variable Windows uses for the machine-wide data folder.
const PROGRAM_DATA_ENV: &str = "ProgramData";
/// Fallback when the environment variable is missing (e.g. a stripped service environment).
const PROGRAM_DATA_FALLBACK: &str = r"C:\ProgramData";

/// The machine-wide configuration directory, e.g. `C:\ProgramData\argb-controller`.
#[must_use]
pub fn default_config_dir() -> PathBuf {
    let base = std::env::var_os(PROGRAM_DATA_ENV)
        .map_or_else(|| PathBuf::from(PROGRAM_DATA_FALLBACK), PathBuf::from);
    base.join(SERVICE_DIR_NAME)
}

/// The machine-wide configuration file path.
#[must_use]
pub fn default_config_path() -> PathBuf {
    default_config_dir().join(CONFIG_FILE_NAME)
}
