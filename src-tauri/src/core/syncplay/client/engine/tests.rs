//! The engine's decisions, one branch at a time.
//!
//! Every test drives the engine the way the connection task does — server
//! messages, polls of a player snapshot, player events — with the clock and
//! the epoch passed in by hand, so thresholds can be hit exactly.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::*;
use crate::core::syncplay::protocol::{hash_password, IgnoringOnTheFly};

/// The epoch every step runs at. The engine only ever subtracts stamps, so a
/// fixed value keeps the arithmetic readable.
const EPOCH: f64 = 1_800_000_000.0;
/// The server's own stamp on every state, which a reply must echo.
const STAMP: f64 = 1_799_999_999.5;
const FILM_LENGTH: f64 = 7200.0;
/// How long [`Session::watching`] waits for the file to settle. A quarter
/// second past the window, and exact in binary, so the positions below add up
/// without rounding.
const SETTLED_AFTER: u64 = 1250;

fn config() -> EngineConfig {
    EngineConfig {
        username: "alice".to_owned(),
        room: "MovieNight".to_owned(),
        password: "swordfish".to_owned(),
        load_settle: Duration::from_secs(1),
        tuning: Tuning::default(),
    }
}

fn message(value: Value) -> ServerMessage {
    ServerMessage::from_line(&value.to_string()).expect("message")
}

fn hello(username: &str) -> Value {
    json!({
        "Hello": {
            "username": username,
            "room": { "name": "MovieNight" },
            "version": "1.2.255",
            "realversion": "1.7.6",
            "features": { "readiness": true },
            "motd": "",
        }
    })
}

fn state_value(position: f64, paused: bool, do_seek: Option<bool>, set_by: Option<&str>) -> Value {
    json!({
        "State": {
            "ping": { "latencyCalculation": STAMP, "serverRtt": 0 },
            "playstate": { "position": position, "paused": paused, "doSeek": do_seek, "setBy": set_by },
        }
    })
}

fn with_ignoring(mut value: Value, server: Option<u64>, client: Option<u64>) -> Value {
    let mut envelope = serde_json::Map::new();
    if let Some(server) = server {
        envelope.insert("server".to_owned(), server.into());
    }
    if let Some(client) = client {
        envelope.insert("client".to_owned(), client.into());
    }
    value["State"]["ignoringOnTheFly"] = envelope.into();
    value
}

fn film(length: f64) -> LoadedFile {
    LoadedFile {
        path: "/films/Film.mkv".to_owned(),
        name: "Film.mkv".to_owned(),
        duration: Some(length),
        size: Some(1234),
    }
}

fn idle() -> PlayerSnapshot {
    PlayerSnapshot::default()
}

fn showing(position: f64, paused: bool, now: Instant, length: f64) -> PlayerSnapshot {
    PlayerSnapshot {
        paused,
        time_pos: Some(position),
        time_pos_at: Some(now),
        duration: Some(length),
        file: Some(film(length)),
        ..PlayerSnapshot::default()
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

fn states(step: &Step) -> Vec<&ClientState> {
    step.send
        .iter()
        .filter_map(|message| match message {
            ClientMessage::State(state) => Some(state),
            _ => None,
        })
        .collect()
}

/// The single `State` a step sent.
fn only_state(step: &Step) -> &ClientState {
    let states = states(step);
    assert_eq!(states.len(), 1, "expected exactly one State in {step:?}");
    states[0]
}

fn playstate(step: &Step) -> Option<ClientPlaystate> {
    only_state(step).playstate
}

fn envelope(step: &Step) -> IgnoringOnTheFly {
    only_state(step).ignoring_on_the_fly
}

fn seeks(step: &Step) -> Vec<f64> {
    step.player
        .iter()
        .filter_map(|command| match command {
            PlayerCommand::Seek(position) => Some(*position),
            _ => None,
        })
        .collect()
}

fn speeds(step: &Step) -> Vec<f64> {
    step.player
        .iter()
        .filter_map(|command| match command {
            PlayerCommand::SetSpeed(speed) => Some(*speed),
            _ => None,
        })
        .collect()
}

fn readiness(step: &Step) -> Vec<(bool, bool)> {
    step.send
        .iter()
        .filter_map(|message| match message {
            ClientMessage::SetReady {
                is_ready,
                manually_initiated,
            } => Some((*is_ready, *manually_initiated)),
            _ => None,
        })
        .collect()
}

fn is_quiet(step: &Step) -> bool {
    step.send.is_empty() && step.player.is_empty() && step.notices.is_empty()
}

/// One client's view of a room, with a hand-turned clock.
struct Session {
    engine: Engine,
    now: Instant,
    length: f64,
}

impl Session {
    /// Connected and greeted as alice, in a room with bob, who is ready and
    /// has the same film open. No player yet.
    fn greeted() -> Self {
        Self::greeted_with(config())
    }

    fn greeted_with(config: EngineConfig) -> Self {
        let mut session = Self {
            engine: Engine::new(config),
            now: Instant::now(),
            length: FILM_LENGTH,
        };
        session.engine.connected();
        session.receive(hello("alice"));
        session.list(true);
        session
    }

    /// Watching with the room at `position`: a player attached, the film
    /// open and settled, and the room's state applied. The clock has moved on
    /// past the settle window, and so has the room if it is playing.
    fn watching(position: f64, paused: bool) -> Self {
        Self::watching_film(position, paused, FILM_LENGTH)
    }

    fn watching_film(position: f64, paused: bool, length: f64) -> Self {
        let mut session = Self::greeted();
        session.length = length;
        session.attach();
        session.load();
        session.state(position, paused, Some("bob"));

        session.after(SETTLED_AFTER);
        let now_at = if paused { position } else { position + 1.25 };
        let step = session.poll(now_at, paused);
        assert!(is_quiet(&step), "settling in should be quiet: {step:?}");
        session
    }

    fn after(&mut self, millis: u64) {
        self.now += Duration::from_millis(millis);
    }

    fn receive(&mut self, value: Value) -> Step {
        self.engine.receive(message(value), self.now, EPOCH)
    }

    fn state(&mut self, position: f64, paused: bool, set_by: Option<&str>) -> Step {
        self.receive(state_value(position, paused, None, set_by))
    }

    fn seek_state(&mut self, position: f64, paused: bool, set_by: &str) -> Step {
        self.receive(state_value(position, paused, Some(true), Some(set_by)))
    }

    /// The room list, with bob ready or not.
    fn list(&mut self, bob_ready: bool) -> Step {
        self.receive(json!({
            "List": { "MovieNight": {
                "alice": { "position": 0, "file": { "name": "Film.mkv", "duration": FILM_LENGTH, "size": 1234 }, "controller": false, "isReady": false, "features": {} },
                "bob": { "position": 0, "file": { "name": "Film.mkv", "duration": FILM_LENGTH, "size": 1234 }, "controller": false, "isReady": bob_ready, "features": {} },
                "syncparty-panel": { "position": 0, "file": {}, "controller": false, "isReady": false, "features": {} },
            }}
        }))
    }

    fn set_ready_echo(&mut self, username: &str, ready: bool) -> Step {
        self.receive(json!({
            "Set": { "ready": { "username": username, "isReady": ready, "manuallyInitiated": true } }
        }))
    }

    fn reconnect(&mut self) {
        self.engine.disconnected();
        self.engine.connected();
        self.receive(hello("alice"));
    }

    fn attach(&mut self) -> Step {
        self.engine.attach(&idle(), self.now, EPOCH)
    }

    fn load(&mut self) -> Step {
        self.engine.file_loaded(&film(self.length), self.now, EPOCH)
    }

    fn poll(&mut self, position: f64, paused: bool) -> Step {
        let snapshot = showing(position, paused, self.now, self.length);
        self.engine.poll(&snapshot, self.now, EPOCH)
    }
}

// ------------------------------------------------------------------ greeting

#[test]
fn the_greeting_names_the_room_and_hashes_the_password() {
    let mut engine = Engine::new(config());

    let step = engine.connected();

    let [ClientMessage::Hello(hello)] = step.send.as_slice() else {
        panic!("expected only a greeting: {step:?}");
    };
    assert_eq!(hello.username, "alice");
    assert_eq!(hello.room.name, "MovieNight");
    assert_eq!(
        hello.password.as_deref(),
        Some(hash_password("swordfish").as_str())
    );
    assert_eq!(
        hello.features,
        crate::core::syncplay::protocol::ClientFeatures::participant()
    );
}

#[test]
fn a_room_nobody_named_falls_back_to_the_username() {
    let mut engine = Engine::new(EngineConfig {
        room: " ".to_owned(),
        ..config()
    });

    let step = engine.connected();

    let [ClientMessage::Hello(hello)] = step.send.as_slice() else {
        panic!("expected only a greeting");
    };
    assert_eq!(hello.room.name, "alice");
}

#[test]
fn the_name_the_server_settled_on_is_adopted_and_kept_for_the_next_connection() {
    let mut engine = Engine::new(config());
    let now = Instant::now();
    engine.connected();
    assert_eq!(engine.username(), None, "nobody has said yet");

    engine.receive(message(hello("alice_")), now, EPOCH);
    assert_eq!(engine.username(), Some("alice_"));

    engine.disconnected();
    let step = engine.connected();
    let [ClientMessage::Hello(hello)] = step.send.as_slice() else {
        panic!("expected only a greeting");
    };
    assert_eq!(hello.username, "alice_");
    assert_eq!(engine.username(), Some("alice_"));
}

#[test]
fn the_greeting_is_answered_with_readiness_then_the_file_then_a_list_request() {
    let mut engine = Engine::new(config());
    let now = Instant::now();
    engine.connected();
    engine.attach(&idle(), now, EPOCH);
    let before = engine.file_loaded(&film(120.0), now, EPOCH);
    assert!(
        before.send.is_empty(),
        "nothing goes to a server that has not greeted us"
    );

    let step = engine.receive(message(hello("alice")), now, EPOCH);

    assert_eq!(
        step.send,
        vec![
            ClientMessage::SetReady {
                is_ready: false,
                manually_initiated: false
            },
            ClientMessage::SetFile(FileReport {
                name: "Film.mkv".to_owned(),
                duration: 120.0,
                size: 1234,
            }),
            ClientMessage::ListRequest,
        ]
    );
    assert_eq!(step.notices, vec![Notice::Connected]);
}

#[test]
fn without_a_file_the_greeting_still_asks_who_is_here() {
    let mut engine = Engine::new(config());
    engine.connected();

    let step = engine.receive(message(hello("alice")), Instant::now(), EPOCH);

    assert_eq!(
        step.send,
        vec![
            ClientMessage::SetReady {
                is_ready: false,
                manually_initiated: false
            },
            ClientMessage::ListRequest,
        ]
    );
}

#[test]
fn readiness_chosen_before_the_greeting_goes_out_with_it() {
    let mut engine = Engine::new(config());
    engine.connected();

    assert!(engine.set_ready(true).send.is_empty());
    assert!(engine.ready());
    let step = engine.receive(message(hello("alice")), Instant::now(), EPOCH);

    assert_eq!(readiness(&step), vec![(true, false)]);
}

// ------------------------------------------------------------------ replies

#[test]
fn every_server_state_gets_exactly_one_reply_echoing_its_stamp() {
    let mut session = Session::watching(10.0, true);

    for _ in 0..3 {
        session.after(1000);
        let step = session.state(10.0, true, Some("bob"));

        assert_eq!(step.send.len(), 1, "{step:?}");
        let reply = only_state(&step);
        assert_eq!(reply.latency_calculation, Some(STAMP));
        assert_eq!(reply.client_latency_calculation, EPOCH);
        assert_eq!(reply.client_rtt, 0.0);
        assert_eq!(
            reply.playstate,
            Some(ClientPlaystate {
                position: 10.0,
                paused: true,
                do_seek: false
            })
        );
    }
}

#[test]
fn a_zero_stamp_is_not_echoed() {
    let mut session = Session::watching(10.0, true);

    let step = session.receive(json!({
        "State": { "ping": { "latencyCalculation": 0, "serverRtt": 0 }, "playstate": { "position": 10.0, "paused": true } }
    }));

    assert_eq!(only_state(&step).latency_calculation, None);
}

#[test]
fn a_state_without_a_ping_is_still_answered() {
    let mut session = Session::watching(10.0, true);

    let step =
        session.receive(json!({ "State": { "playstate": { "position": 10.0, "paused": true } } }));

    assert_eq!(only_state(&step).latency_calculation, None);
    assert!(playstate(&step).is_some());
}

#[test]
fn a_state_without_a_pause_flag_is_answered_but_not_applied() {
    let mut session = Session::watching(10.0, true);

    let step = session.receive(json!({ "State": { "playstate": { "position": 50.0 } } }));

    assert!(step.player.is_empty(), "{step:?}");
    assert_eq!(
        playstate(&step).map(|playstate| playstate.position),
        Some(10.0)
    );
}

#[test]
fn nothing_is_said_about_playback_without_a_player() {
    let mut session = Session::greeted();

    let step = session.state(10.0, false, Some("bob"));

    assert_eq!(playstate(&step), None);
    assert!(step.player.is_empty());
}

#[test]
fn nothing_is_said_about_playback_until_a_state_has_been_applied_to_the_player() {
    let mut session = Session::greeted();
    session.attach();
    session.load();

    session.after(SETTLED_AFTER);
    let poll = session.poll(42.0, false);
    assert!(
        states(&poll).is_empty(),
        "an unpause before the room is known reaches nobody: {poll:?}"
    );

    let step = session.state(10.0, true, Some("bob"));

    assert_eq!(seeks(&step), vec![10.0]);
    assert_eq!(
        playstate(&step),
        Some(ClientPlaystate {
            position: 10.0,
            paused: true,
            do_seek: false
        }),
        "the first applied state brings the player to the room, and the reply says so"
    );
}

#[test]
fn the_first_state_brings_an_idle_player_to_the_rooms_pause_state() {
    let mut session = Session::greeted();
    session.attach();

    let step = session.state(100.0, false, Some("bob"));

    assert_eq!(step.player, vec![PlayerCommand::SetPaused(false)]);
    assert!(
        step.notices.is_empty(),
        "joining is not somebody unpausing: {:?}",
        step.notices
    );
    assert_eq!(
        playstate(&step),
        Some(ClientPlaystate {
            position: 100.0,
            paused: false,
            do_seek: false
        })
    );
}

#[test]
fn a_state_that_matches_the_player_changes_nothing() {
    let mut session = Session::watching(10.0, false);

    session.after(500);
    let step = session.state(11.75, false, Some("bob"));

    assert!(step.player.is_empty(), "{step:?}");
    assert!(step.notices.is_empty(), "{step:?}");
}

// ------------------------------------------------------------------ counters

#[test]
fn a_local_change_raises_the_client_counter_and_carries_the_new_state() {
    let mut session = Session::watching(10.0, true);

    let step = session.poll(10.0, false);

    assert_eq!(
        playstate(&step),
        Some(ClientPlaystate {
            position: 10.0,
            paused: false,
            do_seek: false
        })
    );
    assert_eq!(only_state(&step).latency_calculation, None);
    assert_eq!(envelope(&step), (None, Some(1)));
}

#[test]
fn while_a_change_is_unacknowledged_the_room_is_ignored_and_replies_carry_no_playstate() {
    let mut session = Session::watching(10.0, true);
    session.poll(10.0, false);

    session.after(100);
    let step = session.state(10.0, true, Some("bob"));

    assert!(
        step.player.is_empty(),
        "a state from before our change must not undo it: {step:?}"
    );
    assert_eq!(playstate(&step), None);
    assert_eq!(
        envelope(&step),
        (None, Some(1)),
        "still waiting for the echo"
    );
}

#[test]
fn an_echo_of_the_client_counter_clears_it() {
    let mut session = Session::watching(10.0, true);
    session.poll(10.0, false);

    session.after(100);
    let step = session.receive(with_ignoring(
        state_value(10.1, false, None, Some("alice")),
        None,
        Some(1),
    ));

    assert!(playstate(&step).is_some());
    assert_eq!(envelope(&step), (None, None));
}

#[test]
fn an_echo_of_some_other_count_is_not_an_acknowledgement() {
    let mut session = Session::watching(10.0, true);
    session.poll(10.0, false);

    session.after(100);
    let step = session.receive(with_ignoring(
        state_value(10.0, true, None, Some("bob")),
        None,
        Some(7),
    ));

    assert!(step.player.is_empty());
    assert_eq!(playstate(&step), None);
    assert_eq!(envelope(&step), (None, Some(1)));
}

#[test]
fn a_forced_state_is_applied_and_its_reply_acknowledges_it_with_the_state_it_caused() {
    let mut session = Session::watching(10.0, true);

    let step = session.receive(with_ignoring(
        state_value(50.0, true, Some(true), Some("bob")),
        Some(3),
        None,
    ));

    assert_eq!(seeks(&step), vec![50.0]);
    assert_eq!(
        playstate(&step),
        Some(ClientPlaystate {
            position: 50.0,
            paused: true,
            do_seek: false
        }),
        "the server applies the acknowledgement's playstate, so it must be the post-seek one"
    );
    assert_eq!(envelope(&step), (Some(3), None));

    session.after(1000);
    let next = session.state(50.0, true, Some("bob"));
    assert_eq!(
        envelope(&next),
        (None, None),
        "the server counter is echoed once"
    );
}

#[test]
fn the_server_echoing_our_own_change_clears_our_counter_and_takes_its_own_ack() {
    let mut session = Session::watching(10.0, true);
    session.poll(10.0, false);

    session.after(50);
    let step = session.receive(with_ignoring(
        state_value(10.0, false, None, Some("alice")),
        Some(1),
        Some(1),
    ));

    assert!(
        playstate(&step).is_some(),
        "an acknowledgement carries a playstate even right after a change"
    );
    assert_eq!(envelope(&step), (Some(1), None));
}

#[test]
fn a_second_change_inside_one_round_trip_goes_out_without_a_playstate() {
    let mut session = Session::watching(10.0, true);
    session.poll(10.0, false);

    session.after(100);
    let step = session.poll(40.0, false);

    assert_eq!(playstate(&step), None, "upstream's quirk, kept on purpose");
    assert_eq!(envelope(&step), (None, Some(2)));
}

// ------------------------------------------------------------------ ping

/// Teaches the engine a 0.2 s round trip, which with a fresh average makes
/// the forward delay 0.1 + 0.2 = 0.3 s.
fn measure_round_trip(session: &mut Session) {
    let step = session.receive(json!({
        "State": {
            "ping": { "latencyCalculation": STAMP, "serverRtt": 0, "clientLatencyCalculation": EPOCH - 0.2 },
            "playstate": { "position": 10.0, "paused": true },
        }
    }));
    assert!(close(only_state(&step).client_rtt, 0.2));
}

#[test]
fn a_playing_rooms_position_is_advanced_by_how_long_the_message_took() {
    let mut session = Session::watching(10.0, true);
    measure_round_trip(&mut session);

    let step = session.seek_state(50.0, false, "bob");

    let seeks = seeks(&step);
    assert_eq!(seeks.len(), 1, "{step:?}");
    assert!(close(seeks[0], 50.3), "{seeks:?}");
}

#[test]
fn a_paused_rooms_position_is_taken_as_it_is() {
    let mut session = Session::watching(10.0, true);
    measure_round_trip(&mut session);

    let step = session.seek_state(50.0, true, "bob");

    assert_eq!(seeks(&step), vec![50.0]);
}

#[test]
fn the_reply_reports_the_last_round_trip_measured() {
    let mut session = Session::watching(10.0, true);
    measure_round_trip(&mut session);

    session.after(1000);
    let step = session.state(10.0, true, Some("bob"));

    assert!(close(only_state(&step).client_rtt, 0.2));
}

// ------------------------------------------------------------------ decisions

#[test]
fn a_seek_by_somebody_else_moves_the_player_and_says_from_where() {
    let mut session = Session::watching(10.0, true);

    let step = session.seek_state(50.0, true, "bob");

    assert_eq!(seeks(&step), vec![50.0]);
    assert_eq!(
        step.notices,
        vec![Notice::Seeked {
            user: Some("bob".to_owned()),
            from: 10.0,
            to: 50.0
        }]
    );
}

#[test]
fn our_own_seek_coming_back_is_announced_but_not_applied_again() {
    let mut session = Session::watching(10.0, true);
    session.poll(80.0, true);

    session.after(50);
    let step = session.receive(with_ignoring(
        state_value(80.0, true, Some(true), Some("alice")),
        Some(1),
        Some(1),
    ));

    assert!(seeks(&step).is_empty(), "{step:?}");
    assert_eq!(
        step.notices,
        vec![Notice::Seeked {
            user: Some("alice".to_owned()),
            from: 10.0,
            to: 80.0
        }]
    );
}

#[test]
fn more_than_four_seconds_ahead_of_the_room_rewinds_to_it() {
    let mut session = Session::watching(100.0, true);

    let step = session.state(95.9, true, Some("bob"));

    assert_eq!(seeks(&step), vec![95.9]);
    assert_eq!(
        step.notices,
        vec![Notice::Rewound {
            user: Some("bob".to_owned())
        }]
    );
}

#[test]
fn four_seconds_ahead_is_not_yet_a_rewind() {
    let mut session = Session::watching(100.0, true);

    let step = session.state(96.0, true, Some("bob"));

    assert!(seeks(&step).is_empty(), "{step:?}");
    assert!(step.notices.is_empty());
}

#[test]
fn nobody_is_rewound_or_slowed_against_themselves() {
    let mut session = Session::watching(100.0, false);

    let step = session.state(95.0, false, Some("alice"));

    assert!(step.player.is_empty(), "{step:?}");
    assert!(step.notices.is_empty());
}

#[test]
fn a_seek_is_not_also_a_rewind() {
    let mut session = Session::watching(100.0, true);

    let step = session.seek_state(20.0, true, "bob");

    assert_eq!(seeks(&step), vec![20.0], "one seek, not two");
}

#[test]
fn falling_behind_does_not_fast_forward_by_default() {
    let mut session = Session::watching(100.0, false);
    session.state(111.25, false, Some("bob"));

    session.after(4000);
    let step = session.state(115.25, false, Some("bob"));

    assert!(
        seeks(&step).is_empty(),
        "upstream only fast-forwards clients that cannot control the room: {step:?}"
    );
}

/// With `dontSlowDownWithMe`, upstream fast-forwards a client that has been
/// more than five seconds behind for long enough.
fn behind_session() -> Session {
    let mut session = Session::greeted_with(EngineConfig {
        tuning: Tuning {
            dont_slow_down_with_me: true,
            ..Tuning::default()
        },
        ..config()
    });
    session.attach();
    session.load();
    session.state(100.0, true, Some("bob"));
    session.after(SETTLED_AFTER);
    session.poll(100.0, true);
    session
}

#[test]
fn being_behind_long_enough_fast_forwards_past_the_room() {
    let mut session = behind_session();
    let first = session.state(106.0, true, Some("bob"));
    assert!(seeks(&first).is_empty(), "the clock starts: {first:?}");

    session.after(3200);
    let early = session.state(106.0, true, Some("bob"));
    assert!(seeks(&early).is_empty(), "3.2 s is not yet 5 - 1.75 s");

    session.after(100);
    let step = session.state(106.0, true, Some("bob"));

    assert_eq!(seeks(&step), vec![106.25]);
    assert_eq!(
        step.notices,
        vec![Notice::FastForwarded {
            user: Some("bob".to_owned())
        }]
    );
}

#[test]
fn after_fast_forwarding_the_clock_is_held_off() {
    let mut session = behind_session();
    session.state(106.0, true, Some("bob"));
    session.after(3300);
    session.state(106.0, true, Some("bob"));

    session.after(100);
    let step = session.state(112.0, true, Some("bob"));

    assert!(seeks(&step).is_empty(), "{step:?}");
}

#[test]
fn catching_up_restarts_the_fast_forward_clock() {
    let mut session = behind_session();
    session.state(106.0, true, Some("bob"));
    session.after(3000);
    session.state(101.0, true, Some("bob"));

    session.after(300);
    let step = session.state(106.0, true, Some("bob"));

    assert!(seeks(&step).is_empty(), "{step:?}");
}

#[test]
fn being_five_seconds_behind_or_less_never_fast_forwards() {
    let mut session = behind_session();
    session.state(105.0, true, Some("bob"));

    session.after(4000);
    let step = session.state(105.0, true, Some("bob"));

    assert!(seeks(&step).is_empty(), "{step:?}");
}

#[test]
fn drifting_more_than_one_and_a_half_seconds_ahead_slows_down_quietly() {
    let mut session = Session::watching(100.0, false);

    let step = session.state(99.0, false, Some("bob"));

    assert_eq!(step.player, vec![PlayerCommand::SetSpeed(0.95)]);
    assert!(step.notices.is_empty(), "slowing down is never announced");
}

#[test]
fn one_and_a_half_seconds_ahead_is_not_slowed() {
    let mut session = Session::watching(100.0, false);

    let step = session.state(99.75, false, Some("bob"));

    assert!(step.player.is_empty(), "{step:?}");
}

#[test]
fn rewinding_and_slowing_down_can_happen_in_the_same_state() {
    let mut session = Session::watching(100.0, false);

    let step = session.state(97.0, false, Some("bob"));

    assert_eq!(
        step.player,
        vec![PlayerCommand::Seek(97.0), PlayerCommand::SetSpeed(0.95)]
    );
}

#[test]
fn the_slowdown_ends_once_within_a_tenth_of_a_second() {
    let mut session = Session::watching(100.0, false);
    session.state(99.0, false, Some("bob"));

    // The slowed player is modelled at normal speed, as upstream models it.
    session.after(1000);
    let still_ahead = session.state(101.75, false, Some("bob"));
    assert!(speeds(&still_ahead).is_empty(), "{still_ahead:?}");

    session.after(1000);
    let step = session.state(103.2, false, Some("bob"));

    assert_eq!(speeds(&step), vec![1.0]);
}

#[test]
fn a_slowdown_outlives_a_pause() {
    let mut session = Session::watching(100.0, false);
    session.state(99.0, false, Some("bob"));

    let step = session.state(101.25, true, Some("bob"));

    assert!(speeds(&step).is_empty(), "{step:?}");
}

#[test]
fn nobody_slows_down_for_themselves() {
    let mut session = Session::watching(100.0, false);

    let step = session.state(99.0, false, Some("alice"));

    assert!(speeds(&step).is_empty(), "{step:?}");
}

#[test]
fn an_unpause_by_somebody_else_unpauses_the_player_and_says_so() {
    let mut session = Session::watching(10.0, true);

    let step = session.state(10.0, false, Some("bob"));

    assert_eq!(step.player, vec![PlayerCommand::SetPaused(false)]);
    assert_eq!(
        step.notices,
        vec![Notice::Unpaused {
            user: Some("bob".to_owned())
        }]
    );
}

#[test]
fn a_pause_by_somebody_else_lines_up_on_the_frame_the_room_paused_at() {
    let mut session = Session::watching(10.0, false);

    let step = session.state(11.0, true, Some("bob"));

    assert_eq!(
        step.player,
        vec![PlayerCommand::Seek(11.0), PlayerCommand::SetPaused(true)]
    );
    assert_eq!(
        step.notices,
        vec![Notice::Paused {
            user: Some("bob".to_owned()),
            at: 11.0
        }]
    );
}

#[test]
fn our_own_pause_coming_back_is_announced_without_a_seek() {
    let mut session = Session::watching(10.0, false);
    session.poll(11.25, true);

    let step = session.receive(with_ignoring(
        state_value(11.25, true, None, Some("alice")),
        Some(1),
        Some(1),
    ));

    assert_eq!(step.player, vec![PlayerCommand::SetPaused(true)]);
    assert_eq!(
        step.notices,
        vec![Notice::Paused {
            user: Some("alice".to_owned()),
            at: 11.25
        }]
    );
}

#[test]
fn a_seek_before_the_start_is_clamped_and_reported() {
    let mut session = Session::watching(10.0, true);

    let step = session.seek_state(-5.0, true, "bob");

    assert_eq!(seeks(&step), vec![0.0]);
    let sent = states(&step);
    assert_eq!(sent.len(), 2, "the correction, then the reply: {step:?}");
    assert!(sent[0].playstate.expect("playstate").do_seek);
    assert_eq!(sent[0].ignoring_on_the_fly.1, Some(1));
}

// ------------------------------------------------------------------ detection

#[test]
fn a_jump_of_more_than_a_second_is_the_user_seeking() {
    let mut session = Session::watching(10.0, true);

    let step = session.poll(11.01, true);

    assert_eq!(
        playstate(&step),
        Some(ClientPlaystate {
            position: 11.01,
            paused: true,
            do_seek: true
        })
    );
    assert_eq!(envelope(&step), (None, Some(1)));
}

#[test]
fn a_jump_of_a_second_or_less_is_not() {
    let mut session = Session::watching(10.0, true);

    let step = session.poll(11.0, true);

    assert!(step.send.is_empty(), "{step:?}");
}

#[test]
fn a_jump_to_where_the_room_already_is_is_not_a_seek() {
    let mut session = Session::watching(10.0, true);
    // The room is at 7 s. Less than four seconds ahead, the player stays put.
    let quiet = session.state(7.0, true, Some("bob"));
    assert!(seeks(&quiet).is_empty(), "{quiet:?}");

    let step = session.poll(7.0, true);

    assert!(step.send.is_empty(), "{step:?}");
}

#[test]
fn the_players_own_echo_of_a_server_pause_is_not_the_user_pausing() {
    let mut session = Session::watching(10.0, false);
    session.state(11.0, true, Some("bob"));

    session.after(100);
    let step = session.poll(11.0, true);

    assert!(is_quiet(&step), "{step:?}");
}

#[test]
fn the_players_own_echo_of_a_server_unpause_is_not_the_user_unpausing() {
    let mut session = Session::watching(10.0, true);
    session.state(10.0, false, Some("bob"));

    session.after(100);
    let step = session.poll(10.1, false);

    assert!(is_quiet(&step), "{step:?}");
}

#[test]
fn the_players_own_echo_of_a_server_seek_is_not_the_user_seeking() {
    let mut session = Session::watching(10.0, true);
    session.seek_state(50.0, true, "bob");

    session.after(100);
    let step = session.poll(50.0, true);

    assert!(is_quiet(&step), "{step:?}");
}

#[test]
fn the_players_own_echo_of_a_rewind_is_not_the_user_seeking() {
    let mut session = Session::watching(100.0, true);
    session.state(95.0, true, Some("bob"));

    session.after(100);
    let step = session.poll(95.0, true);

    assert!(is_quiet(&step), "{step:?}");
}

// ------------------------------------------------------------------ files

#[test]
fn for_a_second_after_a_file_opens_its_reports_stand_in_for_the_room() {
    let mut session = Session::greeted();
    session.attach();
    session.state(30.0, true, Some("bob"));
    session.load();

    session.after(500);
    let early = session.poll(0.0, false);
    assert!(
        is_quiet(&early),
        "a file that has not settled says nothing: {early:?}"
    );

    session.after(510);
    let settled = session.poll(30.0, true);
    assert!(is_quiet(&settled), "{settled:?}");

    let unpaused = session.poll(30.0, false);
    assert_eq!(
        playstate(&unpaused).map(|playstate| playstate.paused),
        Some(false),
        "once settled, the player is believed"
    );
}

#[test]
fn opening_a_file_in_a_playing_room_joins_it_where_it_is() {
    let mut session = Session::greeted();
    session.attach();
    session.state(100.0, false, Some("bob"));

    session.after(2000);
    let step = session.load();

    assert_eq!(
        step.player,
        vec![PlayerCommand::Seek(102.0), PlayerCommand::SetPaused(false)]
    );
    assert_eq!(
        step.send,
        vec![
            ClientMessage::SetFile(FileReport {
                name: "Film.mkv".to_owned(),
                duration: FILM_LENGTH,
                size: 1234,
            }),
            ClientMessage::ListRequest,
        ]
    );
}

#[test]
fn opening_a_file_in_a_paused_room_holds_it_on_the_rooms_frame() {
    let mut session = Session::greeted();
    session.attach();
    session.state(30.0, true, Some("bob"));

    let step = session.load();

    assert_eq!(
        step.player,
        vec![PlayerCommand::Seek(30.0), PlayerCommand::SetPaused(true)]
    );
}

#[test]
fn a_file_opened_before_the_room_is_known_waits_paused_then_joins_it() {
    let mut session = Session::greeted();
    session.attach();

    let opened = session.load();
    assert_eq!(opened.player, vec![PlayerCommand::SetPaused(true)]);

    let step = session.state(250.0, false, Some("bob"));
    assert_eq!(
        step.player,
        vec![PlayerCommand::Seek(250.0), PlayerCommand::SetPaused(false)]
    );
}

#[test]
fn a_joiner_never_pauses_or_rewinds_a_playing_room() {
    let mut session = Session::greeted();
    session.attach();
    session.state(100.0, false, Some("bob"));
    session.after(2000);
    session.load();

    // Polls and heartbeats while the file settles, then for a while after.
    // mpv is where it was told to be.
    let mut sent = Vec::new();
    for tick in 1..=30u64 {
        session.after(100);
        let position = 102.0 + tick as f64 * 0.1;
        sent.extend(session.poll(position, false).send);
        if tick % 10 == 0 {
            sent.extend(session.state(position, false, Some("bob")).send);
        }
    }

    assert_eq!(sent.len(), 3, "one reply per heartbeat and nothing else");
    for message in &sent {
        let ClientMessage::State(state) = message else {
            panic!("only replies were expected: {message:?}");
        };
        let playstate = state.playstate.expect("a playstate");
        assert!(!playstate.paused, "{state:?}");
        assert!(!playstate.do_seek, "{state:?}");
        assert_eq!(state.ignoring_on_the_fly, (None, None), "{state:?}");
    }
}

#[test]
fn the_film_ending_is_a_pause_the_room_follows_without_anyone_becoming_unready() {
    let mut session = Session::watching_film(110.0, false, 120.0);
    session.set_ready_echo("alice", true);

    session.after(8750);
    let step = session.poll(120.0, true);

    assert!(readiness(&step).is_empty(), "{step:?}");
    assert_eq!(
        playstate(&step),
        Some(ClientPlaystate {
            position: 120.0,
            paused: true,
            do_seek: false
        })
    );
    assert_eq!(envelope(&step), (None, Some(1)));
}

#[test]
fn a_pause_well_before_the_end_makes_this_client_unready() {
    let mut session = Session::watching_film(10.0, false, 120.0);
    session.set_ready_echo("alice", true);

    let step = session.poll(11.25, true);

    assert_eq!(readiness(&step), vec![(false, false)]);
    assert!(
        matches!(
            step.send.as_slice(),
            [ClientMessage::SetReady { .. }, ClientMessage::State(_)]
        ),
        "readiness goes out before the state, as upstream sends it: {step:?}"
    );
}

#[test]
fn a_closed_file_is_no_longer_believed() {
    let mut session = Session::watching(10.0, true);

    session.engine.file_unloaded();
    let step = session.poll(500.0, false);

    assert!(step.send.is_empty(), "{step:?}");
}

// ------------------------------------------------------------------ readiness

#[test]
fn unpausing_while_somebody_is_not_ready_only_marks_this_client_ready() {
    let mut session = Session::watching(10.0, true);
    session.list(false);

    let step = session.poll(10.0, false);

    assert_eq!(step.player, vec![PlayerCommand::SetPaused(true)]);
    assert_eq!(readiness(&step), vec![(true, true)]);
    assert!(
        states(&step).is_empty(),
        "the room is not unpaused: {step:?}"
    );
    assert_eq!(step.notices, vec![Notice::ReadyToUnpause]);
}

#[test]
fn unpausing_again_once_ready_unpauses_the_room() {
    let mut session = Session::watching(10.0, true);
    session.list(false);
    session.poll(10.0, false);
    session.set_ready_echo("alice", true);

    session.after(100);
    let repaused = session.poll(10.0, true);
    assert!(
        repaused.send.is_empty(),
        "the player went back to paused as told"
    );

    session.after(1000);
    let step = session.poll(10.0, false);

    assert!(readiness(&step).is_empty(), "already ready");
    assert_eq!(
        playstate(&step).map(|playstate| playstate.paused),
        Some(false)
    );
}

#[test]
fn somebody_with_no_file_open_does_not_hold_the_room_up() {
    let mut session = Session::watching(10.0, true);
    session.receive(json!({
        "List": { "MovieNight": {
            "alice": { "file": { "name": "Film.mkv", "duration": FILM_LENGTH }, "isReady": false },
            "bob": { "file": {}, "isReady": false },
        }}
    }));

    let step = session.poll(10.0, false);

    assert_eq!(readiness(&step), vec![(true, false)]);
    assert_eq!(states(&step).len(), 1);
}

#[test]
fn somebody_who_has_never_said_does_not_hold_the_room_up() {
    let mut session = Session::watching(10.0, true);
    session.receive(json!({
        "List": { "MovieNight": {
            "alice": { "file": { "name": "Film.mkv", "duration": FILM_LENGTH }, "isReady": false },
            "bob": { "file": { "name": "Film.mkv", "duration": FILM_LENGTH }, "isReady": null },
        }}
    }));

    let step = session.poll(10.0, false);

    assert_eq!(states(&step).len(), 1, "{step:?}");
}

#[test]
fn readiness_follows_what_the_server_says() {
    let mut session = Session::greeted();
    assert!(!session.engine.ready(), "the list said not ready");

    session.set_ready_echo("alice", true);
    assert!(session.engine.ready());

    session.set_ready_echo("bob", false);
    assert!(
        session.engine.ready(),
        "somebody else's readiness is theirs"
    );
}

#[test]
fn the_users_own_toggle_is_sent_as_manual_and_only_when_it_changes_something() {
    let mut session = Session::greeted();

    let step = session.engine.set_ready(true);
    assert_eq!(readiness(&step), vec![(true, true)]);

    session.set_ready_echo("alice", true);
    assert!(session.engine.set_ready(true).send.is_empty());
}

#[test]
fn a_server_without_readiness_hears_nothing_about_it() {
    let mut session = Session::greeted();
    session.receive(json!({
        "Hello": { "username": "alice", "room": { "name": "MovieNight" }, "features": { "readiness": false } }
    }));
    session.list(false);
    session.attach();
    session.load();
    session.state(10.0, true, Some("bob"));
    session.after(SETTLED_AFTER);
    session.poll(10.0, true);

    let step = session.poll(10.0, false);

    assert!(readiness(&step).is_empty(), "{step:?}");
    assert!(
        step.player.is_empty(),
        "nothing to wait for, so nothing re-pauses"
    );
    assert_eq!(states(&step).len(), 1);
}

// ------------------------------------------------------------------ the room

#[test]
fn the_room_view_comes_from_the_list_and_leaves_the_monitor_out() {
    let session = Session::greeted();

    let room = session.engine.room();
    assert!(room.connected);
    let watchers: Vec<&str> = room.rooms[0]
        .watchers
        .iter()
        .map(|watcher| watcher.name.as_str())
        .collect();
    assert_eq!(watchers, vec!["alice", "bob"]);
}

#[test]
fn the_room_view_is_disconnected_until_the_first_list_and_after_a_drop() {
    let mut engine = Engine::new(config());
    engine.connected();
    engine.receive(message(hello("alice")), Instant::now(), EPOCH);
    assert!(!engine.room().connected);

    let mut session = Session::greeted();
    session.engine.disconnected();
    assert_eq!(*session.engine.room(), RoomSnapshot::disconnected());
}

#[test]
fn somebody_joining_is_announced_and_the_list_asked_for_again() {
    let mut session = Session::greeted();

    let step = session.receive(json!({
        "Set": { "user": { "carol": { "room": { "name": "MovieNight" }, "event": { "joined": true } } } }
    }));

    assert_eq!(
        step.notices,
        vec![Notice::Joined {
            user: "carol".to_owned()
        }]
    );
    assert_eq!(step.send, vec![ClientMessage::ListRequest]);
    assert!(session.engine.room().rooms[0]
        .watchers
        .iter()
        .any(|watcher| watcher.name == "carol"));
}

#[test]
fn somebody_leaving_is_announced() {
    let mut session = Session::greeted();

    let step = session.receive(json!({
        "Set": { "user": { "bob": { "room": { "name": "MovieNight" }, "event": { "left": true } } } }
    }));

    assert_eq!(
        step.notices,
        vec![Notice::Left {
            user: "bob".to_owned()
        }]
    );
    assert!(!session.engine.room().rooms[0]
        .watchers
        .iter()
        .any(|watcher| watcher.name == "bob"));
}

#[test]
fn a_file_that_is_not_the_same_film_is_pointed_out_once() {
    let mut session = Session::greeted();
    let other_film = json!({
        "Set": { "user": { "bob": { "room": { "name": "MovieNight" }, "file": { "name": "Other.mkv", "duration": 5400.0 } } } }
    });

    let first = session.receive(other_film.clone());
    assert_eq!(
        first.notices,
        vec![Notice::FileDiffers {
            user: "bob".to_owned()
        }]
    );

    let again = session.receive(other_film);
    assert!(again.notices.is_empty(), "{again:?}");
}

#[test]
fn a_different_name_with_the_same_length_is_the_same_film() {
    let mut session = Session::greeted();

    let step = session.receive(json!({
        "Set": { "user": { "bob": { "room": { "name": "MovieNight" }, "file": { "name": "Film.1080p.mkv", "duration": FILM_LENGTH + 1.0 } } } }
    }));

    assert!(step.notices.is_empty(), "{step:?}");
}

#[test]
fn being_moved_to_another_room_follows_the_server() {
    let mut session = Session::greeted();

    session.receive(json!({ "Set": { "room": { "name": "Elsewhere" } } }));
    let step = session.receive(json!({
        "Set": { "user": { "carol": { "room": { "name": "MovieNight" }, "event": { "joined": true } } } }
    }));

    assert!(
        step.notices.is_empty(),
        "a join in a room this client left is not news"
    );
}

// ------------------------------------------------------------------ reconnecting

#[test]
fn a_new_connection_starts_with_fresh_counters_and_a_fresh_ping() {
    let mut session = Session::watching(10.0, true);
    measure_round_trip(&mut session);
    session.poll(30.0, true);

    session.reconnect();
    let step = session.state(30.0, true, Some("bob"));

    assert_eq!(envelope(&step), (None, None), "no counter survives");
    assert_eq!(only_state(&step).client_rtt, 0.0, "no round trip survives");
    assert!(playstate(&step).is_some());
}

#[test]
fn after_a_reconnect_nothing_is_reported_until_the_room_is_known_again() {
    let mut session = Session::watching(10.0, true);
    session.reconnect();

    let step = session.poll(30.0, true);

    assert!(step.send.is_empty(), "{step:?}");
}

#[test]
fn a_reconnect_in_the_middle_of_a_film_does_not_jump() {
    let mut session = Session::watching(100.0, false);
    session.engine.disconnected();

    session.after(2000);
    session.poll(103.25, false);
    session.engine.connected();
    session.receive(hello("alice"));
    session.after(100);
    let step = session.state(103.0, false, Some("bob"));

    assert!(seeks(&step).is_empty(), "{step:?}");
    assert!(step.notices.is_empty(), "{step:?}");
}

#[test]
fn a_reconnect_after_the_room_moved_on_catches_up() {
    let mut session = Session::watching(100.0, true);
    session.reconnect();

    let step = session.state(400.0, false, Some("bob"));

    assert_eq!(
        step.player,
        vec![PlayerCommand::Seek(400.0), PlayerCommand::SetPaused(false)]
    );
}

// ------------------------------------------------------------------ the player going

#[test]
fn a_detached_player_leaves_replies_without_playstate_and_is_not_polled() {
    let mut session = Session::watching(10.0, true);

    session.engine.detach();
    let polled = session.poll(50.0, false);
    let step = session.state(10.0, false, Some("bob"));

    assert!(is_quiet(&polled), "{polled:?}");
    assert!(step.player.is_empty());
    assert_eq!(playstate(&step), None);
}

#[test]
fn a_new_player_catches_up_with_the_room_before_it_reports() {
    let mut session = Session::watching(10.0, true);
    session.engine.detach();
    session.attach();

    let step = session.state(10.0, false, Some("bob"));

    assert_eq!(step.player, vec![PlayerCommand::SetPaused(false)]);
    assert!(playstate(&step).is_some());
}
