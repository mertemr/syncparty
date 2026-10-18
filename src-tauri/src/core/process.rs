//! Running external programs without flashing console windows at the user.
//!
//! Every subprocess syncparty starts — `winget`, `brew`, mpv — goes through
//! here. On Windows a bare
//! [`tokio::process::Command`] pops a console window for a fraction of a
//! second, which looks broken in a GUI app; `CREATE_NO_WINDOW` suppresses it.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;

use tokio::process::Command;

use crate::core::error::{Result, SyncPartyError};

/// Windows process creation flag that suppresses the console window.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Builds a [`Command`] that stays invisible and captures both output streams.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    command
}

/// Builds a [`Command`] for a long-lived program syncparty talks to some
/// other way than through its output — mpv, which is driven over a socket.
///
/// All three streams are null rather than piped: nobody reads a pipe here,
/// and a child that writes enough into an unread one blocks once the pipe is
/// full. The process is killed if its handle is dropped, so a syncparty that
/// goes away mid-film does not leave behind a player nothing controls.
pub fn quiet(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);

    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    command
}

pub struct CapturedOutput {
    pub stdout: String,
    pub stderr: String,
}

/// Runs a program to completion and returns its output, failing on a non-zero
/// exit status.
pub async fn capture<I, S>(program: impl AsRef<OsStr>, args: I) -> Result<CapturedOutput>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let program_name = program.as_ref().to_string_lossy().into_owned();
    let output = command(program)
        .args(args)
        .output()
        .await
        .map_err(|error| SyncPartyError::CommandFailed {
            command: program_name.clone(),
            status: "could not start".to_owned(),
            stderr: error.to_string(),
        })?;

    let captured = CapturedOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    };

    if !output.status.success() {
        return Err(SyncPartyError::CommandFailed {
            command: program_name,
            status: output.status.to_string(),
            stderr: first_meaningful_line(&captured.stderr, &captured.stdout),
        });
    }

    Ok(captured)
}

/// Resolves an executable, preferring `PATH` and falling back to a list of
/// well-known install locations.
///
/// mpv installs somewhere predictable but does not reliably add itself to
/// `PATH`, so looking in both places is what makes detection work on a stock
/// machine.
pub fn locate(binary: &str, fallbacks: &[&str]) -> Option<std::path::PathBuf> {
    if let Ok(found) = which::which(binary) {
        return Some(found);
    }

    fallbacks
        .iter()
        .map(Path::new)
        .find(|candidate| candidate.is_file())
        .map(Path::to_path_buf)
}

/// Turns a path the user picked into an executable.
///
/// Accepts either the program itself or a directory containing it. Portable
/// builds arrive as an extracted folder, and "where is mpv?" is a question
/// people answer with a folder — so both are treated as valid answers.
pub fn resolve_manual(raw: &str, binary: &str) -> Option<std::path::PathBuf> {
    let candidate = Path::new(raw.trim());

    if candidate.is_file() {
        return Some(candidate.to_path_buf());
    }

    if candidate.is_dir() {
        let executable = if cfg!(windows) {
            format!("{binary}.exe")
        } else {
            binary.to_owned()
        };

        // macOS application bundles keep the binary several levels down.
        for relative in [
            executable.as_str(),
            &format!("bin/{executable}"),
            &format!("Contents/MacOS/{binary}"),
        ] {
            let inside = candidate.join(relative);
            if inside.is_file() {
                return Some(inside);
            }
        }
    }

    None
}

/// Picks the most useful line to show the user when a command fails. Some
/// tools write the real reason to stdout and leave stderr empty.
fn first_meaningful_line(stderr: &str, stdout: &str) -> String {
    stderr
        .lines()
        .chain(stdout.lines())
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("no output")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_stderr_then_falls_back_to_stdout() {
        assert_eq!(first_meaningful_line("  \nboom\n", "ignored"), "boom");
        assert_eq!(first_meaningful_line("", "  \nfallback"), "fallback");
        assert_eq!(first_meaningful_line("", ""), "no output");
    }

    #[test]
    fn locate_returns_none_when_nothing_matches() {
        assert!(locate("syncparty-does-not-exist", &["/nope/also-not-here"]).is_none());
    }

    fn scratch(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("syncparty-manual-{label}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent");
        }
        std::fs::write(path, b"").expect("touch");
    }

    fn binary_name(stem: &str) -> String {
        if cfg!(windows) {
            format!("{stem}.exe")
        } else {
            stem.to_owned()
        }
    }

    #[test]
    fn accepts_the_executable_itself() {
        let dir = scratch("file");
        let executable = dir.join(binary_name("mpv"));
        touch(&executable);

        assert_eq!(
            resolve_manual(executable.to_str().expect("utf8"), "mpv"),
            Some(executable)
        );
    }

    #[test]
    fn accepts_the_folder_a_portable_build_was_extracted_into() {
        let dir = scratch("folder");
        let executable = dir.join(binary_name("mpv"));
        touch(&executable);

        assert_eq!(
            resolve_manual(dir.to_str().expect("utf8"), "mpv"),
            Some(executable)
        );
    }

    #[test]
    fn looks_inside_bin_and_macos_bundles() {
        let bin_dir = scratch("bin");
        let in_bin = bin_dir.join("bin").join(binary_name("mpv"));
        touch(&in_bin);
        assert_eq!(
            resolve_manual(bin_dir.to_str().expect("utf8"), "mpv"),
            Some(in_bin)
        );

        let bundle = scratch("bundle");
        let in_bundle = bundle.join("Contents").join("MacOS").join("mpv");
        touch(&in_bundle);
        assert_eq!(
            resolve_manual(bundle.to_str().expect("utf8"), "mpv"),
            Some(in_bundle)
        );
    }

    #[test]
    fn tolerates_surrounding_whitespace_from_a_pasted_path() {
        let dir = scratch("padded");
        let executable = dir.join(binary_name("mpv"));
        touch(&executable);

        let padded = format!("  {}  ", dir.to_str().expect("utf8"));
        assert_eq!(resolve_manual(&padded, "mpv"), Some(executable));
    }

    #[test]
    fn rejects_a_folder_without_the_program_in_it() {
        let dir = scratch("empty");

        assert!(resolve_manual(dir.to_str().expect("utf8"), "mpv").is_none());
        assert!(resolve_manual("", "mpv").is_none());
        assert!(resolve_manual("/definitely/not/here", "mpv").is_none());
    }
}
