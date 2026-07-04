//! Device credential storage (PRD-06 §3). The refresh secret is minted once at
//! pairing and must survive restarts; the OS keychain is the real home for it.
//!
//! A `SecretStore` trait abstracts the backend so headless boxes and CI (which
//! have no unlocked keychain) can fall back to a `0600` file under the config
//! dir. Backend is chosen by `SAVR_SECRET_BACKEND` = `keyring` | `file`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use savr_core::{AccountId, DeviceId};

/// The long-lived credential a device holds after pairing (PRD-06 §2–3). The
/// access JWT is short-lived and kept only in memory — never persisted here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Credentials {
    pub device_id: DeviceId,
    pub account_id: AccountId,
    /// Returned exactly once at pairing; exchanged for access tokens.
    pub refresh_secret: String,
}

/// Persistent store for the device credential.
pub trait SecretStore: Send + Sync {
    fn load(&self) -> anyhow::Result<Option<Credentials>>;
    fn store(&self, creds: &Credentials) -> anyhow::Result<()>;
    fn clear(&self) -> anyhow::Result<()>;
}

const SERVICE: &str = "savr-daemon";
const ACCOUNT: &str = "device-credential";

/// OS-keychain backend (macOS Keychain / Windows Credential Manager / Linux
/// Secret Service), via the `keyring` crate. Stores the whole credential as one
/// JSON secret under a fixed (service, account) pair.
pub struct KeyringStore;

impl SecretStore for KeyringStore {
    fn load(&self) -> anyhow::Result<Option<Credentials>> {
        let entry = keyring::Entry::new(SERVICE, ACCOUNT)?;
        match entry.get_password() {
            Ok(json) => Ok(Some(serde_json::from_str(&json)?)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn store(&self, creds: &Credentials) -> anyhow::Result<()> {
        let entry = keyring::Entry::new(SERVICE, ACCOUNT)?;
        entry.set_password(&serde_json::to_string(creds)?)?;
        Ok(())
    }

    fn clear(&self) -> anyhow::Result<()> {
        let entry = keyring::Entry::new(SERVICE, ACCOUNT)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// Plaintext-file fallback for headless / CI use.
///
/// ponytail: this writes the refresh secret in the clear under the config dir
/// with `0600` perms — acceptable only where no keychain exists (a locked-down
/// service box, a CI runner). The keychain backend is the real answer (PRD-06
/// §3 "Never plaintext on disk"); this is the documented escape hatch, gated
/// behind `SAVR_SECRET_BACKEND=file` so it is never chosen silently.
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl SecretStore for FileStore {
    fn load(&self) -> anyhow::Result<Option<Credentials>> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => Ok(Some(serde_json::from_str(&text)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn store(&self, creds: &Credentials) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(creds)?;
        std::fs::write(&self.path, json)?;
        restrict_perms(&self.path)?;
        Ok(())
    }

    fn clear(&self) -> anyhow::Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(unix)]
fn restrict_perms(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_perms(_path: &Path) -> anyhow::Result<()> {
    // ponytail: Windows ACL tightening not wired (the keychain backend is the
    // expected path there). Leave the file at the user's default umask.
    Ok(())
}

/// Pick the backend from `SAVR_SECRET_BACKEND` (default: keyring). `config_dir`
/// is where the file backend lives.
pub fn from_env(config_dir: &Path) -> Box<dyn SecretStore> {
    match std::env::var("SAVR_SECRET_BACKEND").as_deref() {
        Ok("file") => Box::new(FileStore::new(config_dir.join("credentials.json"))),
        _ => Box::new(KeyringStore),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn file_store_roundtrip_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path().join("credentials.json"));
        assert!(store.load().unwrap().is_none());

        let creds = Credentials {
            device_id: Uuid::now_v7(),
            account_id: Uuid::now_v7(),
            refresh_secret: "s3cr3t".into(),
        };
        store.store(&creds).unwrap();
        assert_eq!(store.load().unwrap().unwrap(), creds);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("credentials.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "secret file must be owner-only");
        }

        store.clear().unwrap();
        assert!(store.load().unwrap().is_none());
    }
}
