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

    #[error("the Syncplay server is not running")]
    ServerNotRunning,

    #[error("the Syncplay server is already running")]
    ServerAlreadyRunning,

    #[error("the Syncplay server failed to start: {0}")]
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
            Self::InstallFailed { .. } => "install_failed",
            Self::InstallUnsupported { .. } => "install_unsupported",
            Self::EndpointBindFailed(_) => "endpoint_bind_failed",
            Self::EndpointOffline => "endpoint_offline",
            Self::ServerNotRunning => "server_not_running",
            Self::ServerAlreadyRunning => "server_already_running",
            Self::ServerStartFailed(_) => "server_start_failed",
            Self::InvalidInvite(_) => "invalid_invite",
            Self::MonitorFailed(_) => "monitor_failed",
            Self::PartyUnreachable { .. } => "party_unreachable",
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

impl From<reqwest::Error> for SyncPartyError {
    fn from(value: reqwest::Error) -> Self {
        Self::Network(value.to_string())
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
}
