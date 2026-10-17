//! mpv, driven over its JSON IPC.
//!
//! syncparty starts mpv with a private socket (a named pipe on Windows),
//! observes the properties it cares about, and from then on mpv tells it
//! what happens. Nothing is polled: the mirror behind [`Player::snapshot`] is
//! kept up to date by the events mpv pushes, and every command syncparty
//! sends is matched against the change it causes, which is how a pause from
//! the room is told apart from one at the keyboard.
//!
//! The file is never named on mpv's command line. It is loaded over the
//! socket once mpv is listening, so argv carries options and nothing else.

mod args;
mod connection;
mod endpoint;
mod tracker;
mod transport;
mod version;

#[cfg(all(test, unix))]
mod real_tests;
#[cfg(all(test, unix))]
mod tests;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot, watch, Notify};

use crate::core::config::ConfigStore;
use crate::core::error::{Result, SyncPartyError};
use crate::core::player::{
    find_mpv, LaunchMode, Player, PlayerEvents, PlayerLauncher, PlayerSnapshot, MPV_KEY,
};
use crate::core::process;

use self::connection::{CommandError, Liveness, Shared, Wiring};
use self::endpoint::Endpoint;
use self::tracker::{Effect, LoadFailure, OBSERVED};
use self::transport::{LineReader, MpvProcess, Process};

/// How long a command waits for mpv's answer. mpv answers at once — it
/// replies before it acts — so a command still unanswered after this means
/// mpv has hung.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a file gets from `loadfile` to playing. Local files open in
/// milliseconds; the margin is for a network share waking up.
const LOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// How long mpv gets to die once killed.
const KILL_GRACE: Duration = Duration::from_secs(2);

/// How long the driver gives mpv for the things that can run out of time.
/// Always the defaults in use; the tests shorten them, so that running out
/// takes milliseconds instead of the seconds a real mpv deserves.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Timing {
    /// To start listening. Upstream Syncplay waits as long.
    connect: Duration,
    /// To quit when asked, before it is killed.
    quit_grace: Duration,
    liveness: Liveness,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            quit_grace: Duration::from_secs(3),
            liveness: Liveness::default(),
        }
    }
}

/// Starts mpv processes, each with its own private IPC endpoint.
#[derive(Debug, Clone)]
pub struct MpvLauncher {
    executable: PathBuf,
    mode: LaunchMode,
    timing: Timing,
}

impl MpvLauncher {
    pub fn new(executable: PathBuf, mode: LaunchMode) -> Self {
        Self {
            executable,
            mode,
            timing: Timing::default(),
        }
    }

    #[cfg(test)]
    fn with_timing(self, timing: Timing) -> Self {
        Self { timing, ..self }
    }

    /// Resolves mpv the way the dependency check does, honouring a path the
    /// user chose, so a player reported as ready is one this can launch.
    pub fn discover(settings: &ConfigStore, mode: LaunchMode) -> Result<Self> {
        let manual = settings.executable_override(MPV_KEY);

        find_mpv(manual.as_deref())
            .map(|executable| Self::new(executable, mode))
            .ok_or_else(|| SyncPartyError::DependencyMissing("mpv".to_owned()))
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn mode(&self) -> LaunchMode {
        self.mode
    }

    /// Starts mpv and returns the concrete player, for callers — the
    /// end-to-end test — that need to reach its IPC endpoint themselves.
    pub async fn spawn(&self) -> Result<(Arc<MpvPlayer>, PlayerEvents)> {
        self.spawn_at(Endpoint::create()?).await
    }

    /// The same, listening on an endpoint the caller made — the tests put it
    /// somewhere they can see is cleaned up after a failed start.
    async fn spawn_at(&self, endpoint: Endpoint) -> Result<(Arc<MpvPlayer>, PlayerEvents)> {
        let mut command = process::quiet(&self.executable);
        command.args(args::arguments(
            &self.executable,
            self.mode,
            endpoint.name(),
        ));
        #[cfg(target_os = "macos")]
        if let Some(path) = args::child_path(std::env::var_os("PATH")) {
            command.env("PATH", path);
        }

        let mut child = command.spawn().map_err(|error| {
            SyncPartyError::PlayerFailed(format!(
                "could not start {}: {error}",
                self.executable.display()
            ))
        })?;

        let stream =
            match transport::connect(endpoint.name(), &mut child, self.timing.connect).await {
                Ok(stream) => stream,
                Err(error) => {
                    let _ = child.start_kill();
                    let _ = tokio::time::timeout(KILL_GRACE, child.wait()).await;
                    return Err(error);
                }
            };

        start(stream, MpvProcess(child), endpoint, self.timing).await
    }
}

#[async_trait]
impl PlayerLauncher for MpvLauncher {
    async fn launch(&self) -> Result<(Arc<dyn Player>, PlayerEvents)> {
        let (player, events) = self.spawn().await?;
        Ok((player as Arc<dyn Player>, events))
    }
}

/// One running mpv.
///
/// Dropping the last handle kills the process, so a player can never
/// outlive everything that could control it.
pub struct MpvPlayer {
    endpoint: String,
    shared: Arc<Shared>,
    writer: mpsc::UnboundedSender<Vec<u8>>,
    kill: Arc<Notify>,
    exited: watch::Receiver<bool>,
    quit_grace: Duration,
}

impl fmt::Debug for MpvPlayer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MpvPlayer")
            .field("endpoint", &self.endpoint)
            .field("exited", &self.has_exited())
            .finish_non_exhaustive()
    }
}

/// Takes over a connected mpv: starts the reader and the writer, then
/// observes everything the mirror needs.
///
/// Generic over the stream and the process so the tests can stand in for
/// both — a socket they speak mpv's part on, and an exit they decide.
async fn start<S, P>(
    stream: S,
    process: P,
    endpoint: Endpoint,
    timing: Timing,
) -> Result<(Arc<MpvPlayer>, PlayerEvents)>
where
    S: AsyncRead + AsyncWrite + Send + 'static,
    P: Process,
{
    let (reader, writer) = tokio::io::split(stream);
    let (line_sender, line_receiver) = mpsc::unbounded_channel();
    let (event_sender, events) = mpsc::unbounded_channel();
    let (exited_sender, exited) = watch::channel(false);
    let kill = Arc::new(Notify::new());
    let shared = Arc::new(Shared::new(event_sender));

    let player = MpvPlayer {
        endpoint: endpoint.name().to_owned(),
        shared: Arc::clone(&shared),
        writer: line_sender.clone(),
        kill: Arc::clone(&kill),
        exited,
        quit_grace: timing.quit_grace,
    };

    tokio::spawn(transport::write_lines(writer, line_receiver));
    tokio::spawn(connection::run(
        LineReader::new(reader),
        process,
        Wiring {
            shared,
            endpoint,
            writer: line_sender,
            kill,
            exited: exited_sender,
            liveness: timing.liveness,
        },
    ));

    // Should any of this fail, dropping `player` takes mpv down with it.
    player.observe().await?;
    Ok((Arc::new(player), events))
}

impl MpvPlayer {
    /// The socket path or pipe name this player listens on. Anything that
    /// connects to it can drive mpv the way a user at the keyboard would,
    /// which is exactly what the end-to-end test needs and why it is private
    /// to this user everywhere else.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Checks the version, then asks mpv to report every property the mirror
    /// is built from. Each one arrives at once with its current value, which
    /// seeds the mirror.
    ///
    /// The version comes first, so an mpv too old for the rest is refused
    /// with that reason rather than with whatever it fails at first.
    async fn observe(&self) -> Result<()> {
        match self
            .request(json!(["get_property", "mpv-version"]), Effect::None)
            .await
        {
            Ok(version) => {
                let version = version.as_str().unwrap_or_default();
                if version::too_old(version) {
                    let (major, minor) = version::MINIMUM;
                    return Err(SyncPartyError::PlayerFailed(format!(
                        "{version} is too old for syncparty, which needs mpv {major}.{minor} or \
                         newer"
                    )));
                }
                tracing::info!(version, "mpv is listening");
            }
            // Whatever stops mpv answering this stops the observing below,
            // which says so.
            Err(error) => tracing::debug!(?error, "mpv did not say which version it is"),
        }

        for (index, name) in OBSERVED.iter().enumerate() {
            self.request(json!(["observe_property", index + 1, name]), Effect::None)
                .await
                .map_err(|error| error.into_player_error("report its state"))?;
        }

        // Only errors: warnings come in their hundreds for some files.
        if let Err(error) = self
            .request(json!(["request_log_messages", "error"]), Effect::None)
            .await
        {
            tracing::debug!(?error, "mpv will not forward its error messages");
        }
        Ok(())
    }

    /// Sends one command and waits for mpv's answer to it.
    async fn request(
        &self,
        command: Value,
        effect: Effect,
    ) -> std::result::Result<Value, CommandError> {
        let (id, reply) = self
            .shared
            .send(&self.writer, &command, effect)
            .ok_or(CommandError::Disconnected)?;

        match tokio::time::timeout(REPLY_TIMEOUT, reply).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => Err(CommandError::Disconnected),
            Err(_) => {
                self.shared.abandon(id);
                Err(CommandError::Timeout)
            }
        }
    }

    fn has_exited(&self) -> bool {
        *self.exited.borrow()
    }

    async fn wait_exited(&self) {
        let mut exited = self.exited.clone();
        // An error means the connection task is gone, and mpv with it.
        let _ = exited.wait_for(|exited| *exited).await;
    }
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        if !self.has_exited() {
            self.shared.quitting.store(true, Ordering::SeqCst);
            self.kill.notify_one();
        }
    }
}

#[async_trait]
impl Player for MpvPlayer {
    fn snapshot(&self) -> PlayerSnapshot {
        self.shared.snapshot()
    }

    async fn set_paused(&self, paused: bool) -> Result<()> {
        let needed = self
            .shared
            .with_tracker(|tracker| tracker.expect_pause(paused, Instant::now()));
        if !needed {
            return Ok(());
        }

        self.request(
            json!(["set_property", "pause", paused]),
            Effect::Pause(paused),
        )
        .await
        .map(drop)
        .map_err(|error| error.into_player_error(if paused { "pause" } else { "resume" }))
    }

    async fn seek(&self, position: f64) -> Result<()> {
        if !position.is_finite() {
            return Err(SyncPartyError::PlayerFailed(format!(
                "cannot seek to {position}"
            )));
        }
        // A negative absolute seek would count back from the end.
        let target = position.max(0.0);

        let id = self
            .shared
            .with_tracker(|tracker| tracker.expect_seek(target, Instant::now()))
            .ok_or_else(|| SyncPartyError::PlayerFailed("nothing is open to seek in".to_owned()))?;

        // `exact` so a user's `hr-seek=no` cannot make the room's seeks
        // land on a keyframe seconds away.
        self.request(json!(["seek", target, "absolute+exact"]), Effect::Seek(id))
            .await
            .map(drop)
            .map_err(|error| error.into_player_error("seek"))
    }

    async fn set_speed(&self, speed: f64) -> Result<()> {
        if !(0.01..=100.0).contains(&speed) {
            return Err(SyncPartyError::PlayerFailed(format!(
                "mpv cannot play at speed {speed}"
            )));
        }

        let needed = self
            .shared
            .with_tracker(|tracker| tracker.expect_speed(speed, Instant::now()));
        if !needed {
            return Ok(());
        }

        self.request(
            json!(["set_property", "speed", speed]),
            Effect::Speed(speed),
        )
        .await
        .map(drop)
        .map_err(|error| error.into_player_error("change speed"))
    }

    /// Opens a file and waits until it is playing — paused or not — so that
    /// by the time this returns the snapshot names the file, with its
    /// duration and size.
    ///
    /// Only the three-argument `loadfile` is used: since 0.38 the fourth
    /// argument is an index, before that it was the options, and three mean
    /// the same on every version. Pause is never passed as a per-file option
    /// either; it stays whatever it was.
    async fn load(&self, path: &Path) -> Result<()> {
        let path = playable(path).await?;

        let (waiter, outcome) = oneshot::channel();
        let generation = self
            .shared
            .with_tracker(|tracker| tracker.expect_load(waiter));

        if let Err(error) = self
            .request(
                json!(["loadfile", path, "replace"]),
                Effect::Load(generation),
            )
            .await
        {
            self.shared
                .with_tracker(|tracker| tracker.forget_load(generation));
            return Err(error.into_player_error("open the file"));
        }

        match tokio::time::timeout(LOAD_TIMEOUT, outcome).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(LoadFailure::Failed(reason)))) => Err(SyncPartyError::PlayerFailed(format!(
                "could not open the file: {reason}"
            ))),
            Ok(Ok(Err(LoadFailure::Playlist))) => {
                // mpv has gone on to what the playlist lists; `stop` ends
                // that and empties the playlist, so nothing else in it plays.
                if let Err(error) = self.request(json!(["stop"]), Effect::None).await {
                    tracing::warn!(?error, "could not stop what a refused playlist listed");
                }
                Err(SyncPartyError::PlayerFailed(format!(
                    "could not open the file: {}",
                    tracker::NOT_A_FILM
                )))
            }
            Ok(Err(_)) => Err(SyncPartyError::PlayerFailed(
                "mpv closed before the file started playing".to_owned(),
            )),
            Err(_) => {
                self.shared
                    .with_tracker(|tracker| tracker.forget_load(generation));
                Err(SyncPartyError::PlayerFailed(format!(
                    "mpv did not start playing the file within {} s",
                    LOAD_TIMEOUT.as_secs()
                )))
            }
        }
    }

    /// `show-text` with the text as an array argument, which mpv draws
    /// exactly as given: no `${…}` expansion, no styling, no escapes. So
    /// nothing is escaped here either — escaping would show on screen — and
    /// `expand-properties` is never added, because this text comes from
    /// other people in the room.
    async fn show_text(&self, text: &str, duration: Duration) -> Result<()> {
        // mpv reads the duration as a 32-bit int.
        let milliseconds = u64::try_from(duration.as_millis())
            .unwrap_or(u64::MAX)
            .min(i32::MAX as u64);

        self.request(
            json!(["show-text", on_screen(text), milliseconds]),
            Effect::None,
        )
        .await
        .map(drop)
        .map_err(|error| error.into_player_error("show text"))
    }

    /// Asks mpv to quit, and kills it if it has not gone within a few
    /// seconds. Returns once the process is gone and its socket removed.
    async fn quit(&self) -> Result<()> {
        if self.has_exited() {
            return Ok(());
        }
        self.shared.quitting.store(true, Ordering::SeqCst);

        let asked = tokio::time::timeout(self.quit_grace, async {
            // mpv may close the socket before it answers; either is fine.
            let _ = self.request(json!(["quit"]), Effect::None).await;
            self.wait_exited().await;
        })
        .await;

        if asked.is_err() {
            tracing::warn!("mpv did not quit when asked; killing it");
            self.kill.notify_one();
            if tokio::time::timeout(KILL_GRACE, self.wait_exited())
                .await
                .is_err()
            {
                return Err(SyncPartyError::PlayerFailed(
                    "mpv would not exit".to_owned(),
                ));
            }
        }
        Ok(())
    }
}

impl CommandError {
    fn into_player_error(self, action: &str) -> SyncPartyError {
        SyncPartyError::PlayerFailed(match self {
            Self::Refused(reason) => format!("mpv would not {action}: {reason}"),
            Self::Timeout => format!(
                "mpv did not answer within {} s when asked to {action}",
                REPLY_TIMEOUT.as_secs()
            ),
            Self::Disconnected => "mpv is not running any more".to_owned(),
        })
    }
}

/// Turns a path the caller wants opened into what `loadfile` is given.
///
/// Only an existing regular file gets through. `loadfile` also takes URLs,
/// stream specifiers, `-` for stdin and devices, and none of those belong in
/// a party: everybody opens the same film from their own disk. The path is
/// made absolute, which also stops a name that looks like a protocol prefix
/// from being read as one.
async fn playable(path: &Path) -> Result<String> {
    let absolute = std::path::absolute(path).map_err(|error| {
        SyncPartyError::PlayerFailed(format!("cannot open {}: {error}", path.display()))
    })?;

    let metadata = tokio::fs::metadata(&absolute).await.map_err(|error| {
        SyncPartyError::PlayerFailed(match error.kind() {
            std::io::ErrorKind::NotFound => format!("there is no file at {}", absolute.display()),
            _ => format!("cannot open {}: {error}", absolute.display()),
        })
    })?;
    if !metadata.is_file() {
        return Err(SyncPartyError::PlayerFailed(format!(
            "{} is not a file",
            absolute.display()
        )));
    }

    absolute.into_os_string().into_string().map_err(|path| {
        SyncPartyError::PlayerFailed(format!(
            "cannot open {}: its name is not valid Unicode, which mpv's control socket cannot \
             carry",
            Path::new(&path).display()
        ))
    })
}

/// Text as it may go on screen: every control character is removed except
/// the line feed, which mpv turns into a line break.
fn on_screen(text: &str) -> String {
    text.chars()
        .filter(|character| *character == '\n' || !character.is_control())
        .collect()
}
