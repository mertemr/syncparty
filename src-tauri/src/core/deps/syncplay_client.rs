//! The Syncplay desktop client as a managed dependency.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::core::config::ConfigStore;
use crate::core::deps::installer::{install_and_verify, PackageManagedInstall, PackageSpec};
use crate::core::deps::{
    Dependency, DependencyId, DependencyStatus, ModeRequirement, PlayerChoice,
};
use crate::core::error::Result;
use crate::core::events::ProgressSink;
use crate::core::process;
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

        if let Some(reason) = refuses_to_start(&path).await {
            return DependencyStatus::Unusable {
                path: path.to_string_lossy().into_owned(),
                reason,
            };
        }

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

/// Whether the client fails to start, and what it said on the way down.
///
/// Linux only, because that is where the client comes from a distribution
/// rather than from us. Ubuntu 24.04 pairs Syncplay 1.7.0 with Python 3.12,
/// which removed the `configparser.SafeConfigParser` that version imports, so
/// the binary is installed and cannot run — a state its presence on disk says
/// nothing about. Windows and macOS install an official build through winget
/// or Homebrew, where running a GUI application to interrogate it risks
/// flashing a window and catches nothing.
///
/// `--help` is the probe because it never reaches argparse: the entry point
/// imports the client package first, which is exactly where a broken install
/// dies. On a working client it prints usage and exits in about half a second
/// without opening a window.
#[cfg(target_os = "linux")]
async fn refuses_to_start(path: &Path) -> Option<String> {
    let run = process::command(path).args(["--help"]).output();

    let output = match tokio::time::timeout(PROBE_TIMEOUT, run).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => return Some(error.to_string()),
        Err(_) => {
            return Some(format!(
                "did not answer --help within {}s",
                PROBE_TIMEOUT.as_secs()
            ))
        }
    };

    if output.status.success() {
        return None;
    }

    Some(last_meaningful_line(&output))
}

#[cfg(not(target_os = "linux"))]
async fn refuses_to_start(_path: &Path) -> Option<String> {
    None
}

/// How long the client gets to answer. Generous next to the half second it
/// takes on a warm machine, because a cold Python interpreter on a slow disk
/// is much worse than that, and short enough that a wedged client delays
/// start-up rather than hanging it.
#[cfg(target_os = "linux")]
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The last line the client printed, preferring stderr.
///
/// The last rather than the first: a Python traceback opens with "Traceback
/// (most recent call last):" and closes with the line that names the error,
/// and only the second one tells anybody anything.
#[cfg(target_os = "linux")]
fn last_meaningful_line(output: &std::process::Output) -> String {
    let last = |raw: &[u8]| {
        String::from_utf8_lossy(raw)
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .map(ToOwned::to_owned)
    };

    last(&output.stderr)
        .or_else(|| last(&output.stdout))
        .unwrap_or_else(|| output.status.to_string())
}

// Linux only, like the probe itself: these drive a shell script standing in
// for the client.
#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::core::paths::AppPaths;

    static NEXT_TEST_DIR: AtomicUsize = AtomicUsize::new(0);

    /// A dependency pointed at `script` through the manual override, so the
    /// test never depends on what is or is not installed on the machine.
    fn pointed_at(script: &str) -> SyncplayClientDependency {
        let id = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("syncparty-client-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");

        let client = dir.join("syncplay");
        std::fs::write(&client, script).expect("fake client");
        std::fs::set_permissions(&client, std::fs::Permissions::from_mode(0o755))
            .expect("the fake client must be executable");

        let settings = Arc::new(ConfigStore::load(AppPaths::rooted_at(dir)).expect("settings"));
        settings
            .set_executable_override(
                SYNCPLAY_CLIENT_KEY,
                Some(client.to_string_lossy().into_owned()),
            )
            .expect("override");

        SyncplayClientDependency::new(settings)
    }

    #[tokio::test]
    async fn a_client_that_cannot_start_is_unusable_rather_than_installed() {
        // What Ubuntu 24.04's Syncplay 1.7.0 does on Python 3.12.
        let dependency = pointed_at(
            "#!/bin/sh\n\
             echo 'Traceback (most recent call last):' >&2\n\
             echo \"ImportError: cannot import name 'SafeConfigParser'\" >&2\n\
             exit 1\n",
        );

        match dependency.detect().await {
            DependencyStatus::Unusable { reason, .. } => assert_eq!(
                reason, "ImportError: cannot import name 'SafeConfigParser'",
                "the last line is the one worth showing, not the traceback"
            ),
            other => panic!("expected Unusable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_client_that_starts_is_installed() {
        let dependency = pointed_at("#!/bin/sh\nexit 0\n");

        assert!(dependency.detect().await.is_installed());
    }
}
