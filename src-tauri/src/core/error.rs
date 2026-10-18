//! The single error type crossing every `core` boundary.
//!
//! Errors surface in the UI, so each variant carries a message written for a
//! person rather than a stack trace. [`SyncPartyError::kind`] gives the
//! frontend a stable discriminant to branch on without parsing prose.

use serde::{Serialize, Serializer};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, SyncPartyError>;

#[derive(Debug, Error)]
pub enum SyncPartyError {
    #[error("{0} is not installed")]
    DependencyMissing(String),

    /// A location the user chose for a dependency that does not hold it, or
    /// holds something that cannot be used. The message is the whole reason.
    #[error("{0}")]
    InvalidLocation(String),

    #[error("could not install {name}: {reason}")]
    InstallFailed { name: String, reason: String },

    #[error("no automatic installer is available for {name} on this platform")]
    InstallUnsupported { name: String },

    #[error("could not open a connection to the syncparty network: {0}")]
    EndpointBindFailed(String),

    /// The endpoint bound, but never worked out how it can be reached. Without
    /// that there is no home relay and nothing has been published, so an
    /// invite generated now would name an address no guest could resolve.
    #[error(
        "could not reach the syncparty network. Check this machine's internet connection and \
         try again — a firewall that blocks outbound UDP will also cause this."
    )]
    EndpointOffline,

    #[error("the sync server is not running")]
    ServerNotRunning,

    #[error("the sync server is already running")]
    ServerAlreadyRunning,

    #[error("not currently in a party")]
    NotInParty,

    #[error("TMDB is not configured — add an API key in Settings")]
    MovieProviderNotConfigured,

    #[error("the sync server failed to start: {0}")]
    ServerStartFailed(String),

    #[error("the invite code is not valid: {0}")]
    InvalidInvite(String),

    #[error("could not reach the room monitor: {0}")]
    MonitorFailed(String),

    /// The host's endpoint could not be dialled. Unlike the tailnet this
    /// replaced there is no membership to check, so the causes are narrow: the
    /// host is not running a party right now, or one of the two machines
    /// cannot get out to the network at all.
    #[error(
        "could not reach the host. They may have ended the movie night, or not started it yet — \
         ask them to check syncparty is still running, then try again."
    )]
    PartyUnreachable { host: String, reason: String },

    /// mpv could not be started, stopped answering, or refused a command.
    #[error("the player failed: {0}")]
    PlayerFailed(String),

    /// The sync server turned this client away — almost always a password
    /// that does not match, which means an invite from an older party.
    #[error("the party did not let this machine in: {0}")]
    SyncRejected(String),

    /// The sync server could not be reached or stopped answering.
    #[error("could not connect to the party: {0}")]
    SyncFailed(String),

    #[error("nothing is being watched on this machine")]
    NotWatching,

    /// The user called the operation off before it finished — stopped a
    /// party that was still starting, or left one that was still being
    /// joined. Not a failure, so the UI says nothing about it.
    #[error("called off before it finished")]
    Cancelled,

    #[error("{command} exited with status {status}: {stderr}")]
    CommandFailed {
        command: String,
        status: String,
        stderr: String,
    },

    #[error("could not read or write settings: {0}")]
    Config(String),

    #[error("could not read or write a stored secret: {0}")]
    Secret(String),

    #[error("network request failed: {0}")]
    Network(String),

    #[error("{0}")]
    Io(String),

    #[error("{0}")]
    Other(String),
}

impl SyncPartyError {
    /// Stable discriminant for the frontend. Never change an existing string;
    /// the UI branches on these to decide which recovery action to offer.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::DependencyMissing(_) => "dependency_missing",
            Self::InvalidLocation(_) => "invalid_location",
            Self::InstallFailed { .. } => "install_failed",
            Self::InstallUnsupported { .. } => "install_unsupported",
            Self::EndpointBindFailed(_) => "endpoint_bind_failed",
            Self::EndpointOffline => "endpoint_offline",
            Self::ServerNotRunning => "server_not_running",
            Self::ServerAlreadyRunning => "server_already_running",
            Self::NotInParty => "not_in_party",
            Self::MovieProviderNotConfigured => "movie_provider_not_configured",
            Self::ServerStartFailed(_) => "server_start_failed",
            Self::InvalidInvite(_) => "invalid_invite",
            Self::MonitorFailed(_) => "monitor_failed",
            Self::PartyUnreachable { .. } => "party_unreachable",
            Self::PlayerFailed(_) => "player_failed",
            Self::SyncRejected(_) => "sync_rejected",
            Self::SyncFailed(_) => "sync_failed",
            Self::NotWatching => "not_watching",
            Self::Cancelled => "cancelled",
            Self::CommandFailed { .. } => "command_failed",
            Self::Config(_) => "config",
            Self::Secret(_) => "secret",
            Self::Network(_) => "network",
            Self::Io(_) => "io",
            Self::Other(_) => "other",
        }
    }
}

/// Tauri requires command errors to be `Serialize`. The stable kind and the
/// human-readable message are all the UI branches on.
impl Serialize for SyncPartyError {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("SyncPartyError", 2)?;
        state.serialize_field("kind", self.kind())?;
        state.serialize_field("message", &self.to_string())?;
        state.end()
    }
}

impl From<std::io::Error> for SyncPartyError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

impl From<serde_json::Error> for SyncPartyError {
    fn from(value: serde_json::Error) -> Self {
        Self::Other(format!("malformed JSON: {value}"))
    }
}

/// The URL comes off before anything is turned into text. Every request
/// syncparty makes carries a secret in it — the Discord webhook token is part
/// of the path, the TMDB key is a query parameter — and reqwest names the URL
/// in its message, which would carry the secret into the failure banner, the
/// event stream and the logs.
impl From<reqwest::Error> for SyncPartyError {
    fn from(value: reqwest::Error) -> Self {
        let value = value.without_url();
        let mut message = value.to_string();
        let mut source = std::error::Error::source(&value);
        while let Some(err) = source {
            message.push_str(": ");
            message.push_str(&err.to_string());
            source = err.source();
        }
        Self::Network(message)
    }
}

impl From<keyring::Error> for SyncPartyError {
    fn from(value: keyring::Error) -> Self {
        Self::Secret(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_carry_a_stable_kind_and_a_readable_message() {
        let value = serde_json::to_value(SyncPartyError::EndpointOffline).expect("serialize");

        assert_eq!(value["kind"], "endpoint_offline");
        assert!(value["message"]
            .as_str()
            .expect("message")
            .contains("syncparty network"));
    }

    #[test]
    fn an_unreachable_party_does_not_leak_the_dial_error_into_the_message() {
        // The reason is kept for diagnostics, but a QUIC handshake failure is
        // not something a guest can act on, so it stays out of what they read.
        let error = SyncPartyError::PartyUnreachable {
            host: "ab12cd34".to_owned(),
            reason: "handshake timed out".to_owned(),
        };

        let message = error.to_string();
        assert!(!message.contains("handshake timed out"), "{message}");
        assert_eq!(error.kind(), "party_unreachable");
    }

    /// A request that fails says why, never where it was going — the where
    /// is a webhook token or an API key.
    #[tokio::test]
    async fn a_failed_request_does_not_repeat_the_secret_in_its_url() {
        // Port 1 on loopback refuses at once, so this needs no network.
        let error = reqwest::Client::new()
            .get("http://127.0.0.1:1/api/webhooks/42/hunter2-token?api_key=hunter2-key")
            .send()
            .await
            .expect_err("nothing listens on port 1");

        let message = SyncPartyError::from(error).to_string();

        assert!(!message.contains("hunter2"), "{message}");
        assert!(!message.contains("127.0.0.1:1/"), "{message}");
    }
}
