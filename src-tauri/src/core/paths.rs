//! Where syncparty keeps its data on each platform.
//!
//! Resolved once at startup and passed down, so no module has to guess at a
//! directory layout or reach for an environment variable on its own.

use std::path::{Path, PathBuf};

use crate::core::error::{Result, SyncPartyError};

const APP_DIR_NAME: &str = "syncparty";

/// Overrides the per-user data directory. The headless host points this at its
/// mounted volume.
pub const DATA_DIR_VAR: &str = "SYNCPARTY_DATA_DIR";

/// Points at a Python interpreter that already has Syncplay's dependencies.
/// The container image bakes one in rather than building it with `uv`.
pub const SERVER_PYTHON_VAR: &str = "SYNCPARTY_SERVER_PYTHON";

/// Points at `syncplayServer.py` in a pre-installed Syncplay checkout.
pub const SERVER_ENTRYPOINT_VAR: &str = "SYNCPARTY_SERVER_ENTRYPOINT";

#[derive(Debug, Clone)]
pub struct AppPaths {
    data_dir: PathBuf,
    /// Set only when the runtime came from outside rather than being built by
    /// `uv` under [`Self::data_dir`].
    server_python: Option<PathBuf>,
    server_entrypoint: Option<PathBuf>,
}

impl AppPaths {
    /// Resolves the per-user data directory, creating it if it does not exist.
    pub fn resolve() -> Result<Self> {
        let data_dir = match std::env::var_os(DATA_DIR_VAR) {
            Some(value) => PathBuf::from(value),
            None => platform_data_root()?.join(APP_DIR_NAME),
        };
        std::fs::create_dir_all(&data_dir)?;

        Ok(Self {
            data_dir,
            server_python: std::env::var_os(SERVER_PYTHON_VAR).map(PathBuf::from),
            server_entrypoint: std::env::var_os(SERVER_ENTRYPOINT_VAR).map(PathBuf::from),
        })
    }

    /// Points every path at `root`. Used by tests to stay off the real profile.
    ///
    /// Ignores the environment: a test that picked up a developer's
    /// `SYNCPARTY_DATA_DIR` would pass or fail depending on their shell.
    pub fn rooted_at(root: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: root.into(),
            server_python: None,
            server_entrypoint: None,
        }
    }

    /// Uses a Syncplay installation that already exists, instead of the one
    /// `uv` would build under the data directory.
    pub fn with_server_runtime(
        mut self,
        python: impl Into<PathBuf>,
        entrypoint: impl Into<PathBuf>,
    ) -> Self {
        self.server_python = Some(python.into());
        self.server_entrypoint = Some(entrypoint.into());
        self
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn settings_file(&self) -> PathBuf {
        self.data_dir.join("settings.json")
    }

    /// Secrets, for hosts with no OS keychain to put them in.
    pub fn secrets_file(&self) -> PathBuf {
        self.data_dir.join("secrets.json")
    }

    /// Where the headless host writes the current invite, so it can be read
    /// off the volume without scraping the log.
    pub fn invite_file(&self) -> PathBuf {
        self.data_dir.join("invite.txt")
    }

    /// Root of the managed Python environment and Syncplay checkout.
    pub fn server_runtime_dir(&self) -> PathBuf {
        self.data_dir.join("server-runtime")
    }

    /// The directory the server process runs in. Syncplay resolves its own
    /// resources relative to this, so it follows a supplied entrypoint.
    pub fn syncplay_source_dir(&self) -> PathBuf {
        match &self.server_entrypoint {
            Some(entrypoint) => entrypoint
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.server_runtime_dir().join("syncplay-source")),
            None => self.server_runtime_dir().join("syncplay-source"),
        }
    }

    pub fn server_venv_dir(&self) -> PathBuf {
        self.server_runtime_dir().join("venv")
    }

    /// Python interpreter inside the managed virtual environment.
    pub fn server_python(&self) -> PathBuf {
        if let Some(python) = &self.server_python {
            return python.clone();
        }

        let venv = self.server_venv_dir();
        if cfg!(windows) {
            venv.join("Scripts").join("python.exe")
        } else {
            venv.join("bin").join("python")
        }
    }

    pub fn server_entrypoint(&self) -> PathBuf {
        self.server_entrypoint
            .clone()
            .unwrap_or_else(|| self.syncplay_source_dir().join("syncplayServer.py"))
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
        assert!(paths.server_entrypoint().ends_with("syncplayServer.py"));
        assert!(paths.server_python().starts_with(paths.server_venv_dir()));
    }

    #[test]
    fn a_supplied_runtime_replaces_the_managed_one() {
        let paths = AppPaths::rooted_at("/data")
            .with_server_runtime("/opt/venv/bin/python", "/opt/syncplay/syncplayServer.py");

        assert_eq!(paths.server_python(), PathBuf::from("/opt/venv/bin/python"));
        assert_eq!(
            paths.server_entrypoint(),
            PathBuf::from("/opt/syncplay/syncplayServer.py")
        );
    }

    #[test]
    fn the_working_directory_follows_a_supplied_entrypoint() {
        let paths = AppPaths::rooted_at("/data")
            .with_server_runtime("/opt/venv/bin/python", "/opt/syncplay/syncplayServer.py");

        assert_eq!(
            paths.syncplay_source_dir(),
            PathBuf::from("/opt/syncplay"),
            "Syncplay resolves its resources relative to where it is started"
        );
    }

    #[test]
    fn state_stays_under_the_data_dir_even_with_an_external_runtime() {
        let paths = AppPaths::rooted_at("/data")
            .with_server_runtime("/opt/venv/bin/python", "/opt/syncplay/syncplayServer.py");

        // Everything written at runtime has to land on the mounted volume,
        // not next to a read-only Syncplay baked into the image.
        assert!(paths.secrets_file().starts_with("/data"));
        assert!(paths.settings_file().starts_with("/data"));
        assert!(paths.invite_file().starts_with("/data"));
        assert!(paths.server_log().starts_with("/data"));
    }
}
