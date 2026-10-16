//! mpv as a managed dependency.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::core::config::ConfigStore;
use crate::core::deps::installer::{install_and_verify, PackageManagedInstall, PackageSpec};
use crate::core::deps::{Dependency, DependencyId, DependencyStatus, ModeRequirement};
use crate::core::error::Result;
use crate::core::events::ProgressSink;
use crate::core::player::{find_mpv, MPV_KEY};
use crate::core::process;

const DISPLAY_NAME: &str = "mpv";
const MANUAL_URL: &str = "https://mpv.io/installation/";

/// Where mpv comes from on each platform.
const SPEC: PackageSpec = PackageSpec {
    winget_id: Some("shinchiro.mpv"),
    brew_formula: Some("mpv"),
};

/// How long `mpv --version` gets to answer. It takes a few tens of
/// milliseconds on a warm machine; the margin is for a first run that the
/// operating system scans before letting it start, and a player that hangs
/// still only delays the checklist rather than wedging it.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

pub struct MpvDependency {
    installer: PackageManagedInstall,
    settings: Arc<ConfigStore>,
    probe_timeout: Duration,
}

impl MpvDependency {
    pub fn new(settings: Arc<ConfigStore>) -> Self {
        Self {
            installer: PackageManagedInstall {
                display_name: DISPLAY_NAME,
                spec: SPEC,
            },
            settings,
            probe_timeout: PROBE_TIMEOUT,
        }
    }

    /// The same dependency, giving `mpv --version` only `timeout` to answer,
    /// so a test of a player that never does can finish quickly.
    #[cfg(test)]
    fn with_probe_timeout(mut self, timeout: Duration) -> Self {
        self.probe_timeout = timeout;
        self
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

    /// The host watches along with everyone else, so both modes need it.
    fn required_for(&self) -> ModeRequirement {
        ModeRequirement::Both
    }

    /// Finding a file is not enough. A location the user chose by hand is
    /// taken at its word, and older versions of syncparty accepted VLC there,
    /// so a location saved back then can point at a perfectly good program
    /// that is not mpv. Running it is the only way to tell.
    async fn detect(&self) -> DependencyStatus {
        let manual = self.settings.executable_override(MPV_KEY);

        let Some(path) = find_mpv(manual.as_deref()) else {
            return DependencyStatus::Missing;
        };

        identify(&path, self.probe_timeout).await
    }

    async fn install(&self, progress: &dyn ProgressSink) -> Result<()> {
        install_and_verify(self, &self.installer, progress).await
    }

    fn manual_url(&self) -> &str {
        MANUAL_URL
    }

    fn needs_elevation(&self) -> bool {
        false
    }

    async fn can_auto_install(&self) -> bool {
        self.installer.is_supported()
    }

    /// mpv is very often a portable build sitting in a folder somewhere.
    fn supports_manual_path(&self) -> bool {
        true
    }

    fn manual_path_key(&self) -> Option<&'static str> {
        Some(MPV_KEY)
    }
}

/// Runs `path --version` and decides whether what answered is mpv.
///
/// mpv opens its version banner with `mpv ` — `mpv v0.41.0 Copyright © …`
/// today, `mpv 0.34.1 Copyright © …` a few releases ago — and nothing else
/// likely to sit at a chosen path does. VLC, the likeliest stranger, opens
/// with `VLC media player`.
async fn identify(path: &Path, timeout: Duration) -> DependencyStatus {
    let shown = path.to_string_lossy().into_owned();

    // mpv.net is its own front end around libmpv, and its `--version` cannot
    // be relied on to print mpv's banner — or anything — into a pipe. It is
    // only ever found under its own name, so the name is taken as the answer.
    if is_mpv_net(path) {
        return DependencyStatus::Installed {
            version: None,
            path: Some(shown),
        };
    }

    let run = process::command(path)
        .arg("--version")
        // Without this a probe that times out leaves the program running with
        // nobody left to wait for it.
        .kill_on_drop(true)
        .output();

    let output = match tokio::time::timeout(timeout, run).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            return DependencyStatus::Unusable {
                path: shown,
                reason: error.to_string(),
            };
        }
        Err(_) => {
            return DependencyStatus::Unusable {
                path: shown,
                // Duration's Debug form is the readable one: "10s", "300ms".
                reason: format!("did not answer --version within {timeout:?}"),
            };
        }
    };

    if !output.status.success() {
        return DependencyStatus::Unusable {
            path: shown,
            reason: last_meaningful_line(&output),
        };
    }

    let banner = first_line(&output.stdout).or_else(|| first_line(&output.stderr));

    match banner.as_deref().and_then(|line| line.strip_prefix("mpv ")) {
        Some(rest) => DependencyStatus::Installed {
            version: version_in(rest),
            path: Some(shown),
        },
        None => DependencyStatus::Unusable {
            path: shown,
            reason: match banner {
                Some(line) => format!("not mpv: --version printed \"{line}\""),
                None => "not mpv: --version printed nothing".to_owned(),
            },
        },
    }
}

fn is_mpv_net(path: &Path) -> bool {
    path.file_stem()
        .is_some_and(|stem| stem.eq_ignore_ascii_case("mpvnet"))
}

/// The version number out of what follows `mpv ` in the banner.
///
/// Just the number, because the checklist puts it after the name: "mpv
/// 0.41.0" rather than "mpv mpv v0.41.0 Copyright © 2000-2025 …".
fn version_in(rest: &str) -> Option<String> {
    let token = rest.split_whitespace().next()?;
    let number = token.strip_prefix('v').unwrap_or(token);

    (!number.is_empty()).then(|| number.to_owned())
}

fn first_line(raw: &[u8]) -> Option<String> {
    String::from_utf8_lossy(raw)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToOwned::to_owned)
}

/// The last line the program printed, preferring stderr.
///
/// The last rather than the first: a program that fails to start tends to say
/// what it was doing first and why it stopped last, and only the second one
/// tells anybody anything.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::paths::AppPaths;

    #[test]
    fn mpv_names_itself_to_both_package_managers() {
        assert_eq!(SPEC.winget_id, Some("shinchiro.mpv"));
        assert_eq!(
            SPEC.brew_formula,
            Some("mpv"),
            "the Homebrew formula: the mpv cask is disabled"
        );
    }

    #[test]
    fn the_checklist_calls_it_mpv() {
        let paths = AppPaths::rooted_at(std::env::temp_dir().join("syncparty-mpv-name-test"));
        let settings = Arc::new(ConfigStore::load(paths).expect("settings"));

        assert_eq!(MpvDependency::new(settings).display_name(), "mpv");
    }

    #[test]
    fn the_version_is_read_out_of_every_banner_style() {
        assert_eq!(
            version_in("v0.41.0 Copyright © 2000-2025 mpv/MPlayer/mplayer2 projects").as_deref(),
            Some("0.41.0")
        );
        assert_eq!(
            version_in("0.34.1 Copyright © 2000-2021 mpv/MPlayer/mplayer2 projects").as_deref(),
            Some("0.34.1"),
            "older releases print the number without a v"
        );
        assert_eq!(
            version_in("v0.41.0-77-g1a2b3c4d Copyright").as_deref(),
            Some("0.41.0-77-g1a2b3c4d"),
            "a build from git keeps its suffix"
        );
        assert_eq!(version_in("").as_deref(), None);
    }

    #[test]
    fn mpv_net_is_recognised_by_its_name() {
        assert!(is_mpv_net(Path::new("mpvnet.exe")));
        assert!(is_mpv_net(Path::new("/opt/mpv.net/MpvNet")));
        assert!(!is_mpv_net(Path::new("/usr/bin/mpv")));
        assert!(!is_mpv_net(Path::new("mpv.exe")));
    }

    /// These drive shell scripts standing in for mpv, pointed at through the
    /// manual override exactly as a user would, so they depend on nothing
    /// being installed on the machine.
    #[cfg(unix)]
    mod probe {
        use std::os::unix::fs::PermissionsExt;
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Instant;

        use super::*;

        static NEXT_TEST_DIR: AtomicUsize = AtomicUsize::new(0);

        /// A dependency whose override points at `script`, saved as `name`.
        fn script_named(name: &str, script: &str) -> (PathBuf, MpvDependency) {
            let id = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("syncparty-mpv-{}-{id}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("temp dir");

            let player = dir.join(name);
            std::fs::write(&player, script).expect("fake player");
            std::fs::set_permissions(&player, std::fs::Permissions::from_mode(0o755))
                .expect("the fake player must be executable");

            let settings = Arc::new(ConfigStore::load(AppPaths::rooted_at(dir)).expect("settings"));
            settings
                .set_executable_override(MPV_KEY, Some(player.to_string_lossy().into_owned()))
                .expect("override");

            (player, MpvDependency::new(settings))
        }

        fn pointed_at(script: &str) -> (PathBuf, MpvDependency) {
            script_named("mpv", script)
        }

        fn shown(path: &Path) -> String {
            path.to_string_lossy().into_owned()
        }

        #[tokio::test]
        async fn an_mpv_banner_is_installed_with_its_version() {
            // What mpv 0.41 prints, first two lines.
            let (player, dependency) = pointed_at(
                "#!/bin/sh\n\
                 echo 'mpv v0.41.0 Copyright © 2000-2025 mpv/MPlayer/mplayer2 projects'\n\
                 echo ' built on Aug 29 2026 22:13:49'\n",
            );

            assert_eq!(
                dependency.detect().await,
                DependencyStatus::Installed {
                    version: Some("0.41.0".to_owned()),
                    path: Some(shown(&player)),
                }
            );
        }

        /// A location saved when syncparty still accepted VLC: a program that
        /// runs perfectly well and would fail the moment it was asked to play.
        #[tokio::test]
        async fn a_location_that_turns_out_to_be_vlc_is_unusable() {
            let (player, dependency) = pointed_at(
                "#!/bin/sh\n\
                 echo 'VLC media player 3.0.21 Vetinari (revision 3.0.21-0-gdd8bfdbabe8)'\n",
            );

            match dependency.detect().await {
                DependencyStatus::Unusable { path, reason } => {
                    assert_eq!(path, shown(&player));
                    assert!(
                        reason.starts_with("not mpv") && reason.contains("VLC media player 3.0"),
                        "the reason has to say what is there instead: {reason}"
                    );
                }
                other => panic!("expected Unusable, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn a_program_that_prints_nothing_is_not_taken_for_mpv() {
            let (player, dependency) = pointed_at("#!/bin/sh\nexit 0\n");

            assert_eq!(
                dependency.detect().await,
                DependencyStatus::Unusable {
                    path: shown(&player),
                    reason: "not mpv: --version printed nothing".to_owned(),
                }
            );
        }

        #[tokio::test]
        async fn an_mpv_that_fails_to_start_reports_its_last_words() {
            let (_, dependency) = pointed_at(
                "#!/bin/sh\n\
                 echo 'mpv: starting' >&2\n\
                 echo 'mpv: error while loading shared libraries: libplacebo.so.351' >&2\n\
                 exit 1\n",
            );

            match dependency.detect().await {
                DependencyStatus::Unusable { reason, .. } => assert_eq!(
                    reason, "mpv: error while loading shared libraries: libplacebo.so.351",
                    "the last line is the one that names the problem"
                ),
                other => panic!("expected Unusable, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn an_mpv_that_never_answers_is_given_up_on() {
            // `exec`, so the process the probe kills is the one that hangs,
            // rather than a shell that would leave an orphaned sleep behind.
            let (_, dependency) = pointed_at("#!/bin/sh\nexec sleep 30\n");
            let dependency = dependency.with_probe_timeout(Duration::from_millis(300));

            let started = Instant::now();
            let status = dependency.detect().await;

            assert!(
                started.elapsed() < Duration::from_secs(10),
                "the timeout has to cut the probe short"
            );
            match status {
                DependencyStatus::Unusable { reason, .. } => assert_eq!(
                    reason, "did not answer --version within 300ms",
                    "the reason says how long it waited"
                ),
                other => panic!("expected Unusable, got {other:?}"),
            }
        }

        #[tokio::test]
        async fn mpv_net_is_taken_at_its_name_without_being_run() {
            // Unusable twice over if it were run: it fails, and it is not mpv.
            let (player, dependency) =
                script_named("mpvnet", "#!/bin/sh\necho 'mpv.net 7.1.1.0'\nexit 1\n");

            assert_eq!(
                dependency.detect().await,
                DependencyStatus::Installed {
                    version: None,
                    path: Some(shown(&player)),
                }
            );
        }

        #[tokio::test]
        async fn a_file_that_cannot_be_executed_is_unusable_rather_than_missing() {
            let (player, dependency) = pointed_at("not a program");
            std::fs::set_permissions(&player, std::fs::Permissions::from_mode(0o644))
                .expect("drop the executable bit");

            assert!(matches!(
                dependency.detect().await,
                DependencyStatus::Unusable { .. }
            ));
        }
    }
}
