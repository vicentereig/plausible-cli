use super::{ConfigError, ConfigPaths};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Top-level account store coordinating metadata and secrets.
#[derive(Debug, Clone)]
pub struct AccountStore {
    paths: ConfigPaths,
}

impl AccountStore {
    /// Initialise an account store for the provided configuration paths.
    pub fn new(paths: ConfigPaths) -> Result<Self, AccountStoreError> {
        paths.ensure_exists()?;
        Ok(Self { paths })
    }

    /// Add or update an account entry and store its API key.
    pub fn add_account(
        &self,
        alias: &str,
        api_key: &str,
        profile: AccountProfile,
    ) -> Result<(), AccountStoreError> {
        validate_alias(alias)?;
        if api_key.trim().is_empty() {
            return Err(AccountStoreError::EmptyApiKey);
        }
        let mut state = self.read_state()?;
        if state.accounts.contains_key(alias) {
            return Err(AccountStoreError::DuplicateAlias(alias.to_string()));
        }
        state.accounts.insert(alias.to_string(), profile);
        if state.default_account.is_none() {
            state.default_account = Some(alias.to_string());
        }
        self.write_state(&state)?;
        self.write_secret(alias, api_key)?;
        Ok(())
    }

    /// Retrieve metadata and API key for a given alias.
    pub fn get_account(&self, alias: &str) -> Result<AccountRecord, AccountStoreError> {
        let state = self.read_state()?;
        let profile = state
            .accounts
            .get(alias)
            .cloned()
            .ok_or_else(|| AccountStoreError::AccountNotFound(alias.to_string()))?;
        let api_key = self.read_secret(alias)?;
        Ok(AccountRecord {
            alias: alias.to_string(),
            api_key,
            profile,
            is_default: state
                .default_account
                .as_ref()
                .map(|default| default == alias)
                .unwrap_or(false),
        })
    }

    /// Enumerate stored accounts.
    pub fn list_accounts(&self) -> Result<Vec<AccountSummary>, AccountStoreError> {
        let state = self.read_state()?;
        let mut summaries = Vec::with_capacity(state.accounts.len());
        for (alias, profile) in state.accounts.iter() {
            summaries.push(AccountSummary {
                alias: alias.clone(),
                profile: profile.clone(),
                is_default: state
                    .default_account
                    .as_ref()
                    .map(|default| default == alias)
                    .unwrap_or(false),
            });
        }
        Ok(summaries)
    }

    /// Retrieve the configured default account alias, if any.
    pub fn default_alias(&self) -> Result<Option<String>, AccountStoreError> {
        let state = self.read_state()?;
        Ok(state.default_account)
    }

    /// Update default account alias.
    pub fn set_default(&self, alias: &str) -> Result<(), AccountStoreError> {
        let mut state = self.read_state()?;
        if !state.accounts.contains_key(alias) {
            return Err(AccountStoreError::AccountNotFound(alias.to_string()));
        }
        state.default_account = Some(alias.to_string());
        self.write_state(&state)?;
        Ok(())
    }

    /// Remove an account and associated secret.
    pub fn remove_account(&self, alias: &str) -> Result<(), AccountStoreError> {
        let mut state = self.read_state()?;
        if state.accounts.remove(alias).is_none() {
            return Err(AccountStoreError::AccountNotFound(alias.to_string()));
        }
        if state.default_account.as_deref() == Some(alias) {
            state.default_account = state.accounts.keys().next().cloned();
        }
        self.write_state(&state)?;
        self.delete_secret(alias)?;
        Ok(())
    }

    /// Export account metadata without secrets.
    pub fn export_accounts(&self) -> Result<Vec<AccountExport>, AccountStoreError> {
        Ok(self
            .list_accounts()?
            .into_iter()
            .map(|summary| AccountExport {
                alias: summary.alias,
                is_default: summary.is_default,
                label: summary.profile.label,
                email: summary.profile.email,
                description: summary.profile.description,
            })
            .collect())
    }

    fn read_state(&self) -> Result<AccountsState, AccountStoreError> {
        let path = self.paths.accounts_file();
        match fs::File::open(&path) {
            Ok(mut file) => {
                let mut contents = String::new();
                file.read_to_string(&mut contents)
                    .map_err(|source| AccountStoreError::Io {
                        path: path.clone(),
                        source,
                    })?;
                if contents.trim().is_empty() {
                    return Ok(AccountsState::default());
                }
                serde_json::from_str(&contents).map_err(AccountStoreError::Deserialize)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(AccountsState::default()),
            Err(source) => Err(AccountStoreError::Io { path, source }),
        }
    }

    fn write_state(&self, state: &AccountsState) -> Result<(), AccountStoreError> {
        let path = self.paths.accounts_file();
        let contents = serde_json::to_string_pretty(state).map_err(AccountStoreError::Serialize)?;
        fs::write(&path, contents).map_err(|source| AccountStoreError::Io { path, source })
    }

    fn write_secret(&self, alias: &str, api_key: &str) -> Result<(), AccountStoreError> {
        let path = self.secret_path(alias);
        let mut file = fs::File::create(&path).map_err(|source| AccountStoreError::Io {
            path: path.clone(),
            source,
        })?;
        set_restricted_permissions(&path, &mut file)?;
        file.write_all(api_key.as_bytes())
            .map_err(|source| AccountStoreError::Io {
                path: path.clone(),
                source,
            })
    }

    fn read_secret(&self, alias: &str) -> Result<String, AccountStoreError> {
        let path = self.secret_path(alias);
        let contents = fs::read_to_string(&path).map_err(|source| AccountStoreError::Io {
            path: path.clone(),
            source,
        })?;
        Ok(contents.trim().to_string())
    }

    fn delete_secret(&self, alias: &str) -> Result<(), AccountStoreError> {
        let path = self.secret_path(alias);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(AccountStoreError::Io { path, source }),
        }
    }

    fn secret_path(&self, alias: &str) -> PathBuf {
        self.paths.accounts_dir().join(format!("{alias}.key"))
    }
}

fn validate_alias(alias: &str) -> Result<(), AccountStoreError> {
    if alias.trim().is_empty() {
        return Err(AccountStoreError::InvalidAlias(
            "alias cannot be empty".to_string(),
        ));
    }
    if alias.contains('/') || alias.contains('\\') {
        return Err(AccountStoreError::InvalidAlias(
            "alias may not contain path separators".to_string(),
        ));
    }
    if alias.contains("..") {
        return Err(AccountStoreError::InvalidAlias(
            "alias may not contain parent directory segments".to_string(),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn set_restricted_permissions(path: &Path, file: &mut fs::File) -> Result<(), AccountStoreError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = file
        .metadata()
        .map_err(|source| AccountStoreError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .permissions();
    perms.set_mode(0o600);
    fs::set_permissions(path, perms).map_err(|source| AccountStoreError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn set_restricted_permissions(_path: &Path, _file: &mut fs::File) -> Result<(), AccountStoreError> {
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AccountProfile {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// Metadata exposed when listing accounts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSummary {
    pub alias: String,
    pub profile: AccountProfile,
    pub is_default: bool,
}

/// Exported account metadata for sharing without secrets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountExport {
    pub alias: String,
    pub is_default: bool,
    pub label: Option<String>,
    pub email: Option<String>,
    pub description: Option<String>,
}

/// Full account record with secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRecord {
    pub alias: String,
    pub api_key: String,
    pub profile: AccountProfile,
    pub is_default: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AccountsState {
    #[serde(default)]
    default_account: Option<String>,
    #[serde(default)]
    accounts: BTreeMap<String, AccountProfile>,
}

#[derive(thiserror::Error, Debug)]
pub enum AccountStoreError {
    #[error("configuration error: {0}")]
    Config(#[from] ConfigError),
    #[error("I/O error at {path:?}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse accounts file: {0}")]
    Deserialize(serde_json::Error),
    #[error("failed to serialise accounts file: {0}")]
    Serialize(serde_json::Error),
    #[error("account alias already exists: {0}")]
    DuplicateAlias(String),
    #[error("account alias is invalid: {0}")]
    InvalidAlias(String),
    #[error("account not found: {0}")]
    AccountNotFound(String),
    #[error("API key cannot be empty")]
    EmptyApiKey,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn store_for_test() -> (AccountStore, tempfile::TempDir) {
        let tmp = tempdir().expect("tmpdir");
        let paths = ConfigPaths::from_base_dir(tmp.path());
        let store = AccountStore::new(paths).expect("store");
        (store, tmp)
    }

    #[test]
    fn add_account_persists_metadata_and_secret() {
        let (store, _tmp) = store_for_test();
        store
            .add_account(
                "prod",
                "test-key",
                AccountProfile {
                    label: Some("Production".into()),
                    email: Some("ops@example.com".into()),
                    description: Some("Primary account".into()),
                },
            )
            .expect("add account");

        let accounts = store.list_accounts().expect("list");
        assert_eq!(accounts.len(), 1);
        let summary = &accounts[0];
        assert_eq!(summary.alias, "prod");
        assert!(summary.is_default);
        assert_eq!(summary.profile.label.as_deref(), Some("Production"));

        let record = store.get_account("prod").expect("get account");
        assert_eq!(record.api_key, "test-key");
    }

    #[test]
    fn cannot_add_duplicate_alias() {
        let (store, _tmp) = store_for_test();
        store
            .add_account("dup", "k1", AccountProfile::default())
            .expect("add first");
        let err = store
            .add_account("dup", "k2", AccountProfile::default())
            .expect_err("duplicate");
        assert!(matches!(
            err,
            AccountStoreError::DuplicateAlias(alias) if alias == "dup"
        ));
    }

    #[test]
    fn set_default_updates_state() {
        let (store, _tmp) = store_for_test();
        store
            .add_account("a", "ka", AccountProfile::default())
            .expect("add a");
        store
            .add_account("b", "kb", AccountProfile::default())
            .expect("add b");

        store.set_default("b").expect("set default");
        let accounts = store.list_accounts().expect("list");
        let defaults: Vec<_> = accounts
            .into_iter()
            .filter(|summary| summary.is_default)
            .collect();
        assert_eq!(defaults.len(), 1);
        assert_eq!(defaults[0].alias, "b");
    }

    #[test]
    fn remove_account_drops_secret_and_selects_new_default() {
        let (store, _tmp) = store_for_test();
        store
            .add_account("first", "k1", AccountProfile::default())
            .expect("add first");
        store
            .add_account("second", "k2", AccountProfile::default())
            .expect("add second");

        store.remove_account("first").expect("remove account");
        let accounts = store.list_accounts().expect("list");
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].alias, "second");
        assert!(accounts[0].is_default);
        assert!(store.get_account("first").is_err());
    }

    #[test]
    fn alias_validation_rejects_path_segments() {
        assert!(validate_alias("valid-alias").is_ok());
        assert!(validate_alias("nested/alias").is_err());
        assert!(validate_alias("..").is_err());
        assert!(validate_alias("").is_err());
    }
}
