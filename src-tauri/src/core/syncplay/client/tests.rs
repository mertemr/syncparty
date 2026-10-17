//! The connection task against a scripted server.
//!
//! The server here is a plain listener and a test reading and writing lines,
//! so every byte the client sends can be checked and every silence timed. The
//! player is the shared fake. Timings are shortened; nothing else is.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};

use super::*;
use crate::core::player::fake::{Command as Played, FakePlayer};
use crate::core::syncplay::protocol::hash_password;

/// Long enough that a loaded machine does not fail a test, short enough that
/// a hang is reported rather than waited out.
const PATIENCE: Duration = Duration::from_secs(5);

fn timing() -> ClientTiming {
    ClientTiming {
        connect_timeout: Duration::from_secs(1),
        protocol_timeout: Duration::from_millis(400),
        player_poll: Duration::from_millis(20),
        reconnect_base: Duration::from_millis(10),
        load_settle: Duration::from_millis(100),
    }
}

fn config(address: SocketAddr) -> ClientConfig {
    ClientConfig {
        address,
        username: "alice".to_owned(),
        room: "MovieNight".to_owned(),
        password: "swordfish".to_owned(),
        language: "en".to_owned(),
        timing: timing(),
    }
}

struct Server {
    listener: TcpListener,
}

impl Server {
    async fn bind() -> Self {
        Self {
            listener: TcpListener::bind("127.0.0.1:0").await.expect("bind"),
        }
    }

    fn address(&self) -> SocketAddr {
        self.listener.local_addr().expect("address")
    }

    async fn accept(&self) -> Peer {
        let (stream, _) = tokio::time::timeout(PATIENCE, self.listener.accept())
            .await
            .expect("the client never connected")
            .expect("accept");
        Peer::new(stream)
    }

    /// Whether a connection arrives within `wait`.
    async fn connects_within(&self, wait: Duration) -> bool {
        tokio::time::timeout(wait, self.listener.accept())
            .await
            .is_ok()
    }
}

/// One accepted connection, seen from the server's side.
struct Peer {
    lines: Lines<BufReader<OwnedReadHalf>>,
    write: OwnedWriteHalf,
}

impl Peer {
    fn new(stream: TcpStream) -> Self {
        let (read, write) = stream.into_split();
        Self {
            lines: BufReader::new(read).lines(),
            write,
        }
    }

    async fn send(&mut self, value: Value) {
        self.write
            .write_all(format!("{value}\r\n").as_bytes())
            .await
            .expect("write");
    }

    async fn send_raw(&mut self, line: &str) {
        self.write
            .write_all(format!("{line}\r\n").as_bytes())
            .await
            .expect("write");
    }

    /// The next line, which must arrive within the patience allowed.
    async fn next(&mut self) -> Value {
        let line = tokio::time::timeout(PATIENCE, self.lines.next_line())
            .await
            .expect("the client went quiet")
            .expect("read")
            .expect("the client hung up");
        serde_json::from_str(&line).expect("the client sent JSON")
    }

    /// The next message under `key`, skipping anything else.
    async fn next_of(&mut self, key: &str) -> Value {
        loop {
            let value = self.next().await;
            if let Some(payload) = value.get(key) {
                return payload.clone();
            }
        }
    }

    /// The next `Set` of `setting`, skipping anything else.
    async fn next_setting(&mut self, setting: &str) -> Value {
        loop {
            if let Some(value) = self.next_of("Set").await.get(setting) {
                return value.clone();
            }
        }
    }

    /// Lines that arrive within `wait`, which ends the collection.
    async fn collect_for(&mut self, wait: Duration) -> Vec<Value> {
        let mut collected = Vec::new();
        let until = tokio::time::Instant::now() + wait;
        while let Ok(line) = tokio::time::timeout_at(until, self.lines.next_line()).await {
            match line.expect("read") {
                Some(line) => collected.push(serde_json::from_str(&line).expect("json")),
                None => break,
            }
        }
        collected
    }

    /// Whether the client closed its end within the patience allowed.
    async fn hung_up(&mut self) -> bool {
        loop {
            match tokio::time::timeout(PATIENCE, self.lines.next_line()).await {
                Ok(Ok(Some(_))) => continue,
                Ok(Ok(None)) | Ok(Err(_)) => return true,
                Err(_) => return false,
            }
        }
    }

    /// Reads the greeting and answers it as `username`. Returns the greeting.
    async fn greet_as(&mut self, username: &str) -> Value {
        let hello = self.next_of("Hello").await;
        self.send(json!({
            "Hello": {
                "username": username,
                "room": { "name": "MovieNight" },
                "version": "1.2.255",
                "realversion": "1.7.6",
                "features": { "readiness": true, "isolateRooms": true },
                "motd": "",
            }
        }))
        .await;
        hello
    }

    /// Greets, then reads up to the list request the greeting is answered
    /// with.
    async fn greet(&mut self) {
        self.greet_as("alice").await;
        self.next_of("List").await;
    }

    async fn heartbeat(&mut self, position: f64, paused: bool, set_by: &str) {
        self.send(json!({
            "State": {
                "ping": { "latencyCalculation": 1234.5, "serverRtt": 0 },
                "playstate": { "position": position, "paused": paused, "doSeek": false, "setBy": set_by },
            }
        }))
        .await;
    }
}

/// Waits for the status to satisfy `wanted`.
async fn until_status(client: &SyncClient, wanted: impl Fn(&ClientStatus) -> bool) -> ClientStatus {
    let mut status = client.subscribe();
    tokio::time::timeout(PATIENCE, async {
        loop {
            {
                let current = status.borrow_and_update();
                if wanted(&current) {
                    return current.clone();
                }
            }
            status.changed().await.expect("the client stopped");
        }
    })
    .await
    .unwrap_or_else(|_| panic!("status never arrived; last was {:?}", client.status()))
}

/// Waits until the fake has been told something matching `wanted`.
async fn until_played(player: &FakePlayer, wanted: impl Fn(&[Played]) -> bool) -> Vec<Played> {
    tokio::time::timeout(PATIENCE, async {
        loop {
            let commands = player.commands();
            if wanted(&commands) {
                return commands;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the player was never told; it got {:?}", player.commands()))
}

fn attach(client: &SyncClient) -> Arc<FakePlayer> {
    let (player, events) = FakePlayer::new();
    client.attach_player(player.clone(), events);
    player
}

#[tokio::test]
async fn the_greeting_hashes_the_password_and_adopts_the_name_the_server_assigned() {
    let server = Server::bind().await;
    let client = SyncClient::start(config(server.address()));
    let mut peer = server.accept().await;

    let hello = peer.greet_as("alice_").await;
    client.wait_connected(PATIENCE).await.expect("greeted");

    assert_eq!(hello["username"], "alice");
    assert_eq!(hello["room"]["name"], "MovieNight");
    assert_eq!(hello["password"], hash_password("swordfish"));
    assert_eq!(hello["features"]["uiMode"], "GUI");
    let status = client.status();
    assert_eq!(status.connection, ConnectionState::Connected);
    assert_eq!(status.username.as_deref(), Some("alice_"));
}

#[tokio::test]
async fn on_connecting_the_client_says_it_is_not_ready_names_its_file_and_asks_for_the_list() {
    let server = Server::bind().await;
    let client = SyncClient::start(config(server.address()));
    let (player, events) = FakePlayer::new();
    player.user_opens("/films/Film.mkv", Some(120.0));
    client.attach_player(player.clone(), events);
    until_status(&client, |status| status.player_attached).await;
    let mut peer = server.accept().await;

    peer.greet_as("alice").await;

    assert_eq!(
        peer.next().await,
        json!({ "Set": { "ready": { "isReady": false, "manuallyInitiated": false } } })
    );
    assert_eq!(
        peer.next().await,
        json!({ "Set": { "file": { "name": "Film.mkv", "duration": 120.0, "size": 0 } } })
    );
    assert_eq!(peer.next().await, json!({ "List": null }));
}

#[tokio::test]
async fn every_server_state_gets_exactly_one_reply() {
    let server = Server::bind().await;
    let client = SyncClient::start(config(server.address()));
    let mut peer = server.accept().await;
    peer.greet().await;

    for _ in 0..3 {
        peer.heartbeat(10.0, true, "bob").await;
    }
    let replies = peer.collect_for(Duration::from_millis(250)).await;

    assert_eq!(replies.len(), 3, "{replies:?}");
    for reply in &replies {
        assert_eq!(reply["State"]["ping"]["latencyCalculation"], 1234.5);
        assert!(
            reply["State"].get("playstate").is_none(),
            "no player, so nothing to say about playback: {reply}"
        );
    }
    assert_eq!(client.status().connection, ConnectionState::Connected);
}

#[tokio::test]
async fn a_refusal_rejects_the_client_for_good() {
    let server = Server::bind().await;
    let client = SyncClient::start(config(server.address()));
    let mut peer = server.accept().await;

    peer.next_of("Hello").await;
    peer.send(json!({ "Error": { "message": "the room password is wrong" } }))
        .await;

    let error = client.wait_connected(PATIENCE).await.expect_err("refused");
    assert_eq!(error.kind(), "sync_rejected");
    assert!(error.to_string().contains("the room password is wrong"));
    assert_eq!(
        client.status().connection,
        ConnectionState::Rejected {
            message: "the room password is wrong".to_owned()
        }
    );
    assert!(peer.hung_up().await, "a rejected client closes its end");
    assert!(
        !server.connects_within(Duration::from_millis(300)).await,
        "and never tries again"
    );
}

#[tokio::test]
async fn silence_for_the_protocol_timeout_reconnects_with_backoff() {
    let server = Server::bind().await;
    let client = SyncClient::start(config(server.address()));
    let (player, events) = FakePlayer::new();
    player.user_opens("/films/Film.mkv", Some(120.0));
    client.attach_player(player.clone(), events);
    let mut first = server.accept().await;
    first.greet().await;
    first.heartbeat(10.0, true, "bob").await;
    first.next_of("State").await;

    let went_quiet = Instant::now();
    let status = until_status(&client, |status| {
        matches!(status.connection, ConnectionState::Reconnecting { .. })
    })
    .await;
    assert_eq!(
        status.connection,
        ConnectionState::Reconnecting { attempt: 1 }
    );
    assert!(
        went_quiet.elapsed() >= timing().protocol_timeout - Duration::from_millis(50),
        "gave up after only {:?}",
        went_quiet.elapsed()
    );
    assert!(first.hung_up().await, "the dead connection is closed");

    let mut second = server.accept().await;
    second.greet_as("alice").await;
    assert_eq!(
        second.next_setting("file").await["name"],
        "Film.mkv",
        "the file is announced again on the new connection"
    );
    until_status(&client, |status| {
        status.connection == ConnectionState::Connected
    })
    .await;
    assert!(
        player
            .texts()
            .contains(&"Lost the connection to the party, reconnecting…".to_owned()),
        "{:?}",
        player.texts()
    );
}

#[tokio::test]
async fn heartbeats_keep_the_connection_alive_past_the_timeout() {
    let server = Server::bind().await;
    let client = SyncClient::start(config(server.address()));
    let mut peer = server.accept().await;
    peer.greet().await;

    for _ in 0..8 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        peer.heartbeat(10.0, true, "bob").await;
        peer.next_of("State").await;
    }

    assert_eq!(client.status().connection, ConnectionState::Connected);
}

#[tokio::test]
async fn retries_back_off_until_a_server_answers() {
    let server = Server::bind().await;
    let client = SyncClient::start(config(server.address()));

    // Accept and hang up four times, timing the gaps.
    let mut accepted = Vec::new();
    for _ in 0..4 {
        let peer = server.accept().await;
        accepted.push(Instant::now());
        drop(peer);
    }
    let gaps: Vec<Duration> = accepted
        .windows(2)
        .map(|pair| pair[1].duration_since(pair[0]))
        .collect();

    // Attempts 1, 2 and 3 wait 20, 40 and 80 ms.
    for (gap, wait) in gaps.iter().zip([20, 40, 80]) {
        assert!(
            *gap >= Duration::from_millis(wait),
            "retried after {gap:?}, sooner than {wait} ms: {gaps:?}"
        );
    }
    assert_eq!(
        client.status().connection,
        ConnectionState::Connecting,
        "never greeted, so still connecting rather than reconnecting"
    );

    let mut peer = server.accept().await;
    peer.greet().await;
    client.wait_connected(PATIENCE).await.expect("greeted");
}

#[tokio::test]
async fn messages_the_client_does_not_know_are_ignored() {
    let server = Server::bind().await;
    let client = SyncClient::start(config(server.address()));
    let mut peer = server.accept().await;
    peer.greet().await;

    peer.send(json!({ "SomethingNew": { "a": 1 } })).await;
    peer.send(json!({ "TLS": { "startTLS": "false" } })).await;
    peer.send(json!({ "Chat": { "username": "bob", "message": "hi" } }))
        .await;
    peer.send_raw("this is not json {").await;
    peer.send_raw("").await;
    peer.heartbeat(10.0, true, "bob").await;

    let reply = peer.next_of("State").await;
    assert_eq!(reply["ping"]["latencyCalculation"], 1234.5);
    assert_eq!(client.status().connection, ConnectionState::Connected);
}

#[tokio::test]
async fn waiting_for_a_server_that_never_answers_times_out() {
    let server = Server::bind().await;
    let client = SyncClient::start(config(server.address()));
    let _peer = server.accept().await;

    let error = client
        .wait_connected(Duration::from_millis(100))
        .await
        .expect_err("no answer");

    assert_eq!(error.kind(), "sync_failed");
}

#[tokio::test]
async fn the_room_comes_from_the_servers_list() {
    let server = Server::bind().await;
    let client = SyncClient::start(config(server.address()));
    let mut peer = server.accept().await;
    peer.greet().await;
    assert!(!client.status().room.connected, "no list yet");

    peer.send(json!({
        "List": { "MovieNight": {
            "alice": { "position": 0, "file": {}, "controller": false, "isReady": true, "features": {} },
            "bob": { "position": 0, "file": { "name": "Film.mkv", "duration": 120.0 }, "controller": false, "isReady": false, "features": {} },
            "syncparty-panel": { "position": 0, "file": {}, "controller": false, "isReady": false, "features": {} },
        }}
    }))
    .await;

    let status = until_status(&client, |status| status.room.connected).await;
    let watchers: Vec<&str> = status.room.rooms[0]
        .watchers
        .iter()
        .map(|watcher| watcher.name.as_str())
        .collect();
    assert_eq!(watchers, vec!["alice", "bob"]);
    assert!(status.ready, "the list says alice is ready");
}

#[tokio::test]
async fn a_player_that_closes_is_detached_and_the_connection_stays() {
    let server = Server::bind().await;
    let client = SyncClient::start(config(server.address()));
    let mut peer = server.accept().await;
    peer.greet().await;
    let player = attach(&client);
    until_status(&client, |status| status.player_attached).await;
    peer.heartbeat(10.0, true, "bob").await;
    let with_player = peer.next_of("State").await;
    assert!(with_player.get("playstate").is_some(), "{with_player}");

    player.user_closes();

    let status = until_status(&client, |status| !status.player_attached).await;
    assert_eq!(status.connection, ConnectionState::Connected);
    peer.heartbeat(10.0, true, "bob").await;
    let without = peer.next_of("State").await;
    assert!(without.get("playstate").is_none(), "{without}");
}

#[tokio::test]
async fn the_player_follows_the_room_and_the_room_hears_the_user() {
    let server = Server::bind().await;
    let client = SyncClient::start(config(server.address()));
    let mut peer = server.accept().await;
    peer.greet().await;
    let player = attach(&client);
    player.user_opens("/films/Film.mkv", Some(7200.0));
    peer.next_setting("file").await;

    peer.heartbeat(30.0, false, "bob").await;
    let reply = peer.next_of("State").await;
    assert_eq!(reply["playstate"]["paused"], false);
    until_played(&player, |commands| {
        commands.contains(&Played::SetPaused(false))
    })
    .await;
    assert!(!player.snapshot().paused);

    // Past the settle window, the user pauses.
    tokio::time::sleep(Duration::from_millis(150)).await;
    player.user_pauses(true);

    assert_eq!(
        peer.next_setting("ready").await,
        json!({ "isReady": false, "manuallyInitiated": false })
    );
    let state = peer.next_of("State").await;
    assert_eq!(state["playstate"]["paused"], true);
    assert_eq!(state["ignoringOnTheFly"]["client"], 1);
}

/// Watching at 1500 s, the user opens `path` over the film. Returns what the
/// client sent the room in the next moments.
///
/// The player switches to the new file before the client hears that it has,
/// and an event queued ahead of the switch makes the client look at the
/// player in between. What it sees there is the new file at its start.
async fn messages_after_opening_over_the_film(path: &str) -> Vec<Value> {
    let server = Server::bind().await;
    let client = SyncClient::start(config(server.address()));
    let mut peer = server.accept().await;
    peer.greet().await;
    let player = attach(&client);
    player.user_opens("/films/Film.mkv", Some(7200.0));
    peer.next_setting("file").await;
    peer.heartbeat(1500.0, false, "bob").await;
    peer.next_of("State").await;
    until_played(&player, |commands| {
        commands.contains(&Played::SetPaused(false))
    })
    .await;
    tokio::time::sleep(timing().load_settle + Duration::from_millis(50)).await;

    let here = player
        .snapshot()
        .position(Instant::now())
        .expect("a position");
    player.user_seeks(here);
    player.user_opens(path, Some(7200.0));

    peer.collect_for(Duration::from_millis(300)).await
}

fn seeks_sent(messages: &[Value]) -> Vec<&Value> {
    messages
        .iter()
        .filter(|message| message["State"]["playstate"]["doSeek"] == true)
        .collect()
}

#[tokio::test]
async fn a_file_opened_over_another_is_not_mistaken_for_a_seek_to_its_start() {
    let sent = messages_after_opening_over_the_film("/films/Other.mkv").await;

    assert!(
        seeks_sent(&sent).is_empty(),
        "the room was sent a seek: {sent:?}"
    );
    assert!(
        sent.iter()
            .any(|message| message["Set"]["file"]["name"] == "Other.mkv"),
        "{sent:?}"
    );
}

/// The same path, so only the order of events can tell the reopened file
/// at its start from the user seeking there.
#[tokio::test]
async fn the_same_file_opened_again_is_not_mistaken_for_a_seek_to_its_start() {
    let sent = messages_after_opening_over_the_film("/films/Film.mkv").await;

    assert!(
        seeks_sent(&sent).is_empty(),
        "the room was sent a seek: {sent:?}"
    );
}

/// The room is an hour and a half in; the joiner's file is twenty minutes
/// long. Sent to the room's position, the player parks on its last frame.
#[tokio::test]
async fn a_joiner_with_a_shorter_file_leaves_the_room_where_it_is() {
    let server = Server::bind().await;
    let client = SyncClient::start(config(server.address()));
    let mut peer = server.accept().await;
    peer.greet().await;
    let player = attach(&client);
    until_status(&client, |status| status.player_attached).await;
    peer.heartbeat(6000.0, false, "bob").await;
    peer.next_of("State").await;

    player.user_opens("/films/Short.mkv", Some(1200.0));
    let started = Instant::now();
    let mut sent = Vec::new();
    for _ in 0..5 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let room = 6000.0 + started.elapsed().as_secs_f64();
        peer.heartbeat(room, false, "bob").await;
        sent.extend(peer.collect_for(Duration::from_millis(20)).await);
    }

    let snapshot = player.snapshot();
    assert!(
        snapshot.eof_reached && snapshot.paused,
        "parked on the last frame: {snapshot:?}"
    );
    let playstates: Vec<&Value> = sent
        .iter()
        .filter_map(|message| message["State"].get("playstate"))
        .collect();
    assert!(!playstates.is_empty(), "{sent:?}");
    for playstate in playstates {
        assert_eq!(playstate["paused"], false, "{sent:?}");
        assert_ne!(playstate["doSeek"], true, "{sent:?}");
        assert!(
            playstate["position"].as_f64().expect("a position") > 6000.0,
            "{sent:?}"
        );
    }
}

/// A failed `loadfile … replace` has already closed the old file, and says
/// so only by failing.
#[tokio::test]
async fn a_file_that_failed_to_open_is_not_open_any_more() {
    let server = Server::bind().await;
    let client = SyncClient::start(config(server.address()));
    let mut peer = server.accept().await;
    peer.greet().await;
    let player = attach(&client);
    player.user_opens("/films/Film.mkv", Some(7200.0));
    peer.next_setting("file").await;
    peer.heartbeat(10.0, true, "bob").await;
    peer.next_of("State").await;

    player.update(|snapshot| {
        snapshot.file = None;
        snapshot.time_pos = None;
        snapshot.duration = None;
    });
    player.push(PlayerEvent::FileFailed {
        error: "loading failed".to_owned(),
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    peer.send(json!({
        "State": { "playstate": { "position": 50.0, "paused": true, "doSeek": true, "setBy": "bob" } }
    }))
    .await;
    peer.next_of("State").await;
    // One more round trip, so whatever the seek asked of the player is done.
    peer.heartbeat(50.0, true, "bob").await;
    peer.next_of("State").await;
    assert!(
        !player.commands().contains(&Played::Seek(50.0)),
        "an idle player was told to seek: {:?}",
        player.commands()
    );

    drop(peer);
    let mut again = server.accept().await;
    again.greet_as("alice").await;
    let mut greeting = Vec::new();
    loop {
        let message = again.next().await;
        let done = message.get("List").is_some();
        greeting.push(message);
        if done {
            break;
        }
    }
    assert!(
        !greeting
            .iter()
            .any(|message| message["Set"].get("file").is_some()),
        "a file that is not open was announced: {greeting:?}"
    );
}

/// A player that has stopped answering holds up every step that needs it.
/// The server meanwhile keeps talking, and must neither give up on this
/// client nor be given up on by it.
#[tokio::test]
async fn a_player_that_stops_answering_does_not_cost_the_connection() {
    let server = Server::bind().await;
    let client = SyncClient::start(ClientConfig {
        timing: ClientTiming {
            protocol_timeout: Duration::from_millis(250),
            ..timing()
        },
        ..config(server.address())
    });
    let mut peer = server.accept().await;
    peer.greet().await;
    let player = attach(&client);
    player.user_opens("/films/Film.mkv", Some(7200.0));
    peer.next_setting("file").await;
    peer.heartbeat(10.0, true, "bob").await;
    peer.next_of("State").await;
    let replies = |lines: &[Value]| {
        lines
            .iter()
            .filter(|line| line.get("State").is_some())
            .count()
    };

    player.hang(true);
    // Every arrival is a notice for the player to show, and each one waits
    // on it in turn while the heartbeats queue up behind.
    for name in [
        "carol", "dave", "erin", "frank", "gus", "hal", "ivy", "jo", "kim", "lea", "max", "ned",
    ] {
        peer.send(json!({
            "Set": { "user": { name: { "room": { "name": "MovieNight" }, "event": { "joined": true } } } }
        }))
        .await;
    }
    let mut heartbeats = 0;
    let mut answered_while_hung = 0;
    for _ in 0..40 {
        peer.heartbeat(10.0, true, "bob").await;
        heartbeats += 1;
        answered_while_hung += replies(&peer.collect_for(Duration::from_millis(50)).await);
    }
    player.hang(false);

    let mut answered = answered_while_hung;
    for _ in 0..5 {
        peer.heartbeat(10.0, true, "bob").await;
        heartbeats += 1;
        answered += replies(&peer.collect_for(Duration::from_millis(50)).await);
    }

    assert!(
        answered_while_hung > 0,
        "the server heard nothing for as long as the player was hung"
    );
    assert_eq!(
        answered, heartbeats,
        "every heartbeat, on the one connection"
    );
    assert_eq!(client.status().connection, ConnectionState::Connected);
    assert!(
        !server.connects_within(Duration::from_millis(50)).await,
        "the client reconnected, so it had dropped the connection"
    );
}

#[tokio::test]
async fn notices_are_shown_on_the_player_in_the_chosen_language() {
    let server = Server::bind().await;
    let client = SyncClient::start(ClientConfig {
        language: "tr".to_owned(),
        ..config(server.address())
    });
    let player = attach(&client);
    until_status(&client, |status| status.player_attached).await;
    let mut peer = server.accept().await;

    peer.greet().await;

    let shown = until_played(&player, |commands| {
        commands
            .iter()
            .any(|command| matches!(command, Played::ShowText { .. }))
    })
    .await;
    assert!(
        shown.contains(&Played::ShowText {
            text: "Partiye bağlanıldı".to_owned(),
            duration: Duration::from_secs(3),
        }),
        "{shown:?}"
    );
}

#[tokio::test]
async fn the_users_readiness_toggle_reaches_the_server() {
    let server = Server::bind().await;
    let client = SyncClient::start(config(server.address()));
    let mut peer = server.accept().await;
    peer.greet().await;

    client.set_ready(true);

    assert_eq!(
        peer.next_setting("ready").await,
        json!({ "isReady": true, "manuallyInitiated": true })
    );
}

#[tokio::test]
async fn stopping_closes_the_connection_and_can_be_repeated() {
    let server = Server::bind().await;
    let client = SyncClient::start(config(server.address()));
    let mut peer = server.accept().await;
    peer.greet().await;

    client.stop().await;
    client.stop().await;

    assert!(peer.hung_up().await);
    assert!(
        client
            .wait_connected(Duration::from_millis(100))
            .await
            .is_ok(),
        "the last thing it knew was that it was connected"
    );
    assert!(!server.connects_within(Duration::from_millis(200)).await);
}

#[tokio::test]
async fn dropping_the_client_closes_the_connection() {
    let server = Server::bind().await;
    let client = SyncClient::start(config(server.address()));
    let mut peer = server.accept().await;
    peer.greet().await;

    drop(client);

    assert!(peer.hung_up().await);
}

#[tokio::test]
async fn waiting_on_a_stopped_client_fails_rather_than_hangs() {
    let server = Server::bind().await;
    let client = SyncClient::start(config(server.address()));
    let _peer = server.accept().await;

    client.stop().await;
    let error = client
        .wait_connected(PATIENCE)
        .await
        .expect_err("stopped before it was greeted");

    assert_eq!(error.kind(), "sync_failed");
}
