//! The command line mpv is started with.
//!
//! Command-line options win over `mpv.conf`, so everything the driver depends
//! on is set here and holds whatever the user has configured. Everything else
//! — their scaling, subtitles, key bindings, scripts — is left alone in a
//! window, because it is their player.

use std::path::Path;

use crate::core::player::LaunchMode;

/// The window title. mpv expands properties in option values, so this
/// follows whatever is open.
const TITLE: &str = "--title=${?media-title:${media-title} - }syncparty";

/// What every mpv syncparty drives needs, window or not.
const ALWAYS: &[&str] = &[
    // No playlist advance and no quit at the end of a file: the room decides
    // what happens next, and the file stays open on its last frame.
    "--keep-open=always",
    // Paused at the end, as Syncplay has it. Without the pause mpv says it is
    // playing while it stands on the last frame, and the room position
    // running on past it looks like falling out of sync.
    "--keep-open-pause=yes",
    "--hr-seek=yes",
    // The first file starts paused. Pause is set globally, never as a
    // per-file option: mpv restores per-file options when the file ends,
    // which shows up as pause changes nobody made.
    "--pause",
    "--no-terminal",
    // A position saved by quit-watch-later would be restored on the next
    // load, and look like the user seeking.
    "--resume-playback=no",
    // A loop starts the film again from zero, which also looks like a seek.
    "--loop-file=no",
    "--loop-playlist=no",
    "--speed=1",
    // A file only ever opens itself. Without this a playlist — any file
    // starting "#EXTM3U", whatever it is called — makes mpv open what it
    // lists, URLs included, within the same millisecond; and a user's
    // `autocreate-playlist` turns every film into a playlist of its folder,
    // so the load syncparty asked for never comes back as its own. Both were
    // seen on mpv 0.41; with this, both fail or play as the one file. The
    // cost is ordered chapters and files inside archives, which a party
    // whose members each open their own copy is better off without anyway.
    "--access-references=no",
];

/// The arguments for one mpv listening on `endpoint`.
pub(super) fn arguments(executable: &Path, mode: LaunchMode, endpoint: &str) -> Vec<String> {
    let mut arguments = vec![format!("--input-ipc-server={endpoint}")];

    match mode {
        LaunchMode::Window => {
            // A window from the start, so there is something on screen to
            // drop a file on before one is loaded.
            arguments.extend(["--idle=yes", "--force-window=yes"].map(str::to_owned));
        }
        LaunchMode::Headless => {
            arguments.extend(
                [
                    "--no-config",
                    "--load-scripts=no",
                    "--idle=yes",
                    "--vo=null",
                    "--ao=null",
                    "--ytdl=no",
                ]
                .map(str::to_owned),
            );
        }
    }

    arguments.extend(ALWAYS.iter().map(|argument| (*argument).to_owned()));
    arguments.push(TITLE.to_owned());

    if is_mpv_net(executable) {
        // mpv.net otherwise queues every file in the same folder after the
        // one loaded, and may hand the launch to a copy already running and
        // exit — leaving syncparty connected to nothing.
        arguments.extend(["--auto-load-folder=no", "--process-instance=multi"].map(str::to_owned));
    }

    arguments
}

/// mpv.net is a different front end around the same player, and takes the
/// same options plus a few of its own.
fn is_mpv_net(executable: &Path) -> bool {
    executable
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("mpvnet"))
}

/// The `PATH` an mpv started from a macOS app bundle should see.
///
/// An app launched from Finder gets a minimal `PATH` without Homebrew in it,
/// so mpv's own helpers there — `yt-dlp`, mostly — would not be found.
#[cfg(any(target_os = "macos", test))]
pub(super) fn child_path(current: Option<std::ffi::OsString>) -> Option<std::ffi::OsString> {
    let mut directories: Vec<std::path::PathBuf> = current
        .as_deref()
        .map(|path| std::env::split_paths(path).collect())
        .unwrap_or_default();

    for extra in ["/opt/homebrew/bin", "/usr/local/bin"] {
        if !directories
            .iter()
            .any(|directory| directory == Path::new(extra))
        {
            directories.push(extra.into());
        }
    }

    std::env::join_paths(directories).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(executable: &str, mode: LaunchMode) -> Vec<String> {
        arguments(Path::new(executable), mode, "/run/user/1000/sp/mpv.sock")
    }

    #[test]
    fn a_window_keeps_the_users_configuration_and_opens_before_a_file_does() {
        let arguments = argv("/usr/bin/mpv", LaunchMode::Window);

        assert_eq!(
            arguments[0],
            "--input-ipc-server=/run/user/1000/sp/mpv.sock"
        );
        for expected in [
            "--idle=yes",
            "--force-window=yes",
            "--keep-open=always",
            "--keep-open-pause=yes",
            "--hr-seek=yes",
            "--pause",
            "--no-terminal",
            "--resume-playback=no",
            "--loop-file=no",
            "--loop-playlist=no",
            "--access-references=no",
        ] {
            assert!(
                arguments.iter().any(|a| a == expected),
                "{expected} missing: {arguments:?}"
            );
        }
        assert!(arguments.iter().any(|a| a.starts_with("--title=")));
        assert!(!arguments.iter().any(|a| a == "--no-config"));
        assert!(!arguments.iter().any(|a| a.starts_with("--vo")));
    }

    #[test]
    fn headless_runs_without_a_window_sound_or_the_users_configuration() {
        let arguments = argv("/usr/bin/mpv", LaunchMode::Headless);

        for expected in [
            "--no-config",
            "--load-scripts=no",
            "--vo=null",
            "--ao=null",
            "--ytdl=no",
            "--idle=yes",
            "--keep-open=always",
            "--keep-open-pause=yes",
            "--pause",
            "--access-references=no",
        ] {
            assert!(
                arguments.iter().any(|a| a == expected),
                "{expected} missing: {arguments:?}"
            );
        }
        assert!(!arguments.iter().any(|a| a.starts_with("--force-window")));
    }

    #[test]
    fn mpv_net_is_kept_to_one_instance_and_one_file() {
        // Forward slashes so the path splits the same way on every platform
        // the tests run on; mpv.net itself only exists on Windows.
        let arguments = argv("mpv.net/mpvnet.exe", LaunchMode::Window);
        assert!(arguments.iter().any(|a| a == "--auto-load-folder=no"));
        assert!(arguments.iter().any(|a| a == "--process-instance=multi"));

        let plain = argv("/usr/bin/mpv", LaunchMode::Window);
        assert!(!plain.iter().any(|a| a.starts_with("--auto-load-folder")));
    }

    #[test]
    fn no_argument_carries_anything_secret_or_a_file_to_open() {
        // The file is loaded over IPC once mpv is up, never named on the
        // command line, so argv holds nothing but options.
        for mode in [LaunchMode::Window, LaunchMode::Headless] {
            assert!(argv("/usr/bin/mpv", mode)
                .iter()
                .all(|a| a.starts_with("--")));
        }
    }

    #[test]
    fn homebrew_is_added_to_the_path_once() {
        let path = child_path(Some("/usr/bin:/bin:/usr/local/bin".into())).expect("path");
        let directories: Vec<_> = std::env::split_paths(&path).collect();

        assert_eq!(
            directories,
            ["/usr/bin", "/bin", "/usr/local/bin", "/opt/homebrew/bin"]
                .map(std::path::PathBuf::from)
        );
        assert!(child_path(None).is_some());
    }
}
