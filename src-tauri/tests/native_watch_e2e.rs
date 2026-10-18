//! A movie night end to end: syncparty's server, syncparty's sync client and
//! real mpv, all on this machine.
//!
//! Ignored by default because it needs mpv. CI installs it in one Linux job
//! and runs this with `-- --ignored`; locally:
//!
//! ```text
//! cd src-tauri && cargo test --test native_watch_e2e -- --ignored --test-threads=1
//! ```
//!
//! Everyone watches through a [`WatchSession`], the same as in the app, with
//! mpv headless. The test plays the people in the room. What a person does at
//! the keyboard reaches mpv as a command on its IPC socket, so a second
//! connection to that socket is a person: it presses pause, it drags the seek
//! bar. Every assertion is about what the *other* players then did.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tokio_util::sync::CancellationToken;

use syncparty_lib::core::error::Result;
use syncparty_lib::core::events::NullEventBus;
use syncparty_lib::core::player::{
    find_mpv, LaunchMode, MpvLauncher, MpvPlayer, Player, PlayerEvents, PlayerLauncher,
    PlayerSnapshot,
};
use syncparty_lib::core::syncplay::{
    ClientConfig, ClientTiming, MonitorConfig, NativeServer, RoomMonitor, ServerConfig,
    ServerController, ServerTiming,
};
use syncparty_lib::core::watch::{WatchConfig, WatchRole, WatchSession, CONNECT_TIMEOUT};

const PASSWORD: &str = "swordfish";
const ROOM: &str = "E2E";
const FILM_SECONDS: u32 = 30;

/// How long anything here may take to happen. Generous: a loaded CI runner
/// is slow, and a sync that gets there late still got there.
const PATIENCE: Duration = Duration::from_secs(15);

/// How far apart two players may be and still count as together. Upstream
/// does not correct anything under a second either.
const TOGETHER: f64 = 1.0;

fn mpv() -> PathBuf {
    find_mpv(None).expect("mpv is installed; this test is meant to be skipped without it")
}

/// A real film to open: 30 s of test pattern, encoded by mpv itself. The lavfi
/// source played directly would be simpler and is useless — it reports a
/// duration of 0.04 s.
struct Fixture {
    dir: PathBuf,
    film: PathBuf,
}

impl Fixture {
    fn make(label: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("syncparty-e2e-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("fixture dir");
        let film = dir.join("fixture.mkv");

        let status = std::process::Command::new(mpv())
            .args([
                "--no-config".to_owned(),
                "--really-quiet".to_owned(),
                format!("av://lavfi:testsrc=duration={FILM_SECONDS}:size=320x240:rate=25"),
                format!("--o={}", film.display()),
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("mpv runs");
        assert!(
            status.success() && film.is_file(),
            "mpv made no clip: {status}"
        );

        Self { dir, film }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Launches headless mpv and keeps hold of each one, so the test can reach
/// its IPC endpoint and read its state.
struct Headless {
    launcher: MpvLauncher,
    launched: Mutex<Vec<Arc<MpvPlayer>>>,
}

#[async_trait]
impl PlayerLauncher for Headless {
    async fn launch(&self) -> Result<(Arc<dyn Player>, PlayerEvents)> {
        let (player, events) = self.launcher.spawn().await?;
        self.launched
            .lock()
            .expect("launched")
            .push(Arc::clone(&player));
        Ok((player as Arc<dyn Player>, events))
    }
}

/// The party's server and the room panel's monitor.
struct Party {
    server: NativeServer,
    config: ServerConfig,
    monitor: RoomMonitor,
}

impl Party {
    async fn start() -> Self {
        let config = ServerConfig {
            port: free_port(),
            password: PASSWORD.to_owned(),
            salt: "PEPPER".to_owned(),
            timing: ServerTiming::default(),
        };
        let server = NativeServer::new(Arc::new(NullEventBus));
        server.start(&config).await.expect("the server starts");

        let monitor = RoomMonitor::start(MonitorConfig {
            host: "127.0.0.1".to_owned(),
            port: config.port,
            password: PASSWORD.to_owned(),
            room: ROOM.to_owned(),
        });

        Self {
            server,
            config,
            monitor,
        }
    }

    async fn stop(self) {
        drop(self.monitor);
        self.server.stop().await.expect("the server stops");
    }

    /// Waits until the room panel shows everyone in `names` with `file` open.
    async fn until_listed(&self, names: &[&str], file: &str) {
        let listed = until(PATIENCE, || {
            let snapshot = self.monitor.snapshot();
            names.iter().all(|name| {
                snapshot.rooms.iter().any(|room| {
                    room.name == ROOM
                        && room.watchers.iter().any(|watcher| {
                            watcher.name == *name
                                && watcher.file.as_ref().is_some_and(|open| open.name == file)
                        })
                })
            })
        })
        .await;

        assert!(
            listed,
            "the room panel never showed {names:?} with {file}: {:?}",
            self.monitor.snapshot()
        );
    }

    async fn until_ready(&self, names: &[&str]) {
        let ready = until(PATIENCE, || {
            let snapshot = self.monitor.snapshot();
            names.iter().all(|name| {
                snapshot.rooms.iter().any(|room| {
                    room.watchers
                        .iter()
                        .any(|watcher| watcher.name == *name && watcher.is_ready)
                })
            })
        })
        .await;

        assert!(ready, "never all ready: {:?}", self.monitor.snapshot());
    }
}

/// One person in the room: their watch, and the mpv it opened.
struct Viewer {
    name: &'static str,
    watch: WatchSession,
    players: Arc<Headless>,
}

impl Viewer {
    async fn join(name: &'static str, party: &Party) -> Self {
        let players = Arc::new(Headless {
            launcher: MpvLauncher::new(mpv(), LaunchMode::Headless),
            launched: Mutex::new(Vec::new()),
        });

        let watch = WatchSession::start(
            WatchConfig {
                role: WatchRole::Guest,
                client: ClientConfig {
                    address: party.config.local_address(),
                    username: name.to_owned(),
                    room: ROOM.to_owned(),
                    password: PASSWORD.to_owned(),
                    language: "en".to_owned(),
                    timing: ClientTiming::default(),
                },
                connect_timeout: CONNECT_TIMEOUT,
                cancel: CancellationToken::new(),
            },
            Arc::clone(&players) as Arc<dyn PlayerLauncher>,
            Arc::new(NullEventBus),
            true,
        )
        .await
        .unwrap_or_else(|error| panic!("{name} could not join: {error}"));

        Self {
            name,
            watch,
            players,
        }
    }

    fn player(&self) -> Arc<MpvPlayer> {
        self.players
            .launched
            .lock()
            .expect("launched")
            .last()
            .cloned()
            .unwrap_or_else(|| panic!("{} has no player", self.name))
    }

    fn snapshot(&self) -> PlayerSnapshot {
        self.player().snapshot()
    }

    fn position(&self) -> f64 {
        self.snapshot()
            .position(Instant::now())
            .unwrap_or_else(|| panic!("{} has nothing open", self.name))
    }

    fn playing(&self) -> bool {
        let snapshot = self.snapshot();
        snapshot.is_loaded() && !snapshot.paused
    }

    async fn open(&self, film: &Path) {
        self.watch
            .open_file(film)
            .await
            .unwrap_or_else(|error| panic!("{} could not open the film: {error}", self.name));
    }

    /// Sits down at this viewer's mpv.
    async fn keyboard(&self) -> Keyboard {
        Keyboard::connect(self.player().endpoint()).await
    }
}

/// A person at one mpv: a second connection to its IPC socket. Whatever mpv
/// sends back is read and thrown away, so its buffer never fills.
struct Keyboard {
    writer: OwnedWriteHalf,
}

impl Keyboard {
    async fn connect(endpoint: &str) -> Self {
        let stream = UnixStream::connect(endpoint)
            .await
            .expect("a second connection to mpv");
        let (mut reader, writer) = stream.into_split();
        tokio::spawn(async move {
            let mut sink = [0_u8; 4096];
            while matches!(reader.read(&mut sink).await, Ok(read) if read > 0) {}
        });
        Self { writer }
    }

    async fn command(&mut self, command: Value) {
        let mut line = json!({ "command": command }).to_string().into_bytes();
        line.push(b'\n');
        self.writer.write_all(&line).await.expect("write to mpv");
    }

    async fn press_play(&mut self) {
        self.command(json!(["set_property", "pause", false])).await;
    }

    async fn press_pause(&mut self) {
        self.command(json!(["set_property", "pause", true])).await;
    }

    async fn drag_to(&mut self, position: f64) {
        self.command(json!(["seek", position, "absolute"])).await;
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("a free port")
        .local_addr()
        .expect("address")
        .port()
}

async fn until(within: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    condition()
}

/// Two viewers who have both opened the film and said they are ready, so an
/// unpause goes straight to the room rather than becoming "I am ready".
async fn two_ready_viewers(party: &Party, film: &Path) -> (Viewer, Viewer) {
    let alice = Viewer::join("alice", party).await;
    let bob = Viewer::join("bob", party).await;
    alice.open(film).await;
    bob.open(film).await;
    party.until_listed(&["alice", "bob"], "fixture.mkv").await;

    alice.watch.set_ready(true);
    bob.watch.set_ready(true);
    party.until_ready(&["alice", "bob"]).await;

    (alice, bob)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs mpv; run in the native client job"]
async fn what_one_viewer_does_the_other_follows() {
    let fixture = Fixture::make("follow");
    let party = Party::start().await;
    let (alice, bob) = two_ready_viewers(&party, &fixture.film).await;
    assert!(!alice.playing() && !bob.playing(), "nobody has started it");

    let mut at_alices = alice.keyboard().await;

    at_alices.press_play().await;
    assert!(
        until(PATIENCE, || bob.playing()).await,
        "alice pressed play and bob did not follow: {:?}",
        bob.snapshot()
    );
    assert!(alice.playing(), "alice's own play was taken back");

    at_alices.drag_to(15.0).await;
    let caught_up = until(PATIENCE, || {
        let (alices, bobs) = (alice.position(), bob.position());
        alices >= 14.5 && (alices - bobs).abs() <= TOGETHER
    })
    .await;
    assert!(
        caught_up,
        "alice jumped to 15 s and bob did not follow: alice {:.2}, bob {:.2}",
        alice.position(),
        bob.position()
    );
    assert!(bob.playing(), "following a seek is no reason to stop");

    at_alices.press_pause().await;
    let paused_together = until(PATIENCE, || {
        !alice.playing() && !bob.playing() && (alice.position() - bob.position()).abs() <= TOGETHER
    })
    .await;
    assert!(
        paused_together,
        "alice paused and bob did not stop beside her: alice {:?}, bob {:?}",
        alice.snapshot(),
        bob.snapshot()
    );

    let endpoint = bob.player().endpoint().to_owned();
    bob.watch.stop().await;
    assert!(
        UnixStream::connect(&endpoint).await.is_err(),
        "bob left and his mpv is still listening"
    );
    assert!(
        !Path::new(&endpoint).exists(),
        "bob's mpv left its socket behind"
    );

    alice.watch.stop().await;
    party.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs mpv; run in the native client job"]
async fn a_late_joiner_catches_up_without_moving_the_room() {
    let fixture = Fixture::make("late");
    let party = Party::start().await;
    let (alice, bob) = two_ready_viewers(&party, &fixture.film).await;

    alice.keyboard().await.press_play().await;
    assert!(
        until(PATIENCE, || alice.playing() && bob.playing()).await,
        "the room never started: alice {:?}, bob {:?}",
        alice.snapshot(),
        bob.snapshot()
    );
    // Far enough in that a rewind to the start could not pass for jitter.
    assert!(
        until(PATIENCE, || alice.position() >= 3.0).await,
        "the film is not moving: {:?}",
        alice.snapshot()
    );

    // Carol arrives and opens the film while the others watch. Nothing she
    // does on the way in may pause the room or send it back.
    let carol = {
        let joining = async {
            let carol = Viewer::join("carol", &party).await;
            carol.open(&fixture.film).await;
            carol
        };
        tokio::pin!(joining);

        let mut last = (alice.position(), bob.position());
        loop {
            tokio::select! {
                carol = &mut joining => break carol,
                () = tokio::time::sleep(Duration::from_millis(50)) => {
                    let now = (alice.position(), bob.position());
                    assert!(
                        alice.playing() && bob.playing(),
                        "carol joining paused the room: alice {:?}, bob {:?}",
                        alice.snapshot(),
                        bob.snapshot()
                    );
                    assert!(
                        now.0 >= last.0 - 0.5 && now.1 >= last.1 - 0.5,
                        "carol joining sent the room back: {last:?} -> {now:?}"
                    );
                    last = now;
                }
            }
        }
    };

    let with_the_room = until(PATIENCE, || {
        carol.playing() && (carol.position() - alice.position()).abs() <= TOGETHER
    })
    .await;
    assert!(
        with_the_room,
        "carol did not end up playing with the room: carol {:?}, alice at {:.2}",
        carol.snapshot(),
        alice.position()
    );
    assert!(
        alice.playing() && bob.playing(),
        "the room stopped once carol was in: alice {:?}, bob {:?}",
        alice.snapshot(),
        bob.snapshot()
    );
    party
        .until_listed(&["alice", "bob", "carol"], "fixture.mkv")
        .await;

    let endpoint = carol.player().endpoint().to_owned();
    carol.watch.stop().await;
    assert!(
        UnixStream::connect(&endpoint).await.is_err(),
        "carol left and her mpv is still listening"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        alice.playing() && bob.playing(),
        "carol leaving stopped the room"
    );

    alice.watch.stop().await;
    bob.watch.stop().await;
    party.stop().await;
}
