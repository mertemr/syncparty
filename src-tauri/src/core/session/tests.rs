use std::sync::atomic::AtomicUsize;

use async_trait::async_trait;

use super::*;
use crate::core::config::AppMode;
use crate::core::events::test_support::RecordingEventBus;
use crate::core::events::NullEventBus;
use crate::core::paths::AppPaths;
use crate::core::player::fake::{Command, FakeLauncher, FakePlayer};
use crate::core::syncplay::{NativeServer, ServerTiming};

/// Discards whatever arrives on the control channel — these tests are not
/// exercising movie voting.
struct NullControl;
impl ControlChannel for NullControl {
    fn on_message(self: Arc<Self>, _peer: iroh::EndpointId, _bytes: Vec<u8>) {}
}

/// A server that records what it was asked to do and runs nothing.
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

/// Stands a server on this machine in for the host: every invite leads
/// straight to it, with no endpoint and no tunnel.
struct LoopbackDialer {
    address: SocketAddr,
    dials: AtomicUsize,
    closes: Arc<AtomicUsize>,
}

impl LoopbackDialer {
    fn new(address: SocketAddr) -> Self {
        Self {
            address,
            dials: AtomicUsize::new(0),
            closes: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn dials(&self) -> usize {
        self.dials.load(Ordering::SeqCst)
    }

    fn closes(&self) -> usize {
        self.closes.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Dialer for LoopbackDialer {
    async fn dial(
        &self,
        _invite: &Invite,
        _control: Arc<dyn ControlChannel>,
    ) -> Result<Box<dyn Route>> {
        self.dials.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(LoopbackRoute {
            address: self.address,
            closes: Arc::clone(&self.closes),
        }))
    }
}

struct LoopbackRoute {
    address: SocketAddr,
    closes: Arc<AtomicUsize>,
}

#[async_trait]
impl Route for LoopbackRoute {
    fn local_addr(&self) -> SocketAddr {
        self.address
    }

    fn report(&self) -> TransportReport {
        TransportReport {
            endpoint_id: "loopback".to_owned(),
            addresses: Vec::new(),
            behind_carrier_nat: None,
            relays: Vec::new(),
            peers: Vec::new(),
        }
    }

    async fn send_control(&self, _bytes: &[u8]) -> Result<()> {
        Ok(())
    }

    async fn close(self: Box<Self>) {
        self.closes.fetch_add(1, Ordering::SeqCst);
    }
}

/// Each test gets its own directory: they run in parallel, and a shared one
/// means whichever test wipes it first breaks the others.
fn stores(label: &str) -> (Arc<ConfigStore>, Arc<SecretStore>) {
    let dir =
        std::env::temp_dir().join(format!("syncparty-session-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");

    let paths = AppPaths::rooted_at(dir);
    let settings = Arc::new(ConfigStore::load(paths.clone()).expect("settings"));
    settings
        .update(|s| {
            s.mode = Some(AppMode::Host);
            s.nickname = "alice".to_owned();
        })
        .expect("update");

    (settings, Arc::new(SecretStore::in_file(paths)))
}

fn players_from(launcher: &Arc<FakeLauncher>) -> PlayerSource {
    let launcher = Arc::clone(launcher);
    Arc::new(move |_: &ConfigStore| Ok(Arc::clone(&launcher) as Arc<dyn PlayerLauncher>))
}

fn session_with(label: &str, server: Arc<FakeServer>) -> (PartySession, Arc<RecordingEventBus>) {
    let (settings, secrets) = stores(label);
    let bus = Arc::new(RecordingEventBus::default());

    let session = PartySession::new(
        settings,
        Arc::clone(&secrets),
        server,
        Arc::new(DiscordNotifier::new(secrets)),
        Arc::clone(&bus) as Arc<dyn EventBus>,
        Arc::new(NullControl),
        players_from(&Arc::new(FakeLauncher::new())),
    );

    (session, bus)
}

const PASSWORD: &str = "swordfish";

/// A session wired to a real server on loopback and to fake players, so a
/// party can be joined, hosted and left with nothing leaving this machine.
struct Party {
    session: PartySession,
    bus: Arc<RecordingEventBus>,
    secrets: Arc<SecretStore>,
    launcher: Arc<FakeLauncher>,
    dialer: Arc<LoopbackDialer>,
    server: Arc<NativeServer>,
    config: ServerConfig,
}

impl Party {
    async fn new(label: &str) -> Self {
        Self::with_players(label, None).await
    }

    /// `players` stands in for how mpv is found; `None` finds the fakes.
    async fn with_players(label: &str, players: Option<PlayerSource>) -> Self {
        let (settings, secrets) = stores(label);
        let bus = Arc::new(RecordingEventBus::default());
        let launcher = Arc::new(FakeLauncher::new());

        let config = ServerConfig {
            port: free_port(),
            password: PASSWORD.to_owned(),
            salt: "PEPPER".to_owned(),
            timing: ServerTiming {
                first_state_delay: Duration::from_millis(10),
                state_interval: Duration::from_millis(200),
                ..ServerTiming::default()
            },
        };
        let server = Arc::new(NativeServer::new(Arc::new(NullEventBus)));
        server.start(&config).await.expect("the server starts");
        let dialer = Arc::new(LoopbackDialer::new(config.local_address()));

        let session = PartySession::new(
            settings,
            Arc::clone(&secrets),
            Arc::clone(&server) as Arc<dyn ServerController>,
            Arc::new(DiscordNotifier::new(Arc::clone(&secrets))),
            Arc::clone(&bus) as Arc<dyn EventBus>,
            Arc::new(NullControl),
            players.unwrap_or_else(|| players_from(&launcher)),
        )
        .with_dialer(Arc::clone(&dialer) as Arc<dyn Dialer>);

        Self {
            session,
            bus,
            secrets,
            launcher,
            dialer,
            server,
            config,
        }
    }

    fn invite(&self, room: &str, password: &str) -> Invite {
        Invite {
            endpoint: iroh::SecretKey::generate().public().to_string(),
            password: password.to_owned(),
            room: room.to_owned(),
        }
    }

    /// Puts the session where `start_hosting` leaves it, without the
    /// endpoint that would need the internet.
    async fn hosting(&self, monitor_attached: bool) {
        let info = HostingInfo {
            invite: self.invite("MovieNight", PASSWORD),
            invite_code: String::new(),
            deep_link: String::new(),
            server_address: self.config.local_address().to_string(),
            server: self.server.state().await,
            monitor_attached,
        };
        *self.session.state.lock().await = SessionState::Hosting(Box::new(info));
    }

    fn player(&self) -> Arc<FakePlayer> {
        self.launcher.last().expect("a player was launched")
    }

    fn remembered(&self) -> Option<Invite> {
        self.secrets
            .get(SecretKey::LastInvite)
            .expect("read")
            .and_then(|raw| parse_last_invite(&raw))
    }

    fn rooms_published(&self) -> usize {
        self.bus
            .events()
            .iter()
            .filter(|event| matches!(event, AppEvent::RoomUpdated { .. }))
            .count()
    }

    async fn until_watching(&self, wanted: impl Fn(&WatchState) -> bool) -> WatchState {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(state) = self.session.watch_state().filter(|state| wanted(state)) {
                return state;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "never got there: {:?}",
                self.session.watch_state()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("a free port")
        .local_addr()
        .expect("address")
        .port()
}

/// A file to open, in a folder of its own.
fn film(label: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("syncparty-film-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("Film.mkv");
    std::fs::write(&path, b"not really a film").expect("film");
    (dir, path)
}

#[tokio::test]
async fn a_new_session_is_idle() {
    let (session, _) = session_with("idle", Arc::new(FakeServer::default()));

    assert!(matches!(session.state().await, SessionState::Idle));
    assert_eq!(session.watch_state(), None);
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

#[tokio::test]
async fn joining_opens_the_player_and_remembers_the_party() {
    let party = Party::new("join").await;
    let invite = party.invite("MovieNight", PASSWORD);

    party.session.join(&invite).await.expect("joined");

    let state = party.session.watch_state().expect("watching");
    assert_eq!(state.role, WatchRole::Guest);
    assert_eq!(
        state.connection,
        crate::core::watch::WatchConnection::Connected
    );
    assert_eq!(state.username.as_deref(), Some("alice"));
    assert_eq!(party.launcher.launched().len(), 1);
    assert_eq!(party.remembered(), Some(invite));
    party.session.leave().await.expect("leave");
}

#[tokio::test]
async fn joining_the_party_already_joined_does_nothing() {
    let party = Party::new("join-twice").await;
    let invite = party.invite("MovieNight", PASSWORD);

    party.session.join(&invite).await.expect("joined");
    party.session.join(&invite).await.expect("joined again");
    let resumed = party.session.resume_last_session().await.expect("resumed");

    assert_eq!(resumed, Some(invite));
    assert_eq!(party.launcher.launched().len(), 1, "one player, not three");
    assert_eq!(party.dialer.dials(), 1);
    assert!(!party.player().is_closed());
    party.session.leave().await.expect("leave");
}

#[tokio::test]
async fn joining_a_different_party_leaves_the_first() {
    let party = Party::new("join-other").await;
    let first = party.invite("MovieNight", PASSWORD);
    let second = party.invite("Matinee", PASSWORD);

    party.session.join(&first).await.expect("first");
    let first_player = party.player();
    party.session.join(&second).await.expect("second");

    assert!(
        first_player.is_closed(),
        "the first party's player is closed"
    );
    assert_eq!(party.dialer.closes(), 1);
    assert_eq!(party.launcher.launched().len(), 2);
    assert_eq!(
        party.session.watch_state().map(|state| state.room),
        Some("Matinee".to_owned())
    );
    assert_eq!(party.remembered(), Some(second));
    party.session.leave().await.expect("leave");
}

#[tokio::test]
async fn a_refused_password_is_sync_rejected_and_not_remembered() {
    let party = Party::new("refused").await;

    let error = party
        .session
        .join(&party.invite("MovieNight", "an old password"))
        .await
        .expect_err("refused");

    assert_eq!(error.kind(), "sync_rejected");
    assert_eq!(party.remembered(), None);
    assert_eq!(party.session.watch_state(), None);
    assert!(party.launcher.launched().is_empty());
    assert_eq!(party.dialer.closes(), 1, "the route is closed again");
}

#[tokio::test]
async fn a_saved_party_that_refuses_the_password_is_forgotten() {
    let party = Party::new("refused-resume").await;
    let stale = party.invite("MovieNight", "an old password");
    party
        .secrets
        .set(
            SecretKey::LastInvite,
            &encode_last_invite(&stale).expect("encode"),
        )
        .expect("seed");

    let error = party
        .session
        .resume_last_session()
        .await
        .expect_err("refused");

    assert_eq!(error.kind(), "sync_rejected");
    assert_eq!(party.remembered(), None);
}

#[tokio::test]
async fn a_missing_player_fails_before_the_host_is_dialled() {
    let missing: PlayerSource =
        Arc::new(|_: &ConfigStore| Err(SyncPartyError::DependencyMissing("mpv".to_owned())));
    let party = Party::with_players("no-mpv", Some(missing)).await;

    let error = party
        .session
        .join(&party.invite("MovieNight", PASSWORD))
        .await
        .expect_err("no player");

    assert_eq!(error.kind(), "dependency_missing");
    assert_eq!(party.dialer.dials(), 0);
    assert_eq!(party.remembered(), None);
}

#[tokio::test]
async fn leaving_closes_the_player_and_the_route() {
    let party = Party::new("leave").await;
    party
        .session
        .join(&party.invite("MovieNight", PASSWORD))
        .await
        .expect("joined");

    party.session.leave().await.expect("left");

    assert!(party.player().is_closed());
    assert_eq!(party.dialer.closes(), 1);
    assert_eq!(party.session.watch_state(), None);
    assert!(matches!(
        party.bus.events().last(),
        Some(AppEvent::WatchChanged { state: None })
    ));
}

#[tokio::test]
async fn a_guest_gets_the_room_from_its_own_client() {
    let party = Party::new("guest-room").await;
    party
        .session
        .join(&party.invite("MovieNight", PASSWORD))
        .await
        .expect("joined");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while party.rooms_published() == 0 {
        assert!(tokio::time::Instant::now() < deadline, "no room published");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    party.session.leave().await.expect("leave");
}

#[tokio::test]
async fn watching_the_hosted_party_twice_opens_one_player() {
    let party = Party::new("host-twice").await;
    party.hosting(true).await;

    party.session.join_as_host().await.expect("watching");
    party.session.join_as_host().await.expect("still watching");

    assert_eq!(party.launcher.launched().len(), 1);
    let state = party.session.watch_state().expect("watching");
    assert_eq!(state.role, WatchRole::Host);
    assert_eq!(
        party.remembered(),
        None,
        "the host's own party is not what a restart resumes"
    );
    party.session.stop_hosting().await.expect("stop");
}

#[tokio::test]
async fn watching_needs_a_party_to_be_running() {
    let party = Party::new("host-idle").await;

    let error = party.session.join_as_host().await.expect_err("no party");

    assert_eq!(error.kind(), "server_not_running");
    assert!(party.launcher.launched().is_empty());
}

#[tokio::test]
async fn stopping_the_party_closes_the_hosts_player_first() {
    let party = Party::new("host-stop").await;
    party.hosting(true).await;
    party.session.join_as_host().await.expect("watching");

    party.session.stop_hosting().await.expect("stopped");

    assert!(party.player().is_closed());
    assert_eq!(party.session.watch_state(), None);
    assert!(matches!(party.session.state().await, SessionState::Idle));
    assert_eq!(party.server.state().await, ServerState::Stopped);
}

#[tokio::test]
async fn the_host_can_stop_watching_and_keep_the_party_running() {
    let party = Party::new("host-stop-watching").await;
    party.hosting(true).await;
    party.session.join_as_host().await.expect("watching");

    party
        .session
        .stop_watching()
        .await
        .expect("stopped watching");

    assert!(party.player().is_closed());
    assert_eq!(party.session.watch_state(), None);
    assert!(matches!(
        party.session.state().await,
        SessionState::Hosting(_)
    ));

    party.session.join_as_host().await.expect("watching again");
    assert_eq!(party.launcher.launched().len(), 2);
    party.session.stop_hosting().await.expect("stop");
}

#[tokio::test]
async fn stopping_watching_leaves_a_guest_in_the_party() {
    let party = Party::new("guest-stop-watching").await;
    party
        .session
        .join(&party.invite("MovieNight", PASSWORD))
        .await
        .expect("joined");

    party.session.stop_watching().await.expect("a no-op");

    assert!(!party.player().is_closed());
    assert!(party.session.watch_state().is_some());
    party.session.leave().await.expect("leave");
}

#[tokio::test]
async fn a_host_with_the_monitor_off_gets_the_room_from_its_own_client() {
    let party = Party::new("host-room").await;
    party.hosting(false).await;

    party.session.join_as_host().await.expect("watching");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while party.rooms_published() == 0 {
        assert!(tokio::time::Instant::now() < deadline, "no room published");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    party.session.stop_hosting().await.expect("stop");
}

#[tokio::test]
async fn a_host_with_the_monitor_on_leaves_the_room_to_it() {
    let party = Party::new("host-monitor").await;
    party.hosting(true).await;

    party.session.join_as_host().await.expect("watching");
    tokio::time::sleep(Duration::from_millis(400)).await;
    party.session.stop_watching().await.expect("stop watching");

    assert_eq!(party.rooms_published(), 0);
    party.session.stop_hosting().await.expect("stop");
}

#[tokio::test]
async fn nothing_watched_means_nothing_to_open_ready_or_reopen() {
    let party = Party::new("not-watching").await;
    let (_dir, path) = film("not-watching");

    let opened = party.session.open_watch_file(&path).await;
    let reopened = party.session.reopen_player().await;
    let ready = party.session.set_ready(true);

    for outcome in [opened, reopened, ready] {
        assert_eq!(
            outcome.err().map(|error| error.kind()),
            Some("not_watching")
        );
    }
}

#[tokio::test]
async fn only_a_file_on_disk_is_handed_to_the_player() {
    let party = Party::new("open-file").await;
    party
        .session
        .join(&party.invite("MovieNight", PASSWORD))
        .await
        .expect("joined");
    let (dir, path) = film("open-file");

    let folder = party.session.open_watch_file(&dir).await;
    let missing = party
        .session
        .open_watch_file(&dir.join("nothing-here.mkv"))
        .await;
    party
        .session
        .open_watch_file(&path)
        .await
        .expect("a real file opens");

    assert_eq!(folder.err().map(|e| e.kind()), Some("player_failed"));
    assert_eq!(missing.err().map(|e| e.kind()), Some("player_failed"));
    let loads: Vec<Command> = party
        .player()
        .commands()
        .into_iter()
        .filter(|command| matches!(command, Command::Load(_)))
        .collect();
    assert_eq!(loads, vec![Command::Load(path)]);
    party.session.leave().await.expect("leave");
}

#[tokio::test]
async fn reopening_the_player_opens_the_last_file_again() {
    let party = Party::new("reopen").await;
    party
        .session
        .join(&party.invite("MovieNight", PASSWORD))
        .await
        .expect("joined");
    let (_dir, path) = film("reopen");
    party.session.open_watch_file(&path).await.expect("opened");

    party.player().user_closes();
    party
        .until_watching(|state| state.player == crate::core::watch::PlayerStatus::Closed)
        .await;
    party.session.reopen_player().await.expect("reopened");

    assert_eq!(party.launcher.launched().len(), 2);
    assert!(party.player().commands().contains(&Command::Load(path)));
    party.session.leave().await.expect("leave");
}

#[tokio::test]
async fn shutting_down_closes_the_hosts_player_and_stops_the_party() {
    let party = Party::new("shutdown-host").await;
    party.hosting(true).await;
    party.session.join_as_host().await.expect("watching");

    party.session.shutdown().await;

    assert!(party.player().is_closed());
    assert_eq!(party.session.watch_state(), None);
    assert!(matches!(party.session.state().await, SessionState::Idle));
    assert_eq!(party.server.state().await, ServerState::Stopped);
}

#[tokio::test]
async fn shutting_down_leaves_the_party_a_guest_is_in() {
    let party = Party::new("shutdown-guest").await;
    party
        .session
        .join(&party.invite("MovieNight", PASSWORD))
        .await
        .expect("joined");

    party.session.shutdown().await;

    assert!(party.player().is_closed());
    assert_eq!(party.dialer.closes(), 1);
    assert_eq!(party.session.watch_state(), None);
    assert!(
        party.remembered().is_some(),
        "quitting the app is not leaving: the party is resumed next time"
    );
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
