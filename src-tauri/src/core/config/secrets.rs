//! Secret storage backed by the OS keychain, with a file fallback.
//!
//! The PowerShell prototype kept the server password in a plaintext JSON file
//! next to the script and passed it on the command line, where any local
//! process could read it out of the process table. Everything sensitive now
//! lives in Windows Credential Manager, the macOS Keychain, or the Secret
//! Service on Linux, and reaches the server through environment variables.
//!
//! Linux is the reason for the fallback. Secret Service is a daemon
//! (gnome-keyring, kwallet, KeePassXC) and a machine running a minimal window
//! manager may have none, so the packages list it as a *recommendation* rather
//! than a hard dependency. When it is absent the store degrades to a `0600`
//! file in the app's data directory instead of failing — a party should not be
//! impossible to host because there is no keyring daemon.
//!
//! That fallback is deliberately not called encryption. A key kept next to the
//! data it protects is theatre; what actually guards the file is its mode and
//! the fact that it lives under the user's own `$HOME`. It is weaker than the
//! keychain, which is why it is second choice and why `storage_backend` is
//! surfaced in diagnostics.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use keyring::Entry;

use crate::core::error::{Result, SyncPartyError};
use crate::core::paths::AppPaths;

const SERVICE: &str = "syncparty";

/// Identifies a stored secret. Values are the keychain account names, so
/// renaming one orphans the old entry — add a migration instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretKey {
    /// Password clients must supply to reach the Syncplay server.
    ServerPassword,
    /// Salt that keeps room operator passwords valid across restarts.
    ServerSalt,
    /// Discord webhook used to announce that the party is ready.
    DiscordWebhook,
    /// The most recent guest invite, so restarting the app reopens the room.
    LastInvite,
    /// This machine's iroh endpoint key. Its public half is the address an
    /// invite names, so losing it invalidates every code already handed out.
    EndpointKey,
}

impl SecretKey {
    fn account(self) -> &'static str {
        match self {
            Self::ServerPassword => "server-password",
            Self::ServerSalt => "server-salt",
            Self::DiscordWebhook => "discord-webhook",
            Self::LastInvite => "last-invite",
            Self::EndpointKey => "endpoint-key",
        }
    }
}

/// Which store the secrets actually landed in.
///
/// Decided once at startup rather than per call, so a keyring that starts
/// answering halfway through a session cannot leave half the secrets in one
/// place and half in the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageBackend {
    /// Credential Manager, Keychain, or Secret Service.
    Keychain,
    /// A `0600` file under the app's data directory.
    File,
}

impl StorageBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keychain => "keychain",
            Self::File => "file",
        }
    }
}

pub struct SecretStore {
    backend: StorageBackend,
    file: FileStore,
}

impl SecretStore {
    pub fn new(paths: AppPaths) -> Self {
        let file = FileStore::new(paths.secrets_file());
        let backend = if keychain_is_usable() {
            StorageBackend::Keychain
        } else {
            tracing::warn!(
                "no OS keychain available — falling back to {}. Install a Secret \
                 Service provider (gnome-keyring, kwallet, or KeePassXC) to store \
                 secrets in the keyring instead.",
                file.path.display()
            );
            StorageBackend::File
        };

        Self { backend, file }
    }

    pub fn backend(&self) -> StorageBackend {
        self.backend
    }

    pub fn get(&self, key: SecretKey) -> Result<Option<String>> {
        match self.backend {
            StorageBackend::Keychain => match Entry::new(SERVICE, key.account())?.get_password() {
                Ok(value) => Ok(Some(value)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(error) => Err(error.into()),
            },
            StorageBackend::File => self.file.get(key.account()),
        }
    }

    pub fn set(&self, key: SecretKey, value: &str) -> Result<()> {
        match self.backend {
            StorageBackend::Keychain => {
                Entry::new(SERVICE, key.account())?.set_password(value)?;
                Ok(())
            }
            StorageBackend::File => self.file.set(key.account(), value),
        }
    }

    pub fn delete(&self, key: SecretKey) -> Result<()> {
        match self.backend {
            StorageBackend::Keychain => {
                match Entry::new(SERVICE, key.account())?.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                    Err(error) => Err(error.into()),
                }
            }
            StorageBackend::File => self.file.delete(key.account()),
        }
    }

    /// Returns the stored secret, generating and persisting one on first use.
    ///
    /// This is how the server password and salt come into existence: the user
    /// never types them, and the salt in particular must stay stable forever
    /// or every room operator password breaks on the next restart.
    pub fn get_or_create(&self, key: SecretKey, length: usize) -> Result<String> {
        if let Some(existing) = self.get(key)? {
            return Ok(existing);
        }

        let generated = generate_token(length)?;
        self.set(key, &generated)?;
        Ok(generated)
    }
}

/// Probes the platform keychain with a read that is expected to find nothing.
///
/// `NoEntry` is the success case: the backend answered, it simply has no such
/// secret yet. Anything else — no Secret Service on the bus, a locked
/// collection, keyring compiled without a backend for this target — means
/// storing here would not survive, so the file store takes over.
fn keychain_is_usable() -> bool {
    match Entry::new(SERVICE, "probe").map(|entry| entry.get_password()) {
        Ok(Ok(_)) | Ok(Err(keyring::Error::NoEntry)) => true,
        Ok(Err(_)) | Err(_) => false,
    }
}

/// A `0600` JSON file holding the same account/value pairs the keychain would.
///
/// Loaded once and held in memory, because every read happens while a party is
/// being set up and re-reading the file each time buys nothing.
struct FileStore {
    path: PathBuf,
    cache: Mutex<BTreeMap<String, String>>,
}

impl FileStore {
    fn new(path: PathBuf) -> Self {
        let cache = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();

        Self {
            path,
            cache: Mutex::new(cache),
        }
    }

    fn get(&self, account: &str) -> Result<Option<String>> {
        Ok(self.lock()?.get(account).cloned())
    }

    fn set(&self, account: &str, value: &str) -> Result<()> {
        let mut cache = self.lock()?;
        cache.insert(account.to_owned(), value.to_owned());
        self.persist(&cache)
    }

    fn delete(&self, account: &str) -> Result<()> {
        let mut cache = self.lock()?;
        cache.remove(account);
        self.persist(&cache)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, BTreeMap<String, String>>> {
        self.cache
            .lock()
            .map_err(|_| SyncPartyError::Other("the secret store lock was poisoned".to_owned()))
    }

    /// Writes through a temporary file so a crash mid-write cannot leave a
    /// truncated store behind — losing the salt is exactly the failure this
    /// whole module exists to prevent.
    fn persist(&self, cache: &BTreeMap<String, String>) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let temporary = self.path.with_extension("tmp");
        let serialised = serde_json::to_vec_pretty(cache)
            .map_err(|error| SyncPartyError::Other(format!("could not encode secrets: {error}")))?;

        write_private(&temporary, &serialised)?;
        std::fs::rename(&temporary, &self.path)?;

        Ok(())
    }
}

/// Creates the file unreadable by anyone but its owner.
///
/// The mode is set in `OpenOptions` rather than afterwards, so the secrets are
/// never briefly world-readable between creation and `set_permissions`.
#[cfg(unix)]
fn write_private(path: &std::path::Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;

    file.write_all(contents)?;
    file.sync_all()?;

    Ok(())
}

/// Windows and macOS reach the keychain, so this only runs if one of them is
/// somehow without it. `LOCALAPPDATA` is already per-user.
#[cfg(not(unix))]
fn write_private(path: &std::path::Path, contents: &[u8]) -> Result<()> {
    std::fs::write(path, contents)?;
    Ok(())
}

/// Alphabet without look-alike characters, because these strings get read
/// aloud and retyped on movie night.
const TOKEN_ALPHABET: &[u8] = b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// Generates a random token by rejection sampling, which keeps the character
/// distribution uniform. Modulo would quietly bias the low end of the alphabet.
pub fn generate_token(length: usize) -> Result<String> {
    let mut token = String::with_capacity(length);
    let limit = (u8::MAX as usize / TOKEN_ALPHABET.len() * TOKEN_ALPHABET.len()) as u8;

    while token.len() < length {
        let mut buffer = [0_u8; 32];
        getrandom::fill(&mut buffer).map_err(|error| {
            SyncPartyError::Other(format!("no secure randomness available: {error}"))
        })?;

        for byte in buffer {
            if token.len() == length {
                break;
            }
            if byte < limit {
                token.push(TOKEN_ALPHABET[byte as usize % TOKEN_ALPHABET.len()] as char);
            }
        }
    }

    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_have_the_requested_length() {
        assert_eq!(generate_token(18).expect("token").len(), 18);
        assert_eq!(generate_token(1).expect("token").len(), 1);
    }

    #[test]
    fn tokens_use_only_the_unambiguous_alphabet() {
        let token = generate_token(256).expect("token");

        assert!(token.bytes().all(|byte| TOKEN_ALPHABET.contains(&byte)));
    }

    #[test]
    fn tokens_do_not_repeat() {
        assert_ne!(
            generate_token(24).expect("token"),
            generate_token(24).expect("token")
        );
    }

    fn temporary_store(label: &str) -> FileStore {
        let dir = std::env::temp_dir().join(format!("syncparty-secrets-{label}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        FileStore::new(dir.join("secrets.json"))
    }

    /// The whole point of the fallback: a value written now is still there
    /// after a restart. The mock store keyring used before this existed
    /// returned `None` here, which is how the salt kept regenerating.
    #[test]
    fn the_file_store_survives_being_reopened() {
        let store = temporary_store("reopen");
        store.set("server-salt", "a-stable-salt").expect("set");

        let reopened = FileStore::new(store.path.clone());

        assert_eq!(
            reopened.get("server-salt").expect("get"),
            Some("a-stable-salt".to_owned())
        );
    }

    #[test]
    fn the_file_store_reports_nothing_for_an_unknown_key() {
        let store = temporary_store("unknown");

        assert_eq!(store.get("server-password").expect("get"), None);
    }

    #[test]
    fn deleting_from_the_file_store_removes_the_value() {
        let store = temporary_store("delete");
        store.set("discord-webhook", "https://example.invalid").expect("set");
        store.delete("discord-webhook").expect("delete");

        assert_eq!(store.get("discord-webhook").expect("get"), None);
        assert_eq!(
            FileStore::new(store.path.clone())
                .get("discord-webhook")
                .expect("get"),
            None
        );
    }

    /// Secrets in a world-readable file would be worse than the keychain by
    /// more than the doc comment admits.
    #[cfg(unix)]
    #[test]
    fn the_file_store_is_readable_only_by_its_owner() {
        use std::os::unix::fs::PermissionsExt;

        let store = temporary_store("permissions");
        store.set("server-password", "hunter2").expect("set");

        let mode = std::fs::metadata(&store.path)
            .expect("metadata")
            .permissions()
            .mode();

        assert_eq!(mode & 0o777, 0o600, "expected 0600, got {:o}", mode & 0o777);
    }
}
