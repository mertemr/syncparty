//! The managed Python environment that runs the Syncplay server.
//!
//! Hosts only. Syncplay is not published to PyPI, so this downloads a pinned
//! source release from GitHub and builds an isolated virtual environment next
//! to it with `uv`. Nothing is installed system-wide and nothing collides with
//! a Python the user already has.

use std::path::Path;

use async_trait::async_trait;

use crate::core::deps::installer::{PackageManagedInstall, PackageSpec};
use crate::core::deps::{
    Dependency, DependencyId, DependencyStatus, ModeRequirement, PlayerChoice,
};
use crate::core::error::{Result, SyncPartyError};
use crate::core::events::ProgressSink;
use crate::core::paths::AppPaths;
use crate::core::process;

const DISPLAY_NAME: &str = "Syncplay server runtime";
const MANUAL_URL: &str = "https://github.com/Syncplay/syncplay/releases";

/// Pinned so a party never breaks because upstream published something new
/// mid-evening. Bump deliberately, after testing.
const SYNCPLAY_VERSION: &str = "1.7.5";

/// Python the virtual environment is built against. `uv` downloads it if the
/// machine has none, which is why the user never has to install Python.
const PYTHON_VERSION: &str = "3.12";

pub struct ServerRuntimeDependency {
    paths: AppPaths,
    uv_installer: PackageManagedInstall,
}

impl ServerRuntimeDependency {
    pub fn new(paths: AppPaths) -> Self {
        Self {
            paths,
            uv_installer: PackageManagedInstall {
                display_name: "uv",
                spec: PackageSpec {
                    winget_id: Some("astral-sh.uv"),
                    brew_cask: None,
                },
            },
        }
    }

    fn source_url() -> String {
        format!("https://github.com/Syncplay/syncplay/archive/refs/tags/v{SYNCPLAY_VERSION}.tar.gz")
    }

    /// Locates `uv`, installing it through the package manager if needed.
    ///
    /// Homebrew ships `uv` as a formula rather than a cask, so on macOS the
    /// shared cask installer does not apply and `brew install uv` runs here.
    async fn ensure_uv(&self, progress: &dyn ProgressSink) -> Result<std::path::PathBuf> {
        if let Ok(found) = which::which("uv") {
            return Ok(found);
        }

        progress.report("installing", None, Some("uv".to_owned()));

        if cfg!(target_os = "macos") {
            process::capture("brew", ["install", "uv"]).await?;
        } else if self.uv_installer.is_supported() {
            // `install_and_verify` re-detects through a `Dependency`; uv is
            // an implementation detail rather than a listed dependency, so it
            // is verified directly below instead.
            let _ = process::capture(
                "winget",
                [
                    "install",
                    "--id",
                    "astral-sh.uv",
                    "--source",
                    "winget",
                    "--exact",
                    "--accept-package-agreements",
                    "--accept-source-agreements",
                    "--disable-interactivity",
                ],
            )
            .await;
        }

        which::which("uv").map_err(|_| SyncPartyError::InstallFailed {
            name: "uv".to_owned(),
            reason: "uv could not be installed automatically".to_owned(),
        })
    }

    /// Downloads and unpacks the pinned Syncplay source release.
    async fn fetch_source(&self, progress: &dyn ProgressSink) -> Result<()> {
        progress.report(
            "downloading",
            None,
            Some(format!("Syncplay {SYNCPLAY_VERSION}")),
        );

        let archive = reqwest::get(Self::source_url())
            .await?
            .error_for_status()?
            .bytes()
            .await?;

        progress.report("extracting", None, None);

        let runtime_dir = self.paths.server_runtime_dir();
        let staging = runtime_dir.join("unpack");
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging)?;

        let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(archive));
        tar::Archive::new(decoder).unpack(&staging)?;

        // The archive wraps everything in a `syncplay-<version>/` directory.
        let unpacked = single_child_directory(&staging)?;
        let destination = self.paths.syncplay_source_dir();
        let _ = std::fs::remove_dir_all(&destination);
        std::fs::rename(&unpacked, &destination)?;
        let _ = std::fs::remove_dir_all(&staging);

        Ok(())
    }

    /// Creates the virtual environment and installs the server's dependencies.
    async fn build_environment(&self, uv: &Path, progress: &dyn ProgressSink) -> Result<()> {
        progress.report(
            "creating environment",
            None,
            Some(format!("Python {PYTHON_VERSION}")),
        );

        let venv = self.paths.server_venv_dir();
        let _ = std::fs::remove_dir_all(&venv);

        process::capture(
            uv,
            [
                "venv".as_ref(),
                "--python".as_ref(),
                PYTHON_VERSION.as_ref(),
                venv.as_os_str(),
            ],
        )
        .await?;

        progress.report("installing dependencies", None, None);

        let requirements = self.paths.syncplay_source_dir().join("requirements.txt");
        process::capture(
            uv,
            [
                "pip".as_ref(),
                "install".as_ref(),
                "--python".as_ref(),
                self.paths.server_python().as_os_str(),
                "--requirement".as_ref(),
                requirements.as_os_str(),
            ],
        )
        .await?;

        Ok(())
    }
}

#[async_trait]
impl Dependency for ServerRuntimeDependency {
    fn id(&self) -> DependencyId {
        DependencyId::ServerRuntime
    }

    fn display_name(&self) -> &str {
        DISPLAY_NAME
    }

    /// Guests never run a server, so they never download Python.
    fn required_for(&self) -> ModeRequirement {
        ModeRequirement::HostOnly
    }

    async fn detect(&self) -> DependencyStatus {
        let python = self.paths.server_python();
        let entrypoint = self.paths.server_entrypoint();

        if !python.is_file() || !entrypoint.is_file() {
            return DependencyStatus::Missing;
        }

        DependencyStatus::Installed {
            version: Some(SYNCPLAY_VERSION.to_owned()),
            path: Some(
                self.paths
                    .server_runtime_dir()
                    .to_string_lossy()
                    .into_owned(),
            ),
        }
    }

    async fn install(
        &self,
        progress: &dyn ProgressSink,
        _choice: Option<PlayerChoice>,
    ) -> Result<()> {
        std::fs::create_dir_all(self.paths.server_runtime_dir())?;

        let uv = self.ensure_uv(progress).await?;
        self.fetch_source(progress).await?;
        self.build_environment(&uv, progress).await?;

        progress.report("verifying", None, None);
        if self.detect().await.is_installed() {
            return Ok(());
        }

        Err(SyncPartyError::InstallFailed {
            name: DISPLAY_NAME.to_owned(),
            reason: "the environment was built but the server entrypoint is missing".to_owned(),
        })
    }

    fn manual_url(&self) -> &str {
        MANUAL_URL
    }

    /// Everything lands under the user's own data directory.
    fn needs_elevation(&self) -> bool {
        false
    }

    async fn can_auto_install(&self) -> bool {
        which::which("uv").is_ok() || self.uv_installer.is_supported() || cfg!(target_os = "macos")
    }
}

/// Returns the single directory inside `parent`, which is how a GitHub source
/// archive is always shaped.
fn single_child_directory(parent: &Path) -> Result<std::path::PathBuf> {
    let mut directories = std::fs::read_dir(parent)?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir());

    let first = directories
        .next()
        .ok_or_else(|| SyncPartyError::InstallFailed {
            name: DISPLAY_NAME.to_owned(),
            reason: "the downloaded archive was empty".to_owned(),
        })?;

    if directories.next().is_some() {
        return Err(SyncPartyError::InstallFailed {
            name: DISPLAY_NAME.to_owned(),
            reason: "the downloaded archive had an unexpected layout".to_owned(),
        });
    }

    Ok(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_source_url_points_at_the_pinned_tag() {
        let url = ServerRuntimeDependency::source_url();

        assert!(url.starts_with("https://github.com/Syncplay/syncplay/"));
        assert!(url.ends_with(&format!("v{SYNCPLAY_VERSION}.tar.gz")));
    }

    #[test]
    fn detects_as_missing_when_the_runtime_directory_is_empty() {
        let dir = std::env::temp_dir().join("syncparty-runtime-missing");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        let dependency = ServerRuntimeDependency::new(AppPaths::rooted_at(dir));
        let status = tokio_test_block_on(dependency.detect());

        assert_eq!(status, DependencyStatus::Missing);
    }

    #[test]
    fn rejects_an_archive_with_more_than_one_root() {
        let dir = std::env::temp_dir().join("syncparty-archive-shape");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("one")).expect("temp dir");
        std::fs::create_dir_all(dir.join("two")).expect("temp dir");

        assert!(single_child_directory(&dir).is_err());
    }

    /// Minimal runtime so these stay plain `#[test]`s.
    fn tokio_test_block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(future)
    }
}
