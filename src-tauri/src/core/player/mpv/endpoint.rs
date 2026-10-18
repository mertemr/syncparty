//! Where mpv listens for syncparty.
//!
//! mpv's IPC is explicitly insecure: anything that connects can make it run
//! a program. So the endpoint must be one only this user can reach, and a
//! fresh one for every player — a second mpv told to listen on a path that is
//! already taken quietly takes it over, and the first one can no longer be
//! reached.
//!
//! On Unix that means a socket inside a directory created for this player
//! alone, mode 0700. The directory is created without `recursive`, so if
//! anything already sits at that name the creation fails rather than
//! adopting it, and the name is random, so nobody can sit there in advance.
//! On Windows the pipe namespace has no directories, so the pipe name itself
//! carries the randomness.

#[cfg(unix)]
use std::path::{Path, PathBuf};

use crate::core::error::{Result, SyncPartyError};

/// The longest socket path the platform accepts. `sun_path` is 108 bytes on
/// Linux and 104 on macOS and the BSDs, terminator included. mpv does not
/// complain about a path that does not fit — it starts and simply has no
/// socket — so the check has to happen here.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(super) const MAX_SOCKET_PATH: usize = 107;
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
pub(super) const MAX_SOCKET_PATH: usize = 103;

/// The socket's name inside its private directory.
#[cfg(unix)]
const SOCKET_NAME: &str = "mpv.sock";

/// A private place for one mpv to listen, removed again once it is done.
#[derive(Debug)]
pub(super) struct Endpoint {
    name: String,
    /// The socket and the directory around it. `None` once removed.
    #[cfg(unix)]
    files: Option<(PathBuf, PathBuf)>,
}

impl Endpoint {
    /// What mpv is given as `--input-ipc-server`, and what a client opens.
    pub(super) fn name(&self) -> &str {
        &self.name
    }

    #[cfg(windows)]
    pub(super) fn create() -> Result<Self> {
        Ok(Self {
            name: format!(
                r"\\.\pipe\syncparty-mpv-{}-{}",
                std::process::id(),
                random_hex()?
            ),
        })
    }

    #[cfg(unix)]
    pub(super) fn create() -> Result<Self> {
        Self::create_in(&candidate_bases())
    }

    /// Makes the private directory under the first base that can hold it.
    ///
    /// The runtime directory comes first because it is already private to
    /// this user; the temporary directories after it are shared, which the
    /// 0700 directory makes safe. A base is skipped when the socket path would
    /// be too long there — a deep `$TMPDIR` on macOS gets close — and only if
    /// none fits is that an error.
    #[cfg(unix)]
    pub(super) fn create_in(bases: &[PathBuf]) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;

        let mut refusal = None;

        for base in bases {
            for _ in 0..4 {
                let dir = base.join(format!(
                    "syncparty-mpv-{}-{}",
                    std::process::id(),
                    random_hex()?
                ));
                let socket = dir.join(SOCKET_NAME);

                // The name crosses into mpv's argv and back out of
                // `MpvPlayer::endpoint` as text.
                let Some(name) = socket.to_str().map(str::to_owned) else {
                    break;
                };
                if name.len() > MAX_SOCKET_PATH {
                    refusal = Some(too_long(&name));
                    break;
                }

                match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
                    Ok(()) => {
                        return Ok(Self {
                            name,
                            files: Some((socket, dir)),
                        })
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => {
                        refusal = Some(SyncPartyError::PlayerFailed(format!(
                            "could not create a private folder for mpv's control socket in {}: \
                             {error}",
                            base.display()
                        )));
                        break;
                    }
                }
            }
        }

        Err(refusal.unwrap_or_else(|| {
            SyncPartyError::PlayerFailed(
                "found no folder to put mpv's control socket in".to_owned(),
            )
        }))
    }

    /// Deletes the socket and its directory. mpv never removes its socket,
    /// not even on a clean quit, so without this every film would leave one
    /// behind.
    pub(super) fn remove(mut self) {
        self.clean_up();
    }

    fn clean_up(&mut self) {
        #[cfg(unix)]
        if let Some((socket, dir)) = self.files.take() {
            remove_quietly(&socket, &dir);
        }
    }
}

impl Drop for Endpoint {
    /// The same clean-up when the task that owns the endpoint is torn down
    /// before it got to [`Endpoint::remove`] — at runtime shutdown, say.
    fn drop(&mut self) {
        self.clean_up();
    }
}

#[cfg(unix)]
fn remove_quietly(socket: &Path, dir: &Path) {
    if let Err(error) = std::fs::remove_file(socket) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::debug!(%error, "could not remove mpv's control socket");
        }
    }
    if let Err(error) = std::fs::remove_dir(dir) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::debug!(%error, "could not remove mpv's socket folder");
        }
    }
}

#[cfg(unix)]
fn candidate_bases() -> Vec<PathBuf> {
    let mut bases = Vec::new();

    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        bases.push(runtime);
    }
    for shared in [std::env::temp_dir(), PathBuf::from("/tmp")] {
        if !bases.contains(&shared) {
            bases.push(shared);
        }
    }

    bases
}

#[cfg(unix)]
fn too_long(path: &str) -> SyncPartyError {
    SyncPartyError::PlayerFailed(format!(
        "mpv's control socket would be at {path}, which is {} bytes long; a socket path can be \
         at most {MAX_SOCKET_PATH} bytes here. Point TMPDIR at a shorter folder.",
        path.len()
    ))
}

/// 64 random bits, as 16 hex digits.
fn random_hex() -> Result<String> {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).map_err(|error| {
        SyncPartyError::Other(format!("no secure randomness available: {error}"))
    })?;

    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// Under `/tmp` rather than the temporary directory: on macOS that is a
    /// deep `/var/folders/…` path, and one more folder of the test's own on
    /// top of it is enough to leave no room for the socket — which is exactly
    /// why `/tmp` is one of the bases the real thing falls back to.
    fn scratch(label: &str) -> PathBuf {
        let dir = PathBuf::from("/tmp").join(format!("spe-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn each_player_gets_a_fresh_directory_only_this_user_can_enter() {
        let base = scratch("private");

        let first = Endpoint::create_in(std::slice::from_ref(&base)).expect("first");
        let second = Endpoint::create_in(std::slice::from_ref(&base)).expect("second");
        assert_ne!(first.name(), second.name());

        let dir = Path::new(first.name()).parent().expect("parent");
        assert_eq!(dir.parent(), Some(base.as_path()));
        let mode = std::fs::metadata(dir)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "mode {mode:o}");
        assert!(first.name().len() <= MAX_SOCKET_PATH);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn removing_the_endpoint_takes_the_socket_and_its_directory_with_it() {
        let base = scratch("removed");
        let endpoint = Endpoint::create_in(std::slice::from_ref(&base)).expect("endpoint");
        let socket = PathBuf::from(endpoint.name());
        let dir = socket.parent().expect("parent").to_path_buf();
        std::fs::write(&socket, b"").expect("stand-in socket");

        endpoint.remove();

        assert!(!socket.exists());
        assert!(!dir.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_base_too_deep_for_a_socket_path_is_passed_over_for_the_next() {
        let deep = PathBuf::from(format!("/{}", "d".repeat(MAX_SOCKET_PATH)));
        let base = scratch("fallback");

        let endpoint = Endpoint::create_in(&[deep, base.clone()]).expect("endpoint");

        assert!(endpoint.name().starts_with(base.to_str().expect("utf8")));
        drop(endpoint);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_socket_path_that_cannot_fit_anywhere_is_refused_with_the_reason() {
        let deep = PathBuf::from(format!("/{}", "d".repeat(MAX_SOCKET_PATH)));

        let error = Endpoint::create_in(&[deep]).expect_err("too long");

        let message = error.to_string();
        assert!(
            message.contains(&format!("at most {MAX_SOCKET_PATH} bytes")),
            "{message}"
        );
        assert_eq!(error.kind(), "player_failed");
    }

    #[test]
    fn the_runtime_directory_is_preferred_when_there_is_one() {
        let bases = candidate_bases();

        match std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
            Some(runtime) if runtime.is_absolute() => assert_eq!(bases[0], runtime),
            _ => assert_eq!(bases[0], std::env::temp_dir()),
        }
        assert!(bases.contains(&PathBuf::from("/tmp")));
    }
}
