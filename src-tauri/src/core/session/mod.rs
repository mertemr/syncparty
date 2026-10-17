//! The orchestrator the UI talks to.
//!
//! The transport, the server, the room monitor, the watch session and Discord
//! are each useful on their own; this is the piece that knows the order they
//! go in and how to unwind when a step fails partway through.
//!
//! It also keeps the one rule that holds across host and guest: this machine
//! watches one party at a time, in one player. Whatever starts a watch stops
//! the one before it.

mod route;

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::{Mutex, OnceCell};
use ts_rs::TS;

use crate::core::config::{generate_token, ConfigStore, SecretKey, SecretStore};
use crate::core::error::{Result, SyncPartyError};
use crate::core::events::{AppEvent, EventBus};
use crate::core::invite::Invite;
use crate::core::movie::MovieStore;
use crate::core::net::{ControlChannel, HostTunnel, PartyEndpoint, TransportReport};
use crate::core::notify::{self, DiscordNotifier};
use crate::core::player::PlayerLauncher;
use crate::core::syncplay::{
    ClientConfig, ClientTiming, MonitorConfig, RoomMonitor, ServerConfig, ServerController,
    ServerState,
};
use crate::core::watch::{
    WatchConfig, WatchConnection, WatchRole, WatchSession, WatchState, CONNECT_TIMEOUT,
};

use self::route::{Dialer, Route, TunnelDialer};

/// Finds the player to launch, at the moment a watch starts.
///
/// Resolved then rather than once at startup, so an mpv installed or located
/// while the app is open is the one that gets used. Fails with
/// `dependency_missing` when there is no player to be found.
pub type PlayerSource = Arc<dyn Fn(&ConfigStore) -> Result<Arc<dyn PlayerLauncher>> + Send + Sync>;

/// How long the app gives everything to wind down when it exits: long enough
/// for mpv to be asked to quit and, failing that, killed.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(8);

/// How long telling Discord the party is over may take. The webhook client
/// has no timeout of its own, and a network that swallows the request would
/// otherwise hold stopping the party — or the app's exit — for as long as it
/// liked.
const ANNOUNCE_TIMEOUT: Duration = Duration::from_secs(3);

/// Length of the generated server password. Long enough that guessing is
/// hopeless, short enough to read aloud if the link ever fails.
const PASSWORD_LENGTH: usize = 18;
const SALT_LENGTH: usize = 10;

/// Where the host is in the start-up sequence.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub enum SessionState {
    Idle,
    /// Carries a translation key rather than prose, so the UI picks the
    /// wording and this module stays out of the i18n business.
    Starting {
        step: StartupStep,
    },
    Hosting(Box<HostingInfo>),
    Failed {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum StartupStep {
    JoiningNetwork,
    StartingServer,
    OpeningTunnel,
    AttachingMonitor,
}

/// Everything the host screen needs once a party is live.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct HostingInfo {
    pub invite: Invite,
    pub invite_code: String,
    pub deep_link: String,
    /// Where the sync server is listening, for the host's own client and for
    /// diagnostics. Always on loopback — guests never see this.
    pub server_address: String,
    pub server: ServerState,
    pub monitor_attached: bool,
}

/// What a guest holds open for as long as it is in a party. Its watch is in
/// the session's one watch slot, beside it.
struct GuestSession {
    /// The party joined, so joining the same one again can be recognised.
    invite: Invite,
    route: Box<dyn Route>,
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// What a host holds open while a party is running.
struct HostNetwork {
    tunnel: HostTunnel,
    endpoint: PartyEndpoint,
}

pub struct PartySession {
    settings: Arc<ConfigStore>,
    secrets: Arc<SecretStore>,
    server: Arc<dyn ServerController>,
    discord: Arc<DiscordNotifier>,
    bus: Arc<dyn EventBus>,
    /// Whoever owns the control channel's application data — movie voting,
    /// today. `session` only knows it needs handing to every tunnel it opens.
    control: Arc<dyn ControlChannel>,
    state: Mutex<SessionState>,
    monitor: Mutex<Option<RoomMonitor>>,
    network: Mutex<Option<HostNetwork>>,
    guest: Mutex<Option<GuestSession>>,
    players: PlayerSource,
    dialer: Arc<dyn Dialer>,
    /// The one watch on this machine, the host's own or a guest's. A `std`
    /// mutex: it is only ever held to look or swap, never across an `await`,
    /// so reading the state never waits behind a file being opened.
    watch: std::sync::Mutex<Option<Arc<WatchSession>>>,
    /// Held by everything that starts or ends a watch or a party, so that two
    /// joins — a double click, a resume racing a paste — happen one after
    /// the other and the second finds the first already done.
    lifecycle: Mutex<()>,
    /// Set once the app is exiting, so a join still in flight does not leave
    /// a player behind.
    closing: AtomicBool,
    /// Where the party log is written. Attached after construction, like
    /// `MovieVote`'s — `session` is built before the store exists.
    store: OnceCell<Arc<MovieStore>>,
    /// The night currently being recorded: its id, and everyone the monitor
    /// has reported in the room since it started.
    log: Arc<Mutex<Option<PartyLog>>>,
}

struct PartyLog {
    id: String,
    /// A set, and never cleared while the party runs: someone who drops out
    /// of the call for ten minutes still watched the film.
    roster: BTreeSet<String>,
}

impl PartySession {
    pub fn new(
        settings: Arc<ConfigStore>,
        secrets: Arc<SecretStore>,
        server: Arc<dyn ServerController>,
        discord: Arc<DiscordNotifier>,
        bus: Arc<dyn EventBus>,
        control: Arc<dyn ControlChannel>,
        players: PlayerSource,
    ) -> Self {
        Self {
            settings,
            secrets,
            server,
            discord,
            bus,
            control,
            state: Mutex::new(SessionState::Idle),
            monitor: Mutex::new(None),
            network: Mutex::new(None),
            guest: Mutex::new(None),
            players,
            dialer: Arc::new(TunnelDialer),
            watch: std::sync::Mutex::new(None),
            lifecycle: Mutex::new(()),
            closing: AtomicBool::new(false),
            store: OnceCell::new(),
            log: Arc::new(Mutex::new(None)),
        }
    }

    /// Swaps the way guests reach the host, for tests that stand a local
    /// server in for one.
    #[cfg(test)]
    fn with_dialer(mut self, dialer: Arc<dyn Dialer>) -> Self {
        self.dialer = dialer;
        self
    }

    pub fn attach_store(&self, store: Arc<MovieStore>) {
        let _ = self.store.set(store);
    }

    /// What the room is watching tonight, recorded against the running
    /// party. Passing `None` clears it — the host picked wrong, or the
    /// evening moved on to something else.
    pub async fn set_now_watching(
        &self,
        tmdb_id: Option<i64>,
        title: Option<&str>,
        poster: Option<&str>,
    ) -> Result<()> {
        let log = self.log.lock().await;
        let (Some(log), Some(store)) = (log.as_ref(), self.store.get()) else {
            return Err(SyncPartyError::Config(
                "no party is running to set a movie on".to_owned(),
            ));
        };

        store.set_party_movie(&log.id, tmdb_id, title, poster)
    }

    /// Sends `bytes` down every guest's control channel. A no-op with no
    /// party running or no guests connected.
    pub async fn broadcast_control(&self, bytes: Vec<u8>) {
        if let Some(network) = self.network.lock().await.as_ref() {
            network.tunnel.broadcast_control(&bytes).await;
        }
    }

    /// Sends `bytes` to the host down this guest's control channel. An error
    /// if this machine is not currently in a party as a guest.
    pub async fn send_control(&self, bytes: Vec<u8>) -> Result<()> {
        match self.guest.lock().await.as_ref() {
            Some(guest) => guest.route.send_control(&bytes).await,
            None => Err(SyncPartyError::NotInParty),
        }
    }

    /// Sends `bytes` down one guest's control channel — used to hydrate a
    /// single guest (on reconnect, say) without broadcasting to everyone.
    pub async fn send_control_to(&self, peer: iroh::EndpointId, bytes: Vec<u8>) {
        if let Some(network) = self.network.lock().await.as_ref() {
            network.tunnel.send_control_to(peer, &bytes).await;
        }
    }

    pub async fn state(&self) -> SessionState {
        self.state.lock().await.clone()
    }

    /// How this machine is placed on the network, and how any live connection
    /// is being carried.
    ///
    /// Measured on whichever endpoint the session already has, so a running
    /// party is reported rather than disturbed. With no party there is no
    /// endpoint to ask, and one is bound for the length of the check — that is
    /// the only way to learn whether the relays answer and what address the
    /// outside world sees, and it is what the user pressed the button for.
    pub async fn transport(&self) -> Result<TransportReport> {
        if let Some(network) = self.network.lock().await.as_ref() {
            return Ok(network.endpoint.report(network.tunnel.peers()));
        }

        if let Some(guest) = self.guest.lock().await.as_ref() {
            return Ok(guest.route.report());
        }

        let endpoint = PartyEndpoint::bind_joining().await?;
        endpoint.wait_online().await?;
        let report = endpoint.report(Vec::new());
        endpoint.close().await;

        Ok(report)
    }

    async fn transition(&self, state: SessionState) {
        *self.state.lock().await = state.clone();
        self.bus.publish(AppEvent::SessionChanged { state });
    }

    /// Brings a party up end to end.
    ///
    /// On failure everything already started is torn down again rather than
    /// left running in the background where the next attempt would collide
    /// with it.
    pub async fn start_hosting(&self) -> Result<HostingInfo> {
        match self.start_hosting_inner().await {
            Ok(info) => Ok(info),
            Err(error) => {
                let _ = self.stop_hosting().await;
                self.transition(SessionState::Failed {
                    message: error.to_string(),
                })
                .await;
                Err(error)
            }
        }
    }

    async fn start_hosting_inner(&self) -> Result<HostingInfo> {
        let settings = self.settings.get();

        self.transition(SessionState::Starting {
            step: StartupStep::JoiningNetwork,
        })
        .await;

        // First, because it is the step most likely to fail and the only one
        // that depends on the outside world. Failing here costs nothing to
        // unwind, whereas failing after the server is up does not.
        let endpoint = PartyEndpoint::bind_hosting(&self.secrets).await?;
        endpoint.wait_online().await?;
        let host_endpoint = endpoint.id();

        // Generated once and reused forever. The salt in particular must not
        // change: room operator passwords are derived from it, as Syncplay
        // derives them, and a new salt silently invalidates every one.
        let password = self
            .secrets
            .get_or_create(SecretKey::ServerPassword, PASSWORD_LENGTH)?;
        let salt = self
            .secrets
            .get_or_create(SecretKey::ServerSalt, SALT_LENGTH)?;

        self.transition(SessionState::Starting {
            step: StartupStep::StartingServer,
        })
        .await;

        let config = ServerConfig {
            port: settings.port,
            password: password.clone(),
            salt,
            timing: Default::default(),
        };
        self.server.start(&config).await?;

        self.transition(SessionState::Starting {
            step: StartupStep::OpeningTunnel,
        })
        .await;

        // Started only once the server is answering, so a guest that arrives
        // in the same instant is forwarded to something that exists.
        let tunnel = HostTunnel::start(
            endpoint.inner().clone(),
            config.local_address(),
            Arc::clone(&self.control),
        );
        *self.network.lock().await = Some(HostNetwork { tunnel, endpoint });

        let invite = Invite {
            endpoint: host_endpoint.to_string(),
            password: password.clone(),
            room: settings.room.clone(),
        };

        let monitor_attached = if settings.monitor_enabled {
            self.transition(SessionState::Starting {
                step: StartupStep::AttachingMonitor,
            })
            .await;

            self.attach_monitor(config.local_address(), &password, &settings.room)
                .await;
            true
        } else {
            false
        };

        let info = HostingInfo {
            invite_code: invite.encode(),
            deep_link: invite.deep_link(),
            server_address: config.local_address().to_string(),
            server: self.server.state().await,
            monitor_attached,
            invite,
        };

        self.transition(SessionState::Hosting(Box::new(info.clone())))
            .await;

        if settings.discord_enabled {
            // A failed announcement must not take the party down with it.
            if let Err(error) = self
                .discord
                .send(&notify::party_ready(&info.invite, &settings.language))
                .await
            {
                self.bus.publish(AppEvent::Failed {
                    error_kind: error.kind().to_owned(),
                    message: error.to_string(),
                });
            }
        }

        Ok(info)
    }

    /// Attaches the monitor and republishes its snapshots as events.
    ///
    /// Connects straight to the server on loopback rather than going out
    /// through the tunnel and back: the monitor runs in this very process, so
    /// a round trip through QUIC would buy nothing but latency.
    async fn attach_monitor(&self, address: SocketAddr, password: &str, room: &str) {
        let monitor = RoomMonitor::start(MonitorConfig {
            host: address.ip().to_string(),
            port: address.port(),
            password: password.to_owned(),
            room: room.to_owned(),
        });

        let mut updates = monitor.subscribe();
        let bus = Arc::clone(&self.bus);
        let log = Arc::clone(&self.log);

        tokio::spawn(async move {
            // Ends on its own when the monitor is dropped and the sender goes
            // away, so there is nothing extra to cancel.
            while updates.changed().await.is_ok() {
                let snapshot = updates.borrow_and_update().clone();

                // The roster is built here rather than read at the end: by
                // the time a party stops, the room is already empty.
                if let Some(entry) = log.lock().await.as_mut() {
                    for room in &snapshot.rooms {
                        for watcher in &room.watchers {
                            entry.roster.insert(watcher.name.clone());
                        }
                    }
                }

                bus.publish(AppEvent::RoomUpdated { snapshot });
            }
        });

        *self.monitor.lock().await = Some(monitor);
        self.open_log().await;
    }

    /// Opens a party's record. Anything that fails here is logged and
    /// dropped: a night that goes unrecorded is a worse outcome than a night
    /// that does not start, so this must never fail a party.
    async fn open_log(&self) {
        let Some(store) = self.store.get() else {
            return;
        };
        let Ok(id) = generate_token(12) else {
            return;
        };

        if let Err(error) = store.open_party_log(&id, now_millis()) {
            tracing::warn!(%error, "could not open the party log");
            return;
        }

        *self.log.lock().await = Some(PartyLog {
            id,
            roster: BTreeSet::new(),
        });
    }

    async fn close_log(&self) {
        let Some(log) = self.log.lock().await.take() else {
            return;
        };
        let Some(store) = self.store.get() else {
            return;
        };

        let roster: Vec<String> = log.roster.into_iter().collect();
        if let Err(error) = store.close_party_log(&log.id, now_millis(), &roster) {
            tracing::warn!(%error, "could not close the party log");
        }
    }

    /// Stops the party.
    ///
    /// The host's own watch goes first, so its player closes rather than
    /// sitting there disconnected. After that the order still matters: the
    /// tunnel goes before the server so no guest is mid-forward into a server
    /// that is about to disappear, and the endpoint is closed rather than
    /// dropped so guests are told the party ended instead of waiting out a
    /// timeout. Discord hears about it last, once everyone in the room
    /// already has.
    pub async fn stop_hosting(&self) -> Result<()> {
        let _lifecycle = self.lifecycle.lock().await;
        self.stop_hosting_locked().await
    }

    async fn stop_hosting_locked(&self) -> Result<()> {
        self.stop_watch(Some(WatchRole::Host)).await;

        let settings = self.settings.get();
        let announce =
            settings.discord_enabled && matches!(self.state().await, SessionState::Hosting(_));

        self.close_log().await;

        // Dropping the monitor aborts its task and closes the connection.
        self.monitor.lock().await.take();

        if let Some(network) = self.network.lock().await.take() {
            drop(network.tunnel);
            network.endpoint.close().await;
        }

        let stopped = self.server.stop().await;
        if stopped.is_ok() {
            self.transition(SessionState::Idle).await;
        }

        // After the party is down rather than before, so a slow webhook can
        // only delay the message, never the guests being told; and bounded,
        // so it cannot use up the time the app has to exit.
        if announce {
            let sent = tokio::time::timeout(
                ANNOUNCE_TIMEOUT,
                self.discord
                    .send(&notify::party_stopped(&settings.language)),
            )
            .await;
            match sent {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => tracing::warn!(%error, "could not tell Discord the party ended"),
                Err(_) => tracing::warn!("gave up telling Discord the party ended"),
            }
        }

        stopped
    }

    /// Joins a party as a guest: opens a route to the host, connects the sync
    /// client through it, then opens the player.
    ///
    /// Joining the party this machine is already in does nothing, which is
    /// what lets the UI resume on every mount without opening a second
    /// player. Joining a different one leaves the first — and so does joining
    /// the same one over a route that has died or a connection the party has
    /// since refused, since staying would leave the guest stuck where it is.
    ///
    /// The invite is only remembered once the party has let this machine in,
    /// so a code that turns out to be unreachable, or a password the party
    /// refuses, is not the one restored on next startup.
    pub async fn join(&self, invite: &Invite) -> Result<()> {
        let _lifecycle = self.lifecycle.lock().await;

        let already_in = match self.guest.lock().await.as_ref() {
            Some(guest) if guest.invite == *invite => self.still_in(guest),
            _ => false,
        };
        if already_in {
            return Ok(());
        }

        // Before anything is opened: with no player to drive there is no
        // point dialling the host, nor leaving a party that is still working.
        let launcher = (self.players)(&self.settings)?;

        self.leave_locked().await;
        self.stop_watch(None).await;

        let route = self.dialer.dial(invite, Arc::clone(&self.control)).await?;
        let config = self.watch_config(
            WatchRole::Guest,
            route.local_addr(),
            &invite.room,
            &invite.password,
        );

        let watch = match WatchSession::start(config, launcher, Arc::clone(&self.bus), true).await {
            Ok(watch) => watch,
            Err(error) => {
                route.close().await;
                return Err(error);
            }
        };

        if self.closing.load(Ordering::SeqCst) {
            watch.stop().await;
            route.close().await;
            return Err(closing());
        }

        self.install_watch(watch);
        *self.guest.lock().await = Some(GuestSession {
            invite: invite.clone(),
            route,
        });

        // The party is joined whether or not it can be remembered; failing
        // the join over it would leave the UI saying no with a film playing.
        let remembered = encode_last_invite(invite)
            .and_then(|encoded| self.secrets.set(SecretKey::LastInvite, &encoded));
        if let Err(error) = remembered {
            tracing::warn!(%error, "could not remember the party for next time");
        }
        Ok(())
    }

    /// Whether the party `guest` is in is still being watched over a route
    /// that works, so that joining it again has nothing to fix.
    fn still_in(&self, guest: &GuestSession) -> bool {
        let watching = self.current_watch().is_some_and(|watch| {
            watch.role() == WatchRole::Guest
                && !matches!(watch.state().connection, WatchConnection::Rejected { .. })
        });
        watching && guest.route.is_alive()
    }

    /// Rejoins the party this machine was last in, after an app restart.
    ///
    /// Safe to call while already in it: joining the same invite is a no-op
    /// as long as the party is still reachable.
    /// A saved invite the party refuses outright is forgotten, since it will
    /// be refused on every start from now on; one whose host is merely away
    /// is kept for when they are back.
    pub async fn resume_last_session(&self) -> Result<Option<Invite>> {
        let Some(raw) = self.secrets.get(SecretKey::LastInvite)? else {
            return Ok(None);
        };

        let invite = match parse_last_invite(&raw) {
            Some(invite) => invite,
            None => {
                self.secrets.delete(SecretKey::LastInvite)?;
                return Ok(None);
            }
        };

        match self.join(&invite).await {
            Ok(()) => Ok(Some(invite)),
            Err(error @ SyncPartyError::SyncRejected(_)) => {
                self.secrets.delete(SecretKey::LastInvite)?;
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    /// Leaves the party a guest is in: closes the player, ends the sync
    /// connection, then closes the route to the host.
    pub async fn leave(&self) -> Result<()> {
        let _lifecycle = self.lifecycle.lock().await;
        self.leave_locked().await;
        Ok(())
    }

    async fn leave_locked(&self) {
        let Some(guest) = self.guest.lock().await.take() else {
            return;
        };

        self.stop_watch(Some(WatchRole::Guest)).await;
        guest.route.close().await;
    }

    /// Starts watching the party this machine is hosting.
    ///
    /// This deliberately bypasses `join`: the host's client connects straight
    /// to the server on loopback, with no tunnel and no endpoint in between,
    /// and a host-local connection must not replace the invite that gets
    /// resumed on next startup. A no-op if the host is already watching.
    pub async fn join_as_host(&self) -> Result<()> {
        let _lifecycle = self.lifecycle.lock().await;

        let SessionState::Hosting(info) = self.state().await else {
            return Err(SyncPartyError::ServerNotRunning);
        };
        if self
            .current_watch()
            .is_some_and(|watch| watch.role() == WatchRole::Host)
        {
            return Ok(());
        }

        let address = info
            .server_address
            .parse::<SocketAddr>()
            .map_err(|error| SyncPartyError::Other(format!("unusable server address: {error}")))?;
        let launcher = (self.players)(&self.settings)?;

        self.leave_locked().await;
        self.stop_watch(None).await;

        let config = self.watch_config(
            WatchRole::Host,
            address,
            &info.invite.room,
            &info.invite.password,
        );
        // With the monitor attached the room panel already has a source, and
        // two writers would take turns overwriting each other.
        let publish_room = !info.monitor_attached;
        let watch =
            WatchSession::start(config, launcher, Arc::clone(&self.bus), publish_room).await?;

        if self.closing.load(Ordering::SeqCst) {
            watch.stop().await;
            return Err(closing());
        }

        self.install_watch(watch);
        Ok(())
    }

    /// Closes the host's own player and its connection, leaving the party
    /// running for everyone else. A guest's watch is left alone: for a guest,
    /// stopping watching is leaving, which is `leave`.
    pub async fn stop_watching(&self) -> Result<()> {
        let _lifecycle = self.lifecycle.lock().await;
        self.stop_watch(Some(WatchRole::Host)).await;
        Ok(())
    }

    /// What this machine is watching, if anything.
    pub fn watch_state(&self) -> Option<WatchState> {
        self.current_watch().map(|watch| watch.state())
    }

    /// Opens a file in this machine's player.
    pub async fn open_watch_file(&self, path: &Path) -> Result<()> {
        self.watching()?.open_file(path).await
    }

    /// Starts the player again after it was closed.
    pub async fn reopen_player(&self) -> Result<()> {
        self.watching()?.reopen_player().await
    }

    /// The Ready toggle.
    pub fn set_ready(&self, ready: bool) -> Result<()> {
        self.watching()?.set_ready(ready);
        Ok(())
    }

    /// Ends everything, for the app exiting.
    ///
    /// The player goes first and without waiting on anything else: it is the
    /// one thing that would outlive syncparty, carrying on unsynchronised.
    /// All of it is bounded, because an exit that hangs is worse than one
    /// that leaves a party to time out.
    pub async fn shutdown(&self) {
        self.closing.store(true, Ordering::SeqCst);

        let everything = async {
            self.stop_watch(None).await;

            let _lifecycle = self.lifecycle.lock().await;
            self.leave_locked().await;
            if !matches!(self.state().await, SessionState::Idle) {
                if let Err(error) = self.stop_hosting_locked().await {
                    tracing::warn!(%error, "could not stop the party cleanly");
                }
            }
        };

        if tokio::time::timeout(SHUTDOWN_TIMEOUT, everything)
            .await
            .is_err()
        {
            tracing::warn!("gave up waiting for the party to wind down");
        }
    }

    fn watch_config(
        &self,
        role: WatchRole,
        address: SocketAddr,
        room: &str,
        password: &str,
    ) -> WatchConfig {
        let settings = self.settings.get();

        WatchConfig {
            role,
            client: ClientConfig {
                address,
                username: settings.nickname,
                room: room.to_owned(),
                password: password.to_owned(),
                language: settings.language,
                timing: ClientTiming::default(),
            },
            connect_timeout: CONNECT_TIMEOUT,
        }
    }

    fn watch_slot(&self) -> std::sync::MutexGuard<'_, Option<Arc<WatchSession>>> {
        self.watch
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn current_watch(&self) -> Option<Arc<WatchSession>> {
        self.watch_slot().clone()
    }

    fn watching(&self) -> Result<Arc<WatchSession>> {
        self.current_watch().ok_or(SyncPartyError::NotWatching)
    }

    /// Takes the watch out of its slot and stops it — any watch, or only one
    /// in `role`.
    async fn stop_watch(&self, role: Option<WatchRole>) {
        let watch = {
            let mut slot = self.watch_slot();
            let matches = slot
                .as_ref()
                .is_some_and(|watch| role.is_none_or(|role| watch.role() == role));
            if matches {
                slot.take()
            } else {
                None
            }
        };

        if let Some(watch) = watch {
            watch.stop().await;
        }
    }

    fn install_watch(&self, watch: WatchSession) {
        let previous = self.watch_slot().replace(Arc::new(watch));
        // Every caller stops the previous watch first, under the lifecycle
        // lock, so there is never one here to replace.
        debug_assert!(
            previous.is_none(),
            "a watch was replaced without being stopped"
        );
    }

    pub fn clear_last_session(&self) -> Result<()> {
        self.secrets.delete(SecretKey::LastInvite)
    }
}

fn parse_last_invite(raw: &str) -> Option<Invite> {
    serde_json::from_str(raw).ok()
}

fn encode_last_invite(invite: &Invite) -> Result<String> {
    Ok(serde_json::to_string(invite)?)
}

fn closing() -> SyncPartyError {
    SyncPartyError::SyncFailed("syncparty is closing".to_owned())
}

#[cfg(test)]
mod tests;
