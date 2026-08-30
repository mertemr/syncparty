//! Secret storage backed by the OS keychain.
//!
//! The PowerShell prototype kept the server password in a plaintext JSON file
//! next to the script and passed it on the command line, where any local
//! process could read it out of the process table. Everything sensitive now
//! lives in Windows Credential Manager or the macOS Keychain, and reaches the
//! server through environment variables.

use keyring::Entry;

use crate::core::error::{Result, SyncPartyError};

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
    /// TMDB API key for movie search/discovery. Entered per-user in Settings,
    /// same as the Discord webhook — never baked into the build.
    TmdbApiKey,
}

impl SecretKey {
    fn account(self) -> &'static str {
        match self {
            Self::ServerPassword => "server-password",
            Self::ServerSalt => "server-salt",
            Self::DiscordWebhook => "discord-webhook",
            Self::LastInvite => "last-invite",
            Self::EndpointKey => "endpoint-key",
            Self::TmdbApiKey => "tmdb-api-key",
        }
    }
}

pub struct SecretStore;

impl SecretStore {
    pub fn new() -> Self {
        Self
    }

    pub fn get(&self, key: SecretKey) -> Result<Option<String>> {
        match Entry::new(SERVICE, key.account())?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn set(&self, key: SecretKey, value: &str) -> Result<()> {
        Entry::new(SERVICE, key.account())?.set_password(value)?;
        Ok(())
    }

    pub fn delete(&self, key: SecretKey) -> Result<()> {
        match Entry::new(SERVICE, key.account())?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.into()),
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

impl Default for SecretStore {
    fn default() -> Self {
        Self::new()
    }
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
}
