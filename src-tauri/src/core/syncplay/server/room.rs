//! One room: who is in it, and the playback state everyone is held to.
//!
//! Arbitration — deciding whether a client's report moves the room — lands here
//! in the next task. What this file establishes is the state that decision
//! reads, and the two facts about it that are easy to get wrong: a room starts
//! paused, and a room that empties forgets where it was.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::core::syncplay::server::auth;

/// What the room is holding everyone to.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackState {
    pub position: f64,
    pub paused: bool,
    /// Who last moved the room. `None` until somebody has.
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
    pub is_ready: bool,
    /// Whether this user may move a controlled room. Meaningless in an
    /// ordinary room, where everybody may.
    pub is_controller: bool,
    /// What their player last reported. `None` until it has.
    pub position: Option<f64>,
    /// Where lines for this user are pushed. Bounded on purpose: a peer that
    /// has stopped reading is a peer that has left, and buffering without
    /// limit would turn one stalled client into the whole server's problem.
    pub outbound: mpsc::Sender<String>,
}

impl User {
    /// Whether this user can be the room's position sample.
    ///
    /// Somebody who has just arrived and opened nothing is not a candidate.
    /// Otherwise a newcomer would drag the whole room back to zero, which is
    /// the opposite of what following the slowest watcher is for.
    pub fn has_playback(&self) -> bool {
        self.position.is_some() && self.file.is_some()
    }
}

/// How long a room's own reading stays authoritative before it starts
/// following its watchers instead.
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
    /// Somebody made a decision the whole room follows.
    Broadcast(PlaybackState),
    /// Somebody who may not move this room tried to.
    ///
    /// Two messages rather than one: `echo` repeats what they asked for and
    /// exists only so clients we did not write keep working, and `real` carries
    /// the state the room is actually in.
    CorrectSender {
        echo: PlaybackState,
        real: PlaybackState,
    },
}

pub struct Room {
    name: String,
    users: HashMap<String, User>,
    playback: PlaybackState,
    /// When `playback.position` was last known to be true.
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

    pub fn playback(&self) -> &PlaybackState {
        &self.playback
    }

    /// Sets the room's playback state outright, without going through
    /// arbitration. For tests and for the paths that have already decided.
    pub fn force_playback(&mut self, playback: PlaybackState) {
        self.playback = playback;
        self.last_update = Instant::now();
    }

    pub fn is_empty(&self) -> bool {
        self.users.is_empty()
    }

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

    pub fn set_position(&mut self, name: &str, position: f64) {
        if let Some(user) = self.users.get_mut(name) {
            user.position = Some(position);
        }
    }

    /// Sets the room's pause flag without going through arbitration.
    pub fn force_paused(&mut self, paused: bool) {
        self.playback.paused = paused;
        self.last_update = Instant::now();
    }

    /// Where the room is at `now`.
    ///
    /// Deliberately takes `now` rather than reading a clock, so the suite is
    /// deterministic and never sleeps.
    ///
    /// Once the room's own reading has gone stale it follows its *slowest*
    /// watcher rather than advancing by elapsed time. That is what stops a
    /// party drifting apart: nobody is allowed to get ahead of the person
    /// furthest behind. Advancing by elapsed time is only the fallback for a
    /// room with nothing to sample.
    pub fn position_at(&self, now: Instant) -> f64 {
        let elapsed = now.saturating_duration_since(self.last_update);

        if elapsed > SAMPLE_AFTER {
            let slowest = self
                .users
                .values()
                .filter(|user| user.has_playback())
                .filter_map(|user| user.position)
                .min_by(f64::total_cmp);

            if let Some(position) = slowest {
                return position;
            }
        }

        if self.playback.paused {
            self.playback.position
        } else {
            self.playback.position + elapsed.as_secs_f64()
        }
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

        if let Some(paused) = pause_change.filter(|_| may_control) {
            self.playback.paused = paused;
            self.playback.set_by = Some(from.to_owned());
        }

        // Recorded whatever their rights are: only the room-level state is
        // refused, never the report itself. A report was already stale when it
        // arrived, so while the film is playing it has moved on by its own age.
        if let Some(position) = update.position {
            let reported = if self.playback.paused {
                position
            } else {
                position + message_age.as_secs_f64()
            };
            self.set_position(from, reported);
        }

        if !update.do_seek && pause_change.is_none() {
            return Force::Nothing;
        }

        if !may_control {
            return Force::CorrectSender {
                echo: PlaybackState {
                    position: self.user_position(from).unwrap_or(self.playback.position),
                    paused: update.paused.unwrap_or(self.playback.paused),
                    set_by: Some(from.to_owned()),
                },
                real: self.playback.clone(),
            };
        }

        if let Some(position) = self.user_position(from) {
            self.playback.position = position;
        }
        self.playback.set_by = Some(from.to_owned());
        self.last_update = now;

        Force::Broadcast(self.playback.clone())
    }

    pub fn add(&mut self, name: &str, outbound: mpsc::Sender<String>) {
        self.insert(User {
            name: name.to_owned(),
            file: None,
            is_ready: false,
            is_controller: false,
            position: None,
            outbound,
        });
    }

    pub fn insert(&mut self, user: User) {
        self.users.insert(user.name.clone(), user);
    }

    /// Removes a user and hands them back, so a room change can carry them
    /// across without losing their file, readiness or connection.
    pub fn remove(&mut self, name: &str) -> Option<User> {
        let user = self.users.remove(name);

        // An empty room forgets where it was. The next party in a room of the
        // same name starts from the beginning rather than halfway through
        // whatever the last one was watching.
        if self.users.is_empty() {
            self.playback = PlaybackState::default();
            self.last_update = Instant::now();
        }

        user
    }
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;

    /// A sender whose receiver is dropped immediately.
    ///
    /// Fine for the tests in this task, which never send: they are about who is
    /// in which room, not about delivery. The tasks that do exercise delivery
    /// hold on to the receiving half instead.
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

    /// An instant within a second of the room's last update, so `position_at`
    /// does not take the slowest-watcher branch.
    fn now() -> Instant {
        Instant::now()
    }

    /// An instant far enough past the room's last update that the reading is
    /// stale and the slowest-watcher branch is the one under test.
    fn stale() -> Instant {
        Instant::now() + Duration::from_secs(2)
    }

    fn five_seconds_on() -> Instant {
        Instant::now() + Duration::from_secs(5)
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

    fn playing_room_with(positions: &[(&str, f64)]) -> Room {
        let mut room = Room::new("MovieNight");
        for (name, position) in positions {
            room.add(name, sender());
            room.set_file(name, Some(watched_file()));
            room.set_position(name, *position);
        }
        room.force_paused(false);
        room
    }

    #[test]
    fn a_rooms_position_is_its_slowest_watcher() {
        let room = playing_room_with(&[("ahmet", 120.0), ("mehmet", 95.0)]);

        assert!(
            (room.position_at(stale()) - 95.0).abs() < 0.01,
            "nobody may get ahead of the person furthest behind"
        );
    }

    #[test]
    fn somebody_with_no_file_open_does_not_drag_the_room_back() {
        let mut room = playing_room_with(&[("ahmet", 120.0)]);
        room.add("newcomer", sender());

        assert!(
            (room.position_at(stale()) - 120.0).abs() < 0.01,
            "a watcher with nothing open is not a candidate for the minimum"
        );
    }

    /// Split from the plan's single test, which asserted that a room paused
    /// after five seconds of play still reads 5.0. It cannot: `force_paused`
    /// takes no `now`, so it cannot capture the position at the moment of the
    /// pause, and microseconds pass between the two calls in real time. What
    /// the spec actually says is that a sampleless room advances by elapsed
    /// time *only while playing*, which is these two tests.
    #[test]
    fn a_room_with_no_sample_advances_by_elapsed_time_while_playing() {
        let mut room = Room::new("MovieNight");
        room.force_paused(false);

        assert!((room.position_at(five_seconds_on()) - 5.0).abs() < 0.01);
    }

    #[test]
    fn a_paused_room_with_no_sample_does_not_move() {
        let room = Room::new("MovieNight");

        assert!(
            room.position_at(five_seconds_on()).abs() < 0.01,
            "a paused room does not move"
        );
    }

    #[test]
    fn a_pause_is_a_decision_and_is_broadcast() {
        let mut room = playing_room_with(&[("ahmet", 10.0), ("mehmet", 10.0)]);

        let force = room.apply("ahmet", pause_at(10.0), Duration::ZERO, now());

        let Force::Broadcast(state) = force else {
            panic!("a pause change must be broadcast, got {force:?}");
        };
        assert!(state.paused);
        assert_eq!(state.set_by.as_deref(), Some("ahmet"));
    }

    #[test]
    fn drift_alone_is_never_forced() {
        let mut room = playing_room_with(&[("ahmet", 10.0), ("mehmet", 10.0)]);

        let force = room.apply("mehmet", playing_at(70.0), Duration::ZERO, now());

        assert!(
            matches!(force, Force::Nothing),
            "drift is the client's to correct, got {force:?}"
        );
    }

    #[test]
    fn an_explicit_seek_is_broadcast_without_a_pause_change() {
        let mut room = playing_room_with(&[("ahmet", 10.0), ("mehmet", 10.0)]);

        let force = room.apply("ahmet", seek_to(600.0), Duration::ZERO, now());

        let Force::Broadcast(state) = force else {
            panic!("doSeek must be broadcast, got {force:?}");
        };
        assert!((state.position - 600.0).abs() < 0.01);
    }

    #[test]
    fn a_stale_report_is_advanced_by_its_own_age_while_playing() {
        let mut room = playing_room_with(&[("ahmet", 10.0), ("mehmet", 10.0)]);

        room.apply("ahmet", seek_to(100.0), Duration::from_secs(2), now());

        assert!(
            (room.playback().position - 102.0).abs() < 0.01,
            "the report was already two seconds old when it arrived"
        );
    }

    #[test]
    fn a_stale_report_is_not_advanced_while_paused() {
        let mut room = playing_room_with(&[("ahmet", 10.0)]);

        room.apply("ahmet", pause_at(100.0), Duration::from_secs(2), now());

        assert!((room.playback().position - 100.0).abs() < 0.01);
    }

    #[test]
    fn a_non_controller_is_corrected_with_both_messages() {
        let mut room = Room::new("+MovieNight:ABCDEF012345");
        room.add("ahmet", sender());
        room.set_file("ahmet", Some(watched_file()));
        room.force_paused(false);

        let force = room.apply("ahmet", pause_at(10.0), Duration::ZERO, now());

        let Force::CorrectSender { echo, real } = force else {
            panic!("a non-controller must be corrected, got {force:?}");
        };
        assert!(echo.paused, "the first message echoes what they asked for");
        assert!(!real.paused, "the second carries the room's real state");
        assert!(!room.playback().paused, "the room must not have moved");
    }

    #[test]
    fn a_controller_moves_a_controlled_room() {
        let mut room = Room::new("+MovieNight:ABCDEF012345");
        room.add("ahmet", sender());
        room.set_file("ahmet", Some(watched_file()));
        room.set_controller("ahmet", true);
        room.force_paused(false);

        let force = room.apply("ahmet", pause_at(10.0), Duration::ZERO, now());

        assert!(matches!(force, Force::Broadcast(_)));
        assert!(room.playback().paused);
    }

    #[test]
    fn a_non_controllers_own_position_is_still_recorded() {
        let mut room = Room::new("+MovieNight:ABCDEF012345");
        room.add("ahmet", sender());
        room.set_file("ahmet", Some(watched_file()));

        room.apply("ahmet", playing_at(42.0), Duration::ZERO, now());

        assert!(
            (room.user_position("ahmet").expect("position") - 42.0).abs() < 0.01,
            "only the room-level state is refused, not the report itself"
        );
    }
}
