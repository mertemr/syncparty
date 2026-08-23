//! Where syncparty keeps its data on each platform.
//!
//! Resolved once at startup and passed down, so no module has to guess at a
//! directory layout or reach for an environment variable on its own.

use std::path::{Path, PathBuf};

use crate::core::error::{Result, SyncPartyError};

const APP_DIR_NAME: &str = "syncparty";

/// Where the `.deb` and the AUR package install the pinned Syncplay server.
/// `/usr/local` is listed second so a hand-installed copy can shadow neither
/// the package's nor be shadowed by it unexpectedly — first match wins.
#[cfg(target_os = "linux")]
const PACKAGED_SOURCE_DIRS: &[&str] = &[
    "/usr/lib/syncparty/syncplay-source",
    "/usr/local/lib/syncparty/syncplay-source",
];

#[derive(Debug, Clone)]
pub struct AppPaths {
    data_dir: PathBuf,
    /// Whether [`Self::syncplay_source_dir`] may answer with a packaged copy
    /// outside `data_dir`. False for test roots, which promise that every path
    /// stays under the root they were given.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    packaged_source_allowed: bool,
}

impl AppPaths {
    /// Resolves the per-user data directory, creating it if it does not exist.
    pub fn resolve() -> Result<Self> {
        let base = platform_data_root()?;
        let data_dir = base.join(APP_DIR_NAME);
        std::fs::create_dir_all(&data_dir)?;
        Ok(Self {
            data_dir,
            packaged_source_allowed: true,
        })
    }

    /// Points every path at `root`. Used by tests to stay off the real profile.
    ///
    /// That includes the packaged Syncplay server: consulting `/usr/lib` here
    /// would make results depend on whether the machine running the tests
    /// happens to have syncparty installed.
    pub fn rooted_at(root: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: root.into(),
            packaged_source_allowed: false,
        }
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn settings_file(&self) -> PathBuf {
        self.data_dir.join("settings.json")
    }

    /// Fallback secret store, used only when no OS keychain answers.
    pub fn secrets_file(&self) -> PathBuf {
        self.data_dir.join("secrets.json")
    }

    /// Root of the managed Python environment and Syncplay checkout.
    pub fn server_runtime_dir(&self) -> PathBuf {
        self.data_dir.join("server-runtime")
    }

    /// Where a downloaded Syncplay checkout is written.
    ///
    /// Always under the data directory, never the packaged copy below — this
    /// is a write target, and `/usr/lib` is not writable by the user.
    pub fn managed_source_dir(&self) -> PathBuf {
        self.server_runtime_dir().join("syncplay-source")
    }

    /// Where the Syncplay server is read from.
    ///
    /// On Linux the native packages ship the pinned server source, because no
    /// distribution carries a new enough one: `--ipv4-only` and
    /// `--interface-ipv4` arrived in Syncplay 1.7.1, and the newest package in
    /// any Ubuntu LTS is 1.7.0. Without those flags the server binds every
    /// interface instead of loopback, which is precisely what
    /// `core::syncplay::server` refuses to do.
    ///
    /// A source build has no packaged copy, so it falls through to the
    /// downloaded one and behaves as Windows and macOS do.
    pub fn syncplay_source_dir(&self) -> PathBuf {
        #[cfg(target_os = "linux")]
        if self.packaged_source_allowed {
            if let Some(packaged) = PACKAGED_SOURCE_DIRS
                .iter()
                .map(PathBuf::from)
                .find(|dir| dir.join("syncplayServer.py").is_file())
            {
                return packaged;
            }
        }

        self.managed_source_dir()
    }

    pub fn server_venv_dir(&self) -> PathBuf {
        self.server_runtime_dir().join("venv")
    }

    /// The interpreter that runs the server.
    ///
    /// Linux uses the system Python and takes Twisted from the distribution,
    /// which is why the Linux packages depend on `python3-twisted` and why
    /// `uv` never has to be installed there — it is in no Debian or Ubuntu
    /// release at all. Everywhere else this is the managed virtual
    /// environment's own interpreter.
    pub fn server_python(&self) -> PathBuf {
        #[cfg(target_os = "linux")]
        {
            which::which("python3").unwrap_or_else(|_| PathBuf::from("/usr/bin/python3"))
        }

        #[cfg(not(target_os = "linux"))]
        {
            let venv = self.server_venv_dir();
            if cfg!(windows) {
                venv.join("Scripts").join("python.exe")
            } else {
                venv.join("bin").join("python")
            }
        }
    }

    pub fn server_entrypoint(&self) -> PathBuf {
        self.syncplay_source_dir().join("syncplayServer.py")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    pub fn server_log(&self) -> PathBuf {
        self.log_dir().join("syncplay-server.log")
    }
}

fn platform_data_root() -> Result<PathBuf> {
    let missing =
        |var: &str| SyncPartyError::Config(format!("the {var} environment variable is not set"));

    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or_else(|| missing("LOCALAPPDATA"))
    } else {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| missing("HOME"))?;

        if cfg!(target_os = "macos") {
            Ok(home.join("Library").join("Application Support"))
        } else {
            Ok(std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local").join("share")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_every_path_from_the_data_dir() {
        let paths = AppPaths::rooted_at("/tmp/syncparty-test");

        assert!(paths.settings_file().starts_with("/tmp/syncparty-test"));
        assert!(paths.secrets_file().starts_with("/tmp/syncparty-test"));
        assert!(paths
            .managed_source_dir()
            .starts_with("/tmp/syncparty-test"));
        assert!(paths.server_entrypoint().ends_with("syncplayServer.py"));
    }

    /// The download destination must never follow the packaged copy, or a
    /// fetch would try to write into `/usr/lib`.
    #[test]
    fn the_download_destination_stays_under_the_data_dir() {
        let paths = AppPaths::rooted_at("/tmp/syncparty-test");

        assert_eq!(
            paths.managed_source_dir(),
            paths.server_runtime_dir().join("syncplay-source")
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn the_interpreter_lives_in_the_managed_environment() {
        let paths = AppPaths::rooted_at("/tmp/syncparty-test");

        assert!(paths.server_python().starts_with(paths.server_venv_dir()));
    }

    /// Linux takes Python and Twisted from the distribution instead of
    /// building a virtual environment, so the interpreter is a system one.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_uses_the_system_interpreter() {
        let paths = AppPaths::rooted_at("/tmp/syncparty-test");

        assert!(!paths.server_python().starts_with(paths.server_venv_dir()));
        assert!(paths.server_python().ends_with("python3"));
    }
}
