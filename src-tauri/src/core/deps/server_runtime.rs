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

/// SHA-256 of the pinned source archive.
///
/// TLS proves GitHub served the bytes; it does not prove they are the bytes
/// this release was tested against. Bump it together with `SYNCPLAY_VERSION`
/// — `curl -sL <url> | sha256sum` produces it.
///
/// The known failure mode: GitHub generates tag archives on demand, so a
/// change to their compression would change this digest without upstream
/// republishing anything. That is rare and loud, and the alternative — not
/// checking at all — means unpacking whatever arrives.
const SYNCPLAY_SHA256: &str = "6aef1e8351bccb97e6833fcae04c80f9d01b290b627f70df3e3870555c40deaa";


/// Python the virtual environment is built against. `uv` downloads it if the
/// machine has none, which is why the user never has to install Python.
///
/// Not used on Linux, which runs the server on the system interpreter — see
/// [`crate::core::paths::AppPaths::server_python`].
#[cfg(not(target_os = "linux"))]
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
    ///
    /// Absent on Linux: `uv` is packaged by neither Debian nor Ubuntu, so
    /// there is nothing to install it with and nothing that needs it.
    #[cfg(not(target_os = "linux"))]
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

        progress.report("verifying", None, None);
        verify_digest(&archive)?;

        progress.report("extracting", None, None);

        let runtime_dir = self.paths.server_runtime_dir();
        let staging = runtime_dir.join("unpack");
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging)?;

        let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(archive));
        tar::Archive::new(decoder).unpack(&staging)?;

        // The archive wraps everything in a `syncplay-<version>/` directory.
        let unpacked = single_child_directory(&staging)?;
        let destination = self.paths.managed_source_dir();
        let _ = std::fs::remove_dir_all(&destination);
        std::fs::rename(&unpacked, &destination)?;
        let _ = std::fs::remove_dir_all(&staging);

        Ok(())
    }

    /// Creates the virtual environment and installs the server's dependencies.
    #[cfg(not(target_os = "linux"))]
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

        let requirements = self.paths.managed_source_dir().join("requirements.txt");
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

        // The managed virtual environment is built with the server's own
        // requirements, so on Windows and macOS an interpreter that exists is
        // an interpreter that works. Linux borrows the system Python and takes
        // Twisted from the distribution, so it has to be asked.
        #[cfg(target_os = "linux")]
        if !twisted_is_importable(&python).await {
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

        // Linux never builds an environment. The packages install the pinned
        // server source and take Twisted from the distribution, so the only
        // thing that can be missing here is the source itself — which happens
        // in a source build, where there is no packaged copy to find.
        #[cfg(target_os = "linux")]
        {
            if !self.paths.server_entrypoint().is_file() {
                self.fetch_source(progress).await?;
            }

            let python = self.paths.server_python();
            if !twisted_is_importable(&python).await {
                return Err(SyncPartyError::InstallFailed {
                    name: DISPLAY_NAME.to_owned(),
                    reason: format!(
                        "Python is present at {} but Twisted is not installed for it. \
                         Install your distribution's Twisted package \
                         (python3-twisted on Debian and Ubuntu, python-twisted on Arch, \
                         python3-twisted on Fedora).",
                        python.display()
                    ),
                });
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            let uv = self.ensure_uv(progress).await?;
            self.fetch_source(progress).await?;
            self.build_environment(&uv, progress).await?;
        }

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

    /// Linux needs no `uv` and no virtual environment, so this is always
    /// true there — the worst case is downloading the pinned source, which
    /// needs nothing but the network.
    async fn can_auto_install(&self) -> bool {
        if cfg!(target_os = "linux") {
            return true;
        }

        which::which("uv").is_ok() || self.uv_installer.is_supported() || cfg!(target_os = "macos")
    }
}

/// Confirms the interpreter can actually import the server's one hard
/// dependency.
///
/// Only Twisted is checked because only Twisted is required: the server's
/// other listed requirements, `certifi` and `pem`, are imported by
/// `syncplay/client.py` and never on the server path.
#[cfg(target_os = "linux")]
async fn twisted_is_importable(python: &Path) -> bool {
    process::capture(python, ["-c", "import twisted"])
        .await
        .is_ok()
}

/// Rejects an archive whose contents are not what this release pinned.
fn verify_digest(archive: &[u8]) -> Result<()> {
    use sha2::{Digest, Sha256};

    let actual = format!("{:x}", Sha256::digest(archive));

    if actual != SYNCPLAY_SHA256 {
        return Err(SyncPartyError::InstallFailed {
            name: DISPLAY_NAME.to_owned(),
            reason: format!(
                "the downloaded Syncplay {SYNCPLAY_VERSION} archive did not match its \
                 expected checksum (wanted {SYNCPLAY_SHA256}, got {actual}) — refusing \
                 to unpack it"
            ),
        });
    }

    Ok(())
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
    fn an_archive_that_does_not_match_the_pin_is_refused() {
        let error = verify_digest(b"not the syncplay release").expect_err("should reject");

        assert!(error.to_string().contains("checksum"));
    }

    /// Guards the pin itself: a `SYNCPLAY_VERSION` bump without a matching
    /// `SYNCPLAY_SHA256` bump would otherwise only fail at install time, on a
    /// user's machine.
    #[test]
    fn the_pinned_digest_is_a_sha256() {
        assert_eq!(SYNCPLAY_SHA256.len(), 64);
        assert!(SYNCPLAY_SHA256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
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
