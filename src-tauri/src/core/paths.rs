//! Where syncparty keeps its data on each platform.
//!
//! Resolved once at startup and passed down, so no module has to guess at a
//! directory layout or reach for an environment variable on its own.

use std::path::{Path, PathBuf};

use crate::core::error::{Result, SyncPartyError};

const APP_DIR_NAME: &str = "syncparty";

#[derive(Debug, Clone)]
pub struct AppPaths {
    data_dir: PathBuf,
}

impl AppPaths {
    /// Resolves the per-user data directory, creating it if it does not exist.
    pub fn resolve() -> Result<Self> {
        let base = platform_data_root()?;
        let data_dir = base.join(APP_DIR_NAME);
        std::fs::create_dir_all(&data_dir)?;
        Ok(Self { data_dir })
    }

    /// Points every path at `root`. Used by tests to stay off the real profile.
    pub fn rooted_at(root: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: root.into(),
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

    /// Root of what the managed Python environment used to be. Nothing writes
    /// here any more; it is kept so the leftovers can be found and deleted.
    pub fn server_runtime_dir(&self) -> PathBuf {
        self.data_dir.join("server-runtime")
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
            .server_runtime_dir()
            .starts_with("/tmp/syncparty-test"));
    }
}
