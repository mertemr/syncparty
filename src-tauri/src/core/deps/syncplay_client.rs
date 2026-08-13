//! The Syncplay desktop client as a managed dependency.

use std::sync::Arc;

use async_trait::async_trait;

use crate::core::config::ConfigStore;
use crate::core::deps::installer::{install_and_verify, PackageManagedInstall, PackageSpec};
use crate::core::deps::{
    Dependency, DependencyId, DependencyStatus, ModeRequirement, PlayerChoice,
};
use crate::core::error::Result;
use crate::core::events::ProgressSink;
use crate::core::syncplay::{find_client, SYNCPLAY_CLIENT_KEY};

const DISPLAY_NAME: &str = "Syncplay";
const MANUAL_URL: &str = "https://syncplay.pl/download/";

pub struct SyncplayClientDependency {
    installer: PackageManagedInstall,
    settings: Arc<ConfigStore>,
}

impl SyncplayClientDependency {
    pub fn new(settings: Arc<ConfigStore>) -> Self {
        Self {
            installer: PackageManagedInstall {
                display_name: DISPLAY_NAME,
                spec: PackageSpec {
                    winget_id: Some("Syncplay.Syncplay"),
                    brew_cask: Some("syncplay"),
                },
            },
            settings,
        }
    }
}

#[async_trait]
impl Dependency for SyncplayClientDependency {
    fn id(&self) -> DependencyId {
        DependencyId::SyncplayClient
    }

    fn display_name(&self) -> &str {
        DISPLAY_NAME
    }

    /// The host watches along with everyone else, so both modes need it.
    fn required_for(&self) -> ModeRequirement {
        ModeRequirement::Both
    }

    async fn detect(&self) -> DependencyStatus {
        let manual = self.settings.executable_override(SYNCPLAY_CLIENT_KEY);

        let Some(path) = find_client(manual.as_deref()) else {
            return DependencyStatus::Missing;
        };

        // The GUI client opens a window when asked for its version, so the
        // path alone is the answer here.
        DependencyStatus::Installed {
            version: None,
            path: Some(path.to_string_lossy().into_owned()),
        }
    }

    async fn install(
        &self,
        progress: &dyn ProgressSink,
        _choice: Option<PlayerChoice>,
    ) -> Result<()> {
        install_and_verify(self, &self.installer, progress).await
    }

    fn manual_url(&self) -> &str {
        MANUAL_URL
    }

    fn needs_elevation(&self) -> bool {
        cfg!(windows)
    }

    async fn can_auto_install(&self) -> bool {
        self.installer.is_supported()
    }

    /// Syncplay publishes a portable build alongside its installer.
    fn supports_manual_path(&self) -> bool {
        true
    }

    fn manual_path_key(&self) -> Option<&'static str> {
        Some(SYNCPLAY_CLIENT_KEY)
    }
}
