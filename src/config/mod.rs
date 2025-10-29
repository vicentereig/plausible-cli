use std::path::{Path, PathBuf};

pub mod accounts;

/// Namespace for resolving configuration directories and filenames.
#[derive(Debug, Clone)]
pub struct ConfigPaths {
    base_dir: PathBuf,
}

impl ConfigPaths {
    /// Create configuration paths using the user's platform conventions.
    pub fn with_project_dirs() -> Result<Self, ConfigError> {
        let project_dirs = directories::ProjectDirs::from("io", "plausible", "plausible-cli")
            .ok_or(ConfigError::UnsupportedPlatform)?;
        Ok(Self {
            base_dir: project_dirs.config_dir().to_path_buf(),
        })
    }

    /// Construct from a custom base directory (primarily for testing).
    pub fn from_base_dir(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    /// Location of the persisted configuration file.
    pub fn config_file(&self) -> PathBuf {
        self.base_dir.join("config.toml")
    }

    /// Directory that stores rate-limit usage counters.
    pub fn usage_dir(&self) -> PathBuf {
        self.base_dir.join("usage")
    }

    /// Directory that stores account metadata.
    pub fn accounts_dir(&self) -> PathBuf {
        self.base_dir.join("accounts")
    }

    /// File path storing account metadata index.
    pub fn accounts_file(&self) -> PathBuf {
        self.base_dir.join("accounts.json")
    }

    /// Ensure the configuration root exists on disk.
    pub fn ensure_exists(&self) -> Result<(), ConfigError> {
        let base = self.base_dir.clone();
        std::fs::create_dir_all(&base).map_err(|source| ConfigError::Io {
            path: base.clone(),
            source,
        })?;

        let accounts_dir = self.accounts_dir();
        std::fs::create_dir_all(&accounts_dir).map_err(|source| ConfigError::Io {
            path: accounts_dir.clone(),
            source,
        })?;

        let usage_dir = self.usage_dir();
        std::fs::create_dir_all(&usage_dir).map_err(|source| ConfigError::Io {
            path: usage_dir.clone(),
            source,
        })?;
        Ok(())
    }

    /// Retrieve the base directory path.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ConfigError {
    #[error("failed to determine configuration directory for this platform")]
    UnsupportedPlatform,
    #[error("I/O error interacting with {path:?}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn config_paths_resolve_expected_files() {
        let tmp = tempfile::tempdir().expect("temporary dir");
        let paths = ConfigPaths::from_base_dir(tmp.path());

        assert_eq!(paths.config_file(), tmp.path().join("config.toml"));
        assert_eq!(paths.usage_dir(), tmp.path().join("usage"));
        assert_eq!(paths.accounts_dir(), tmp.path().join("accounts"));
        assert_eq!(paths.accounts_file(), tmp.path().join("accounts.json"));
    }

    #[test]
    fn ensure_exists_creates_directory_tree() {
        let tmp = tempfile::tempdir().expect("temporary dir");
        let base = tmp.path().join("nested").join("plausible");
        let paths = ConfigPaths::from_base_dir(&base);

        paths.ensure_exists().expect("create dirs");
        assert!(fs::metadata(paths.accounts_dir())
            .expect("metadata")
            .is_dir());
        assert!(fs::metadata(paths.usage_dir()).expect("metadata").is_dir());
        assert!(fs::metadata(&base).expect("metadata").is_dir());
    }
}
