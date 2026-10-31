//! Locked, atomic persistence of the configuration file.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use fd_lock::RwLock;
use thiserror::Error;

use crate::paths::LOCK_FILE_NAME;
use crate::schema::ConfigFile;

/// Errors from reading, writing or validating configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// A file system operation failed.
    #[error("{action} {path}: {source}")]
    Io {
        /// What was being attempted.
        action: &'static str,
        /// File involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The TOML could not be parsed into the schema.
    #[error("invalid configuration: {0}")]
    Parse(#[from] toml::de::Error),
    /// The schema could not be serialized to TOML.
    #[error("could not serialize configuration: {0}")]
    Serialize(#[from] toml::ser::Error),
    /// The document declares a schema version this build does not understand.
    #[error(
        "configuration schema version {found} is not supported (this build supports {supported})"
    )]
    UnsupportedSchemaVersion {
        /// Version found in the file.
        found: u32,
        /// Version this build supports.
        supported: u32,
    },
    /// A semantic rule was violated.
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

/// Reads and writes one configuration file safely.
///
/// Writers take an exclusive advisory lock on a sidecar file and replace the
/// configuration atomically (write to a temporary file in the same directory,
/// then rename). Readers never see partial content.
#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    /// A store for the file at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The configuration file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the configuration file exists.
    #[must_use]
    pub fn exists(&self) -> bool {
        self.path.is_file()
    }

    /// Reads and validates the file.
    ///
    /// # Errors
    /// I/O errors, parse errors and validation errors as [`ConfigError`].
    pub fn load(&self) -> Result<ConfigFile, ConfigError> {
        let text = fs::read_to_string(&self.path).map_err(|source| ConfigError::Io {
            action: "reading",
            path: self.path.clone(),
            source,
        })?;
        ConfigFile::parse(&text)
    }

    /// Validates and writes `config`, replacing the file atomically.
    ///
    /// # Errors
    /// Validation errors and I/O errors as [`ConfigError`].
    pub fn save(&self, config: &ConfigFile) -> Result<(), ConfigError> {
        let mut lock = self.lock_file()?;
        let _guard = lock.write().map_err(|source| self.io("locking", source))?;
        self.write_unlocked(config)
    }

    /// Loads the file, lets `edit` change it, then saves it, all under one lock.
    ///
    /// Use this for every CLI mutation so concurrent edits serialize instead
    /// of clobbering each other.
    ///
    /// # Errors
    /// Errors from loading, from `edit`, or from saving.
    pub fn update<F>(&self, edit: F) -> Result<ConfigFile, ConfigError>
    where
        F: FnOnce(&mut ConfigFile) -> Result<(), ConfigError>,
    {
        let mut lock = self.lock_file()?;
        let _guard = lock.write().map_err(|source| self.io("locking", source))?;
        let mut config = self.load()?;
        edit(&mut config)?;
        self.write_unlocked(&config)?;
        Ok(config)
    }

    fn write_unlocked(&self, config: &ConfigFile) -> Result<(), ConfigError> {
        config.validate()?;
        let text = config.to_toml()?;
        let dir = self.parent_dir();
        fs::create_dir_all(dir).map_err(|source| ConfigError::Io {
            action: "creating directory",
            path: dir.to_path_buf(),
            source,
        })?;
        let mut temp = tempfile::Builder::new()
            .prefix(".config-")
            .suffix(".tmp")
            .tempfile_in(dir)
            .map_err(|source| self.io("creating temporary file for", source))?;
        temp.write_all(text.as_bytes())
            .map_err(|source| self.io("writing", source))?;
        temp.as_file()
            .sync_all()
            .map_err(|source| self.io("flushing", source))?;
        temp.persist(&self.path)
            .map_err(|error| self.io("replacing", error.error))?;
        Ok(())
    }

    fn lock_file(&self) -> Result<RwLock<File>, ConfigError> {
        let dir = self.parent_dir();
        fs::create_dir_all(dir).map_err(|source| ConfigError::Io {
            action: "creating directory",
            path: dir.to_path_buf(),
            source,
        })?;
        let lock_path = dir.join(LOCK_FILE_NAME);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .read(true)
            .open(&lock_path)
            .map_err(|source| ConfigError::Io {
                action: "opening",
                path: lock_path,
                source,
            })?;
        Ok(RwLock::new(file))
    }

    fn parent_dir(&self) -> &Path {
        self.path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
    }

    fn io(&self, action: &'static str, source: std::io::Error) -> ConfigError {
        ConfigError::Io {
            action,
            path: self.path.clone(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::DeviceSchema;
    use argb_domain::{Backend, Brightness, DeviceAlias, LightingMode, PowerState, Rgb};

    #[test]
    fn save_then_load_round_trips_and_creates_directories() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(dir.path().join("nested").join("config.toml"));
        assert!(!store.exists());
        store.save(&ConfigFile::starter()).unwrap();
        assert!(store.exists());
        assert_eq!(store.load().unwrap(), ConfigFile::starter());
    }

    #[test]
    fn update_applies_edit_under_lock() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(dir.path().join("config.toml"));
        store.save(&ConfigFile::starter()).unwrap();
        let updated = store
            .update(|config| {
                config.devices.insert(
                    DeviceAlias::new("ram").unwrap(),
                    DeviceSchema {
                        backend: Backend::OpenRgb,
                        match_name: "Vengeance".into(),
                        states: [(
                            PowerState::Active,
                            LightingMode::Static {
                                color: Rgb::WHITE,
                                brightness: Brightness::MAX,
                            },
                        )]
                        .into_iter()
                        .collect(),
                    },
                );
                Ok(())
            })
            .unwrap();
        assert_eq!(store.load().unwrap(), updated);
        assert_eq!(updated.devices.len(), 1);
    }

    #[test]
    fn invalid_edit_is_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(dir.path().join("config.toml"));
        store.save(&ConfigFile::starter()).unwrap();
        let result = store.update(|config| {
            config.schema_version = 99;
            Ok(())
        });
        assert!(matches!(
            result,
            Err(ConfigError::UnsupportedSchemaVersion { .. })
        ));
        assert_eq!(store.load().unwrap(), ConfigFile::starter());
    }

    #[test]
    fn missing_file_reports_io_error_with_path() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(dir.path().join("absent.toml"));
        match store.load() {
            Err(ConfigError::Io {
                action: "reading",
                path,
                ..
            }) => assert_eq!(path, store.path()),
            other => panic!("unexpected result: {other:?}"),
        }
    }
}
