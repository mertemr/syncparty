//! The driver against a real mpv, headless.
//!
//! Ignored by default because they need mpv installed; run them with
//! `cargo test player::mpv -- --ignored`. The clip is made by mpv itself at
//! test time — 20 s of test pattern, encoded to a real file. Playing the
//! lavfi source directly would be simpler and is useless here: it reports a
//! duration of 0.04 s and no size.
//!
//! "Someone else" is a second connection to the same endpoint, which is what
//! a key press amounts to: mpv reports it exactly as it reports our own
//! commands, so it is the only way to see `Origin::External` for real.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;

use super::{MpvLauncher, MpvPlayer};
use crate::core::player::{
    find_mpv, CloseReason, LaunchMode, Origin, Player, PlayerEvent, PlayerEvents,
};

const FILM_SECONDS: f64 = 20.0;

fn mpv() -> PathBuf {
    find_mpv(None).expect("mpv is installed")
}

/// A folder with the test clip in it, removed afterwards.
struct Fixture {
    dir: PathBuf,
    film: PathBuf,
}

impl Fixture {
    fn make(label: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("syncparty-mpv-real-{label}-{}", std::process::id()));
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

async fn headless() -> (Arc<MpvPlayer>, PlayerEvents) {
    MpvLauncher::new(mpv(), LaunchMode::Headless)
        .spawn()
        .await
        .expect("mpv starts")
}

/// Waits for the first event `wanted` picks out, and returns it along with
/// every event that came before it.
async fn wait_for<T>(
    events: &mut PlayerEvents,
    within: Duration,
    mut wanted: impl FnMut(&PlayerEvent) -> Option<T>,
) -> (T, Vec<PlayerEvent>) {
    let deadline = tokio::time::Instant::now() + within;
    let mut skipped = Vec::new();

    loop {
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .unwrap_or_else(|_| panic!("no such event in time; saw {skipped:?}"))
            .unwrap_or_else(|| panic!("the stream ended; saw {skipped:?}"));
        if let Some(found) = wanted(&event) {
            return (found, skipped);
        }
        skipped.push(event);
    }
}

const SOON: Duration = Duration::from_secs(3);

/// Somebody else at the same mpv. Whatever mpv sends this connection is
/// read and thrown away, so its buffer never fills.
struct Elsewhere {
    writer: OwnedWriteHalf,
}

impl Elsewhere {
    async fn connect(player: &MpvPlayer) -> Self {
        let stream = UnixStream::connect(player.endpoint())
            .await
            .expect("a second connection");
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
        self.writer.write_all(&line).await.expect("write");
    }
}

fn near(value: f64, expected: f64, tolerance: f64) -> bool {
    (value - expected).abs() <= tolerance
}

async fn loaded(film: &Path) -> (Arc<MpvPlayer>, PlayerEvents) {
    let (player, mut events) = headless().await;
    player.load(film).await.expect("loaded");
    wait_for(&mut events, SOON, |event| match event {
        PlayerEvent::FileLoaded {
            origin: Origin::Ours,
            ..
        } => Some(()),
        _ => None,
    })
    .await;
    (player, events)
}

async fn quit_and_check_closed(player: &MpvPlayer, events: &mut PlayerEvents) {
    let socket = PathBuf::from(player.endpoint());
    player.quit().await.expect("quit");
    let (reason, _) = wait_for(events, SOON, |event| match event {
        PlayerEvent::Closed(reason) => Some(reason.clone()),
        _ => None,
    })
    .await;
    assert_eq!(reason, CloseReason::QuitRequested);
    assert!(
        !socket.exists(),
        "socket left behind at {}",
        socket.display()
    );
    assert!(!socket.parent().expect("dir").exists());
}

#[tokio::test]
#[ignore = "needs mpv"]
async fn real_mpv_loads_a_file_and_reports_it_once_with_its_duration_and_size() {
    let fixture = Fixture::make("load");
    let (player, mut events) = headless().await;

    let idle = player.snapshot();
    assert!(idle.paused, "started with --pause");
    assert!(!idle.is_loaded());
    assert!(Path::new(player.endpoint()).exists());

    player.load(&fixture.film).await.expect("loaded");

    let file = player.snapshot().file.expect("the snapshot names the file");
    assert_eq!(file.name, "fixture.mkv");
    assert_eq!(Path::new(&file.path), fixture.film);
    assert!(
        file.duration
            .is_some_and(|duration| near(duration, FILM_SECONDS, 0.1)),
        "{file:?}"
    );
    let on_disk = std::fs::metadata(&fixture.film).expect("metadata").len();
    assert_eq!(file.size, Some(on_disk));
    assert!(on_disk > 0);

    let (reported, skipped) = wait_for(&mut events, SOON, |event| match event {
        PlayerEvent::FileLoaded { file, origin } => Some((file.clone(), *origin)),
        _ => None,
    })
    .await;
    assert_eq!(reported, (file, Origin::Ours));
    assert!(skipped.is_empty(), "{skipped:?}");
    assert!(player.snapshot().paused, "a file opens paused");

    player
        .show_text("syncparty\nsecond line", Duration::from_secs(1))
        .await
        .expect("text shown");

    quit_and_check_closed(&player, &mut events).await;
}

#[tokio::test]
#[ignore = "needs mpv"]
async fn real_mpv_pauses_seeks_and_changes_speed_on_our_word_and_says_so_as_ours() {
    let fixture = Fixture::make("ours");
    let (player, mut events) = loaded(&fixture.film).await;

    player.set_paused(false).await.expect("resumed");
    assert!(!player.snapshot().paused, "mirrored when accepted");
    let (_, skipped) = wait_for(&mut events, SOON, |event| {
        (*event
            == PlayerEvent::PauseChanged {
                paused: false,
                origin: Origin::Ours,
            })
        .then_some(())
    })
    .await;
    assert!(skipped.is_empty(), "{skipped:?}");

    player.seek(10.0).await.expect("sought");
    assert_eq!(
        player.snapshot().time_pos,
        Some(10.0),
        "mirrored when accepted"
    );
    let (position, skipped) = wait_for(&mut events, SOON, |event| match event {
        PlayerEvent::Seeked { position, origin } => Some((*position, *origin)),
        _ => None,
    })
    .await;
    assert!(skipped.is_empty(), "{skipped:?}");
    assert_eq!(position.1, Origin::Ours);
    assert!(near(position.0, 10.0, 0.05), "{position:?}");

    // Playing: the estimate moves on between reports, at the film's pace.
    // At least the half second slept; a slow machine may add a little.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let estimate = player
        .snapshot()
        .position(Instant::now())
        .expect("a position");
    assert!((10.4..11.2).contains(&estimate), "{estimate}");

    player.set_paused(true).await.expect("paused");
    assert!(player.snapshot().paused);
    wait_for(&mut events, SOON, |event| {
        (*event
            == PlayerEvent::PauseChanged {
                paused: true,
                origin: Origin::Ours,
            })
        .then_some(())
    })
    .await;

    // Seeking while paused stays paused.
    player.seek(3.0).await.expect("sought");
    let (position, _) = wait_for(&mut events, SOON, |event| match event {
        PlayerEvent::Seeked { position, origin } => Some((*position, *origin)),
        _ => None,
    })
    .await;
    assert_eq!(position.1, Origin::Ours);
    assert!(near(position.0, 3.0, 0.05), "{position:?}");
    assert!(player.snapshot().paused);

    player.set_speed(0.95).await.expect("slowed");
    assert_eq!(player.snapshot().speed, 0.95);
    let (speed, _) = wait_for(&mut events, SOON, |event| match event {
        PlayerEvent::SpeedChanged { speed, origin } => Some((*speed, *origin)),
        _ => None,
    })
    .await;
    assert_eq!(speed.1, Origin::Ours);
    assert!(near(speed.0, 0.95, 1e-6));

    quit_and_check_closed(&player, &mut events).await;
}

#[tokio::test]
#[ignore = "needs mpv"]
async fn real_mpv_reports_what_another_client_does_as_external() {
    let fixture = Fixture::make("external");
    let (player, mut events) = loaded(&fixture.film).await;
    let mut elsewhere = Elsewhere::connect(&player).await;

    elsewhere
        .command(json!(["set_property", "pause", false]))
        .await;
    let (_, skipped) = wait_for(&mut events, SOON, |event| {
        (*event
            == PlayerEvent::PauseChanged {
                paused: false,
                origin: Origin::External,
            })
        .then_some(())
    })
    .await;
    assert!(skipped.is_empty(), "{skipped:?}");
    assert!(!player.snapshot().paused);

    elsewhere
        .command(json!(["seek", 5, "absolute+exact"]))
        .await;
    let (position, _) = wait_for(&mut events, SOON, |event| match event {
        PlayerEvent::Seeked { position, origin } => Some((*position, *origin)),
        _ => None,
    })
    .await;
    assert_eq!(position.1, Origin::External);
    assert!(near(position.0, 5.0, 0.05), "{position:?}");

    elsewhere
        .command(json!(["set_property", "pause", true]))
        .await;
    wait_for(&mut events, SOON, |event| {
        (*event
            == PlayerEvent::PauseChanged {
                paused: true,
                origin: Origin::External,
            })
        .then_some(())
    })
    .await;
    assert!(player.snapshot().paused);

    quit_and_check_closed(&player, &mut events).await;
}

#[tokio::test]
#[ignore = "needs mpv"]
async fn real_mpv_reports_the_end_of_the_file_and_not_the_pause_it_makes_there() {
    let fixture = Fixture::make("eof");
    let (player, mut events) = loaded(&fixture.film).await;

    let is_end = |event: &PlayerEvent| (*event == PlayerEvent::EndOfFile).then_some(());
    let no_stray_pause = |skipped: &[PlayerEvent]| {
        assert!(
            !skipped
                .iter()
                .any(|event| matches!(event, PlayerEvent::PauseChanged { paused: true, .. })),
            "{skipped:?}"
        );
    };

    // Played into the end: mpv says the file ended, then pauses.
    player.seek(FILM_SECONDS - 1.5).await.expect("sought");
    player.set_paused(false).await.expect("resumed");
    let (_, skipped) = wait_for(&mut events, Duration::from_secs(5), is_end).await;
    no_stray_pause(&skipped);
    let snapshot = player.snapshot();
    assert!(snapshot.paused && snapshot.eof_reached, "{snapshot:?}");

    // Back from the end, then a seek past it: mpv pauses first this time.
    player.seek(5.0).await.expect("sought");
    wait_for(&mut events, SOON, |event| {
        matches!(event, PlayerEvent::Seeked { .. }).then_some(())
    })
    .await;
    player.set_paused(false).await.expect("resumed");
    player
        .seek(FILM_SECONDS + 5.0)
        .await
        .expect("sought past the end");
    let (_, skipped) = wait_for(&mut events, SOON, is_end).await;
    no_stray_pause(&skipped);

    // Nothing follows: the pause there is the end of the file, not news.
    tokio::time::sleep(Duration::from_millis(600)).await;
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(
                event,
                PlayerEvent::PauseChanged { paused: true, .. } | PlayerEvent::EndOfFile
            ),
            "{event:?}"
        );
    }

    quit_and_check_closed(&player, &mut events).await;
}

#[tokio::test]
#[ignore = "needs mpv"]
async fn real_mpv_closed_by_the_user_is_a_user_quit_and_leaves_no_socket() {
    let fixture = Fixture::make("user-quit");
    let (player, mut events) = loaded(&fixture.film).await;
    let socket = PathBuf::from(player.endpoint());
    let mut elsewhere = Elsewhere::connect(&player).await;

    // The same command `q` and the window's close button run.
    elsewhere.command(json!(["quit"])).await;

    let (reason, _) = wait_for(&mut events, SOON, |event| match event {
        PlayerEvent::Closed(reason) => Some(reason.clone()),
        _ => None,
    })
    .await;
    assert_eq!(reason, CloseReason::UserQuit);
    assert!(!socket.exists());
    assert!(!socket.parent().expect("dir").exists());

    let error = player.set_paused(false).await.expect_err("gone");
    assert_eq!(error.kind(), "player_failed");
    player
        .quit()
        .await
        .expect("quitting a closed player is fine");
}

#[tokio::test]
#[ignore = "needs mpv"]
async fn real_mpv_quits_when_asked_and_leaves_no_socket_behind() {
    let (player, mut events) = headless().await;
    let socket = PathBuf::from(player.endpoint());
    assert!(socket.exists());

    quit_and_check_closed(&player, &mut events).await;
    assert!(!socket.exists());
}
