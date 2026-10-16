//! The video player syncparty drives.
//!
//! syncparty used to launch the Syncplay desktop client and let it drive the
//! player. It drives the player itself now, so this is the seam between the
//! sync logic and whatever is actually showing the film: the sync client only
//! ever sees a [`Player`], and only `mpv` knows that the thing behind it is a
//! process at the other end of a JSON socket.
//!
//! The trait exists for the same reason `ServerController` does — so the
//! decision-making above it can be tested with a fake that records what it
//! was told, without a window opening anywhere.

pub mod mpv;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::core::error::Result;
use crate::core::process;

pub use mpv::{MpvLauncher, MpvPlayer};

/// Settings key under which a manually chosen mpv is stored. Unchanged from
/// when the Syncplay launcher owned it, so an override set by an older
/// version keeps working.
pub const MPV_KEY: &str = "mpv";

#[cfg(windows)]
const MPV_FALLBACKS: &[&str] = &[
    // winget's `shinchiro.mpv` installs here and does not touch `PATH`, so
    // without this entry an automatic install finishes and then reports the
    // player missing.
    r"C:\Program Files\MPV Player\mpv.exe",
    r"C:\Program Files\mpv\mpv.exe",
    r"C:\Program Files\mpv.net\mpvnet.exe",
];

#[cfg(target_os = "macos")]
const MPV_FALLBACKS: &[&str] = &[
    "/Applications/mpv.app/Contents/MacOS/mpv",
    "/opt/homebrew/bin/mpv",
    "/usr/local/bin/mpv",
];

#[cfg(not(any(windows, target_os = "macos")))]
const MPV_FALLBACKS: &[&str] = &["/usr/bin/mpv", "/usr/local/bin/mpv"];

/// Locates mpv.
///
/// A path the user set by hand wins over everything else — they told us where
/// it is, so second-guessing them would be strange. Whether what they pointed
/// at really is mpv is the dependency check's question, not this one's.
pub fn find_mpv(manual: Option<&str>) -> Option<PathBuf> {
    manual
        .and_then(|raw| process::resolve_manual(raw, "mpv"))
        .or_else(|| process::locate("mpv", MPV_FALLBACKS))
}

/// The file the player has open, once it has actually started playing it.
///
/// Only reported after the first `playback-restart` following a load: mpv
/// does not know the duration or the size any earlier than that, and a file
/// announced without them would be announced twice.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedFile {
    /// As mpv reports it. Lossily decoded, because mpv passes through bytes
    /// that are not UTF-8 and JSON cannot carry them.
    pub path: String,
    /// The last component of the path — what the room is told.
    pub name: String,
    pub duration: Option<f64>,
    /// Absent for anything that is not a plain file.
    pub size: Option<u64>,
}

/// What the player is doing, mirrored from the events it pushes.
///
/// A mirror rather than a query: the sync client looks at this ten times a
/// second, and asking the player that often would put a socket round trip in
/// front of every decision.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerSnapshot {
    pub paused: bool,
    /// The last position the player reported, and when it reported it.
    pub time_pos: Option<f64>,
    pub time_pos_at: Option<Instant>,
    pub duration: Option<f64>,
    /// `None` while idle, and while a file is loading but has not started.
    pub file: Option<LoadedFile>,
    pub speed: f64,
    pub seeking: bool,
    pub eof_reached: bool,
    /// Waiting on the cache. Not the same as paused: the user did not ask for
    /// it, and it must not look like falling out of sync.
    pub buffering: bool,
}

impl Default for PlayerSnapshot {
    /// Paused, with nothing open: how mpv comes up when it is launched with
    /// `--pause --idle`.
    fn default() -> Self {
        Self {
            paused: true,
            time_pos: None,
            time_pos_at: None,
            duration: None,
            file: None,
            speed: 1.0,
            seeking: false,
            eof_reached: false,
            buffering: false,
        }
    }
}

impl PlayerSnapshot {
    pub fn is_loaded(&self) -> bool {
        self.file.is_some()
    }

    /// Where playback is estimated to be at `now`.
    ///
    /// mpv reports a position once per frame, so between two reports the film
    /// has moved on by however long it has been playing since. The estimate
    /// stands still whenever the film does — paused, seeking, at the end,
    /// buffering, or with nothing open — and never leaves the file.
    pub fn position(&self, now: Instant) -> Option<f64> {
        let reported = self.time_pos?;

        let standing_still = self.paused
            || self.seeking
            || self.eof_reached
            || self.buffering
            || self.file.is_none();

        let estimate = match self.time_pos_at {
            Some(at) if !standing_still => {
                reported + now.saturating_duration_since(at).as_secs_f64() * self.speed
            }
            _ => reported,
        };

        let estimate = estimate.max(0.0);
        Some(match self.duration {
            Some(duration) if duration > 0.0 => estimate.min(duration),
            _ => estimate,
        })
    }
}

/// Who caused a change the player reported.
///
/// mpv never says, so this is the driver matching what it sees against what
/// it last asked for. A hint, not proof: the sync client still applies
/// Syncplay's own thresholds before treating anything as the user's doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Ours,
    External,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseReason {
    /// syncparty asked it to quit.
    QuitRequested,
    /// The user closed the window or pressed `q`.
    UserQuit,
    /// Anything else, with the exit code when there was one.
    Crashed(Option<i32>),
}

/// Something the player did, pushed as it happens.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    FileLoaded {
        file: LoadedFile,
        origin: Origin,
    },
    FileFailed {
        error: String,
    },
    PauseChanged {
        paused: bool,
        origin: Origin,
    },
    Seeked {
        position: f64,
        origin: Origin,
    },
    SpeedChanged {
        speed: f64,
        origin: Origin,
    },
    /// The end of the file. The pause mpv applies there is reported as this,
    /// never as a `PauseChanged`.
    EndOfFile,
    /// The file was closed and the player is idle again.
    Unloaded,
    /// The process is gone. Nothing else arrives after this.
    Closed(CloseReason),
}

pub type PlayerEvents = mpsc::UnboundedReceiver<PlayerEvent>;

/// A running player.
///
/// Every command that changes state returns only once the player has
/// accepted it, and by then [`Player::snapshot`] already shows the commanded
/// value. The sync client relies on that: it is what stops a correction it
/// made itself from looking, a tenth of a second later, like the user seeking.
#[async_trait]
pub trait Player: Send + Sync {
    /// The latest mirrored state. Cheap.
    fn snapshot(&self) -> PlayerSnapshot;

    /// A no-op when the player is already in that state.
    async fn set_paused(&self, paused: bool) -> Result<()>;

    /// Seeks to an absolute position in seconds. An error with nothing loaded.
    async fn seek(&self, position: f64) -> Result<()>;

    /// A no-op when the speed is already that.
    async fn set_speed(&self, speed: f64) -> Result<()>;

    /// Opens a local file. Anything that is not an existing regular file is
    /// refused before it reaches the player.
    async fn load(&self, path: &Path) -> Result<()>;

    /// Shows text over the picture, exactly as given.
    async fn show_text(&self, text: &str, duration: Duration) -> Result<()>;

    /// Closes the player and waits for it to go.
    async fn quit(&self) -> Result<()>;
}

/// Starts players.
///
/// Injected rather than constructed where it is used, so the session's tests
/// can hand out fakes and the end-to-end test can hand out headless mpv.
#[async_trait]
pub trait PlayerLauncher: Send + Sync {
    async fn launch(&self) -> Result<(Arc<dyn Player>, PlayerEvents)>;
}

/// Whether the player gets a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    /// What people watch in. Keeps the user's own mpv configuration.
    Window,
    /// No window, no audio, no user configuration: for tests.
    Headless,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playing_at(position: f64, at: Instant) -> PlayerSnapshot {
        PlayerSnapshot {
            paused: false,
            time_pos: Some(position),
            time_pos_at: Some(at),
            duration: Some(100.0),
            file: Some(LoadedFile {
                path: "/films/Film.mkv".to_owned(),
                name: "Film.mkv".to_owned(),
                duration: Some(100.0),
                size: Some(1),
            }),
            ..PlayerSnapshot::default()
        }
    }

    #[test]
    fn a_playing_file_is_estimated_forward_from_its_last_report() {
        let at = Instant::now();
        let snapshot = playing_at(10.0, at);

        let position = snapshot
            .position(at + Duration::from_millis(500))
            .expect("position");
        assert!((position - 10.5).abs() < 1e-6);
    }

    #[test]
    fn the_estimate_follows_the_playback_speed() {
        let at = Instant::now();
        let snapshot = PlayerSnapshot {
            speed: 0.95,
            ..playing_at(10.0, at)
        };

        let position = snapshot
            .position(at + Duration::from_secs(2))
            .expect("position");
        assert!((position - 11.9).abs() < 1e-6);
    }

    #[test]
    fn a_paused_file_stands_still() {
        let at = Instant::now();
        let snapshot = PlayerSnapshot {
            paused: true,
            ..playing_at(10.0, at)
        };

        assert_eq!(snapshot.position(at + Duration::from_secs(5)), Some(10.0));
    }

    #[test]
    fn the_estimate_never_runs_past_the_end_of_the_file() {
        let at = Instant::now();
        let snapshot = playing_at(99.5, at);

        assert_eq!(snapshot.position(at + Duration::from_secs(5)), Some(100.0));
    }

    #[test]
    fn nothing_reported_means_no_position() {
        assert_eq!(PlayerSnapshot::default().position(Instant::now()), None);
    }

    /// A regression guard with a name, not a change detector.
    ///
    /// winget's `shinchiro.mpv` installs to "MPV Player" and adds nothing to
    /// `PATH`, so this entry is the only thing standing between an automatic
    /// install and the app declaring it missing straight afterwards.
    #[cfg(windows)]
    #[test]
    fn the_directory_winget_installs_mpv_into_is_searched() {
        assert!(
            MPV_FALLBACKS.contains(&r"C:\Program Files\MPV Player\mpv.exe"),
            "MPV_FALLBACKS: {MPV_FALLBACKS:?}"
        );
    }
}
