//! A player that records what it was told, for tests above the player seam.
//!
//! It keeps the promises the [`Player`] contract makes: a command that changes
//! something has changed the snapshot by the time it returns and is reported
//! as [`Origin::Ours`], and a command that would change nothing is a no-op
//! that reports nothing. The test plays the user. It pushes events and edits
//! the snapshot directly, so the code under test sees both kinds of change the
//! way it sees them from mpv.
//!
//! Every event is queued before the change it reports can be seen in the
//! snapshot. Code that looks at the snapshot and then at the queue therefore
//! never finds a change whose event has not arrived yet.
//!
//! The sync client's tests and the session's tests both drive it, which is why
//! it lives here rather than beside either of them.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::{mpsc, watch};

use crate::core::error::{Result, SyncPartyError};
use crate::core::player::{
    CloseReason, LoadedFile, Origin, Player, PlayerEvent, PlayerEvents, PlayerLauncher,
    PlayerSnapshot,
};

/// How long a film is when nothing says otherwise. Long enough that no test
/// runs into the end of it by accident.
const DEFAULT_MEDIA_DURATION: f64 = 7200.0;

/// One call the fake received, in the order it received them. No-ops are
/// recorded too: what was asked matters as much as what changed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Command {
    SetPaused(bool),
    Seek(f64),
    SetSpeed(f64),
    Load(PathBuf),
    ShowText { text: String, duration: Duration },
    Quit,
}

pub(crate) struct FakePlayer {
    inner: Mutex<Inner>,
    events: mpsc::UnboundedSender<PlayerEvent>,
    /// While true, every command waits.
    hung: watch::Sender<bool>,
}

struct Inner {
    snapshot: PlayerSnapshot,
    commands: Vec<Command>,
    closed: bool,
    media_duration: f64,
    /// While set, every command is refused with this message, the way a
    /// player that stopped answering refuses them.
    failure: Option<String>,
}

impl FakePlayer {
    /// A player as mpv comes up: paused, with nothing open.
    pub(crate) fn new() -> (Arc<Self>, PlayerEvents) {
        let (events, receiver) = mpsc::unbounded_channel();
        let player = Arc::new(Self {
            inner: Mutex::new(Inner {
                snapshot: PlayerSnapshot::default(),
                commands: Vec::new(),
                closed: false,
                media_duration: DEFAULT_MEDIA_DURATION,
                failure: None,
            }),
            events,
            hung: watch::Sender::new(false),
        });

        (player, receiver)
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A test that panicked while holding the lock has already failed; the
        // state is still worth reading for the ones that follow.
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Every call so far.
    pub(crate) fn commands(&self) -> Vec<Command> {
        self.lock().commands.clone()
    }

    /// Every call so far, forgetting them, so a test can look at what one
    /// step caused without what came before.
    pub(crate) fn take_commands(&self) -> Vec<Command> {
        std::mem::take(&mut self.lock().commands)
    }

    /// The text shown over the picture so far, oldest first.
    pub(crate) fn texts(&self) -> Vec<String> {
        self.lock()
            .commands
            .iter()
            .filter_map(|command| match command {
                Command::ShowText { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    /// Pushes an event as though the player had sent it.
    pub(crate) fn push(&self, event: PlayerEvent) {
        let _ = self.events.send(event);
    }

    /// Edits the mirrored state behind the code under test's back, which is
    /// what a user at the keyboard does. Pushes nothing: pair it with
    /// [`Self::push`] when the event matters.
    pub(crate) fn update(&self, change: impl FnOnce(&mut PlayerSnapshot)) {
        change(&mut self.lock().snapshot);
    }

    /// How long the next file to be loaded claims to be.
    pub(crate) fn set_media_duration(&self, seconds: f64) {
        self.lock().media_duration = seconds;
    }

    /// Makes every command fail with `message` until called with `None`.
    pub(crate) fn fail_commands(&self, message: Option<&str>) {
        self.lock().failure = message.map(str::to_owned);
    }

    /// While `true`, every command but `quit` is recorded and then left
    /// unanswered, the way a player that has stopped reading its socket
    /// leaves it. Commands still waiting when it is released carry on as
    /// usual. `quit` still works, as it does on a real player, whose process
    /// can be ended whether it answers or not.
    pub(crate) fn hang(&self, hung: bool) {
        self.hung.send_replace(hung);
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// The user opens `path` themselves — dragged onto the window, say. No
    /// file has to exist, because nothing here reads it.
    pub(crate) fn user_opens(&self, path: &str, duration: Option<f64>) -> LoadedFile {
        let file = LoadedFile {
            path: path.to_owned(),
            name: Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_owned()),
            duration,
            size: None,
        };

        self.open(file.clone(), Origin::External);
        file
    }

    /// The user presses space.
    pub(crate) fn user_pauses(&self, paused: bool) {
        let mut inner = self.lock();
        if inner.snapshot.paused == paused {
            return;
        }

        settle(&mut inner.snapshot, Instant::now());
        inner.snapshot.paused = paused;
        self.push(PlayerEvent::PauseChanged {
            paused,
            origin: Origin::External,
        });
    }

    /// The user drags the seek bar.
    pub(crate) fn user_seeks(&self, position: f64) {
        let mut inner = self.lock();
        self.seek_to(&mut inner.snapshot, position, Origin::External);
    }

    /// The film reaches its last frame, where mpv pauses on its own.
    pub(crate) fn plays_to_the_end(&self) {
        let mut inner = self.lock();
        let end = inner.snapshot.duration.unwrap_or_default();
        inner.snapshot.time_pos = Some(end);
        inner.snapshot.time_pos_at = Some(Instant::now());
        inner.snapshot.eof_reached = true;
        inner.snapshot.paused = true;
        self.push(PlayerEvent::EndOfFile);
    }

    /// The user closes the window.
    pub(crate) fn user_closes(&self) {
        self.close(CloseReason::UserQuit);
    }

    /// The process dies under us.
    pub(crate) fn crash(&self, code: Option<i32>) {
        self.close(CloseReason::Crashed(code));
    }

    fn close(&self, reason: CloseReason) {
        let mut inner = self.lock();
        if !std::mem::replace(&mut inner.closed, true) {
            self.push(PlayerEvent::Closed(reason));
        }
    }

    fn open(&self, file: LoadedFile, origin: Origin) {
        let mut inner = self.lock();
        inner.snapshot.duration = file.duration;
        inner.snapshot.file = Some(file.clone());
        inner.snapshot.time_pos = Some(0.0);
        inner.snapshot.time_pos_at = Some(Instant::now());
        inner.snapshot.eof_reached = false;
        inner.snapshot.seeking = false;
        self.push(PlayerEvent::FileLoaded { file, origin });
    }

    /// Moves to `position`, clamped into the file the way mpv clamps it.
    ///
    /// Seeking back from the end clears the end. Seeking to it or past it
    /// lands on the last frame, where mpv pauses as it does when a film plays
    /// out — and reports it the same way.
    fn seek_to(&self, snapshot: &mut PlayerSnapshot, position: f64, origin: Origin) {
        let mut position = position.max(0.0);
        let mut at_the_end = false;
        if let Some(duration) = snapshot.duration.filter(|duration| *duration > 0.0) {
            at_the_end = position >= duration;
            position = position.min(duration);
        }

        snapshot.time_pos = Some(position);
        snapshot.time_pos_at = Some(Instant::now());
        snapshot.eof_reached = at_the_end;
        snapshot.paused |= at_the_end;

        self.push(PlayerEvent::Seeked { position, origin });
        if at_the_end {
            self.push(PlayerEvent::EndOfFile);
        }
    }

    /// Records `command`, waits while the player is hung, and hands back the
    /// state if the player can take it.
    async fn accept(&self, command: Command) -> Result<MutexGuard<'_, Inner>> {
        self.lock().commands.push(command);

        // The sender lives in `self`, so this only ends by being released.
        let mut hung = self.hung.subscribe();
        let _ = hung.wait_for(|hung| !*hung).await;

        let inner = self.lock();
        if inner.closed {
            return Err(SyncPartyError::PlayerFailed(
                "the player has closed".to_owned(),
            ));
        }
        if let Some(message) = inner.failure.clone() {
            return Err(SyncPartyError::PlayerFailed(message));
        }

        Ok(inner)
    }
}

/// Folds the time since the last report into the position, so a change made
/// now starts from where the film actually is.
fn settle(snapshot: &mut PlayerSnapshot, now: Instant) {
    if snapshot.time_pos.is_some() {
        snapshot.time_pos = snapshot.position(now);
        snapshot.time_pos_at = Some(now);
    }
}

#[async_trait]
impl Player for FakePlayer {
    fn snapshot(&self) -> PlayerSnapshot {
        self.lock().snapshot.clone()
    }

    async fn set_paused(&self, paused: bool) -> Result<()> {
        let mut inner = self.accept(Command::SetPaused(paused)).await?;
        if inner.snapshot.paused == paused {
            return Ok(());
        }
        // mpv accepts an unpause on the last frame and then ignores it.
        if !paused && inner.snapshot.eof_reached {
            return Ok(());
        }

        settle(&mut inner.snapshot, Instant::now());
        inner.snapshot.paused = paused;
        self.push(PlayerEvent::PauseChanged {
            paused,
            origin: Origin::Ours,
        });
        Ok(())
    }

    async fn seek(&self, position: f64) -> Result<()> {
        let mut inner = self.accept(Command::Seek(position)).await?;
        if inner.snapshot.file.is_none() {
            return Err(SyncPartyError::PlayerFailed(
                "nothing is loaded to seek in".to_owned(),
            ));
        }

        self.seek_to(&mut inner.snapshot, position, Origin::Ours);
        Ok(())
    }

    async fn set_speed(&self, speed: f64) -> Result<()> {
        let mut inner = self.accept(Command::SetSpeed(speed)).await?;
        if (inner.snapshot.speed - speed).abs() < 1e-3 {
            return Ok(());
        }

        settle(&mut inner.snapshot, Instant::now());
        inner.snapshot.speed = speed;
        self.push(PlayerEvent::SpeedChanged {
            speed,
            origin: Origin::Ours,
        });
        Ok(())
    }

    async fn load(&self, path: &Path) -> Result<()> {
        let duration = self
            .accept(Command::Load(path.to_owned()))
            .await?
            .media_duration;

        let Some(metadata) = std::fs::metadata(path).ok().filter(|meta| meta.is_file()) else {
            return Err(SyncPartyError::PlayerFailed(format!(
                "{} is not a file",
                path.display()
            )));
        };

        self.open(
            LoadedFile {
                path: path.to_string_lossy().into_owned(),
                name: path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                duration: Some(duration),
                size: Some(metadata.len()),
            },
            Origin::Ours,
        );
        Ok(())
    }

    async fn show_text(&self, text: &str, duration: Duration) -> Result<()> {
        self.accept(Command::ShowText {
            text: text.to_owned(),
            duration,
        })
        .await
        .map(drop)
    }

    async fn quit(&self) -> Result<()> {
        let mut inner = self.lock();
        inner.commands.push(Command::Quit);
        if std::mem::replace(&mut inner.closed, true) {
            return Ok(());
        }

        self.push(PlayerEvent::Closed(CloseReason::QuitRequested));
        Ok(())
    }
}

/// Hands out [`FakePlayer`]s and keeps hold of each, so a test can reach the
/// player the code under test launched.
#[derive(Default)]
pub(crate) struct FakeLauncher {
    launched: Mutex<Vec<Arc<FakePlayer>>>,
    failure: Mutex<Option<String>>,
}

impl FakeLauncher {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Every player launched so far, oldest first.
    pub(crate) fn launched(&self) -> Vec<Arc<FakePlayer>> {
        self.launched
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// The player launched most recently.
    pub(crate) fn last(&self) -> Option<Arc<FakePlayer>> {
        self.launched().last().cloned()
    }

    /// Makes every launch fail with `message` until called with `None` — mpv
    /// missing, say, or refusing to start.
    pub(crate) fn fail_launches(&self, message: Option<&str>) {
        *self
            .failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = message.map(str::to_owned);
    }
}

#[async_trait]
impl PlayerLauncher for FakeLauncher {
    async fn launch(&self) -> Result<(Arc<dyn Player>, PlayerEvents)> {
        let failure = self
            .failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(message) = failure {
            return Err(SyncPartyError::PlayerFailed(message));
        }

        let (player, events) = FakePlayer::new();
        self.launched
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(player.clone());

        Ok((player as Arc<dyn Player>, events))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

    fn drain(events: &mut PlayerEvents) -> Vec<PlayerEvent> {
        let mut drained = Vec::new();
        while let Ok(event) = events.try_recv() {
            drained.push(event);
        }
        drained
    }

    /// A directory holding one small file to load.
    fn fixture() -> (PathBuf, PathBuf) {
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("syncparty-fake-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");

        let path = dir.join("Film.mkv");
        std::fs::write(&path, b"not really a film").expect("fixture");
        (dir, path)
    }

    #[tokio::test]
    async fn a_command_shows_in_the_snapshot_by_the_time_it_returns() {
        let (player, mut events) = FakePlayer::new();
        let (_dir, path) = fixture();

        player.load(&path).await.expect("load");
        player.set_paused(false).await.expect("unpause");
        player.seek(42.0).await.expect("seek");
        player.set_speed(0.95).await.expect("speed");

        let snapshot = player.snapshot();
        assert!(!snapshot.paused);
        assert_eq!(snapshot.speed, 0.95);
        assert!((snapshot.time_pos.expect("position") - 42.0).abs() < 0.05);
        let file = snapshot.file.expect("file");
        assert_eq!(file.name, "Film.mkv");
        assert_eq!(file.size, Some(17));

        let origins: Vec<Origin> = drain(&mut events)
            .into_iter()
            .filter_map(|event| match event {
                PlayerEvent::FileLoaded { origin, .. }
                | PlayerEvent::PauseChanged { origin, .. }
                | PlayerEvent::Seeked { origin, .. }
                | PlayerEvent::SpeedChanged { origin, .. } => Some(origin),
                _ => None,
            })
            .collect();
        assert_eq!(origins, vec![Origin::Ours; 4]);
    }

    #[tokio::test]
    async fn a_command_that_changes_nothing_is_recorded_but_reports_nothing() {
        let (player, mut events) = FakePlayer::new();

        player.set_paused(true).await.expect("pause");
        player.set_speed(1.0).await.expect("speed");

        assert_eq!(
            player.commands(),
            vec![Command::SetPaused(true), Command::SetSpeed(1.0)]
        );
        assert!(drain(&mut events).is_empty());
    }

    #[tokio::test]
    async fn seeking_with_nothing_open_is_an_error() {
        let (player, _events) = FakePlayer::new();

        let error = player.seek(10.0).await.expect_err("nothing to seek in");

        assert_eq!(error.kind(), "player_failed");
        assert_eq!(player.commands(), vec![Command::Seek(10.0)]);
    }

    #[tokio::test]
    async fn only_an_existing_file_is_loaded() {
        let (player, mut events) = FakePlayer::new();
        let (dir, _path) = fixture();

        assert!(player.load(&dir.join("missing.mkv")).await.is_err());
        assert!(
            player.load(&dir).await.is_err(),
            "a directory is not a film"
        );

        assert!(player.snapshot().file.is_none());
        assert!(drain(&mut events).is_empty());
    }

    #[tokio::test]
    async fn the_loaded_file_reports_the_duration_it_was_given() {
        let (player, _events) = FakePlayer::new();
        let (_dir, path) = fixture();
        player.set_media_duration(120.0);

        player.load(&path).await.expect("load");

        assert_eq!(player.snapshot().duration, Some(120.0));
    }

    #[tokio::test]
    async fn unpausing_on_the_last_frame_is_ignored_the_way_mpv_ignores_it() {
        let (player, mut events) = FakePlayer::new();
        player.user_opens("/films/Film.mkv", Some(30.0));
        player.user_pauses(false);
        player.plays_to_the_end();
        drain(&mut events);

        player.set_paused(false).await.expect("accepted");

        assert!(player.snapshot().paused);
        assert!(drain(&mut events).is_empty());

        player.seek(10.0).await.expect("seek back");
        player.set_paused(false).await.expect("unpause");
        assert!(!player.snapshot().paused);
    }

    #[tokio::test]
    async fn seeking_past_the_end_lands_on_the_last_frame_paused_the_way_mpv_does() {
        let (player, mut events) = FakePlayer::new();
        player.user_opens("/films/Film.mkv", Some(1200.0));
        player.user_pauses(false);
        drain(&mut events);

        player.seek(6000.0).await.expect("seek");

        let snapshot = player.snapshot();
        assert_eq!(snapshot.time_pos, Some(1200.0));
        assert!(snapshot.eof_reached && snapshot.paused);
        assert_eq!(
            drain(&mut events),
            vec![
                PlayerEvent::Seeked {
                    position: 1200.0,
                    origin: Origin::Ours
                },
                PlayerEvent::EndOfFile,
            ]
        );
    }

    #[tokio::test]
    async fn a_hung_player_answers_nothing_until_it_is_released() {
        let (player, _events) = FakePlayer::new();
        player.hang(true);

        let waited =
            tokio::time::timeout(Duration::from_millis(50), player.set_paused(false)).await;
        assert!(waited.is_err(), "a hung player answered");
        assert_eq!(player.commands(), vec![Command::SetPaused(false)]);
        assert!(player.snapshot().paused);

        let pending = player.set_paused(false);
        player.hang(false);
        pending.await.expect("answered once released");
        assert!(!player.snapshot().paused);
    }

    #[tokio::test]
    async fn what_the_user_does_is_reported_as_theirs() {
        let (player, mut events) = FakePlayer::new();

        let file = player.user_opens("/films/Film.mkv", Some(7200.0));
        player.user_pauses(false);
        player.user_pauses(false);
        player.user_seeks(-3.0);
        player.plays_to_the_end();

        assert_eq!(
            drain(&mut events),
            vec![
                PlayerEvent::FileLoaded {
                    file,
                    origin: Origin::External
                },
                PlayerEvent::PauseChanged {
                    paused: false,
                    origin: Origin::External
                },
                PlayerEvent::Seeked {
                    position: 0.0,
                    origin: Origin::External
                },
                PlayerEvent::EndOfFile,
            ]
        );
        let snapshot = player.snapshot();
        assert!(snapshot.eof_reached && snapshot.paused);
        assert_eq!(snapshot.time_pos, Some(7200.0));
    }

    #[tokio::test]
    async fn a_test_can_change_the_snapshot_and_push_events_directly() {
        let (player, mut events) = FakePlayer::new();

        player.update(|snapshot| snapshot.buffering = true);
        player.push(PlayerEvent::Unloaded);

        assert!(player.snapshot().buffering);
        assert_eq!(drain(&mut events), vec![PlayerEvent::Unloaded]);
    }

    #[tokio::test]
    async fn text_is_recorded_with_how_long_it_stays_up() {
        let (player, _events) = FakePlayer::new();

        player
            .show_text("ahmet paused", Duration::from_secs(3))
            .await
            .expect("text");

        assert_eq!(player.texts(), vec!["ahmet paused".to_owned()]);
        assert_eq!(
            player.take_commands(),
            vec![Command::ShowText {
                text: "ahmet paused".to_owned(),
                duration: Duration::from_secs(3)
            }]
        );
        assert!(player.commands().is_empty());
    }

    #[tokio::test]
    async fn a_failing_player_refuses_everything_until_it_recovers() {
        let (player, _events) = FakePlayer::new();

        player.fail_commands(Some("mpv stopped answering"));
        let error = player.set_paused(false).await.expect_err("refused");
        assert!(error.to_string().contains("mpv stopped answering"));
        assert!(player.snapshot().paused);

        player.fail_commands(None);
        player.set_paused(false).await.expect("accepted");
    }

    #[tokio::test]
    async fn closing_is_reported_once_and_ends_every_command() {
        let (player, mut events) = FakePlayer::new();

        player.quit().await.expect("quit");
        player.quit().await.expect("quitting twice is fine");
        player.user_closes();
        player.crash(Some(1));

        assert!(player.is_closed());
        assert_eq!(
            drain(&mut events),
            vec![PlayerEvent::Closed(CloseReason::QuitRequested)]
        );
        assert!(player.set_paused(false).await.is_err());
    }

    #[tokio::test]
    async fn the_user_closing_the_window_is_not_a_quit_request() {
        let (player, mut events) = FakePlayer::new();

        player.user_closes();

        assert_eq!(
            drain(&mut events),
            vec![PlayerEvent::Closed(CloseReason::UserQuit)]
        );
    }

    #[tokio::test]
    async fn the_launcher_keeps_every_player_it_hands_out() {
        let launcher = FakeLauncher::new();

        let (first, _first_events) = launcher.launch().await.expect("launch");
        let (_second, _second_events) = launcher.launch().await.expect("launch");
        first.set_paused(false).await.expect("unpause");

        assert_eq!(launcher.launched().len(), 2);
        assert_eq!(
            launcher.launched()[0].commands(),
            vec![Command::SetPaused(false)]
        );
        assert!(launcher.last().expect("last").commands().is_empty());
    }

    #[tokio::test]
    async fn a_launcher_told_to_fail_launches_nothing() {
        let launcher = FakeLauncher::new();

        launcher.fail_launches(Some("mpv is not installed"));
        let error = launcher.launch().await.err().expect("refused");
        assert_eq!(error.kind(), "player_failed");
        assert!(launcher.launched().is_empty());

        launcher.fail_launches(None);
        assert!(launcher.launch().await.is_ok());
    }
}
