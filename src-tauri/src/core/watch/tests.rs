//! A watch against the real server on loopback, with the fake player.
//!
//! The server is the one the app runs, so connecting, being refused and the
//! room list are all real. The player is the shared fake: the test plays the
//! user at it, and reads back what the watch told it to do.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;

use super::*;
use crate::core::events::test_support::RecordingEventBus;
use crate::core::events::NullEventBus;
use crate::core::player::fake::{Command, FakeLauncher, FakePlayer};
use crate::core::player::PlayerEvents;
use crate::core::syncplay::{
    ClientTiming, NativeServer, ServerConfig, ServerController, ServerTiming,
};

const PASSWORD: &str = "swordfish";
const ROOM: &str = "MovieNight";

/// Long enough that a loaded machine does not fail a test, short enough that
/// a hang is reported rather than waited out.
const PATIENCE: Duration = Duration::from_secs(5);

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("a free port")
        .local_addr()
        .expect("address")
        .port()
}

/// The app's own server, on a port nobody else is using.
async fn server() -> (NativeServer, ServerConfig) {
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
    let server = NativeServer::new(Arc::new(NullEventBus));
    server.start(&config).await.expect("the server starts");
    (server, config)
}

fn config(server: &ServerConfig, password: &str) -> WatchConfig {
    WatchConfig {
        role: WatchRole::Guest,
        client: ClientConfig {
            address: server.local_address(),
            username: "alice".to_owned(),
            room: ROOM.to_owned(),
            password: password.to_owned(),
            language: "en".to_owned(),
            timing: ClientTiming {
                player_poll: Duration::from_millis(20),
                load_settle: Duration::from_millis(100),
                reconnect_base: Duration::from_millis(10),
                ..ClientTiming::default()
            },
        },
        connect_timeout: PATIENCE,
    }
}

/// A folder holding one small file to open, removed afterwards.
struct Fixture {
    dir: PathBuf,
    film: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("syncparty-watch-{}-{id}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("fixture dir");

        let film = dir.join("Film.mkv");
        std::fs::write(&film, b"not really a film").expect("fixture");
        Self {
            film: film.canonicalize().expect("canonical"),
            dir,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Watching {
    watch: WatchSession,
    launcher: Arc<FakeLauncher>,
    bus: Arc<RecordingEventBus>,
    _server: NativeServer,
}

impl Watching {
    async fn start(publish_room: bool) -> Self {
        let (server, server_config) = server().await;
        let launcher = Arc::new(FakeLauncher::new());
        let bus = Arc::new(RecordingEventBus::default());

        let watch = WatchSession::start(
            config(&server_config, PASSWORD),
            Arc::clone(&launcher) as Arc<dyn PlayerLauncher>,
            Arc::clone(&bus) as Arc<dyn EventBus>,
            publish_room,
        )
        .await
        .expect("the watch starts");

        Self {
            watch,
            launcher,
            bus,
            _server: server,
        }
    }

    fn player(&self) -> Arc<FakePlayer> {
        self.launcher.last().expect("a player was launched")
    }

    async fn until(&self, wanted: impl Fn(&WatchState) -> bool) -> WatchState {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            let state = self.watch.state();
            if wanted(&state) {
                return state;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the watch never got there; it is at {state:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Waits until the UI has been told something `wanted` picks out.
    async fn until_published(&self, wanted: impl Fn(&AppEvent) -> bool) {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        while !self.bus.events().iter().any(&wanted) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "never published; saw {:?}",
                self.bus.events()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn watch_changes(&self) -> Vec<Option<WatchState>> {
        self.bus
            .events()
            .into_iter()
            .filter_map(|event| match event {
                AppEvent::WatchChanged { state } => Some(state),
                _ => None,
            })
            .collect()
    }

    fn rooms(&self) -> Vec<RoomSnapshot> {
        self.bus
            .events()
            .into_iter()
            .filter_map(|event| match event {
                AppEvent::RoomUpdated { snapshot } => Some(snapshot),
                _ => None,
            })
            .collect()
    }
}

fn loaded(state: &WatchState) -> bool {
    matches!(state.player, PlayerStatus::Loaded { .. })
}

#[tokio::test]
async fn a_watch_connects_first_and_then_opens_the_player() {
    let watching = Watching::start(true).await;

    let state = watching.watch.state();
    assert_eq!(state.connection, WatchConnection::Connected);
    assert_eq!(state.username.as_deref(), Some("alice"));
    assert_eq!(state.room, ROOM);
    assert_eq!(state.role, WatchRole::Guest);
    assert_eq!(state.player, PlayerStatus::Idle);
    assert_eq!(watching.launcher.launched().len(), 1);

    watching
        .until_published(|event| {
            matches!(event, AppEvent::WatchChanged { state: Some(state) }
                if state.player == PlayerStatus::Idle)
        })
        .await;
    watching.watch.stop().await;
}

#[tokio::test]
async fn a_party_that_refuses_the_password_opens_no_player() {
    let (_server, server_config) = server().await;
    let launcher = Arc::new(FakeLauncher::new());
    let bus = Arc::new(RecordingEventBus::default());

    let error = WatchSession::start(
        config(&server_config, "not the password"),
        Arc::clone(&launcher) as Arc<dyn PlayerLauncher>,
        Arc::clone(&bus) as Arc<dyn EventBus>,
        true,
    )
    .await
    .err()
    .expect("refused");

    assert_eq!(error.kind(), "sync_rejected");
    assert!(launcher.launched().is_empty());
    assert!(bus.events().is_empty(), "{:?}", bus.events());
}

#[tokio::test]
async fn a_party_that_never_answers_is_a_sync_failure() {
    // Accepts the connection and then says nothing at all.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("address");
    let silent = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });

    let launcher = Arc::new(FakeLauncher::new());
    let mut watch_config = config(
        &ServerConfig {
            port: address.port(),
            password: PASSWORD.to_owned(),
            salt: String::new(),
            timing: ServerTiming::default(),
        },
        PASSWORD,
    );
    watch_config.connect_timeout = Duration::from_millis(300);

    let error = WatchSession::start(
        watch_config,
        Arc::clone(&launcher) as Arc<dyn PlayerLauncher>,
        Arc::new(NullEventBus),
        true,
    )
    .await
    .err()
    .expect("no answer");

    assert_eq!(error.kind(), "sync_failed");
    assert!(launcher.launched().is_empty());
    silent.abort();
}

#[tokio::test]
async fn a_player_that_will_not_start_leaves_the_party_joined_and_says_why() {
    let (_server, server_config) = server().await;
    let launcher = Arc::new(FakeLauncher::new());
    launcher.fail_launches(Some("mpv would not start"));

    let watch = WatchSession::start(
        config(&server_config, PASSWORD),
        Arc::clone(&launcher) as Arc<dyn PlayerLauncher>,
        Arc::new(NullEventBus),
        false,
    )
    .await
    .expect("joined without a player");

    let state = watch.state();
    assert_eq!(state.connection, WatchConnection::Connected);
    assert_eq!(
        state.player,
        PlayerStatus::Failed {
            message: "mpv would not start".to_owned()
        }
    );

    launcher.fail_launches(None);
    watch.reopen_player().await.expect("a second try works");
    assert_eq!(watch.state().player, PlayerStatus::Idle);
    watch.stop().await;
}

#[tokio::test]
async fn an_opened_file_shows_as_loaded_and_reaches_the_room() {
    let watching = Watching::start(true).await;
    let fixture = Fixture::new();

    watching
        .watch
        .open_file(&fixture.film)
        .await
        .expect("opened");

    let state = watching.until(loaded).await;
    let PlayerStatus::Loaded { file, paused } = state.player else {
        unreachable!();
    };
    assert_eq!(file.name, "Film.mkv");
    assert_eq!(file.duration_seconds, Some(7200.0));
    assert!(paused, "a room nobody has started is paused");
    assert_eq!(
        watching.player().commands().first(),
        Some(&Command::Load(fixture.film.clone()))
    );

    watching
        .until_published(|event| match event {
            AppEvent::RoomUpdated { snapshot } => snapshot.rooms.iter().any(|room| {
                room.watchers
                    .iter()
                    .any(|watcher| watcher.name == "alice" && watcher.file.is_some())
            }),
            _ => false,
        })
        .await;
    watching.watch.stop().await;
}

#[tokio::test]
async fn only_an_existing_regular_file_is_opened() {
    let watching = Watching::start(false).await;
    let fixture = Fixture::new();

    let missing = watching
        .watch
        .open_file(&fixture.dir.join("missing.mkv"))
        .await
        .expect_err("nothing there");
    let folder = watching
        .watch
        .open_file(&fixture.dir)
        .await
        .expect_err("a folder is not a film");

    assert_eq!(missing.kind(), "player_failed");
    assert_eq!(folder.kind(), "player_failed");
    assert!(
        !watching
            .player()
            .commands()
            .iter()
            .any(|command| matches!(command, Command::Load(_))),
        "nothing reached the player"
    );
    watching.watch.stop().await;
}

#[tokio::test]
async fn closing_the_player_is_reported_and_the_party_goes_on() {
    let watching = Watching::start(false).await;

    watching.player().user_closes();

    let state = watching
        .until(|state| state.player == PlayerStatus::Closed)
        .await;
    assert_eq!(state.connection, WatchConnection::Connected);
    watching.watch.stop().await;
}

#[tokio::test]
async fn reopening_the_player_opens_the_last_file_again() {
    let watching = Watching::start(false).await;
    let fixture = Fixture::new();
    watching
        .watch
        .open_file(&fixture.film)
        .await
        .expect("opened");
    let first = watching.player();

    first.user_closes();
    watching
        .until(|state| state.player == PlayerStatus::Closed)
        .await;
    watching.watch.reopen_player().await.expect("reopened");

    let launched = watching.launcher.launched();
    assert_eq!(launched.len(), 2);
    assert!(
        launched[1]
            .commands()
            .contains(&Command::Load(fixture.film.clone())),
        "{:?}",
        launched[1].commands()
    );
    watching.until(loaded).await;
    watching.watch.stop().await;
}

#[tokio::test]
async fn a_file_dropped_onto_the_window_is_the_one_reopened() {
    let watching = Watching::start(false).await;
    let fixture = Fixture::new();
    let path = fixture.film.to_string_lossy().into_owned();

    watching.player().user_opens(&path, Some(60.0));
    watching.until(loaded).await;
    watching.player().user_closes();
    watching
        .until(|state| state.player == PlayerStatus::Closed)
        .await;
    watching.watch.reopen_player().await.expect("reopened");

    assert!(watching
        .player()
        .commands()
        .contains(&Command::Load(fixture.film.clone())));
    watching.watch.stop().await;
}

#[tokio::test]
async fn reopening_while_the_player_is_open_opens_no_second_one() {
    let watching = Watching::start(false).await;

    watching.watch.reopen_player().await.expect("a no-op");

    assert_eq!(watching.launcher.launched().len(), 1);
    watching.watch.stop().await;
}

#[tokio::test]
async fn choosing_a_file_with_the_player_closed_opens_it_again() {
    let watching = Watching::start(false).await;
    let fixture = Fixture::new();
    watching.player().user_closes();
    watching
        .until(|state| state.player == PlayerStatus::Closed)
        .await;

    watching
        .watch
        .open_file(&fixture.film)
        .await
        .expect("opened");

    assert_eq!(watching.launcher.launched().len(), 2);
    watching.until(loaded).await;
    watching.watch.stop().await;
}

#[tokio::test]
async fn the_ui_hears_each_change_but_never_a_position_tick() {
    let watching = Watching::start(false).await;
    let fixture = Fixture::new();
    watching
        .watch
        .open_file(&fixture.film)
        .await
        .expect("opened");
    watching.until(loaded).await;
    // Whatever opening the file set off has settled by now.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = watching.watch_changes().len();

    let player = watching.player();
    for second in 1..=5 {
        player.update(|snapshot| {
            snapshot.time_pos = Some(f64::from(second));
            snapshot.time_pos_at = Some(std::time::Instant::now());
        });
        player.user_seeks(f64::from(second) * 60.0);
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        watching.watch_changes().len(),
        before,
        "{:?}",
        &watching.watch_changes()[before..]
    );

    player.user_pauses(false);
    watching
        .until_published(|event| {
            matches!(
                event,
                AppEvent::WatchChanged {
                    state: Some(WatchState {
                        player: PlayerStatus::Loaded { paused: false, .. },
                        ..
                    })
                }
            )
        })
        .await;
    watching.watch.stop().await;
}

#[tokio::test]
async fn the_ready_toggle_shows_in_the_state() {
    let watching = Watching::start(false).await;
    assert!(!watching.watch.state().ready);

    watching.watch.set_ready(true);

    watching.until(|state| state.ready).await;
    watching.watch.stop().await;
}

#[tokio::test]
async fn stopping_closes_the_player_and_the_ui_hears_none_last() {
    let watching = Watching::start(true).await;

    watching.watch.stop().await;
    watching.watch.stop().await;

    assert!(watching.player().is_closed());
    assert_eq!(watching.watch_changes().last(), Some(&None));
    assert_eq!(
        watching
            .watch_changes()
            .iter()
            .filter(|state| state.is_none())
            .count(),
        1,
        "stopping twice is stopping once"
    );
    assert_eq!(
        watching.rooms().last(),
        Some(&RoomSnapshot::disconnected()),
        "the room this watch was the source of is gone with it"
    );
}

#[tokio::test]
async fn the_room_is_left_to_the_monitor_unless_this_watch_is_asked_for_it() {
    let watching = Watching::start(false).await;
    let fixture = Fixture::new();
    watching
        .watch
        .open_file(&fixture.film)
        .await
        .expect("opened");
    tokio::time::sleep(Duration::from_millis(300)).await;

    watching.watch.stop().await;

    assert!(watching.rooms().is_empty(), "{:?}", watching.rooms());
}

/// Hands out fakes, but holds every launch after the first until released.
struct HeldLauncher {
    fake: FakeLauncher,
    hold: AtomicBool,
    released: tokio::sync::Notify,
    launches: AtomicUsize,
}

#[async_trait]
impl PlayerLauncher for HeldLauncher {
    async fn launch(&self) -> Result<(Arc<dyn Player>, PlayerEvents)> {
        if self.launches.fetch_add(1, Ordering::SeqCst) > 0 && self.hold.load(Ordering::SeqCst) {
            self.released.notified().await;
        }
        self.fake.launch().await
    }
}

#[tokio::test]
async fn a_player_that_starts_after_the_watch_stopped_is_closed_again() {
    let (_server, server_config) = server().await;
    let launcher = Arc::new(HeldLauncher {
        fake: FakeLauncher::new(),
        hold: AtomicBool::new(true),
        released: tokio::sync::Notify::new(),
        launches: AtomicUsize::new(0),
    });
    let watch = Arc::new(
        WatchSession::start(
            config(&server_config, PASSWORD),
            Arc::clone(&launcher) as Arc<dyn PlayerLauncher>,
            Arc::new(NullEventBus),
            false,
        )
        .await
        .expect("started"),
    );
    launcher.fake.last().expect("first").user_closes();
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while watch.state().player != PlayerStatus::Closed {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let reopening = tokio::spawn({
        let watch = Arc::clone(&watch);
        async move { watch.reopen_player().await }
    });
    while launcher.launches.load(Ordering::SeqCst) < 2 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    watch.stop().await;
    launcher.released.notify_one();

    let outcome = tokio::time::timeout(PATIENCE, reopening)
        .await
        .expect("the reopen finishes")
        .expect("no panic");
    assert_eq!(
        outcome.err().map(|error| error.kind()),
        Some("not_watching")
    );
    let late = launcher.fake.last().expect("the late player");
    assert!(late.is_closed(), "nobody is left to watch it");
}
