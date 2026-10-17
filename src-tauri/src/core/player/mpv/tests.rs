//! The driver against a scripted mpv.
//!
//! The test listens on the endpoint the driver connects to and plays mpv's
//! part line by line, so each exchange is the one mpv 0.41 was seen to
//! produce — and the orderings that are hard to provoke from a real mpv,
//! like the end of a file announced either way round or a frame arriving
//! after a seek was accepted, happen on demand. The process is scripted
//! too: the test decides when "mpv" exits, and with what.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::watch;

use super::connection::{Liveness, PROBE};
use super::endpoint::Endpoint;
use super::tracker::{NOT_A_FILM, OBSERVED};
use super::transport::{self, Process};
use super::{start, MpvLauncher, MpvPlayer, Timing};
use crate::core::error::Result;
use crate::core::player::{
    CloseReason, LaunchMode, LoadedFile, Origin, Player, PlayerEvent, PlayerEvents, PlayerSnapshot,
};

/// How long anything the fake waits for may take. Everything here is a
/// local socket, so this only matters when a test is about to fail.
const PATIENCE: Duration = Duration::from_secs(2);

const DURATION: f64 = 20.0;
const SIZE: u64 = 86_898;

/// The exit the test decides on, shared with the process the driver holds.
struct ExitControl {
    exit: watch::Sender<Option<Option<i32>>>,
    killed: AtomicBool,
}

impl ExitControl {
    fn exit(&self, code: Option<i32>) {
        self.exit.send_if_modified(|state| {
            let first = state.is_none();
            if first {
                *state = Some(code);
            }
            first
        });
    }

    fn was_killed(&self) -> bool {
        self.killed.load(Ordering::SeqCst)
    }
}

struct ScriptedProcess {
    exit: watch::Receiver<Option<Option<i32>>>,
    control: Arc<ExitControl>,
}

impl Process for ScriptedProcess {
    async fn wait(&mut self) -> Option<i32> {
        match self.exit.wait_for(Option::is_some).await {
            Ok(state) => {
                let code = *state;
                code.flatten()
            }
            Err(_) => None,
        }
    }

    fn kill(&mut self) {
        self.control.killed.store(true, Ordering::SeqCst);
        // Killed by a signal: no exit code.
        self.control.exit(None);
    }
}

fn scripted_process() -> (ScriptedProcess, Arc<ExitControl>) {
    let (sender, receiver) = watch::channel(None);
    let control = Arc::new(ExitControl {
        exit: sender,
        killed: AtomicBool::new(false),
    });
    (
        ScriptedProcess {
            exit: receiver,
            control: Arc::clone(&control),
        },
        control,
    )
}

/// mpv's end of the socket.
struct FakeMpv {
    lines: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl FakeMpv {
    fn new(stream: UnixStream) -> Self {
        let (reader, writer) = stream.into_split();
        Self {
            lines: BufReader::new(reader),
            writer,
        }
    }

    /// The next request the driver sent, whatever it is: its id and its
    /// command.
    async fn next_request(&mut self) -> (i64, Value) {
        self.next_request_within(PATIENCE)
            .await
            .expect("a request in time")
    }

    async fn next_request_within(&mut self, within: Duration) -> Option<(i64, Value)> {
        let mut line = String::new();
        tokio::time::timeout(within, self.lines.read_line(&mut line))
            .await
            .ok()?
            .expect("readable");
        let message: Value = serde_json::from_str(&line).expect("requests are JSON");
        let id = message["request_id"]
            .as_i64()
            .expect("an integer request id");
        assert!(id > 0, "0 is mpv's 'no id': {line}");
        Some((id, message["command"].clone()))
    }

    /// The next request that is not a liveness probe. Probes are answered
    /// on the way, as any mpv answers them, so no test depends on when the
    /// driver happens to ask.
    async fn request(&mut self) -> (i64, Value) {
        loop {
            let (id, command) = self.next_request().await;
            if command != json!(PROBE) {
                return (id, command);
            }
            self.succeed(id, json!(4242)).await;
        }
    }

    /// Answers every probe for `period`, and says how many there were.
    async fn answer_probes_for(&mut self, period: Duration) -> usize {
        let until = Instant::now() + period;
        let mut answered = 0;
        while let Some((id, command)) = self
            .next_request_within(until.saturating_duration_since(Instant::now()))
            .await
        {
            assert_eq!(command, json!(PROBE), "only probes were expected");
            self.succeed(id, json!(4242)).await;
            answered += 1;
        }
        answered
    }

    async fn raw(&mut self, line: &[u8]) {
        self.writer.write_all(line).await.expect("write");
        self.writer.write_all(b"\n").await.expect("write");
    }

    async fn send(&mut self, message: Value) {
        self.raw(message.to_string().as_bytes()).await;
    }

    async fn succeed(&mut self, id: i64, data: Value) {
        self.send(json!({ "request_id": id, "error": "success", "data": data }))
            .await;
    }

    async fn fail(&mut self, id: i64, error: &str) {
        self.send(json!({ "request_id": id, "error": error })).await;
    }

    /// Answers the next request, which must be `expected`.
    async fn accept(&mut self, expected: Value) {
        let (id, command) = self.request().await;
        assert_eq!(command, expected);
        self.succeed(id, Value::Null).await;
    }

    async fn property(&mut self, name: &str, data: Value) {
        self.send(json!({ "event": "property-change", "name": name, "data": data }))
            .await;
    }

    async fn event(&mut self, name: &str) {
        self.send(json!({ "event": name })).await;
    }

    /// What mpv says to the driver's opening requests, as mpv 0.41 says it:
    /// the observe replies carry no data, and each is followed by the
    /// property's current value — missing for one that is unavailable.
    async fn answer_startup(&mut self) {
        self.answer_startup_as("mpv v0.41.0").await;
    }

    async fn answer_startup_as(&mut self, version: &str) {
        self.answer_version(version).await;

        for (index, name) in OBSERVED.iter().enumerate() {
            let (id, command) = self.request().await;
            assert_eq!(command, json!(["observe_property", index + 1, name]));
            self.send(json!({ "request_id": id, "error": "success" }))
                .await;

            let seed = match *name {
                "pause" | "idle-active" => {
                    json!({ "event": "property-change", "name": name, "data": true })
                }
                "speed" => json!({ "event": "property-change", "name": name, "data": 1.0 }),
                _ => json!({ "event": "property-change", "name": name }),
            };
            self.send(seed).await;
        }

        self.accept(json!(["request_log_messages", "error"])).await;
    }

    async fn answer_version(&mut self, version: &str) {
        let (id, command) = self.request().await;
        assert_eq!(command, json!(["get_property", "mpv-version"]));
        self.succeed(id, json!(version)).await;
    }
}

/// A file on disk for `load` to accept, removed with its folder.
struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        Self::at(std::env::temp_dir().join(format!(
            "syncparty-mpv-script-{label}-{}",
            std::process::id()
        )))
    }

    /// One short enough to hold a socket: macOS's `$TMPDIR` alone takes
    /// half of the 103 bytes a socket path may have there.
    fn short(label: &str) -> Self {
        Self::at(PathBuf::from("/tmp").join(format!("sp-{label}-{}", std::process::id())))
    }

    fn at(dir: PathBuf) -> Self {
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self { dir }
    }

    fn file(&self, name: &str) -> PathBuf {
        let path = self.dir.join(name);
        std::fs::write(&path, b"not really a film").expect("scratch file");
        path
    }

    fn folder(&self, name: &str) -> PathBuf {
        let path = self.dir.join(name);
        std::fs::create_dir(&path).expect("scratch folder");
        path
    }

    /// A stand-in for the mpv executable: a shell script that ignores the
    /// options it is given.
    fn stand_in(&self, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = self.dir.join("mpv");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("stand-in");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("executable");
        path
    }
}

/// Starts `launcher` with its endpoint under `base`, where the test can see
/// what is left behind.
///
/// A script written a moment ago can be "busy" to run: a child that another
/// test forked meanwhile holds it open for writing until it execs. That
/// clears up at once, so a busy script is simply tried again.
async fn spawn_under(
    launcher: &MpvLauncher,
    base: &Path,
) -> Result<(Arc<MpvPlayer>, PlayerEvents)> {
    for _ in 0..50 {
        let endpoint = Endpoint::create_in(&[base.to_path_buf()]).expect("endpoint");
        match launcher.spawn_at(endpoint).await {
            Err(error) if error.to_string().contains("os error 26") => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            started => return started,
        }
    }
    panic!("the stand-in stayed busy");
}

fn is_empty(dir: &Path) -> bool {
    std::fs::read_dir(dir).expect("readable").next().is_none()
}

/// Waits until `done` holds, or fails the test.
async fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    tokio::time::timeout(PATIENCE, async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what} did not happen in time"));
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A driver connected to a fake mpv that has answered its start-up.
struct Rig {
    player: Arc<MpvPlayer>,
    events: PlayerEvents,
    mpv: FakeMpv,
    process: Arc<ExitControl>,
    socket: PathBuf,
}

/// The driver's end of a socket and the fake's end, before the driver has
/// said anything.
struct Wired {
    client: UnixStream,
    mpv: FakeMpv,
    process: ScriptedProcess,
    control: Arc<ExitControl>,
    endpoint: Endpoint,
    socket: PathBuf,
}

async fn wire() -> Wired {
    // The real endpoint, so its private directory and its removal are what
    // these tests see too.
    let endpoint = Endpoint::create().expect("endpoint");
    let socket = PathBuf::from(endpoint.name());
    let listener = UnixListener::bind(&socket).expect("listen where mpv would");
    let client = transport::open(endpoint.name()).await.expect("connect");
    let (server, _) = listener.accept().await.expect("accept");
    let (process, control) = scripted_process();

    Wired {
        client,
        mpv: FakeMpv::new(server),
        process,
        control,
        endpoint,
        socket,
    }
}

async fn rig() -> Rig {
    rig_with(Timing::default()).await
}

async fn rig_with(timing: Timing) -> Rig {
    let Wired {
        client,
        mut mpv,
        process,
        control,
        endpoint,
        socket,
    } = wire().await;

    let (started, ()) = tokio::join!(
        start(client, process, endpoint, timing),
        mpv.answer_startup()
    );
    let (player, events) = started.expect("the driver starts");

    Rig {
        player,
        events,
        mpv,
        process: control,
        socket,
    }
}

/// Probes every 40 ms, and a hung mpv after 200 ms of them.
fn quick_liveness() -> Timing {
    Timing {
        liveness: Liveness {
            probe_every: Duration::from_millis(40),
            hung_after: Duration::from_millis(200),
        },
        ..Timing::default()
    }
}

impl Rig {
    async fn next_event(&mut self) -> PlayerEvent {
        tokio::time::timeout(PATIENCE, self.events.recv())
            .await
            .expect("an event in time")
            .expect("the event stream is open")
    }

    /// Waits until the driver has dealt with everything the fake sent so
    /// far. The driver reads lines in order, so once the answer to a
    /// command sent now has been read, so has everything before it.
    async fn settle(&mut self) {
        let player = Arc::clone(&self.player);
        let shown =
            tokio::spawn(async move { player.show_text("settle", Duration::from_millis(1)).await });

        let (id, command) = self.mpv.request().await;
        assert_eq!(
            command,
            json!(["show-text", "settle", 1]),
            "an unexpected request"
        );
        self.mpv.succeed(id, Value::Null).await;

        shown.await.expect("task").expect("shown");
    }

    fn assert_quiet(&mut self) {
        match self.events.try_recv() {
            Err(TryRecvError::Empty) => {}
            other => panic!("expected no event, got {other:?}"),
        }
    }

    /// Opens `path` the way mpv 0.41 does it after `loadfile`, up to the
    /// `FileLoaded` that ends it.
    async fn open(&mut self, path: &Path) -> LoadedFile {
        let player = Arc::clone(&self.player);
        let wanted = path.to_path_buf();
        let loading = tokio::spawn(async move { player.load(&wanted).await });

        let (id, command) = self.mpv.request().await;
        assert_eq!(
            command,
            json!(["loadfile", path.to_str().expect("utf8"), "replace"])
        );
        self.mpv
            .succeed(id, json!({ "playlist_entry_id": 1 }))
            .await;
        self.play_file_from_mpv(1, path.to_str().expect("utf8").as_bytes())
            .await;

        loading.await.expect("task").expect("loaded");
        match self.next_event().await {
            PlayerEvent::FileLoaded {
                file,
                origin: Origin::Ours,
            } => file,
            other => panic!("expected our FileLoaded, got {other:?}"),
        }
    }

    /// mpv's side of a file starting, from `start-file` to the first
    /// `playback-restart`. The path is bytes because mpv's are.
    async fn play_file_from_mpv(&mut self, entry: i64, path: &[u8]) {
        let name = path
            .rsplit(|byte| *byte == b'/')
            .next()
            .expect("a file name");
        self.mpv
            .send(json!({ "event": "start-file", "playlist_entry_id": entry }))
            .await;
        self.mpv.raw(&string_property("path", path)).await;
        self.mpv.raw(&string_property("filename", name)).await;
        self.mpv.property("idle-active", json!(false)).await;
        self.mpv.event("file-loaded").await;
        self.mpv.property("duration", json!(DURATION)).await;
        self.mpv.property("file-size", json!(SIZE)).await;
        self.mpv.property("seeking", json!(true)).await;
        self.mpv.property("eof-reached", json!(false)).await;
        self.mpv.property("time-pos", json!(0.0)).await;
        self.mpv.event("playback-restart").await;
        self.mpv.property("seeking", json!(false)).await;
    }

    /// The user pressing space in mpv, with a file open and paused.
    async fn play_from_the_keyboard(&mut self) {
        self.mpv.property("pause", json!(false)).await;
        assert_eq!(
            self.next_event().await,
            PlayerEvent::PauseChanged {
                paused: false,
                origin: Origin::External
            }
        );
    }
}

/// A property-change line with a string value given as raw bytes, which is
/// how mpv sends a path that is not UTF-8.
fn string_property(name: &str, value: &[u8]) -> Vec<u8> {
    let mut line = format!(r#"{{"event":"property-change","name":"{name}","data":""#).into_bytes();
    line.extend_from_slice(value);
    line.extend_from_slice(br#""}"#);
    line
}

/// The reason in the `Closed` that ends the stream, checking that the
/// stream does end there.
async fn closed(events: &mut PlayerEvents) -> CloseReason {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("Closed in time")
            .expect("Closed before the stream ends");
        if let PlayerEvent::Closed(reason) = event {
            assert_eq!(events.recv().await, None, "the stream ends at Closed");
            return reason;
        }
    }
}

#[tokio::test]
async fn replies_find_the_request_that_asked_whatever_order_they_come_in() {
    let mut rig = rig().await;

    let player = Arc::clone(&rig.player);
    let speed = tokio::spawn(async move { player.set_speed(0.95).await });
    let player = Arc::clone(&rig.player);
    let text = tokio::spawn(async move { player.show_text("hi", Duration::from_secs(1)).await });

    let first = rig.mpv.request().await;
    let second = rig.mpv.request().await;
    let (speed_request, text_request) = if first.1[0] == "set_property" {
        (first, second)
    } else {
        (second, first)
    };
    assert_eq!(speed_request.1, json!(["set_property", "speed", 0.95]));

    rig.mpv.succeed(text_request.0, Value::Null).await;
    // A line mpv could not parse comes back with id 0 and belongs to nobody.
    rig.mpv
        .send(json!({ "request_id": 0, "error": "invalid parameter" }))
        .await;
    rig.mpv
        .fail(speed_request.0, "unsupported format for accessing property")
        .await;

    text.await.expect("task").expect("text shown");
    let refused = speed.await.expect("task").expect_err("speed refused");
    assert!(
        refused.to_string().contains("unsupported format"),
        "{refused}"
    );
    assert_eq!(refused.kind(), "player_failed");
    assert_eq!(
        rig.player.snapshot().speed,
        1.0,
        "a refusal changes nothing"
    );
}

#[tokio::test]
async fn the_first_value_of_each_property_fills_the_mirror_without_counting_as_a_change() {
    let mut rig = rig().await;
    rig.settle().await;

    assert_eq!(rig.player.snapshot(), PlayerSnapshot::default());
    rig.assert_quiet();

    rig.mpv.property("speed", json!(1.5)).await;
    assert_eq!(
        rig.next_event().await,
        PlayerEvent::SpeedChanged {
            speed: 1.5,
            origin: Origin::External
        }
    );
    assert_eq!(rig.player.snapshot().speed, 1.5);
}

#[tokio::test]
async fn a_load_is_reported_once_it_plays_with_its_duration_and_size() {
    let scratch = Scratch::new("load");
    let film = scratch.file("Film.mkv");
    let mut rig = rig().await;

    let player = Arc::clone(&rig.player);
    let wanted = film.clone();
    let loading = tokio::spawn(async move { player.load(&wanted).await });

    let (id, command) = rig.mpv.request().await;
    let path = film.to_str().expect("utf8");
    assert_eq!(command, json!(["loadfile", path, "replace"]));
    rig.mpv.succeed(id, json!({ "playlist_entry_id": 1 })).await;
    rig.mpv
        .send(json!({ "event": "start-file", "playlist_entry_id": 1 }))
        .await;
    rig.mpv.property("path", json!(path)).await;
    rig.mpv.property("filename", json!("Film.mkv")).await;
    rig.mpv.event("file-loaded").await;
    rig.mpv.property("duration", json!(DURATION)).await;
    rig.mpv.property("file-size", json!(SIZE)).await;
    rig.mpv.property("time-pos", json!(0.0)).await;
    rig.settle().await;

    assert!(!loading.is_finished(), "not playing yet");
    assert!(rig.player.snapshot().file.is_none());
    rig.assert_quiet();

    rig.mpv.event("playback-restart").await;
    loading.await.expect("task").expect("loaded");

    let expected = LoadedFile {
        path: path.to_owned(),
        name: "Film.mkv".to_owned(),
        duration: Some(DURATION),
        size: Some(SIZE),
    };
    assert_eq!(rig.player.snapshot().file, Some(expected.clone()));
    assert_eq!(
        rig.next_event().await,
        PlayerEvent::FileLoaded {
            file: expected,
            origin: Origin::Ours
        }
    );
}

#[tokio::test]
async fn a_file_opened_in_mpv_itself_is_external() {
    let mut rig = rig().await;

    rig.play_file_from_mpv(7, b"/films/Dropped.mkv").await;

    match rig.next_event().await {
        PlayerEvent::FileLoaded { file, origin } => {
            assert_eq!(origin, Origin::External);
            assert_eq!(file.name, "Dropped.mkv");
        }
        other => panic!("expected FileLoaded, got {other:?}"),
    }
}

#[tokio::test]
async fn only_an_existing_regular_file_is_handed_to_mpv() {
    let scratch = Scratch::new("refused");
    let mut rig = rig().await;

    let missing = rig
        .player
        .load(&scratch.dir.join("missing.mkv"))
        .await
        .expect_err("missing");
    assert!(
        missing.to_string().contains("there is no file"),
        "{missing}"
    );

    let folder = rig.player.load(&scratch.dir).await.expect_err("a folder");
    assert!(folder.to_string().contains("is not a file"), "{folder}");

    // The next thing mpv hears is the settle, so neither got as far as it.
    rig.settle().await;
}

#[tokio::test]
async fn a_file_mpv_cannot_open_fails_the_load_with_mpvs_reason() {
    let scratch = Scratch::new("failed");
    let film = scratch.file("Broken.mkv");
    let mut rig = rig().await;

    let player = Arc::clone(&rig.player);
    let wanted = film.clone();
    let loading = tokio::spawn(async move { player.load(&wanted).await });

    let (id, _) = rig.mpv.request().await;
    rig.mpv.succeed(id, json!({ "playlist_entry_id": 3 })).await;
    rig.mpv
        .send(json!({ "event": "start-file", "playlist_entry_id": 3 }))
        .await;
    rig.mpv
        .send(json!({
            "event": "end-file",
            "reason": "error",
            "playlist_entry_id": 3,
            "file_error": "unrecognized file format"
        }))
        .await;

    let error = loading.await.expect("task").expect_err("failed");
    assert!(
        error.to_string().contains("unrecognized file format"),
        "{error}"
    );
    assert_eq!(
        rig.next_event().await,
        PlayerEvent::FileFailed {
            error: "unrecognized file format".to_owned()
        }
    );
}

#[tokio::test]
async fn a_pause_we_asked_for_is_ours_and_in_the_snapshot_as_soon_as_mpv_accepts_it() {
    let scratch = Scratch::new("ours");
    let mut rig = rig().await;
    rig.open(&scratch.file("Film.mkv")).await;

    let player = Arc::clone(&rig.player);
    let resuming = tokio::spawn(async move { player.set_paused(false).await });
    rig.mpv
        .accept(json!(["set_property", "pause", false]))
        .await;
    resuming.await.expect("task").expect("resumed");

    // mpv has not reported the change yet, and the snapshot shows it anyway.
    assert!(!rig.player.snapshot().paused);

    rig.mpv.property("pause", json!(false)).await;
    assert_eq!(
        rig.next_event().await,
        PlayerEvent::PauseChanged {
            paused: false,
            origin: Origin::Ours
        }
    );

    // Already playing: nothing to send.
    rig.player.set_paused(false).await.expect("no-op");
    rig.settle().await;
}

#[tokio::test]
async fn a_pause_made_at_the_keyboard_is_external() {
    let scratch = Scratch::new("external");
    let mut rig = rig().await;
    rig.open(&scratch.file("Film.mkv")).await;

    rig.play_from_the_keyboard().await;
    rig.mpv.property("pause", json!(true)).await;

    assert_eq!(
        rig.next_event().await,
        PlayerEvent::PauseChanged {
            paused: true,
            origin: Origin::External
        }
    );
    assert!(rig.player.snapshot().paused);
}

#[tokio::test]
async fn a_seek_we_asked_for_is_ours_and_frames_from_before_it_do_not_undo_it() {
    let scratch = Scratch::new("seek");
    let mut rig = rig().await;
    rig.open(&scratch.file("Film.mkv")).await;
    rig.play_from_the_keyboard().await;

    let player = Arc::clone(&rig.player);
    let seeking = tokio::spawn(async move { player.seek(12.0).await });
    rig.mpv
        .accept(json!(["seek", 12.0, "absolute+exact"]))
        .await;
    seeking.await.expect("task").expect("sought");
    assert_eq!(rig.player.snapshot().time_pos, Some(12.0));

    // A frame decoded before the seek took effect.
    rig.mpv.property("time-pos", json!(0.08)).await;
    rig.settle().await;
    assert_eq!(rig.player.snapshot().time_pos, Some(12.0));

    rig.mpv.event("seek").await;
    rig.mpv.property("time-pos", json!(12.0)).await;
    rig.mpv.property("seeking", json!(true)).await;
    rig.mpv.event("playback-restart").await;
    rig.mpv.property("seeking", json!(false)).await;
    assert_eq!(
        rig.next_event().await,
        PlayerEvent::Seeked {
            position: 12.0,
            origin: Origin::Ours
        }
    );

    rig.mpv.property("time-pos", json!(12.04)).await;
    rig.settle().await;
    assert_eq!(rig.player.snapshot().time_pos, Some(12.04));
}

#[tokio::test]
async fn a_seek_made_elsewhere_is_external() {
    let scratch = Scratch::new("seek-elsewhere");
    let mut rig = rig().await;
    rig.open(&scratch.file("Film.mkv")).await;

    rig.mpv.event("seek").await;
    rig.mpv.property("time-pos", json!(7.5)).await;
    rig.mpv.event("playback-restart").await;

    assert_eq!(
        rig.next_event().await,
        PlayerEvent::Seeked {
            position: 7.5,
            origin: Origin::External
        }
    );
    assert_eq!(rig.player.snapshot().time_pos, Some(7.5));
}

#[tokio::test]
async fn seeking_with_nothing_open_is_an_error() {
    let rig = rig().await;

    let error = rig.player.seek(10.0).await.expect_err("nothing open");
    assert_eq!(error.kind(), "player_failed");
}

#[tokio::test]
async fn the_end_of_the_file_is_one_event_when_mpv_says_so_before_pausing() {
    let scratch = Scratch::new("eof-first");
    let mut rig = rig().await;
    rig.open(&scratch.file("Film.mkv")).await;
    rig.play_from_the_keyboard().await;

    rig.mpv.property("time-pos", json!(19.96)).await;
    rig.mpv.property("eof-reached", json!(true)).await;
    tokio::time::sleep(Duration::from_millis(120)).await;
    rig.mpv.property("pause", json!(true)).await;

    assert_eq!(rig.next_event().await, PlayerEvent::EndOfFile);
    tokio::time::sleep(Duration::from_millis(500)).await;
    rig.settle().await;
    rig.assert_quiet();
}

#[tokio::test]
async fn the_end_of_the_file_is_one_event_when_mpv_pauses_before_saying_so() {
    let scratch = Scratch::new("pause-first");
    let mut rig = rig().await;
    rig.open(&scratch.file("Film.mkv")).await;
    rig.play_from_the_keyboard().await;

    // A seek past the end: mpv pauses and then says the file ended.
    rig.mpv.property("pause", json!(true)).await;
    rig.mpv.property("eof-reached", json!(true)).await;

    assert_eq!(rig.next_event().await, PlayerEvent::EndOfFile);
    tokio::time::sleep(Duration::from_millis(400)).await;
    rig.settle().await;
    rig.assert_quiet();
    assert!(rig.player.snapshot().paused);
}

#[tokio::test]
async fn closing_the_file_unloads_it() {
    let scratch = Scratch::new("unload");
    let mut rig = rig().await;
    rig.open(&scratch.file("Film.mkv")).await;

    rig.mpv
        .send(json!({ "event": "end-file", "reason": "stop", "playlist_entry_id": 1 }))
        .await;
    rig.mpv.property("idle-active", json!(true)).await;
    rig.mpv.event("idle").await;

    assert_eq!(rig.next_event().await, PlayerEvent::Unloaded);
    rig.settle().await;
    rig.assert_quiet();
    assert!(rig.player.snapshot().file.is_none());
}

#[tokio::test]
async fn text_goes_on_screen_as_given_without_control_characters() {
    let mut rig = rig().await;

    let player = Arc::clone(&rig.player);
    let shown = tokio::spawn(async move {
        player
            .show_text(
                "Ahmet paused\r\nat 01:02\u{7} ${filename} \u{1b}[31m",
                Duration::from_secs(3),
            )
            .await
    });

    rig.mpv
        .accept(json!([
            "show-text",
            "Ahmet paused\nat 01:02 ${filename} [31m",
            3000
        ]))
        .await;
    shown.await.expect("task").expect("shown");
}

#[tokio::test]
async fn lines_that_are_not_utf8_or_not_json_cost_only_themselves() {
    let mut rig = rig().await;

    rig.mpv.raw(b"this is not json {").await;
    rig.mpv.raw(b"").await;
    rig.play_file_from_mpv(2, b"/films/caf\xe9.mkv").await;

    match rig.next_event().await {
        PlayerEvent::FileLoaded { file, .. } => {
            assert_eq!(file.path, "/films/caf\u{fffd}.mkv");
            assert_eq!(file.name, "caf\u{fffd}.mkv");
            assert_eq!(file.duration, Some(DURATION));
        }
        other => panic!("expected FileLoaded, got {other:?}"),
    }
}

#[tokio::test]
async fn quit_returns_once_mpv_has_gone_and_its_socket_with_it() {
    let Rig {
        player,
        mut events,
        mut mpv,
        process,
        socket,
    } = rig().await;
    assert!(socket.exists());

    let quitting = {
        let player = Arc::clone(&player);
        tokio::spawn(async move { player.quit().await })
    };
    mpv.accept(json!(["quit"])).await;
    drop(mpv);
    process.exit(Some(0));

    quitting.await.expect("task").expect("quit");
    assert_eq!(closed(&mut events).await, CloseReason::QuitRequested);
    assert!(!socket.exists(), "mpv never removes its socket itself");
    assert!(!socket.parent().expect("dir").exists());
    assert!(!process.was_killed());

    player.quit().await.expect("quitting twice is fine");
}

#[tokio::test]
async fn a_user_quitting_mpv_is_told_apart_from_a_crash() {
    let scratch = Scratch::new("user-quit");
    let mut rig = rig().await;
    rig.open(&scratch.file("Film.mkv")).await;

    let Rig {
        player: _player,
        mut events,
        mut mpv,
        process,
        socket,
    } = rig;
    // What pressing `q` or closing the window looks like from here.
    mpv.send(json!({ "event": "end-file", "reason": "quit", "playlist_entry_id": 1 }))
        .await;
    drop(mpv);
    process.exit(Some(0));

    assert_eq!(closed(&mut events).await, CloseReason::UserQuit);
    assert!(!socket.exists());
}

#[tokio::test]
async fn mpv_dying_is_a_crash_with_its_exit_code_if_it_had_one() {
    for code in [Some(4), None] {
        let Rig {
            player,
            mut events,
            mpv,
            process,
            ..
        } = rig().await;

        drop(mpv);
        process.exit(code);

        assert_eq!(closed(&mut events).await, CloseReason::Crashed(code));
        let error = tokio::time::timeout(Duration::from_millis(200), player.set_paused(false))
            .await
            .expect("fails at once")
            .expect_err("mpv is gone");
        assert!(error.to_string().contains("not running"), "{error}");
    }
}

#[tokio::test]
async fn an_mpv_that_closes_its_socket_but_keeps_running_is_killed() {
    let Rig {
        player: _player,
        mut events,
        mpv,
        process,
        ..
    } = rig().await;

    drop(mpv);

    assert_eq!(closed(&mut events).await, CloseReason::Crashed(None));
    assert!(process.was_killed());
}

#[tokio::test]
async fn dropping_the_player_kills_mpv() {
    let Rig {
        player,
        mut events,
        mpv,
        process,
        socket,
    } = rig().await;

    drop(player);
    tokio::time::timeout(PATIENCE, async {
        while !process.was_killed() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("killed in time");
    drop(mpv);

    assert_eq!(closed(&mut events).await, CloseReason::QuitRequested);
    assert!(!socket.exists());
}

#[tokio::test]
async fn a_pause_at_the_keyboard_is_reported_before_our_resume_that_follows_it() {
    let scratch = Scratch::new("held-pause");
    let mut rig = rig().await;
    rig.open(&scratch.file("Film.mkv")).await;
    rig.play_from_the_keyboard().await;

    // Held for a possible end of the file...
    rig.mpv.property("pause", json!(true)).await;
    rig.settle().await;
    rig.assert_quiet();

    // ...when the room has the client resume the player.
    let player = Arc::clone(&rig.player);
    let resuming = tokio::spawn(async move { player.set_paused(false).await });
    rig.mpv
        .accept(json!(["set_property", "pause", false]))
        .await;
    rig.mpv.property("pause", json!(false)).await;
    resuming.await.expect("task").expect("resumed");

    assert_eq!(
        rig.next_event().await,
        PlayerEvent::PauseChanged {
            paused: true,
            origin: Origin::External
        }
    );
    assert_eq!(
        rig.next_event().await,
        PlayerEvent::PauseChanged {
            paused: false,
            origin: Origin::Ours
        }
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    rig.settle().await;
    rig.assert_quiet();
    assert!(!rig.player.snapshot().paused);
}

#[tokio::test]
async fn a_playlist_we_were_asked_to_open_fails_at_once_and_is_stopped() {
    let scratch = Scratch::new("playlist");
    let list = scratch.file("Films.m3u");
    let mut rig = rig().await;

    let player = Arc::clone(&rig.player);
    let wanted = list.clone();
    let started = Instant::now();
    let loading = tokio::spawn(async move { player.load(&wanted).await });

    let (id, _) = rig.mpv.request().await;
    rig.mpv.succeed(id, json!({ "playlist_entry_id": 1 })).await;
    rig.mpv
        .send(json!({ "event": "start-file", "playlist_entry_id": 1 }))
        .await;
    rig.mpv
        .send(json!({
            "event": "end-file",
            "reason": "redirect",
            "playlist_entry_id": 1,
            "playlist_insert_id": 2,
            "playlist_insert_num_entries": 1
        }))
        .await;
    rig.mpv
        .send(json!({ "event": "start-file", "playlist_entry_id": 2 }))
        .await;
    rig.mpv.event("file-loaded").await;
    rig.mpv.event("playback-restart").await;

    // What the playlist listed is stopped before the load gives its answer.
    rig.mpv.accept(json!(["stop"])).await;
    rig.mpv
        .send(json!({ "event": "end-file", "reason": "stop", "playlist_entry_id": 2 }))
        .await;
    rig.mpv.property("idle-active", json!(true)).await;

    let error = loading.await.expect("task").expect_err("refused");
    assert!(started.elapsed() < PATIENCE, "not after the load's timeout");
    assert!(error.to_string().contains(NOT_A_FILM), "{error}");
    assert_eq!(error.kind(), "player_failed");

    assert_eq!(
        rig.next_event().await,
        PlayerEvent::FileFailed {
            error: NOT_A_FILM.to_owned()
        }
    );
    rig.settle().await;
    rig.assert_quiet();
    assert!(rig.player.snapshot().file.is_none());
}

#[tokio::test]
async fn a_failed_load_over_a_playing_file_unloads_the_old_one() {
    let scratch = Scratch::new("bad-over-good");
    let mut rig = rig().await;
    rig.open(&scratch.file("Film.mkv")).await;

    let player = Arc::clone(&rig.player);
    let broken = scratch.file("Broken.mkv");
    let loading = tokio::spawn(async move { player.load(&broken).await });

    let (id, _) = rig.mpv.request().await;
    rig.mpv.succeed(id, json!({ "playlist_entry_id": 2 })).await;
    rig.mpv
        .send(json!({ "event": "end-file", "reason": "stop", "playlist_entry_id": 1 }))
        .await;
    rig.mpv
        .send(json!({ "event": "start-file", "playlist_entry_id": 2 }))
        .await;
    rig.mpv
        .send(json!({
            "event": "end-file",
            "reason": "error",
            "playlist_entry_id": 2,
            "file_error": "unrecognized file format"
        }))
        .await;
    rig.mpv.event("idle").await;
    rig.mpv.property("idle-active", json!(true)).await;

    loading.await.expect("task").expect_err("broken");
    assert_eq!(
        rig.next_event().await,
        PlayerEvent::FileFailed {
            error: "unrecognized file format".to_owned()
        }
    );
    assert_eq!(rig.next_event().await, PlayerEvent::Unloaded);
    rig.settle().await;
    rig.assert_quiet();
}

#[tokio::test]
async fn an_mpv_older_than_syncparty_needs_is_refused_with_its_version() {
    let Wired {
        client,
        mut mpv,
        process,
        control,
        endpoint,
        socket,
    } = wire().await;

    let (started, ()) = tokio::join!(
        start(client, process, endpoint, Timing::default()),
        mpv.answer_version("mpv 0.32.0")
    );

    let error = started.expect_err("too old");
    let message = error.to_string();
    assert!(message.contains("mpv 0.32.0 is too old"), "{message}");
    assert!(message.contains("0.33 or newer"), "{message}");
    assert_eq!(error.kind(), "player_failed");

    eventually("killing it", || control.was_killed()).await;
    drop(mpv);
    eventually("removing its socket", || !socket.exists()).await;
}

#[tokio::test]
async fn an_mpv_whose_version_cannot_be_read_is_driven_all_the_same() {
    let Wired {
        client,
        mut mpv,
        process,
        control,
        endpoint,
        ..
    } = wire().await;

    let (started, ()) = tokio::join!(
        start(client, process, endpoint, Timing::default()),
        mpv.answer_startup_as("mpv git-2019-05-01")
    );

    let (player, _events) = started.expect("started");
    assert!(!player.has_exited());
    assert!(!control.was_killed());
}

#[tokio::test]
async fn an_mpv_that_stops_answering_is_killed_and_reported_as_crashed() {
    let Rig {
        player,
        mut events,
        mut mpv,
        process,
        socket,
    } = rig_with(quick_liveness()).await;

    // A probe arrives, and from here on mpv says nothing at all.
    let (_, command) = mpv.next_request().await;
    assert_eq!(command, json!(PROBE));
    let silent_since = Instant::now();

    eventually("killing the silent mpv", || process.was_killed()).await;
    assert!(
        silent_since.elapsed() >= Duration::from_millis(120),
        "killed after {:?}, before several probes had gone unanswered",
        silent_since.elapsed()
    );
    drop(mpv);

    assert_eq!(closed(&mut events).await, CloseReason::Crashed(None));
    assert!(!socket.exists());
    assert!(player.snapshot().file.is_none());
}

#[tokio::test]
async fn an_mpv_that_answers_its_probes_is_left_alone() {
    let mut rig = rig_with(quick_liveness()).await;

    let answered = rig.mpv.answer_probes_for(Duration::from_millis(600)).await;

    assert!(answered >= 5, "{answered} probes in 600 ms");
    assert!(!rig.process.was_killed());
    rig.settle().await;
}

#[tokio::test]
async fn quit_kills_an_mpv_that_does_not_go_when_asked() {
    let Rig {
        player,
        mut events,
        mut mpv,
        process,
        socket,
    } = rig_with(Timing {
        quit_grace: Duration::from_millis(100),
        ..Timing::default()
    })
    .await;

    let quitting = {
        let player = Arc::clone(&player);
        tokio::spawn(async move { player.quit().await })
    };
    // mpv reads the command and then does nothing at all.
    let (_, command) = mpv.request().await;
    assert_eq!(command, json!(["quit"]));

    eventually("killing it", || process.was_killed()).await;
    // A killed mpv's socket goes with it.
    drop(mpv);

    quitting.await.expect("task").expect("quit");
    assert_eq!(closed(&mut events).await, CloseReason::QuitRequested);
    assert!(!socket.exists());
    assert!(!socket.parent().expect("dir").exists());
}

#[tokio::test]
async fn an_mpv_that_exits_as_soon_as_it_starts_is_reported_at_once_and_leaves_nothing_behind() {
    let scratch = Scratch::short("exits");
    let base = scratch.folder("e");
    let launcher = MpvLauncher::new(scratch.stand_in("exit 3"), LaunchMode::Headless);

    let started = Instant::now();
    let error = spawn_under(&launcher, &base).await.expect_err("it exited");

    assert!(started.elapsed() < PATIENCE, "{:?}", started.elapsed());
    let message = error.to_string();
    assert!(
        message.contains("exited as soon as it started"),
        "{message}"
    );
    assert!(message.contains('3'), "the exit code is given: {message}");
    assert_eq!(error.kind(), "player_failed");
    assert!(is_empty(&base), "its socket folder is removed");
}

#[tokio::test]
async fn an_mpv_that_never_opens_its_socket_is_given_up_on_and_killed() {
    let scratch = Scratch::short("never");
    let base = scratch.folder("e");
    let pid_file = scratch.dir.join("pid");
    let stand_in = scratch.stand_in(&format!(
        "echo $$ > '{}'\nexec sleep 30",
        pid_file.display()
    ));
    let launcher = MpvLauncher::new(stand_in, LaunchMode::Headless).with_timing(Timing {
        connect: Duration::from_millis(300),
        ..Timing::default()
    });

    let started = Instant::now();
    let error = spawn_under(&launcher, &base)
        .await
        .expect_err("it never listened");

    let waited = started.elapsed();
    assert!(
        (Duration::from_millis(300)..PATIENCE).contains(&waited),
        "{waited:?}"
    );
    let message = error.to_string();
    assert!(
        message.contains("did not open its control socket within 0.3 s"),
        "{message}"
    );
    assert!(is_empty(&base), "its socket folder is removed");

    let pid = std::fs::read_to_string(&pid_file).expect("the stand-in ran");
    let still_there = std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(std::process::Stdio::null())
        .status()
        .expect("kill runs")
        .success();
    assert!(!still_there, "the stand-in {} was left running", pid.trim());
}
