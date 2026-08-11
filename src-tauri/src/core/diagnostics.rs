//! A single, read-only health snapshot for troubleshooting a movie night.

use serde::Serialize;
use ts_rs::TS;

use crate::core::config::{AppMode, SecretStore};
use crate::core::deps::{DependencyManager, PreflightReport};
use crate::core::net;
use crate::core::session::{PartySession, SessionState};

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsReport {
    pub app_version: String,
    pub operating_system: String,
    pub dependencies: PreflightReport,
    /// This machine's address on the syncparty network.
    ///
    /// Absent until the first time it hosts, since only a host needs a stable
    /// identity. Included because it is the one value that identifies a
    /// machine when comparing notes about a party that would not connect.
    pub endpoint: Option<String>,
    pub session: SessionState,
}

/// Collects independent checks without changing machine or session state.
pub async fn collect(
    dependencies: &DependencyManager,
    session: &PartySession,
    secrets: &SecretStore,
    mode: AppMode,
) -> DiagnosticsReport {
    let (dependencies, session) = tokio::join!(dependencies.preflight(mode), session.state());

    DiagnosticsReport {
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        operating_system: std::env::consts::OS.to_owned(),
        dependencies,
        endpoint: net::stored_endpoint_id(secrets).map(|id| id.to_string()),
        session,
    }
}
