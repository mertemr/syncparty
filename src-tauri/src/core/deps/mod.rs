//! Detecting and installing everything syncparty needs but does not ship.
//!
//! The PowerShell prototype threw an error listing what was missing and left
//! the user to sort it out. Here each external tool implements [`Dependency`],
//! so the UI can render a checklist where every failing row has a working
//! "Install" button and a manual download link as a fallback.

mod installer;
mod manager;
mod mpv;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::core::config::{AppMode, ConfigStore};
use crate::core::error::Result;
use crate::core::events::ProgressSink;

pub use installer::{PackageSpec, SystemPackageManager};
pub use manager::DependencyManager;
pub use mpv::MpvDependency;

/// Stable identifier for each dependency, shared with the frontend.
///
/// One variant, and still an enum: the frontend keys installs and their
/// progress events by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum DependencyId {
    /// The video player syncparty drives.
    Mpv,
}

/// Settings keys under which earlier versions stored the location of a
/// dependency syncparty no longer has.
///
/// `"syncplayClient"` is the Syncplay desktop client, which syncparty used to
/// launch before it became a Syncplay client itself.
const RETIRED_OVERRIDES: &[&str] = &["syncplayClient"];

/// Removes manually chosen locations for dependencies that no longer exist.
///
/// Nothing reads them any more, and nothing in the UI can clear them either:
/// forgetting a location goes through a [`DependencyId`], and theirs is gone.
/// Left alone they would sit in the settings file forever. Returns whether
/// anything was removed, and writes only then, so a launch with nothing to
/// forget does not rewrite the settings file.
pub fn forget_retired_overrides(settings: &ConfigStore) -> Result<bool> {
    let stale = RETIRED_OVERRIDES
        .iter()
        .any(|key| settings.executable_override(key).is_some());

    if !stale {
        return Ok(false);
    }

    settings.update(|settings| {
        for key in RETIRED_OVERRIDES {
            settings.executable_overrides.remove(*key);
        }
    })?;

    Ok(true)
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
    /// Present on disk and unable to run, or running and not the program it
    /// was taken for. Distinct from `Missing` because the remedy is different:
    /// installing it again reinstalls the same broken thing, so the UI must
    /// not offer that as the way out.
    Unusable {
        path: String,
        /// The last line the program printed before giving up — the part that
        /// names the problem, without the traceback around it — or, for a
        /// program that runs fine but is something else, what it says it is.
        reason: String,
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
    async fn install(&self, progress: &dyn ProgressSink) -> Result<()>;

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
    /// invisible to both `PATH` and the registry.
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
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::core::paths::AppPaths;

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

    fn report_of(status: DependencyStatus) -> PreflightReport {
        PreflightReport {
            mode: AppMode::Host,
            items: vec![item(DependencyId::Mpv, status)],
        }
    }

    #[test]
    fn a_report_is_satisfied_only_when_everything_is_installed() {
        let report = report_of(DependencyStatus::Installed {
            version: None,
            path: None,
        });
        assert!(report.is_satisfied());
        assert_eq!(report.missing().count(), 0);

        let report = report_of(DependencyStatus::Missing);
        assert!(!report.is_satisfied());
        assert_eq!(report.missing().count(), 1);

        // On disk is not the same as usable: a player that will not run
        // blocks a party exactly as much as one that is not there.
        let report = report_of(DependencyStatus::Unusable {
            path: "/usr/bin/vlc".to_owned(),
            reason: "not mpv".to_owned(),
        });
        assert!(!report.is_satisfied());
        assert_eq!(report.missing().count(), 1);
    }

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    /// A settings store in a directory of its own, so tests running in
    /// parallel never share a settings file.
    fn fresh_settings() -> (AppPaths, ConfigStore) {
        let id = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "syncparty-retired-overrides-{}-{id}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        let paths = AppPaths::rooted_at(dir);
        let settings = ConfigStore::load(paths.clone()).expect("settings");
        (paths, settings)
    }

    #[test]
    fn the_retired_syncplay_client_location_is_forgotten_and_the_player_kept() {
        let (paths, settings) = fresh_settings();
        settings
            .set_executable_override("syncplayClient", Some("/opt/syncplay/syncplay".to_owned()))
            .expect("seed the retired override");
        settings
            .set_executable_override("mpv", Some("/opt/mpv/mpv".to_owned()))
            .expect("seed the player override");

        assert!(forget_retired_overrides(&settings).expect("forget"));

        let reloaded = ConfigStore::load(paths).expect("reload");
        assert_eq!(
            reloaded.executable_override("syncplayClient"),
            None,
            "the removal has to reach the settings file, not just memory"
        );
        assert_eq!(
            reloaded.executable_override("mpv").as_deref(),
            Some("/opt/mpv/mpv"),
            "a location for a dependency that still exists is the user's choice"
        );
    }

    #[test]
    fn forgetting_retired_locations_twice_finds_nothing_the_second_time() {
        let (_paths, settings) = fresh_settings();
        settings
            .set_executable_override(
                "syncplayClient",
                Some("C:/Syncplay/Syncplay.exe".to_owned()),
            )
            .expect("seed");

        assert!(forget_retired_overrides(&settings).expect("first"));
        assert!(!forget_retired_overrides(&settings).expect("second"));
    }

    #[test]
    fn with_nothing_to_forget_the_settings_file_is_not_written() {
        let (paths, settings) = fresh_settings();
        assert!(!paths.settings_file().exists(), "a fresh install has none");

        assert!(!forget_retired_overrides(&settings).expect("forget"));

        assert!(
            !paths.settings_file().exists(),
            "this runs on every launch, so finding nothing must leave the disk alone"
        );
    }
}
