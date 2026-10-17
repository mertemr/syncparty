//! One room: who is in it, and the playback state everyone is held to.
//!
//! Two decisions live here. Arbitration decides whether a client's report moves
//! the room; sampling decides where the room is between decisions, by following
//! its slowest watcher. Both read the same state, and both are easy to get
//! subtly wrong: a room starts paused, a room that empties forgets where it
//! was, and a room that is told to go somewhere has to forget where everybody
//! *used* to be.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::core::syncplay::ping::PingService;
use crate::core::syncplay::protocol::ServerToClient;
use crate::core::syncplay::server::auth;
use crate::core::syncplay::server::ignore::IgnoreGate;
use crate::core::syncplay::server::stamp;

/// What the room is holding everyone to.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackState {
    pub position: f64,
    pub paused: bool,
    /// Who last moved the room, or the watcher it last followed. `None` until
    /// either has happened.
    pub set_by: Option<String>,
}

impl Default for PlaybackState {
    /// Paused at zero, the way Syncplay opens a room. A room that started
    /// playing would run away from the first person to join it, since nobody
    /// has reported a position yet for it to be held to.
    fn default() -> Self {
        Self {
            position: 0.0,
            paused: true,
            set_by: None,
        }
    }
}

/// One `State` as it goes out, before a recipient's own counters and ping
/// fields are stamped on it.
///
/// Everything here is the same for every recipient of one send; only what
/// [`User::render_state`] adds differs between them.
#[derive(Debug, Clone, PartialEq)]
pub struct Playstate {
    pub position: f64,
    pub paused: bool,
    pub do_seek: bool,
    pub set_by: Option<String>,
}

impl Playstate {
    /// The room as it stands, at a position already worked out for `now`.
    pub fn of(playback: &PlaybackState, position: f64, do_seek: bool) -> Self {
        Self {
            position,
            paused: playback.paused,
            do_seek,
            set_by: playback.set_by.clone(),
        }
    }
}

/// The file a watcher has open, as the server knows it.
///
/// `size` stays an opaque JSON value because clients disagree about whether it
/// is a number or a string, and the server never does anything with it but
/// hand it back out.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenFile {
    pub name: String,
    pub duration: Option<f64>,
    pub size: Option<serde_json::Value>,
}

/// One connected participant.
pub struct User {
    pub name: String,
    pub file: Option<OpenFile>,
    /// `None` until they say. Upstream starts every watcher there, and a
    /// client shows "unknown" differently from "not ready".
    pub is_ready: Option<bool>,
    /// Whether this user may move a controlled room. Meaningless in an
    /// ordinary room, where everybody may.
    pub is_controller: bool,
    /// What their player last reported. `None` until it has.
    pub position: Option<f64>,
    /// When `position` was stored, which is what it is extrapolated from.
    ///
    /// Deliberately not `last_seen`. Upstream uses one timestamp for both, so
    /// a report that carries no position still resets the extrapolation and
    /// the watcher's position freezes until they next mention one.
    pub reported_at: Instant,
    /// When this connection last sent a `State` the room accepted. A
    /// connection that goes quiet for longer than the protocol timeout has
    /// gone, whether or not its socket noticed.
    pub last_seen: Instant,
    /// Where lines for this user are pushed. Bounded on purpose: a peer that
    /// has stopped reading is a peer that has left, and buffering without
    /// limit would turn one stalled client into the whole server's problem.
    pub outbound: mpsc::Sender<String>,
    /// This connection's `ignoringOnTheFly` counters.
    ///
    /// Still per connection — one `User` is one connection — but kept here
    /// rather than beside the socket because a forced broadcast has to stamp
    /// *every recipient's* own counter, and the only thing that can reach them
    /// all is the registry. Upstream does the same: `forcePositionUpdate`
    /// calls each watcher's `sendState(..., forced=True)`, and each of those
    /// bumps that watcher's own counter.
    pub gate: IgnoreGate,
    /// Round trips to this client, measured on the server's own stamps.
    pub ping: PingService,
    /// The client's stamp from its last report and when that report arrived,
    /// waiting to be handed back once with the time the server held it added.
    pub client_latency: Option<(f64, Instant)>,
    /// Set the first time a line for this user could not be queued. From then
    /// on nothing more is queued — a client that missed one message must not
    /// be handed the next as though it had not — and the connection is closed
    /// at its next heartbeat.
    pub lagging: bool,
}

impl User {
    pub fn new(name: &str, outbound: mpsc::Sender<String>, now: Instant) -> Self {
        Self {
            name: name.to_owned(),
            file: None,
            is_ready: None,
            is_controller: false,
            position: None,
            reported_at: now,
            last_seen: now,
            outbound,
            gate: IgnoreGate::default(),
            ping: PingService::new(),
            client_latency: None,
            lagging: false,
        }
    }

    /// Whether this user can be the room's position sample.
    ///
    /// Somebody who has just arrived and opened nothing is not a candidate.
    /// Otherwise a newcomer would drag the whole room back to zero, which is
    /// the opposite of what following the slowest watcher is for.
    pub fn has_playback(&self) -> bool {
        self.position.is_some() && self.file.is_some()
    }

    /// Where this watcher is now, if they are a sample at all.
    ///
    /// A report is a reading taken at one moment; while the room plays, the
    /// watcher has carried on since. Upstream's `Watcher.getPosition`.
    pub fn position_now(&self, room_playing: bool, now: Instant) -> Option<f64> {
        if !self.has_playback() {
            return None;
        }

        let position = self.position?;
        if !room_playing {
            return Some(position);
        }

        Some(
            position
                + now
                    .saturating_duration_since(self.reported_at)
                    .as_secs_f64(),
        )
    }

    pub fn record_position(&mut self, position: f64, now: Instant) {
        self.position = Some(position);
        self.reported_at = now;
    }

    /// Whether this connection has said nothing the room accepted for longer
    /// than `timeout`.
    pub fn is_silent(&self, timeout: Duration, now: Instant) -> bool {
        now.saturating_duration_since(self.last_seen) > timeout
    }

    /// Queues one line without waiting.
    ///
    /// Never awaits, which is what lets every send happen under the registry
    /// lock: the lock is what fixes the order in which one recipient hears
    /// about things, and a send that could wait for a slow peer would hold
    /// everybody else up while it did.
    pub fn push(&mut self, line: String) {
        if self.lagging {
            return;
        }

        if self.outbound.try_send(line).is_err() {
            self.lagging = true;
        }
    }

    pub fn send(&mut self, message: &ServerToClient) {
        if let Ok(line) = message.to_line() {
            self.push(line);
        }
    }

    /// Renders a `State` for this connection, or `None` when one may not go
    /// out now.
    ///
    /// The one place a `State` is made, for heartbeats and forced sends alike
    /// (upstream's `sendState`). That matters because both carry the same two
    /// one-shot payloads — the client's own counter, echoed, and its latency
    /// stamp, handed back — and whichever `State` goes out next must be the
    /// one that carries them.
    ///
    /// A heartbeat is held back while this connection still owes an
    /// acknowledgement for a force: the client would be told the room is
    /// somewhere it has not yet heard the room went. Held back, the one-shot
    /// payloads stay where they are for the next `State` that does go out.
    /// Upstream clears both even when it then sends nothing, and a client
    /// waiting for its counter to come back stays deaf until somebody forces
    /// again.
    pub fn render_state(
        &mut self,
        state: &Playstate,
        forced: bool,
        now: Instant,
    ) -> Option<ServerToClient> {
        if !forced && !self.gate.accepts_updates() {
            return None;
        }

        if forced {
            self.gate.on_forced_send();
        }

        let client_latency_calculation = self
            .client_latency
            .take()
            .map(|(stamp, arrived)| stamp + now.saturating_duration_since(arrived).as_secs_f64());

        Some(ServerToClient::State {
            position: state.position,
            paused: state.paused,
            do_seek: state.do_seek,
            set_by: state.set_by.clone(),
            latency_calculation: stamp(now),
            // The raw reading can go negative after a clock step; the client
            // would take that at face value.
            server_rtt: self.ping.rtt().max(0.0),
            client_latency_calculation,
            ignoring_on_the_fly: self.gate.take_envelope(),
        })
    }

    pub fn send_state(&mut self, state: &Playstate, forced: bool, now: Instant) {
        if let Some(message) = self.render_state(state, forced, now) {
            self.send(&message);
        }
    }
}

/// How long a room's own reading stays authoritative before it starts
/// following its watchers instead. Upstream's `age > 1` in `getPosition`.
const SAMPLE_AFTER: Duration = Duration::from_secs(1);

/// What a client's `State` message asks the room to become.
///
/// Every field is optional because a report is not a demand: a client that says
/// nothing about pausing is not asking for a pause change.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StateUpdate {
    pub position: Option<f64>,
    pub paused: Option<bool>,
    pub do_seek: bool,
}

/// What the room decided everyone should be told.
#[derive(Debug, Clone, PartialEq)]
pub enum Force {
    /// Drift. The client corrects itself; the room says nothing.
    Nothing,
    /// Somebody made a decision the whole room follows, sender included.
    Broadcast(Playstate),
    /// Somebody who may not move this room tried to. Both go to them alone.
    ///
    /// Two messages rather than one: `echo` repeats the pause state they asked
    /// for and exists only so 1.2.x clients keep working, and `real` carries
    /// the state the room is actually in, with a seek so they go there.
    CorrectSender { echo: Playstate, real: Playstate },
}

pub struct Room {
    name: String,
    users: HashMap<String, User>,
    playback: PlaybackState,
    /// When `playback.position` was last known to be true: the last decision
    /// or the last sample, whichever came later.
    last_update: Instant,
    /// Fixed at construction: a room is controlled if its *name* says so, and
    /// a name does not change.
    controlled: bool,
}

impl Room {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            users: HashMap::new(),
            playback: PlaybackState::default(),
            last_update: Instant::now(),
            controlled: auth::is_controlled_room(name),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn users(&self) -> &HashMap<String, User> {
        &self.users
    }

    pub fn user_mut(&mut self, name: &str) -> Option<&mut User> {
        self.users.get_mut(name)
    }

    /// Every watcher, mutably, so a forced broadcast can stamp each one's
    /// counter as it renders their copy.
    pub fn users_mut(&mut self) -> impl Iterator<Item = (&String, &mut User)> {
        self.users.iter_mut()
    }

    /// The pause flag and `setBy` a `State` carries. The position is not read
    /// from here on its own: it is only true together with when it was taken,
    /// which is what [`Room::sample`] accounts for.
    pub fn playback(&self) -> &PlaybackState {
        &self.playback
    }

    /// Sets the room's playback state outright, without going through
    /// arbitration. Nothing in production may skip arbitration, so this exists
    /// only to put a room into a known state for a test.
    #[cfg(test)]
    pub fn force_playback(&mut self, playback: PlaybackState) {
        self.playback = playback;
        self.last_update = Instant::now();
    }

    pub fn is_empty(&self) -> bool {
        self.users.is_empty()
    }

    /// Whether the room's own name says it is controlled.
    pub fn is_controlled(&self) -> bool {
        self.controlled
    }

    /// Whether `user` may move this room.
    ///
    /// Everybody may in an ordinary room. That is not a permission check that
    /// was forgotten — Syncplay's base room returns true unconditionally, and a
    /// private party among friends is exactly the case it was designed for.
    pub fn can_control(&self, user: &str) -> bool {
        if !self.controlled {
            return true;
        }

        self.users.get(user).is_some_and(|user| user.is_controller)
    }

    pub fn set_controller(&mut self, user: &str, is_controller: bool) {
        if let Some(user) = self.users.get_mut(user) {
            user.is_controller = is_controller;
        }
    }

    /// The sender's own last reported position, whether or not they may move
    /// the room.
    pub fn user_position(&self, name: &str) -> Option<f64> {
        self.users.get(name)?.position
    }

    pub fn set_file(&mut self, name: &str, file: Option<OpenFile>) {
        if let Some(user) = self.users.get_mut(name) {
            user.file = file;
        }
    }

    pub fn set_position(&mut self, name: &str, position: f64, now: Instant) {
        if let Some(user) = self.users.get_mut(name) {
            user.record_position(position, now);
        }
    }

    /// Where the room's own reading puts it at `now`, without looking at any
    /// watcher.
    fn advanced(&self, now: Instant) -> f64 {
        if self.playback.paused {
            return self.playback.position;
        }

        self.playback.position
            + now
                .saturating_duration_since(self.last_update)
                .as_secs_f64()
    }

    /// Where the room is at `now`, following its slowest watcher.
    ///
    /// Deliberately takes `now` rather than reading a clock, so the suite is
    /// deterministic and never sleeps.
    ///
    /// Once the room's own reading is more than a second old it takes the
    /// position of the watcher furthest behind — extrapolated to `now`, since
    /// their report is older than that — and that watcher becomes `setBy`.
    /// That is what stops a party drifting apart: nobody is allowed to get
    /// ahead of the person furthest behind, and because clients never correct
    /// against themselves, naming that person is what makes them the
    /// reference everybody else adjusts to. Between samples, and in a room with
    /// nothing to sample, the room advances by elapsed time while it plays.
    ///
    /// In a controlled room only controllers are followed: anybody else being
    /// behind is theirs to fix, not the room's.
    ///
    /// A stale room with nobody to follow still counts as sampled: it writes
    /// down where its own reading puts it and starts the second again, as
    /// upstream's `getPosition` does. Left stale, the next call would sample
    /// whoever has just become a candidate — a mover arriving with a file, say,
    /// who would then be named as having set the room they were only told
    /// about.
    pub fn sample(&mut self, now: Instant) -> f64 {
        let stale = now.saturating_duration_since(self.last_update) > SAMPLE_AFTER;
        if !stale {
            return self.advanced(now);
        }

        let playing = !self.playback.paused;
        let slowest = self
            .users
            .values()
            .filter(|user| !self.is_controlled() || user.is_controller)
            .filter_map(|user| Some((user, user.position_now(playing, now)?)))
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(user, position)| (user.name.clone(), position));

        let position = match slowest {
            Some((name, position)) => {
                self.playback.set_by = Some(name);
                position
            }
            None => self.advanced(now),
        };
        self.playback.position = position;
        self.last_update = now;

        position
    }

    /// Folds one client's report into the room and says what to send.
    ///
    /// The narrow part is what counts as a decision. Position drift alone
    /// forces nothing — only an explicit seek or an actual pause change is
    /// something somebody chose, and a server that also forced on drift would
    /// fight every client that was slightly behind.
    pub fn apply(
        &mut self,
        from: &str,
        update: StateUpdate,
        message_age: Duration,
        now: Instant,
    ) -> Force {
        let may_control = self.can_control(from);
        let pause_change = update
            .paused
            .filter(|paused| *paused != self.playback.paused);
        // Taken before the report changes anything, so a pause lands where the
        // room was when it was asked for rather than where it had stopped.
        let before = self.advanced(now);

        // Recorded whatever their rights are: only the room-level state is
        // refused, never the report itself. A report was already stale when it
        // arrived, so unless it says it is paused, the film it describes has
        // moved on by its own age. Its own flag decides that, not the room's:
        // they differ exactly when the report is a pause change the room has
        // not taken yet.
        if let Some(position) = update.position {
            let reported = if update.paused == Some(true) {
                position
            } else {
                position + message_age.as_secs_f64()
            };
            self.set_position(from, reported, now);
        }

        if !update.do_seek && pause_change.is_none() {
            return Force::Nothing;
        }

        if !may_control {
            let position = self.sample(now);
            return Force::CorrectSender {
                echo: Playstate {
                    position,
                    paused: update.paused.unwrap_or(self.playback.paused),
                    do_seek: false,
                    set_by: Some(from.to_owned()),
                },
                real: Playstate::of(&self.playback, position, true),
            };
        }

        if let Some(paused) = pause_change {
            self.playback.paused = paused;
        }

        let position = match update.position {
            Some(_) => self.user_position(from).unwrap_or(before),
            None => before,
        };
        self.playback.position = position;
        self.playback.set_by = Some(from.to_owned());
        self.last_update = now;

        // Everybody has just been told to go here, so that is where everybody
        // now is. Without this the reports from before the force — still
        // stored, and the newer ones dropped while each gate waits for its
        // acknowledgement — would be sampled a second later, and the room
        // would tell everyone to go back to where they were before the seek.
        for user in self.users.values_mut() {
            user.record_position(position, now);
        }

        Force::Broadcast(Playstate::of(&self.playback, position, update.do_seek))
    }

    pub fn add(&mut self, name: &str, outbound: mpsc::Sender<String>) {
        self.insert(User::new(name, outbound, Instant::now()));
    }

    pub fn insert(&mut self, user: User) {
        self.users.insert(user.name.clone(), user);
    }

    /// Removes a user and hands them back, so a room change can carry them
    /// across without losing their file, readiness or connection.
    pub fn remove(&mut self, name: &str) -> Option<User> {
        let user = self.users.remove(name);

        // Nobody here set the room any more. Kept, the name would outlive them,
        // and if they came back the State that tells them where the room is
        // would say they had put it there, which a client ignores.
        if self.playback.set_by.as_deref() == Some(name) {
            self.playback.set_by = None;
        }

        // An empty room forgets where it was. The next party in a room of the
        // same name starts from the beginning rather than halfway through
        // whatever the last one was watching.
        if self.users.is_empty() {
            self.playback = PlaybackState::default();
            self.last_update = Instant::now();
        }

        user
    }

    /// Sends one message to everyone here but `except`.
    pub fn broadcast(&mut self, except: Option<&str>, message: &ServerToClient) {
        let Ok(line) = message.to_line() else {
            return;
        };

        for (name, user) in &mut self.users {
            if Some(name.as_str()) != except {
                user.push(line.clone());
            }
        }
    }

    pub fn send_to(&mut self, name: &str, message: &ServerToClient) {
        if let Some(user) = self.users.get_mut(name) {
            user.send(message);
        }
    }
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;

    /// A sender whose receiver is dropped immediately.
    ///
    /// Fine for tests about who is in which room, which never send. The tests
    /// that do exercise delivery hold on to the receiving half instead.
    pub fn sender() -> mpsc::Sender<String> {
        mpsc::channel(16).0
    }

    /// The same file every arbitration test opens; only its presence matters
    /// there, never its name.
    pub fn watched_file() -> OpenFile {
        open_file("Film.mkv")
    }

    pub fn open_file(name: &str) -> OpenFile {
        OpenFile {
            name: name.to_owned(),
            duration: Some(7200.0),
            size: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{open_file, sender, watched_file};
    use super::*;

    #[test]
    fn a_fresh_room_is_paused_at_zero() {
        let room = Room::new("MovieNight");

        assert!(room.playback().paused, "Syncplay opens a room paused");
        assert_eq!(room.playback().position, 0.0);
        assert!(room.playback().set_by.is_none());
    }

    #[test]
    fn a_room_that_empties_forgets_where_it_was() {
        let mut room = Room::new("MovieNight");
        room.add("ahmet", sender());
        room.force_playback(PlaybackState {
            position: 3600.0,
            paused: false,
            set_by: Some("ahmet".to_owned()),
        });

        room.remove("ahmet");

        assert_eq!(room.playback().position, 0.0);
        assert!(
            room.playback().paused,
            "the next party starts from the beginning"
        );
    }

    #[test]
    fn a_room_losing_one_of_several_keeps_its_position() {
        let mut room = Room::new("MovieNight");
        room.add("ahmet", sender());
        room.add("mehmet", sender());
        room.force_playback(PlaybackState {
            position: 3600.0,
            paused: false,
            set_by: None,
        });

        room.remove("ahmet");

        assert_eq!(room.playback().position, 3600.0);
    }

    #[test]
    fn everybody_may_control_an_ordinary_room() {
        let mut room = Room::new("MovieNight");
        room.add("ahmet", sender());

        assert!(!room.is_controlled());
        assert!(room.can_control("ahmet"));
    }

    #[test]
    fn only_a_named_controller_may_control_a_controlled_room() {
        let mut room = Room::new("+MovieNight:ABCDEF012345");
        room.add("ahmet", sender());
        room.add("mehmet", sender());
        room.set_controller("ahmet", true);

        assert!(room.is_controlled());
        assert!(room.can_control("ahmet"));
        assert!(!room.can_control("mehmet"));
        assert!(
            !room.can_control("someone-who-left"),
            "a name that is not in the room controls nothing"
        );
    }

    #[test]
    fn a_watcher_with_no_file_is_not_a_position_sample() {
        let mut room = Room::new("MovieNight");
        room.add("ahmet", sender());

        let user = room.user_mut("ahmet").expect("user");
        assert!(!user.has_playback(), "nothing open, nothing reported");

        user.position = Some(10.0);
        assert!(
            !user.has_playback(),
            "a position with no file is not a sample either"
        );

        user.file = Some(open_file("Film.mkv"));
        assert!(user.has_playback());
    }

    #[test]
    fn a_newcomer_has_said_nothing_about_readiness() {
        let mut room = Room::new("MovieNight");
        room.add("ahmet", sender());

        assert_eq!(
            room.users()["ahmet"].is_ready,
            None,
            "unknown is not the same as not ready"
        );
    }

    // ------------------------------------------------------------ sampling

    /// One instant for a whole test, so nothing depends on how long the test
    /// itself takes to run.
    fn origin() -> Instant {
        Instant::now()
    }

    fn after(start: Instant, seconds: f64) -> Instant {
        start + Duration::from_secs_f64(seconds)
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.01
    }

    fn pause_at(position: f64) -> StateUpdate {
        StateUpdate {
            position: Some(position),
            paused: Some(true),
            do_seek: false,
        }
    }

    fn playing_at(position: f64) -> StateUpdate {
        StateUpdate {
            position: Some(position),
            paused: Some(false),
            do_seek: false,
        }
    }

    fn seek_to(position: f64) -> StateUpdate {
        StateUpdate {
            position: Some(position),
            paused: None,
            do_seek: true,
        }
    }

    /// A playing room whose watchers all have the film open, each at the
    /// position given, all reported at `at`. The room's own reading is also
    /// taken at `at`.
    fn playing_room_with(at: Instant, positions: &[(&str, f64)]) -> Room {
        let mut room = Room::new("MovieNight");
        for (name, position) in positions {
            room.add(name, sender());
            room.set_file(name, Some(watched_file()));
            room.set_position(name, *position, at);
        }
        room.playback.paused = false;
        room.last_update = at;
        room
    }

    #[test]
    fn a_rooms_position_is_its_slowest_watcher() {
        let start = origin();
        let mut room = playing_room_with(start, &[("ahmet", 120.0), ("mehmet", 95.0)]);

        let position = room.sample(after(start, 2.0));

        assert!(
            close(position, 97.0),
            "nobody may get ahead of the person furthest behind, \
             who has carried on playing since they reported: {position}"
        );
    }

    /// Why `setBy` is the sampled watcher: clients never correct against
    /// themselves, so the one named is the one everybody else follows.
    #[test]
    fn the_slowest_watcher_is_named_as_who_set_the_room() {
        let start = origin();
        let mut room = playing_room_with(start, &[("ahmet", 120.0), ("mehmet", 95.0)]);

        room.sample(after(start, 2.0));

        assert_eq!(room.playback().set_by.as_deref(), Some("mehmet"));
    }

    #[test]
    fn a_sample_is_taken_at_most_once_a_second() {
        let start = origin();
        let mut room = playing_room_with(start, &[("ahmet", 100.0)]);
        room.sample(after(start, 1.5));

        // Somebody reports a long way behind, but the room's own reading is
        // fresh, so it just advances.
        room.set_position("ahmet", 50.0, after(start, 1.6));
        let position = room.sample(after(start, 2.0));

        assert!(close(position, 102.0), "got {position}");
    }

    #[test]
    fn a_paused_room_samples_without_extrapolating() {
        let start = origin();
        let mut room = playing_room_with(start, &[("ahmet", 120.0)]);
        room.playback.paused = true;

        assert!(close(room.sample(after(start, 5.0)), 120.0));
    }

    #[test]
    fn somebody_with_no_file_open_does_not_drag_the_room_back() {
        let start = origin();
        let mut room = playing_room_with(start, &[("ahmet", 120.0)]);
        room.add("newcomer", sender());
        room.set_position("newcomer", 0.0, start);

        assert!(
            close(room.sample(after(start, 2.0)), 122.0),
            "a watcher with nothing open is not a candidate for the minimum"
        );
    }

    /// A report with no position — a client waiting on its own counter leaves
    /// `playstate` out — must not freeze the watcher where they last were.
    #[test]
    fn a_report_without_a_position_does_not_freeze_the_watcher() {
        let start = origin();
        let mut room = playing_room_with(start, &[("ahmet", 100.0)]);

        room.apply(
            "ahmet",
            StateUpdate::default(),
            Duration::ZERO,
            after(start, 3.0),
        );

        assert!(close(room.sample(after(start, 4.0)), 104.0));
    }

    #[test]
    fn a_controlled_room_follows_only_its_controllers() {
        let start = origin();
        let mut room = Room::new("+MovieNight:ABCDEF012345");
        for (name, position) in [("ahmet", 120.0), ("mehmet", 30.0)] {
            room.add(name, sender());
            room.set_file(name, Some(watched_file()));
            room.set_position(name, position, start);
        }
        room.set_controller("ahmet", true);
        room.last_update = start;

        let position = room.sample(after(start, 2.0));

        assert!(
            close(position, 120.0),
            "somebody who may not move the room does not hold it back: {position}"
        );
        assert_eq!(room.playback().set_by.as_deref(), Some("ahmet"));
    }

    /// Split from the plan's single test, which asserted that a room paused
    /// after five seconds of play still reads 5.0. What the spec actually says
    /// is that a sampleless room advances by elapsed time *only while
    /// playing*, which is these two tests.
    #[test]
    fn a_room_with_no_sample_advances_by_elapsed_time_while_playing() {
        let start = origin();
        let mut room = playing_room_with(start, &[]);

        assert!(close(room.sample(after(start, 5.0)), 5.0));
    }

    /// Upstream starts the second again whether or not it found anybody to
    /// follow. Without that, a stale room samples again at once, and whoever
    /// has just arrived with a file becomes the room — and is named as having
    /// set it.
    #[test]
    fn a_stale_room_with_nobody_to_follow_still_starts_its_second_again() {
        let start = origin();
        let mut room = playing_room_with(start, &[]);
        room.add("monitor", sender());
        assert!(close(room.sample(after(start, 2.0)), 2.0));

        room.add("ahmet", sender());
        room.set_file("ahmet", Some(watched_file()));
        room.set_position("ahmet", 0.0, after(start, 2.0));

        assert!(
            close(room.sample(after(start, 2.5)), 2.5),
            "the room's own reading is half a second old, not stale"
        );
        assert_eq!(room.playback().set_by, None);

        assert!(
            close(room.sample(after(start, 3.5)), 1.5),
            "a second after that it follows him as it would anybody"
        );
        assert_eq!(room.playback().set_by.as_deref(), Some("ahmet"));
    }

    #[test]
    fn a_watcher_who_leaves_is_no_longer_named_as_who_set_the_room() {
        let mut room = Room::new("MovieNight");
        for name in ["ahmet", "mehmet", "monitor"] {
            room.add(name, sender());
        }
        room.force_playback(PlaybackState {
            position: 300.0,
            paused: true,
            set_by: Some("ahmet".to_owned()),
        });

        room.remove("mehmet");
        assert_eq!(
            room.playback().set_by.as_deref(),
            Some("ahmet"),
            "somebody else leaving changes nothing"
        );

        room.remove("ahmet");
        assert_eq!(
            room.playback().set_by,
            None,
            "were he to come back, he would be told he had set it himself"
        );
        assert_eq!(room.playback().position, 300.0, "only the name goes");
    }

    #[test]
    fn a_paused_room_with_no_sample_does_not_move() {
        let start = origin();
        let mut room = Room::new("MovieNight");
        room.last_update = start;

        assert!(
            close(room.sample(after(start, 5.0)), 0.0),
            "a paused room does not move"
        );
    }

    // --------------------------------------------------------- arbitration

    #[test]
    fn a_pause_is_a_decision_and_is_broadcast() {
        let start = origin();
        let mut room = playing_room_with(start, &[("ahmet", 10.0), ("mehmet", 10.0)]);

        let force = room.apply("ahmet", pause_at(10.0), Duration::ZERO, start);

        let Force::Broadcast(state) = force else {
            panic!("a pause change must be broadcast, got {force:?}");
        };
        assert!(state.paused);
        assert!(!state.do_seek, "a pause asks for no seek");
        assert_eq!(state.set_by.as_deref(), Some("ahmet"));
    }

    #[test]
    fn drift_alone_is_never_forced() {
        let start = origin();
        let mut room = playing_room_with(start, &[("ahmet", 10.0), ("mehmet", 10.0)]);

        let force = room.apply("mehmet", playing_at(70.0), Duration::ZERO, start);

        assert!(
            matches!(force, Force::Nothing),
            "drift is the client's to correct, got {force:?}"
        );
    }

    #[test]
    fn an_explicit_seek_is_broadcast_without_a_pause_change() {
        let start = origin();
        let mut room = playing_room_with(start, &[("ahmet", 10.0), ("mehmet", 10.0)]);

        let force = room.apply("ahmet", seek_to(600.0), Duration::ZERO, start);

        let Force::Broadcast(state) = force else {
            panic!("doSeek must be broadcast, got {force:?}");
        };
        assert!(close(state.position, 600.0));
        assert!(state.do_seek, "the seek is passed on as a seek");
    }

    /// The rewind a heartbeat used to cause: mehmet's stored report was from
    /// before the seek, so a second later the room followed him back there.
    #[test]
    fn a_seek_moves_every_watcher_so_nobody_is_sent_back_afterwards() {
        let start = origin();
        let mut room = playing_room_with(start, &[("ahmet", 10.0), ("mehmet", 10.0)]);

        room.apply("ahmet", seek_to(600.0), Duration::ZERO, start);
        let position = room.sample(after(start, 2.0));

        assert!(
            close(position, 602.0),
            "everybody was told to go to 600, so that is where the room is: {position}"
        );
    }

    #[test]
    fn a_pause_lands_where_the_room_was_when_nobody_said_where() {
        let start = origin();
        let mut room = playing_room_with(start, &[("ahmet", 10.0)]);
        room.playback.position = 10.0;

        let force = room.apply(
            "ahmet",
            StateUpdate {
                position: None,
                paused: Some(true),
                do_seek: false,
            },
            Duration::ZERO,
            after(start, 3.0),
        );

        let Force::Broadcast(state) = force else {
            panic!("a pause change must be broadcast, got {force:?}");
        };
        assert!(close(state.position, 13.0), "got {}", state.position);
    }

    #[test]
    fn a_stale_report_is_advanced_by_its_own_age_while_playing() {
        let start = origin();
        let mut room = playing_room_with(start, &[("ahmet", 10.0), ("mehmet", 10.0)]);

        room.apply("ahmet", seek_to(100.0), Duration::from_secs(2), start);

        assert!(
            close(room.playback().position, 102.0),
            "the report was already two seconds old when it arrived"
        );
    }

    #[test]
    fn a_stale_report_is_not_advanced_while_paused() {
        let start = origin();
        let mut room = playing_room_with(start, &[("ahmet", 10.0)]);

        room.apply("ahmet", pause_at(100.0), Duration::from_secs(2), start);

        assert!(close(room.playback().position, 100.0));
    }

    /// The report's own flag decides, not the room's. In a paused room a
    /// report that says nothing about pausing is still aged, as upstream ages
    /// it; and a non-controller's pause in a playing room is not, even though
    /// the room goes on playing.
    #[test]
    fn a_reports_own_pause_flag_decides_whether_it_is_aged() {
        let start = origin();
        let mut room = playing_room_with(start, &[("ahmet", 10.0)]);
        room.playback.paused = true;

        room.apply("ahmet", seek_to(100.0), Duration::from_secs(2), start);
        assert!(close(room.user_position("ahmet").expect("position"), 102.0));

        let mut controlled = Room::new("+MovieNight:ABCDEF012345");
        controlled.add("mehmet", sender());
        controlled.playback.paused = false;
        controlled.apply("mehmet", pause_at(50.0), Duration::from_secs(2), start);
        assert!(close(
            controlled.user_position("mehmet").expect("position"),
            50.0
        ));
    }

    #[test]
    fn a_non_controller_is_corrected_with_both_messages() {
        let start = origin();
        let mut room = Room::new("+MovieNight:ABCDEF012345");
        room.add("ahmet", sender());
        room.set_file("ahmet", Some(watched_file()));
        room.playback.paused = false;
        room.playback.position = 40.0;
        room.last_update = start;

        let force = room.apply("ahmet", pause_at(10.0), Duration::ZERO, start);

        let Force::CorrectSender { echo, real } = force else {
            panic!("a non-controller must be corrected, got {force:?}");
        };
        assert!(echo.paused, "the first message echoes what they asked for");
        assert!(!echo.do_seek);
        assert_eq!(echo.set_by.as_deref(), Some("ahmet"));
        assert!(!real.paused, "the second carries the room's real state");
        assert!(real.do_seek, "and sends them back to it");
        assert!(
            close(echo.position, 40.0) && close(real.position, 40.0),
            "both carry where the room is, not where they asked to be"
        );
        assert!(!room.playback().paused, "the room must not have moved");
    }

    #[test]
    fn a_controller_moves_a_controlled_room() {
        let start = origin();
        let mut room = Room::new("+MovieNight:ABCDEF012345");
        room.add("ahmet", sender());
        room.set_file("ahmet", Some(watched_file()));
        room.set_controller("ahmet", true);
        room.playback.paused = false;

        let force = room.apply("ahmet", pause_at(10.0), Duration::ZERO, start);

        assert!(matches!(force, Force::Broadcast(_)));
        assert!(room.playback().paused);
    }

    #[test]
    fn a_non_controllers_own_position_is_still_recorded() {
        let start = origin();
        let mut room = Room::new("+MovieNight:ABCDEF012345");
        room.add("ahmet", sender());
        room.set_file("ahmet", Some(watched_file()));

        room.apply("ahmet", playing_at(42.0), Duration::ZERO, start);

        assert!(
            close(room.user_position("ahmet").expect("position"), 42.0),
            "only the room-level state is refused, not the report itself"
        );
    }

    // ----------------------------------------------------------- rendering

    fn listening_user() -> (User, mpsc::Receiver<String>) {
        let (outbound, inbound) = mpsc::channel(4);
        (User::new("ahmet", outbound, Instant::now()), inbound)
    }

    fn heartbeat() -> Playstate {
        Playstate {
            position: 12.0,
            paused: false,
            do_seek: false,
            set_by: Some("mehmet".to_owned()),
        }
    }

    fn envelope(message: &ServerToClient) -> Option<(Option<u64>, Option<u64>)> {
        match message {
            ServerToClient::State {
                ignoring_on_the_fly,
                ..
            } => *ignoring_on_the_fly,
            other => panic!("not a State: {other:?}"),
        }
    }

    fn client_latency(message: &ServerToClient) -> Option<f64> {
        match message {
            ServerToClient::State {
                client_latency_calculation,
                ..
            } => *client_latency_calculation,
            other => panic!("not a State: {other:?}"),
        }
    }

    #[test]
    fn a_heartbeat_is_held_back_while_a_force_is_unacknowledged() {
        let (mut user, _inbound) = listening_user();
        let now = Instant::now();
        user.render_state(&heartbeat(), true, now).expect("forced");

        assert!(
            user.render_state(&heartbeat(), false, now).is_none(),
            "the client has not yet heard where the room went"
        );

        user.gate.observe(Some(1), None);
        assert!(user.render_state(&heartbeat(), false, now).is_some());
    }

    /// Upstream clears both even when it sends nothing, and the client waits
    /// for an echo that is never coming.
    #[test]
    fn a_held_back_heartbeat_keeps_the_echo_and_the_stamp_for_the_next_state() {
        let (mut user, _inbound) = listening_user();
        let now = Instant::now();
        user.render_state(&heartbeat(), true, now).expect("forced");
        user.gate.observe(None, Some(4));
        user.client_latency = Some((1000.0, now));

        assert!(user.render_state(&heartbeat(), false, now).is_none());

        user.gate.observe(Some(1), None);
        let next = user
            .render_state(&heartbeat(), false, after(now, 0.25))
            .expect("the gate is open again");
        assert_eq!(envelope(&next), Some((None, Some(4))));
        assert!(close(client_latency(&next).expect("stamp"), 1000.25));
    }

    #[test]
    fn the_clients_stamp_comes_back_once_with_the_time_the_server_held_it() {
        let (mut user, _inbound) = listening_user();
        let arrived = Instant::now();
        user.client_latency = Some((500.0, arrived));

        let first = user
            .render_state(&heartbeat(), false, after(arrived, 0.4))
            .expect("state");
        let second = user
            .render_state(&heartbeat(), false, after(arrived, 1.4))
            .expect("state");

        assert!(close(client_latency(&first).expect("stamp"), 500.4));
        assert_eq!(client_latency(&second), None, "once per client report");
    }

    #[test]
    fn a_forced_state_bumps_the_counter_and_carries_it() {
        let (mut user, _inbound) = listening_user();
        let now = Instant::now();

        let first = user.render_state(&heartbeat(), true, now).expect("forced");
        let second = user.render_state(&heartbeat(), true, now).expect("forced");

        assert_eq!(envelope(&first), Some((Some(1), None)));
        assert_eq!(envelope(&second), Some((Some(2), None)));
    }

    #[test]
    fn a_full_queue_marks_the_user_lagging_and_queues_nothing_more() {
        let (outbound, mut inbound) = mpsc::channel(1);
        let mut user = User::new("ahmet", outbound, Instant::now());

        user.push("one\r\n".to_owned());
        user.push("two\r\n".to_owned());
        assert!(user.lagging, "the second line had nowhere to go");

        let _ = inbound.try_recv();
        user.push("three\r\n".to_owned());
        assert!(
            inbound.try_recv().is_err(),
            "a client that missed a line must not be handed the next as if it had not"
        );
    }

    #[test]
    fn a_closed_connection_counts_as_lagging() {
        let (outbound, inbound) = mpsc::channel(4);
        let mut user = User::new("ahmet", outbound, Instant::now());
        drop(inbound);

        user.push("one\r\n".to_owned());

        assert!(user.lagging);
    }

    #[test]
    fn silence_is_measured_from_the_last_accepted_report() {
        let (mut user, _inbound) = listening_user();
        let start = Instant::now();
        user.last_seen = start;
        let timeout = Duration::from_millis(12_500);

        assert!(!user.is_silent(timeout, after(start, 12.0)));
        assert!(user.is_silent(timeout, after(start, 13.0)));
    }
}
