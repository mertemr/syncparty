//! Watching a party on this machine: one sync client and the player it keeps
//! in step.
//!
//! Host and guest watch the same way — the only difference is where the sync
//! client connects. A host's goes straight to its own server on loopback; a
//! guest's goes to the near end of the tunnel to the host.
//!
//! The types the UI sees are deliberately coarse: whether the connection is
//! up, whether the player is open and what it is playing. The position is not
//! here, because it changes ten times a second and the UI has nothing to do
//! with it.
//!
//! [`WatchSession`] is the thing that holds a watch together. It owns the sync
//! client and the player, launches the player once the party has let this
//! machine in, and turns what the two of them report into a [`WatchState`] the
//! UI is told about whenever it changes.

mod tap;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::{mpsc, watch, Notify};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::core::error::{Result, SyncPartyError};
use crate::core::events::{AppEvent, EventBus};
use crate::core::player::{Player, PlayerEvent, PlayerLauncher, PlayerSnapshot};
use crate::core::syncplay::{
    ClientConfig, ClientStatus, ConnectionState, RoomSnapshot, SyncClient, WatchedFile,
};

use self::tap::{Heard, Tap};

/// How long the party gets to let this machine in. Long enough for a guest's
/// first connection through a relay; a party that has not answered by then is
/// not going to.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Everything the watch card needs.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WatchState {
    pub role: WatchRole,
    pub room: String,
    /// The name the server knows this machine by. Not always the nickname in
    /// settings: a name already taken in the room gets something appended.
    /// `None` until the server has answered.
    pub username: Option<String>,
    pub connection: WatchConnection,
    pub player: PlayerStatus,
    pub ready: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum WatchRole {
    Host,
    Guest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WatchConnection {
    Connecting,
    Connected,
    /// The connection dropped and is being retried. `attempt` counts from 1.
    Reconnecting {
        attempt: u32,
    },
    /// The server turned this client away — a wrong password, usually. Not
    /// retried: trying the same thing again would be turned away again.
    Rejected {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlayerStatus {
    Starting,
    /// Open, with nothing loaded yet.
    Idle,
    Loaded {
        file: WatchedFile,
        paused: bool,
    },
    /// The user closed it. The party goes on; it can be reopened.
    Closed,
    Failed {
        message: String,
    },
}

impl From<&ConnectionState> for WatchConnection {
    fn from(state: &ConnectionState) -> Self {
        match state {
            ConnectionState::Connecting => Self::Connecting,
            ConnectionState::Connected => Self::Connected,
            ConnectionState::Reconnecting { attempt } => Self::Reconnecting { attempt: *attempt },
            ConnectionState::Rejected { message } => Self::Rejected {
                message: message.clone(),
            },
        }
    }
}

impl PlayerStatus {
    /// What an open player is doing, read off its mirror.
    fn of(snapshot: &PlayerSnapshot) -> Self {
        match &snapshot.file {
            Some(file) => Self::Loaded {
                file: WatchedFile {
                    name: file.name.clone(),
                    duration_seconds: file.duration,
                },
                paused: snapshot.paused,
            },
            None => Self::Idle,
        }
    }
}

/// What a watch connects to, and as whom.
#[derive(Debug, Clone)]
pub struct WatchConfig {
    pub role: WatchRole,
    pub client: ClientConfig,
    /// How long to wait for the party to let this machine in.
    /// [`CONNECT_TIMEOUT`] everywhere but the tests.
    pub connect_timeout: Duration,
    /// Cancelled when nobody wants this watch any more — the app is exiting.
    /// It cuts short every wait the watch would otherwise sit out: for the
    /// party to answer, and for mpv to start. Where nothing will ever call a
    /// watch off, a token nobody cancels does.
    pub cancel: CancellationToken,
}

/// One machine watching one party.
///
/// Stopping it is explicit, because quitting mpv is something to wait for.
/// Dropping it without stopping still ends everything, just without waiting:
/// the client's task is aborted and the last handle on the player kills it.
pub struct WatchSession {
    role: WatchRole,
    client: SyncClient,
    launcher: Arc<dyn PlayerLauncher>,
    shared: Arc<Shared>,
    /// Where a newly launched player's events are sent.
    heard: Heard,
    bus: Arc<dyn EventBus>,
    publish_room: bool,
    /// Held across a launch and the load that follows it, so two of them
    /// never race for the one player window. `stop` does not take it: it must
    /// not wait behind a file that takes thirty seconds to open.
    player_lock: tokio::sync::Mutex<()>,
    publisher: Mutex<Option<JoinHandle<()>>>,
    /// Cancelled by `stop`, or with the config's token. A launch still in
    /// progress is abandoned then rather than waited out, which also kills
    /// the mpv it had started.
    cancel: CancellationToken,
}

/// What the publishing task and the handle both look at.
struct Shared {
    role: WatchRole,
    room: String,
    slot: Mutex<Slot>,
    /// Wakes the publishing task when the slot changed without an event —
    /// a launch starting or failing.
    changed: Notify,
}

struct Slot {
    /// Counts launches. An event from a player launched before the current
    /// one is old news and is ignored.
    generation: u64,
    player: Option<Live>,
    status: PlayerStatus,
    /// The last file the player opened, kept so reopening the player can
    /// open it again. Memory only: what somebody watched is not written
    /// anywhere.
    file: Option<PathBuf>,
    stopped: bool,
}

struct Live {
    tap: Arc<Tap>,
    pump: JoinHandle<()>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Slot> {
        self.slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn compose(&self, status: &ClientStatus) -> WatchState {
        WatchState {
            role: self.role,
            room: self.room.clone(),
            username: status.username.clone(),
            connection: WatchConnection::from(&status.connection),
            player: self.lock().status.clone(),
            ready: status.ready,
        }
    }

    /// Folds one player event into the slot.
    ///
    /// The status is read from the player's mirror rather than from the
    /// event, so however events are batched it ends up where the player is.
    fn observe(&self, generation: u64, event: &PlayerEvent) {
        let mut slot = self.lock();
        if generation != slot.generation || slot.stopped {
            return;
        }

        match event {
            PlayerEvent::Closed(reason) => {
                tracing::info!(?reason, "the player closed; the party goes on");
                slot.player = None;
                slot.status = PlayerStatus::Closed;
            }
            event => {
                let Some(live) = slot.player.as_ref() else {
                    return;
                };
                let snapshot = live.tap.peek();

                if let PlayerEvent::FileLoaded { file, .. } = event {
                    // A file dropped onto the window counts as much as one
                    // chosen here. Anything that is not a file on disk — a
                    // stream mpv was handed somehow — could not be reopened.
                    let path = PathBuf::from(&file.path);
                    if path.is_file() {
                        slot.file = Some(path);
                    }
                }
                slot.status = PlayerStatus::of(&snapshot);
            }
        }
    }
}

impl WatchSession {
    /// Connects to the party, then opens the player.
    ///
    /// Returns an error when the party would not have this machine: the
    /// server turned it away (`sync_rejected`) or never answered
    /// (`sync_failed`) — or when the config's token was cancelled before it
    /// did (`not_watching`). A player that will not start is not one of
    /// those. The session is up either way, and the state says the player
    /// failed, so the UI can offer to try again.
    ///
    /// A token cancelled while the player is starting abandons that launch,
    /// and the session still comes back, without a player: whoever cancelled
    /// it is the one to stop it.
    pub async fn start(
        config: WatchConfig,
        launcher: Arc<dyn PlayerLauncher>,
        bus: Arc<dyn EventBus>,
        publish_room: bool,
    ) -> Result<Self> {
        let room = config.client.room.clone();
        let cancel = config.cancel.child_token();
        let client = SyncClient::start(config.client);

        let connected = tokio::select! {
            connected = client.wait_connected(config.connect_timeout) => connected,
            () = cancel.cancelled() => Err(SyncPartyError::NotWatching),
        };
        if let Err(error) = connected {
            client.stop().await;
            return Err(error);
        }

        let shared = Arc::new(Shared {
            role: config.role,
            room,
            slot: Mutex::new(Slot {
                generation: 0,
                player: None,
                status: PlayerStatus::Starting,
                file: None,
                stopped: false,
            }),
            changed: Notify::new(),
        });

        let (heard, events) = mpsc::unbounded_channel();
        let publisher = tokio::spawn(publish(
            Arc::clone(&shared),
            client.subscribe(),
            events,
            Arc::clone(&bus),
            publish_room,
        ));

        let session = Self {
            role: config.role,
            client,
            launcher,
            shared,
            heard,
            bus,
            publish_room,
            player_lock: tokio::sync::Mutex::new(()),
            publisher: Mutex::new(Some(publisher)),
            cancel,
        };

        {
            let _launching = session.player_lock.lock().await;
            // A launch called off is not a player that failed: `launch` has
            // already said so, and the caller is about to stop the session.
            if let Err(error) = session.launch().await {
                if !session.cancel.is_cancelled() {
                    tracing::warn!(%error, "the player did not start; the party is joined without it");
                }
            }
        }

        Ok(session)
    }

    pub fn role(&self) -> WatchRole {
        self.role
    }

    pub fn state(&self) -> WatchState {
        self.shared.compose(&self.client.status())
    }

    /// Opens a file in the player, starting the player first if it has been
    /// closed.
    ///
    /// Only an existing regular file is accepted. The path comes from a file
    /// dialog, but it is checked here all the same: whatever reaches the
    /// player is opened, and a URL or a device has no place in a party.
    pub async fn open_file(&self, path: &Path) -> Result<()> {
        let path = regular_file(path).await?;

        let _loading = self.player_lock.lock().await;
        let player = match self.live_player() {
            Some(player) => player,
            None => {
                self.launch().await?;
                self.live_player().ok_or(SyncPartyError::NotWatching)?
            }
        };

        player.load(&path).await?;
        self.shared.lock().file = Some(path);
        Ok(())
    }

    /// Starts the player again after it was closed, and opens the last file
    /// it had open. The sync client brings that file to wherever the room is
    /// and pauses or plays it to match, exactly as for a file opened any
    /// other way.
    ///
    /// A no-op while a player is open: there is one window, never two.
    pub async fn reopen_player(&self) -> Result<()> {
        let _launching = self.player_lock.lock().await;
        if self.live_player().is_some() {
            return Ok(());
        }

        self.launch().await?;

        let file = self.shared.lock().file.clone();
        if let (Some(file), Some(player)) = (file, self.live_player()) {
            player.load(&file).await?;
        }
        Ok(())
    }

    /// The user's own Ready toggle.
    pub fn set_ready(&self, ready: bool) {
        self.client.set_ready(ready);
    }

    /// Leaves the party and closes the player. Idempotent.
    ///
    /// The UI hears `None` last, after the player has gone, and never a
    /// stale state after it.
    pub async fn stop(&self) {
        let live = {
            let mut slot = self.shared.lock();
            if std::mem::replace(&mut slot.stopped, true) {
                return;
            }
            slot.player.take()
        };
        self.cancel.cancel();

        let publisher = self
            .publisher
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(publisher) = publisher {
            publisher.abort();
            let _ = publisher.await;
        }

        self.client.stop().await;

        if let Some(live) = live {
            if let Err(error) = live.tap.quit().await {
                tracing::warn!(%error, "the player did not quit cleanly");
            }
            live.pump.abort();
        }

        if self.publish_room {
            self.bus.publish(AppEvent::RoomUpdated {
                snapshot: RoomSnapshot::disconnected(),
            });
        }
        self.bus.publish(AppEvent::WatchChanged { state: None });
    }

    /// The player, if one is open.
    fn live_player(&self) -> Option<Arc<Tap>> {
        self.shared
            .lock()
            .player
            .as_ref()
            .map(|live| Arc::clone(&live.tap))
            .filter(|tap| !tap.is_closed())
    }

    /// Launches a player and hands it to the client. The caller holds
    /// `player_lock`.
    async fn launch(&self) -> Result<()> {
        {
            let mut slot = self.shared.lock();
            if slot.stopped || self.cancel.is_cancelled() {
                return Err(SyncPartyError::NotWatching);
            }
            slot.status = PlayerStatus::Starting;
        }
        self.shared.changed.notify_one();

        // Dropping a launch part way through kills the mpv it started, so a
        // watch that is called off does not wait ten seconds for a window
        // only to close it again. A launch that has finished is taken even
        // if the call-off came in the same instant: the player is real by
        // then, and the slot below sees to it.
        let launched = tokio::select! {
            biased;
            launched = self.launcher.launch() => launched,
            () = self.cancel.cancelled() => {
                tracing::info!("a player still starting was abandoned");
                return Err(SyncPartyError::NotWatching);
            }
        };

        // Decided under the lock and acted on after it, so no guard is held
        // across a wait.
        let outcome = {
            let mut slot = self.shared.lock();
            match launched {
                Err(error) => {
                    if !slot.stopped {
                        slot.status = PlayerStatus::Failed {
                            message: reason(&error),
                        };
                    }
                    Err(error)
                }
                // Stopped while the player was starting: nobody is left to
                // watch it.
                Ok((player, _)) if slot.stopped => Ok(Err(player)),
                Ok((player, events)) => {
                    slot.generation += 1;
                    let (tap, to_client) =
                        Tap::new(player, events, slot.generation, self.heard.clone());
                    let pump = tokio::spawn(tap::pump(Arc::clone(&tap)));
                    slot.status = PlayerStatus::of(&tap.peek());
                    slot.player = Some(Live {
                        tap: Arc::clone(&tap),
                        pump,
                    });
                    Ok(Ok((tap, to_client)))
                }
            }
        };

        match outcome {
            Err(error) => {
                self.shared.changed.notify_one();
                Err(error)
            }
            Ok(Err(orphan)) => {
                if let Err(error) = orphan.quit().await {
                    tracing::warn!(%error, "a player started too late did not quit cleanly");
                }
                Err(SyncPartyError::NotWatching)
            }
            Ok(Ok((tap, to_client))) => {
                self.client.attach_player(tap as Arc<dyn Player>, to_client);
                self.shared.changed.notify_one();
                Ok(())
            }
        }
    }
}

impl Drop for WatchSession {
    fn drop(&mut self) {
        if let Some(publisher) = self
            .publisher
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            publisher.abort();
        }
        if let Some(live) = self.shared.lock().player.take() {
            live.pump.abort();
        }
    }
}

/// Tells the UI about every change, and nothing that is not one.
///
/// A position tick never reaches here as a change: the state holds no
/// position, so a tick leaves it equal to what was last published.
async fn publish(
    shared: Arc<Shared>,
    mut status: watch::Receiver<ClientStatus>,
    mut events: mpsc::UnboundedReceiver<(u64, PlayerEvent)>,
    bus: Arc<dyn EventBus>,
    publish_room: bool,
) {
    let mut published: Option<WatchState> = None;
    let mut published_room: Option<RoomSnapshot> = None;
    let mut client_running = true;

    loop {
        let current = status.borrow_and_update().clone();

        let state = shared.compose(&current);
        if published.as_ref() != Some(&state) {
            bus.publish(AppEvent::WatchChanged {
                state: Some(state.clone()),
            });
            published = Some(state);
        }

        if publish_room && published_room.as_ref() != Some(&current.room) {
            bus.publish(AppEvent::RoomUpdated {
                snapshot: current.room.clone(),
            });
            published_room = Some(current.room);
        }

        tokio::select! {
            changed = status.changed(), if client_running => {
                client_running = changed.is_ok();
            }
            Some((generation, event)) = events.recv() => {
                shared.observe(generation, &event);
                while let Ok((generation, event)) = events.try_recv() {
                    shared.observe(generation, &event);
                }
            }
            () = shared.changed.notified() => {}
        }
    }
}

/// The reason a launch failed, without the "the player failed" the error's
/// own message starts with: the state already says the player failed.
fn reason(error: &SyncPartyError) -> String {
    match error {
        SyncPartyError::PlayerFailed(message) => message.clone(),
        other => other.to_string(),
    }
}

/// `path` made absolute, if it names an existing regular file.
///
/// Absolute, not canonical: a symlink is checked through to what it points
/// at, but it is the link that is opened, so the room sees the name the user
/// picked rather than whatever the file is called on the disk behind it. It
/// also keeps Windows' `\\?\` form of a path, which `canonicalize` returns,
/// away from mpv. The same rule the mpv driver applies to whatever it loads.
async fn regular_file(path: &Path) -> Result<PathBuf> {
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

    Ok(absolute)
}
