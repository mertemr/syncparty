//! Detecting and installing everything syncparty needs but does not ship.
//!
//! The PowerShell prototype threw an error listing what was missing and left
//! the user to sort it out. Here each external tool implements [`Dependency`],
//! so the UI can render a checklist where every failing row has a working
//! "Install" button and a manual download link as a fallback.

mod installer;
mod manager;
mod mpv;
mod server_runtime;
mod syncplay_client;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::core::config::AppMode;
use crate::core::error::Result;
use crate::core::events::ProgressSink;

pub use installer::{PackageSpec, SystemPackageManager};
pub use manager::DependencyManager;
pub use mpv::MpvDependency;
pub use server_runtime::ServerRuntimeDependency;
pub use syncplay_client::SyncplayClientDependency;

/// Stable identifier for each dependency, shared with the frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum DependencyId {
    /// The Syncplay desktop client, used to join a party.
    SyncplayClient,
    /// The video player Syncplay drives.
    Mpv,
    /// The managed Python environment that runs the Syncplay server.
    ServerRuntime,
}

/// Which player an automatic install should fetch.
///
/// Only the player has more than one source, so this is the only dependency
/// whose install takes an argument. Detection is unaffected: `find_player`
/// prefers mpv when both are present, whichever one was installed from here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum PlayerChoice {
    #[default]
    Mpv,
    Vlc,
}

/// Whether a dependency is needed by hosts, guests, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeRequirement {
    HostOnly,
    GuestOnly,
    Both,
}

impl ModeRequirement {
    pub fn applies_to(self, mode: AppMode) -> bool {
        matches!(
            (self, mode),
            (Self::Both, _) | (Self::HostOnly, AppMode::Host) | (Self::GuestOnly, AppMode::Guest)
        )
    }
}

/// Result of probing the machine for one dependency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum DependencyStatus {
    Missing,
    Installed {
        /// Absent when the tool is present but refuses to report a version.
        version: Option<String>,
        path: Option<String>,
    },
}

impl DependencyStatus {
    pub fn is_installed(&self) -> bool {
        matches!(self, Self::Installed { .. })
    }
}

/// One external tool syncparty can find, install and point the user at.
///
/// `detect` returns a status rather than a `Result` on purpose: failing to
/// find something is the answer, not an error. Only `install` can fail in a
/// way the user needs to hear about.
#[async_trait]
pub trait Dependency: Send + Sync {
    fn id(&self) -> DependencyId;

    fn display_name(&self) -> &str;

    fn required_for(&self) -> ModeRequirement;

    async fn detect(&self) -> DependencyStatus;

    /// Installs the dependency, reporting progress as it goes.
    ///
    /// `choice` is meaningful only for the player, which is the one dependency
    /// with more than one source. Everything else ignores it.
    async fn install(
        &self,
        progress: &dyn ProgressSink,
        choice: Option<PlayerChoice>,
    ) -> Result<()>;

    /// Where to send the user when the automatic install does not work. Every
    /// dependency must have one — there is no dead end.
    fn manual_url(&self) -> &str;

    /// Whether installing will trigger a UAC or sudo prompt, so the UI can
    /// warn before the dialog appears from nowhere.
    fn needs_elevation(&self) -> bool;

    /// Whether an automatic install is possible on this machine right now.
    async fn can_auto_install(&self) -> bool;

    /// Whether the user can point syncparty at this program by hand.
    ///
    /// True for anything with a portable distribution — an extracted zip is
    /// invisible to both `PATH` and the registry. False for the managed
    /// server runtime, which syncparty puts where it likes.
    fn supports_manual_path(&self) -> bool {
        false
    }

    /// The settings key its manual path is stored under. Only meaningful when
    /// [`Dependency::supports_manual_path`] is true.
    fn manual_path_key(&self) -> Option<&'static str> {
        None
    }
}

/// Snapshot of every dependency relevant to the chosen mode.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PreflightReport {
    pub mode: AppMode,
    pub items: Vec<PreflightItem>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PreflightItem {
    pub id: DependencyId,
    pub display_name: String,
    pub status: DependencyStatus,
    pub can_auto_install: bool,
    pub needs_elevation: bool,
    pub manual_url: String,
    /// Whether the UI should offer a "locate it for me" button.
    pub supports_manual_path: bool,
    /// The path the user already chose, so the UI can show and clear it.
    pub override_path: Option<String>,
}

impl PreflightReport {
    /// True when nothing is blocking the user from starting or joining.
    pub fn is_satisfied(&self) -> bool {
        self.items.iter().all(|item| item.status.is_installed())
    }

    pub fn missing(&self) -> impl Iterator<Item = &PreflightItem> {
        self.items.iter().filter(|item| !item.status.is_installed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_only_dependencies_are_skipped_for_guests() {
        assert!(ModeRequirement::HostOnly.applies_to(AppMode::Host));
        assert!(!ModeRequirement::HostOnly.applies_to(AppMode::Guest));
        assert!(ModeRequirement::Both.applies_to(AppMode::Guest));
        assert!(!ModeRequirement::GuestOnly.applies_to(AppMode::Host));
    }

    fn item(id: DependencyId, status: DependencyStatus) -> PreflightItem {
        PreflightItem {
            id,
            display_name: "test".to_owned(),
            status,
            can_auto_install: true,
            needs_elevation: false,
            manual_url: "https://example.com".to_owned(),
            supports_manual_path: false,
            override_path: None,
        }
    }

    #[test]
    fn a_report_is_satisfied_only_when_everything_is_installed() {
        let installed = DependencyStatus::Installed {
            version: None,
            path: None,
        };

        let report = PreflightReport {
            mode: AppMode::Host,
            items: vec![
                item(DependencyId::SyncplayClient, installed.clone()),
                item(DependencyId::Mpv, installed),
            ],
        };
        assert!(report.is_satisfied());
        assert_eq!(report.missing().count(), 0);

        let report = PreflightReport {
            mode: AppMode::Host,
            items: vec![item(
                DependencyId::SyncplayClient,
                DependencyStatus::Missing,
            )],
        };
        assert!(!report.is_satisfied());
        assert_eq!(report.missing().count(), 1);
    }
}
