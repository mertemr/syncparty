//! Installing dependencies through the platform's own package manager.
//!
//! winget on Windows, Homebrew on macOS. Both are already trusted by the OS
//! or the user, which beats syncparty downloading and running installers it
//! would then have to verify itself.

use crate::core::deps::Dependency;
use crate::core::error::{Result, SyncPartyError};
use crate::core::events::ProgressSink;
use crate::core::process;

/// How one dependency is named in each package manager. A `None` means that
/// manager cannot install it, and the user gets the manual link instead.
#[derive(Debug, Clone, Copy)]
pub struct PackageSpec {
    pub winget_id: Option<&'static str>,
    pub brew_cask: Option<&'static str>,
}

/// The package manager available on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemPackageManager {
    Winget,
    Homebrew,
}

impl SystemPackageManager {
    /// Finds the package manager for this platform, or `None` when there is
    /// none installed — a stripped Windows image without App Installer, for
    /// instance.
    pub fn detect() -> Option<Self> {
        if cfg!(windows) {
            which::which("winget").ok().map(|_| Self::Winget)
        } else if cfg!(target_os = "macos") {
            which::which("brew").ok().map(|_| Self::Homebrew)
        } else {
            None
        }
    }

    fn package_for(self, spec: &PackageSpec) -> Option<&'static str> {
        match self {
            Self::Winget => spec.winget_id,
            Self::Homebrew => spec.brew_cask,
        }
    }

    fn program(self) -> &'static str {
        match self {
            Self::Winget => "winget",
            Self::Homebrew => "brew",
        }
    }

    fn args_for(self, package: &str) -> Vec<String> {
        match self {
            Self::Winget => [
                "install",
                "--id",
                package,
                "--source",
                "winget",
                "--exact",
                "--accept-package-agreements",
                "--accept-source-agreements",
                "--disable-interactivity",
            ]
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect(),
            Self::Homebrew => ["install", "--cask", package]
                .iter()
                .map(|argument| (*argument).to_owned())
                .collect(),
        }
    }
}

/// The install half of a dependency that comes from a package manager.
///
/// Composed into [`Dependency`] implementations rather than inherited, so a
/// dependency installed some other way — the managed Python environment, for
/// one — simply does not have this piece.
pub(crate) struct PackageManagedInstall {
    pub display_name: &'static str,
    pub spec: PackageSpec,
}

impl PackageManagedInstall {
    /// Whether a package manager exists here *and* knows this package.
    pub fn is_supported(&self) -> bool {
        SystemPackageManager::detect()
            .and_then(|manager| manager.package_for(&self.spec))
            .is_some()
    }

    async fn run(&self, progress: &dyn ProgressSink) -> Result<()> {
        let manager =
            SystemPackageManager::detect().ok_or_else(|| SyncPartyError::InstallUnsupported {
                name: self.display_name.to_owned(),
            })?;

        let package =
            manager
                .package_for(&self.spec)
                .ok_or_else(|| SyncPartyError::InstallUnsupported {
                    name: self.display_name.to_owned(),
                })?;

        progress.report(
            "installing",
            None,
            Some(format!("{} {package}", manager.program())),
        );

        process::capture(manager.program(), manager.args_for(package)).await?;
        Ok(())
    }
}

/// Installs a dependency and confirms it by re-detecting.
///
/// The verification matters: winget returns non-zero for outcomes that are
/// really fine (already installed, no applicable update), and conversely can
/// exit zero after a install that left nothing usable behind. What the machine
/// looks like afterwards is the only answer worth trusting.
pub(crate) async fn install_and_verify(
    dependency: &(impl Dependency + ?Sized),
    installer: &PackageManagedInstall,
    progress: &dyn ProgressSink,
) -> Result<()> {
    let outcome = installer.run(progress).await;

    progress.report("verifying", None, None);
    if dependency.detect().await.is_installed() {
        return Ok(());
    }

    // The install reported a real failure — surface that rather than a
    // generic "still missing", since it says why.
    outcome?;

    Err(SyncPartyError::InstallFailed {
        name: installer.display_name.to_owned(),
        reason: "the installer finished but the program still cannot be found".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn winget_arguments_stay_non_interactive() {
        let args = SystemPackageManager::Winget.args_for("Syncplay.Syncplay");

        assert!(args.contains(&"--disable-interactivity".to_owned()));
        assert!(args.contains(&"--accept-package-agreements".to_owned()));
        assert!(args.contains(&"--exact".to_owned()));
        assert!(args.contains(&"Syncplay.Syncplay".to_owned()));
    }

    #[test]
    fn homebrew_installs_casks() {
        assert_eq!(
            SystemPackageManager::Homebrew.args_for("mpv"),
            vec!["install".to_owned(), "--cask".to_owned(), "mpv".to_owned()]
        );
    }

    #[test]
    fn a_manager_reports_no_package_when_the_spec_omits_it() {
        let spec = PackageSpec {
            winget_id: Some("Some.Thing"),
            brew_cask: None,
        };

        assert!(SystemPackageManager::Winget.package_for(&spec).is_some());
        assert!(SystemPackageManager::Homebrew.package_for(&spec).is_none());
    }
}
