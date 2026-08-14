//! mpv as a managed dependency.

use std::sync::Arc;

use async_trait::async_trait;

use crate::core::config::ConfigStore;
use crate::core::deps::installer::{install_and_verify, PackageManagedInstall, PackageSpec};
use crate::core::deps::{
    Dependency, DependencyId, DependencyStatus, ModeRequirement, PlayerChoice,
};
use crate::core::error::Result;
use crate::core::events::ProgressSink;
use crate::core::process;
use crate::core::syncplay::{find_player, MPV_KEY};

const DISPLAY_NAME: &str = "mpv or VLC";
const MANUAL_URL: &str = "https://mpv.io/installation/";

pub struct MpvDependency {
    settings: Arc<ConfigStore>,
}

impl MpvDependency {
    pub fn new(settings: Arc<ConfigStore>) -> Self {
        Self { settings }
    }

    /// Where each player comes from on each platform.
    pub(crate) fn spec_for(choice: PlayerChoice) -> PackageSpec {
        match choice {
            PlayerChoice::Mpv => PackageSpec {
                winget_id: Some("shinchiro.mpv"),
                brew_cask: Some("mpv"),
            },
            PlayerChoice::Vlc => PackageSpec {
                winget_id: Some("VideoLAN.VLC"),
                brew_cask: Some("vlc"),
            },
        }
    }

    fn installer(choice: PlayerChoice) -> PackageManagedInstall {
        PackageManagedInstall {
            display_name: DISPLAY_NAME,
            spec: Self::spec_for(choice),
        }
    }
}

#[async_trait]
impl Dependency for MpvDependency {
    fn id(&self) -> DependencyId {
        DependencyId::Mpv
    }

    fn display_name(&self) -> &str {
        DISPLAY_NAME
    }

    /// Either supported player satisfies the requirement; automatic install
    /// remains mpv because it is available from both package managers.
    fn required_for(&self) -> ModeRequirement {
        ModeRequirement::Both
    }

    async fn detect(&self) -> DependencyStatus {
        let manual = self.settings.executable_override(MPV_KEY);

        let Some(path) = find_player(manual.as_deref()) else {
            return DependencyStatus::Missing;
        };

        DependencyStatus::Installed {
            version: process::probe_version(&path, &["--version"]).await,
            path: Some(path.to_string_lossy().into_owned()),
        }
    }

    async fn install(
        &self,
        progress: &dyn ProgressSink,
        choice: Option<PlayerChoice>,
    ) -> Result<()> {
        let installer = Self::installer(choice.unwrap_or_default());
        install_and_verify(self, &installer, progress).await
    }

    fn manual_url(&self) -> &str {
        MANUAL_URL
    }

    fn needs_elevation(&self) -> bool {
        false
    }

    async fn can_auto_install(&self) -> bool {
        // Either player being installable is enough to offer the button; the
        // control beside it is what picks between them.
        Self::installer(PlayerChoice::Mpv).is_supported()
            || Self::installer(PlayerChoice::Vlc).is_supported()
    }

    /// mpv is very often a portable build sitting in a folder somewhere.
    fn supports_manual_path(&self) -> bool {
        true
    }

    fn manual_path_key(&self) -> Option<&'static str> {
        Some(MPV_KEY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_player_names_itself_to_both_package_managers() {
        let mpv = MpvDependency::spec_for(PlayerChoice::Mpv);
        assert_eq!(mpv.winget_id, Some("shinchiro.mpv"));
        assert_eq!(mpv.brew_cask, Some("mpv"));

        let vlc = MpvDependency::spec_for(PlayerChoice::Vlc);
        assert_eq!(vlc.winget_id, Some("VideoLAN.VLC"));
        assert_eq!(vlc.brew_cask, Some("vlc"));
    }

    /// Nothing chosen means mpv, which is what the row does before the user
    /// touches the control and what every existing caller expects.
    #[test]
    fn no_choice_installs_mpv() {
        assert_eq!(
            MpvDependency::spec_for(PlayerChoice::default()).winget_id,
            Some("shinchiro.mpv")
        );
    }
}
