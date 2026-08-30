//! The orchestrator the UI talks to.
//!
//! The transport, the server process, the room monitor and Discord are each
//! useful on their own; this is the piece that knows the order they go in and
//! how to unwind when a step fails partway through.

use std::net::SocketAddr;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::Mutex;
use ts_rs::TS;

use std::collections::BTreeSet;

use tokio::sync::OnceCell;

use crate::core::config::{generate_token, ConfigStore, SecretKey, SecretStore};
use crate::core::error::{Result, SyncPartyError};
use crate::core::events::{AppEvent, EventBus};
use crate::core::invite::Invite;
use crate::core::movie::MovieStore;
use crate::core::net::{ControlChannel, GuestTunnel, HostTunnel, PartyEndpoint, TransportReport};
use crate::core::notify::{self, DiscordNotifier};
use crate::core::syncplay::{
    ClientLauncher, MonitorConfig, RoomMonitor, ServerConfig, ServerController, ServerState,
};

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
    /// Where the Syncplay server is listening, for the host's own client and
    /// for diagnostics. Always on loopback — guests never see this.
    pub server_address: String,
    pub server: ServerState,
    pub monitor_attached: bool,
}

/// What a guest holds open for as long as it is in a party.
///
/// Both halves have to outlive the call that created them. The tunnel is the
/// route Syncplay's connection actually runs through, and the endpoint owns
/// the QUIC state underneath it, so dropping either mid-film disconnects the
/// guest even though the Syncplay window is still open.
struct GuestSession {
    tunnel: GuestTunnel,
    endpoint: PartyEndpoint,
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
            store: OnceCell::new(),
            log: Arc::new(Mutex::new(None)),
        }
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
            Some(guest) => guest.tunnel.send_control(&bytes).await,
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
            return Ok(guest.endpoint.report(vec![guest.tunnel.host_path()]));
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
        // change: Syncplay derives room operator passwords from it, and a new
        // salt silently invalidates every one of them.
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
    /// The order matters: the tunnel goes first so no guest is mid-forward
    /// into a server that is about to disappear, and the endpoint is closed
    /// rather than dropped so guests are told the party ended instead of
    /// waiting out a timeout.
    pub async fn stop_hosting(&self) -> Result<()> {
        let settings = self.settings.get();

        if settings.discord_enabled && matches!(self.state().await, SessionState::Hosting(_)) {
            let _ = self
                .discord
                .send(&notify::party_stopped(&settings.language))
                .await;
        }

        self.close_log().await;

        // Dropping the monitor aborts its task and closes the connection.
        self.monitor.lock().await.take();

        if let Some(network) = self.network.lock().await.take() {
            drop(network.tunnel);
            network.endpoint.close().await;
        }

        self.server.stop().await?;
        self.transition(SessionState::Idle).await;
        Ok(())
    }

    /// Joins a party as a guest: opens a tunnel to the host, then points
    /// Syncplay at the near end of it.
    ///
    /// The invite is only remembered once Syncplay has actually been launched,
    /// so a code that turns out to be unreachable is not the one restored on
    /// next startup.
    pub async fn join(&self, invite: &Invite) -> Result<()> {
        let nickname = self.settings.get().nickname;

        // Resolved before anything is spawned: a launcher that cannot be found
        // should fail without having opened a connection to the host first.
        let launcher = ClientLauncher::discover(&self.settings)?;

        let endpoint = PartyEndpoint::bind_joining().await?;
        let tunnel = GuestTunnel::open(
            endpoint.inner().clone(),
            invite.endpoint_id()?,
            Arc::clone(&self.control),
        )
        .await?;
        let address = tunnel.local_addr();

        launcher.join(invite, address, &nickname).await?;

        // Held for the length of the party. Assigning before the old session
        // is dropped keeps a re-join from briefly having no tunnel at all.
        *self.guest.lock().await = Some(GuestSession { tunnel, endpoint });

        self.secrets
            .set(SecretKey::LastInvite, &encode_last_invite(invite)?)
    }

    /// Reopens the last successfully launched guest session after an app restart.
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
        self.join(&invite).await?;
        Ok(Some(invite))
    }

    /// Closes a guest's tunnel. Syncplay keeps running; it simply loses the
    /// server, which is what leaving a party means.
    pub async fn leave(&self) -> Result<()> {
        if let Some(guest) = self.guest.lock().await.take() {
            drop(guest.tunnel);
            guest.endpoint.close().await;
        }

        Ok(())
    }

    /// Opens the host's own Syncplay client on the party it is running.
    ///
    /// This deliberately bypasses `join`: the host's client connects straight
    /// to the server on loopback, with no tunnel and no endpoint in between,
    /// and a host-local connection must not replace the invite that gets
    /// resumed on next startup.
    pub async fn join_as_host(&self) -> Result<()> {
        let SessionState::Hosting(info) = self.state().await else {
            return Err(SyncPartyError::ServerNotRunning);
        };

        let address = info
            .server_address
            .parse::<SocketAddr>()
            .map_err(|error| SyncPartyError::Other(format!("unusable server address: {error}")))?;

        let nickname = self.settings.get().nickname;
        ClientLauncher::discover(&self.settings)?
            .join(&info.invite, address, &nickname)
            .await
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

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use super::*;
    use crate::core::config::AppMode;
    use crate::core::events::test_support::RecordingEventBus;
    use crate::core::paths::AppPaths;

    /// Discards whatever arrives on the control channel — these tests are not
    /// exercising movie voting.
    struct NullControl;
    impl ControlChannel for NullControl {
        fn on_message(self: Arc<Self>, _peer: iroh::EndpointId, _bytes: Vec<u8>) {}
    }

    /// A server that records what it was asked to do without starting Python.
    #[derive(Default)]
    struct FakeServer {
        started: Mutex<Vec<ServerConfig>>,
        stops: Mutex<usize>,
        fail_on_start: bool,
    }

    #[async_trait]
    impl ServerController for FakeServer {
        async fn start(&self, config: &ServerConfig) -> Result<()> {
            if self.fail_on_start {
                return Err(SyncPartyError::ServerStartFailed("nope".to_owned()));
            }
            self.started.lock().await.push(config.clone());
            Ok(())
        }

        async fn stop(&self) -> Result<()> {
            *self.stops.lock().await += 1;
            Ok(())
        }

        async fn state(&self) -> ServerState {
            ServerState::Stopped
        }
    }

    /// Each test gets its own directory: they run in parallel, and a shared
    /// one means whichever test wipes it first breaks the others.
    fn session_with(
        label: &str,
        server: Arc<FakeServer>,
    ) -> (PartySession, Arc<RecordingEventBus>) {
        let dir = std::env::temp_dir().join(format!("syncparty-session-{label}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        let settings = Arc::new(ConfigStore::load(AppPaths::rooted_at(dir)).expect("settings"));
        settings
            .update(|s| s.mode = Some(AppMode::Host))
            .expect("update");

        let secrets = Arc::new(SecretStore::new());
        let bus = Arc::new(RecordingEventBus::default());

        let session = PartySession::new(
            settings,
            Arc::clone(&secrets),
            server,
            Arc::new(DiscordNotifier::new(secrets)),
            Arc::clone(&bus) as Arc<dyn EventBus>,
            Arc::new(NullControl),
        );

        (session, bus)
    }

    #[tokio::test]
    async fn a_new_session_is_idle() {
        let (session, _) = session_with("idle", Arc::new(FakeServer::default()));

        assert!(matches!(session.state().await, SessionState::Idle));
    }

    #[tokio::test]
    async fn stopping_an_idle_session_still_stops_the_server_and_returns_to_idle() {
        let server = Arc::new(FakeServer::default());
        let (session, bus) = session_with("stop", Arc::clone(&server));

        session.stop_hosting().await.expect("stop");

        assert_eq!(*server.stops.lock().await, 1);
        assert!(matches!(session.state().await, SessionState::Idle));
        assert!(bus
            .events()
            .iter()
            .any(|event| matches!(event, AppEvent::SessionChanged { .. })));
    }

    #[tokio::test]
    async fn leaving_without_having_joined_is_not_an_error() {
        let (session, _) = session_with("leave-idle", Arc::new(FakeServer::default()));

        assert!(session.leave().await.is_ok());
    }

    #[tokio::test]
    async fn a_failed_start_leaves_no_server_running() {
        let server = Arc::new(FakeServer {
            fail_on_start: true,
            ..FakeServer::default()
        });
        let (session, _) = session_with("failed-start", Arc::clone(&server));

        // On a machine with no network the endpoint step fails first; on one
        // with network the fake server fails instead. Either way the cleanup
        // path is what this is checking.
        let _ = session.start_hosting().await;

        assert!(
            *server.stops.lock().await >= 1,
            "a failed start must stop whatever it started"
        );
        assert!(matches!(
            session.state().await,
            SessionState::Idle | SessionState::Failed { .. }
        ));
    }

    #[test]
    fn ignores_a_corrupt_saved_invite() {
        assert!(parse_last_invite("not an invite").is_none());
    }

    #[test]
    fn saved_invites_round_trip() {
        let invite = Invite {
            endpoint: iroh::SecretKey::generate().public().to_string(),
            password: "swordfish".to_owned(),
            room: "MovieNight".to_owned(),
        };

        assert_eq!(
            parse_last_invite(&encode_last_invite(&invite).expect("encode")),
            Some(invite)
        );
    }

    #[test]
    fn a_saved_invite_from_the_tailscale_era_is_discarded_rather_than_resumed() {
        // The old shape, as it would still be sitting in the keychain after an
        // upgrade. It has no endpoint, so there is nothing to dial; being
        // rejected here is what makes the app clear it and start clean.
        let legacy = r#"{"host":"100.127.167.56","alternateHosts":[],"port":8999,
                         "password":"swordfish","room":"MovieNight"}"#;

        assert!(parse_last_invite(legacy).is_none());
    }
}
