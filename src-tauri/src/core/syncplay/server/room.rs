//! One room: who is in it, and the playback state everyone is held to.
//!
//! Arbitration — deciding whether a client's report moves the room — lands here
//! in the next task. What this file establishes is the state that decision
//! reads, and the two facts about it that are easy to get wrong: a room starts
//! paused, and a room that empties forgets where it was.

use std::collections::HashMap;
use std::time::Instant;

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
    use super::test_support::{open_file, sender};
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
}
