//! Secret storage.
//!
//! Two backends: the OS keychain for the windowed app, and a `0600` JSON file
//! for the headless host, which has no desktop session to unlock one. Nothing
//! ever reaches a process through `argv`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::core::error::{Result, SyncPartyError};

#[cfg(feature = "desktop")]
const SERVICE: &str = "syncparty";

/// Identifies a stored secret. Values are the keychain account names — and the
/// JSON keys in the file backend — so renaming one orphans the old entry; add
/// a migration instead.
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
}

impl SecretKey {
    fn account(self) -> &'static str {
        match self {
            Self::ServerPassword => "server-password",
            Self::ServerSalt => "server-salt",
            Self::DiscordWebhook => "discord-webhook",
            Self::LastInvite => "last-invite",
        }
    }
}

trait SecretBackend: Send + Sync {
    fn get(&self, key: SecretKey) -> Result<Option<String>>;
    fn set(&self, key: SecretKey, value: &str) -> Result<()>;
    fn delete(&self, key: SecretKey) -> Result<()>;
}

pub struct SecretStore {
    backend: Box<dyn SecretBackend>,
}

impl SecretStore {
    #[cfg(feature = "desktop")]
    pub fn new() -> Self {
        Self::keychain()
    }

    #[cfg(feature = "desktop")]
    pub fn keychain() -> Self {
        Self {
            backend: Box::new(KeychainBackend),
        }
    }

    /// A JSON file, for hosts with no keychain to reach.
    ///
    /// The path must be on persistent storage. Losing it regenerates the
    /// password — invalidating every invite already shared — and the salt,
    /// which silently breaks every room operator password derived from it.
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self {
            backend: Box::new(FileBackend::new(path)),
        }
    }

    pub fn get(&self, key: SecretKey) -> Result<Option<String>> {
        self.backend.get(key)
    }

    pub fn set(&self, key: SecretKey, value: &str) -> Result<()> {
        self.backend.set(key, value)
    }

    pub fn delete(&self, key: SecretKey) -> Result<()> {
        self.backend.delete(key)
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

#[cfg(feature = "desktop")]
impl Default for SecretStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "desktop")]
struct KeychainBackend;

#[cfg(feature = "desktop")]
impl SecretBackend for KeychainBackend {
    fn get(&self, key: SecretKey) -> Result<Option<String>> {
        match keyring::Entry::new(SERVICE, key.account())?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn set(&self, key: SecretKey, value: &str) -> Result<()> {
        keyring::Entry::new(SERVICE, key.account())?.set_password(value)?;
        Ok(())
    }

    fn delete(&self, key: SecretKey) -> Result<()> {
        match keyring::Entry::new(SERVICE, key.account())?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

/// Secrets as a flat JSON object on disk.
///
/// Re-read on every access rather than cached, so an operator who edits the
/// file by hand does not have to restart the daemon for it to be noticed.
struct FileBackend {
    path: PathBuf,
    /// Serialises the read-modify-write below; racing writes would otherwise
    /// let one of them lose its key entirely.
    write_lock: Mutex<()>,
}

impl FileBackend {
    fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            write_lock: Mutex::new(()),
        }
    }

    fn read(&self) -> Result<BTreeMap<String, String>> {
        match std::fs::read_to_string(&self.path) {
            Ok(raw) => serde_json::from_str(&raw).map_err(|error| {
                SyncPartyError::Secret(format!(
                    "{} could not be parsed: {error}",
                    self.path.display()
                ))
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(SyncPartyError::Secret(error.to_string())),
        }
    }

    fn mutate(&self, change: impl FnOnce(&mut BTreeMap<String, String>)) -> Result<()> {
        let _guard = self.write_lock.lock().expect("secret mutex poisoned");

        let mut secrets = self.read()?;
        change(&mut secrets);
        write_private(&self.path, &serde_json::to_vec_pretty(&secrets)?)
    }
}

impl SecretBackend for FileBackend {
    fn get(&self, key: SecretKey) -> Result<Option<String>> {
        Ok(self.read()?.get(key.account()).cloned())
    }

    fn set(&self, key: SecretKey, value: &str) -> Result<()> {
        self.mutate(|secrets| {
            secrets.insert(key.account().to_owned(), value.to_owned());
        })
    }

    fn delete(&self, key: SecretKey) -> Result<()> {
        self.mutate(|secrets| {
            secrets.remove(key.account());
        })
    }
}

/// Writes `contents` to `path` readable only by its owner.
///
/// The permissions go on the staging file *before* the rename; creating the
/// target and chmod'ing it after would leave a window where the secrets were
/// world-readable.
fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let staging = path.with_extension("json.tmp");
    std::fs::write(&staging, contents)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o600))?;
    }

    std::fs::rename(&staging, path)?;
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

    fn temp_store(label: &str) -> (SecretStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!("syncparty-secrets-{label}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        let path = dir.join("secrets.json");
        (SecretStore::file(&path), path)
    }

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

    #[test]
    fn a_missing_file_reads_as_no_secrets_rather_than_an_error() {
        let (store, _) = temp_store("missing");

        assert_eq!(store.get(SecretKey::ServerPassword).expect("get"), None);
    }

    #[test]
    fn secrets_survive_a_restart() {
        let (store, path) = temp_store("roundtrip");
        store.set(SecretKey::ServerSalt, "PEPPER").expect("set");

        let reopened = SecretStore::file(&path);
        assert_eq!(
            reopened.get(SecretKey::ServerSalt).expect("get"),
            Some("PEPPER".to_owned())
        );
    }

    #[test]
    fn the_salt_is_generated_once_and_then_kept() {
        let (store, _) = temp_store("stable-salt");

        let first = store
            .get_or_create(SecretKey::ServerSalt, 10)
            .expect("create");
        let second = store
            .get_or_create(SecretKey::ServerSalt, 10)
            .expect("reuse");

        assert_eq!(
            first, second,
            "a new salt on every start silently invalidates room operator passwords"
        );
    }

    #[test]
    fn keys_do_not_clobber_each_other() {
        let (store, _) = temp_store("independent");

        store.set(SecretKey::ServerPassword, "swordfish").expect("a");
        store.set(SecretKey::ServerSalt, "PEPPER").expect("b");

        assert_eq!(
            store.get(SecretKey::ServerPassword).expect("get"),
            Some("swordfish".to_owned())
        );
        assert_eq!(
            store.get(SecretKey::ServerSalt).expect("get"),
            Some("PEPPER".to_owned())
        );
    }

    #[test]
    fn deleting_something_that_was_never_set_is_not_an_error() {
        let (store, _) = temp_store("delete-missing");

        assert!(store.delete(SecretKey::LastInvite).is_ok());
    }

    #[test]
    fn deleting_removes_only_the_named_secret() {
        let (store, _) = temp_store("delete-one");
        store.set(SecretKey::ServerPassword, "swordfish").expect("a");
        store.set(SecretKey::LastInvite, "{}").expect("b");

        store.delete(SecretKey::LastInvite).expect("delete");

        assert_eq!(store.get(SecretKey::LastInvite).expect("get"), None);
        assert_eq!(
            store.get(SecretKey::ServerPassword).expect("get"),
            Some("swordfish".to_owned())
        );
    }

    #[test]
    fn a_corrupt_file_is_reported_rather_than_silently_regenerated() {
        let (store, path) = temp_store("corrupt");
        std::fs::write(&path, b"{ not json").expect("seed");

        let error = store.get(SecretKey::ServerSalt).expect_err("should refuse");
        assert_eq!(
            error.kind(),
            "secret",
            "starting over here would mint a new salt and break every room \
             operator password"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_not_readable_by_anyone_else() {
        use std::os::unix::fs::PermissionsExt;

        let (store, path) = temp_store("permissions");
        store.set(SecretKey::ServerPassword, "swordfish").expect("set");

        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o077, 0, "group and other must have no access");
    }
}
