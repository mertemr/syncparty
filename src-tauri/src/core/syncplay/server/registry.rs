//! Every room, and who is in which.
//!
//! Room isolation is structural here rather than a filter applied at the edge:
//! [`Registry::visible_list`] can only ever return the caller's own room, so
//! there is no code path that could forget to apply it. The server is always
//! started with isolation on today, and this makes that the only shape it has.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{mpsc, RwLock};

use crate::core::syncplay::protocol::MAX_USERNAME_LENGTH;
use crate::core::syncplay::server::room::{Room, User};

#[derive(Default)]
pub struct Registry {
    rooms: HashMap<String, Room>,
    /// Username to room name, so leaving is one lookup rather than a scan of
    /// every room in the server.
    where_is: HashMap<String, String>,
}

impl Registry {
    /// The registry as every connection sees it: one, shared, behind a lock.
    pub fn shared() -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(Self::default()))
    }

    /// The name `wanted` may actually be given, which is not always the one
    /// asked for.
    ///
    /// Without this a second `ahmet` would not collide, they would *replace*
    /// the first: [`Registry::join`] is keyed by name and detaches whoever
    /// holds it. Upstream does this in `addWatcher`, and the shape of the
    /// result is theirs too — trailing underscores, compared case-insensitively
    /// across every room rather than just this one.
    pub fn free_username(&self, wanted: &str) -> String {
        let taken = |candidate: &str| {
            let candidate = candidate.to_lowercase();
            self.where_is
                .keys()
                .any(|held| held.to_lowercase() == candidate)
        };

        let mut name: String = wanted.chars().take(MAX_USERNAME_LENGTH).collect();

        // Trim before growing, or a room of retries ends up wearing ever
        // longer tails.
        if taken(&name) && name.ends_with('_') {
            let trimmed = name.trim_end_matches('_');
            name = if trimmed.is_empty() {
                "_".to_owned()
            } else {
                trimmed.to_owned()
            };
        }

        while taken(&name) {
            name.push('_');
        }

        name
    }

    pub fn join(&mut self, user: &str, room: &str, outbound: mpsc::Sender<String>) {
        self.detach(user);
        self.room_or_create(room).add(user, outbound);
        self.where_is.insert(user.to_owned(), room.to_owned());
    }

    /// Moves a user to another room, carrying their file, readiness and
    /// connection with them.
    ///
    /// Deliberately not remove-then-join: that would lose the file they already
    /// have open, and they would appear in the new room's list with nothing
    /// loaded until their client happened to mention it again.
    ///
    /// Two things are *not* carried. Controller status belongs to the room it
    /// was earned in: carried, it would let somebody who knows one room's
    /// operator password walk into any other controlled room and run it. And
    /// their position belongs to the film the old room was watching, so they
    /// start from wherever this room is — sampled before they arrive, so
    /// their old position cannot become this room's — or, in a room nobody is
    /// in yet, from no position at all.
    ///
    /// Returns that sample, the room's position at `now`, or `None` when
    /// `user` was not anywhere. It is the one reading the mover is told
    /// about: sampling again once they are in would find them a candidate and
    /// could name them as having set the room.
    pub fn move_to(&mut self, user: &str, room: &str, now: Instant) -> Option<f64> {
        let mut carried = self.detach(user)?;

        let destination = self.room_or_create(room);
        let position = destination.sample(now);
        carried.is_controller = false;
        carried.position = (!destination.is_empty()).then_some(position);
        carried.reported_at = now;
        carried.last_seen = now;

        destination.insert(carried);
        self.where_is.insert(user.to_owned(), room.to_owned());

        Some(position)
    }

    pub fn leave(&mut self, user: &str) {
        self.detach(user);
    }

    fn room_or_create(&mut self, name: &str) -> &mut Room {
        self.rooms
            .entry(name.to_owned())
            .or_insert_with(|| Room::new(name))
    }

    /// Takes a user out of whatever room they are in, dropping the room if that
    /// empties it. Returns them so a caller can put them somewhere else.
    fn detach(&mut self, user: &str) -> Option<User> {
        let room_name = self.where_is.remove(user)?;
        let room = self.rooms.get_mut(&room_name)?;

        let carried = room.remove(user);
        if room.is_empty() {
            self.rooms.remove(&room_name);
        }

        carried
    }

    /// Production reaches rooms through the people in them; only a test asks
    /// for one by name.
    #[cfg(test)]
    pub fn room(&self, name: &str) -> Option<&Room> {
        self.rooms.get(name)
    }

    pub fn room_mut(&mut self, name: &str) -> Option<&mut Room> {
        self.rooms.get_mut(name)
    }

    pub fn room_of(&self, user: &str) -> Option<&Room> {
        self.rooms.get(self.where_is.get(user)?)
    }

    pub fn room_of_mut(&mut self, user: &str) -> Option<&mut Room> {
        let name = self.where_is.get(user)?.clone();
        self.rooms.get_mut(&name)
    }

    /// What `user` is allowed to see: their own room and nothing else.
    ///
    /// Shaped as a map because that is what a `List` message is, so the
    /// serialiser walks this directly rather than re-deriving the isolation
    /// rule for itself.
    pub fn visible_list(&self, user: &str) -> HashMap<&str, &Room> {
        self.room_of(user)
            .map(|room| HashMap::from([(room.name(), room)]))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::core::syncplay::server::room::test_support::{open_file, sender};

    #[test]
    fn a_user_joining_an_unknown_room_creates_it() {
        let mut registry = Registry::default();
        registry.join("ahmet", "MovieNight", sender());

        assert_eq!(registry.room("MovieNight").expect("room").users().len(), 1);
    }

    #[test]
    fn the_last_user_leaving_removes_the_room() {
        let mut registry = Registry::default();
        registry.join("ahmet", "MovieNight", sender());

        registry.leave("ahmet");

        assert!(
            registry.room("MovieNight").is_none(),
            "an empty room is not a room"
        );
    }

    #[test]
    fn a_room_outlives_one_of_several_leaving() {
        let mut registry = Registry::default();
        registry.join("ahmet", "MovieNight", sender());
        registry.join("mehmet", "MovieNight", sender());

        registry.leave("ahmet");

        assert_eq!(registry.room("MovieNight").expect("room").users().len(), 1);
    }

    #[test]
    fn isolation_shows_a_user_only_their_own_room() {
        let mut registry = Registry::default();
        registry.join("ahmet", "MovieNight", sender());
        registry.join("mehmet", "OtherRoom", sender());

        let visible = registry.visible_list("ahmet");

        assert_eq!(visible.len(), 1, "room isolation is always on");
        assert!(visible.contains_key("MovieNight"));
        assert!(!visible.contains_key("OtherRoom"));
    }

    #[test]
    fn somebody_in_no_room_sees_nothing() {
        assert!(Registry::default().visible_list("nobody").is_empty());
    }

    #[test]
    fn moving_rooms_leaves_the_old_one_behind() {
        let mut registry = Registry::default();
        registry.join("ahmet", "MovieNight", sender());

        registry.move_to("ahmet", "OtherRoom", Instant::now());

        assert!(registry.room("MovieNight").is_none());
        assert_eq!(registry.room("OtherRoom").expect("room").users().len(), 1);
        assert_eq!(registry.room_of("ahmet").expect("room").name(), "OtherRoom");
    }

    /// The reason `move_to` carries the user rather than re-adding them.
    #[test]
    fn moving_rooms_carries_the_open_file_and_readiness() {
        let mut registry = Registry::default();
        registry.join("ahmet", "MovieNight", sender());

        let user = registry
            .room_mut("MovieNight")
            .expect("room")
            .user_mut("ahmet")
            .expect("user");
        user.file = Some(open_file("Film.mkv"));
        user.is_ready = Some(true);

        registry.move_to("ahmet", "OtherRoom", Instant::now());

        let moved = registry
            .room("OtherRoom")
            .expect("room")
            .users()
            .get("ahmet")
            .expect("user");
        assert_eq!(moved.file.as_ref().expect("file").name, "Film.mkv");
        assert_eq!(moved.is_ready, Some(true));
    }

    /// The privilege escalation this closes: an operator of one controlled
    /// room switching into another and finding themselves in charge of it.
    #[test]
    fn moving_rooms_leaves_controller_status_behind() {
        let mut registry = Registry::default();
        registry.join("ahmet", "+MovieNight:ABCDEF012345", sender());
        registry
            .room_mut("+MovieNight:ABCDEF012345")
            .expect("room")
            .set_controller("ahmet", true);

        registry.move_to("ahmet", "+OtherRoom:ABCDEF012345", Instant::now());

        let room = registry.room("+OtherRoom:ABCDEF012345").expect("room");
        assert!(!room.users()["ahmet"].is_controller);
        assert!(!room.can_control("ahmet"));
    }

    #[test]
    fn a_mover_starts_where_the_room_they_join_is() {
        let mut registry = Registry::default();
        let start = Instant::now();
        registry.join("ahmet", "MovieNight", sender());
        registry.join("mehmet", "OtherRoom", sender());
        for (room, name, position) in [
            ("MovieNight", "ahmet", 3000.0),
            ("OtherRoom", "mehmet", 40.0),
        ] {
            let room = registry.room_mut(room).expect("room");
            room.set_file(name, Some(open_file("Film.mkv")));
            room.set_position(name, position, start);
        }

        let told = registry.move_to("ahmet", "OtherRoom", start + Duration::from_secs(2));

        let moved = &registry.room("OtherRoom").expect("room").users()["ahmet"];
        assert_eq!(
            moved.position,
            Some(40.0),
            "the old film's position must not become a sample in the new room"
        );
        assert_eq!(told, Some(40.0), "and that is where he is told it is");
    }

    /// The room being joined holds only somebody with nothing open, as the
    /// host's room does: the room monitor never opens a file. The mover is
    /// the first candidate it has ever had.
    #[test]
    fn a_mover_is_not_named_as_who_set_the_room_they_join() {
        let mut registry = Registry::default();
        let start = Instant::now();
        registry.join("monitor", "OtherRoom", sender());
        registry.join("ahmet", "MovieNight", sender());
        let room = registry.room_mut("MovieNight").expect("room");
        room.set_file("ahmet", Some(open_file("Film.mkv")));
        room.set_position("ahmet", 3000.0, start);

        let later = start + Duration::from_secs(2);
        let told = registry.move_to("ahmet", "OtherRoom", later);

        assert_eq!(told, Some(0.0));
        let room = registry.room_mut("OtherRoom").expect("room");
        assert_eq!(
            room.sample(later),
            0.0,
            "in the same instant, the room is where he was told it is"
        );
        assert_ne!(room.playback().set_by.as_deref(), Some("ahmet"));
    }

    #[test]
    fn a_mover_into_an_empty_room_has_no_position_yet() {
        let mut registry = Registry::default();
        registry.join("ahmet", "MovieNight", sender());
        registry.room_mut("MovieNight").expect("room").set_position(
            "ahmet",
            3000.0,
            Instant::now(),
        );

        registry.move_to("ahmet", "OtherRoom", Instant::now());

        assert_eq!(
            registry.room("OtherRoom").expect("room").users()["ahmet"].position,
            None
        );
    }

    #[test]
    fn joining_a_second_room_is_a_move_rather_than_a_duplicate() {
        let mut registry = Registry::default();
        registry.join("ahmet", "MovieNight", sender());

        registry.join("ahmet", "OtherRoom", sender());

        assert!(
            registry.room("MovieNight").is_none(),
            "nobody may be in two rooms at once"
        );
        assert_eq!(registry.room_of("ahmet").expect("room").name(), "OtherRoom");
    }

    #[test]
    fn leaving_somebody_who_was_never_here_is_not_an_error() {
        let mut registry = Registry::default();

        registry.leave("nobody");
        let told = registry.move_to("nobody", "OtherRoom", Instant::now());

        assert!(registry.room("OtherRoom").is_none());
        assert_eq!(told, None, "nobody moved, so nobody is told anything");
    }

    #[test]
    fn a_name_nobody_holds_is_given_out_unchanged() {
        let mut registry = Registry::default();
        registry.join("ahmet", "MovieNight", sender());

        assert_eq!(registry.free_username("mehmet"), "mehmet");
    }

    /// The reason this exists: `join` is keyed by name, so without it the
    /// second arrival would evict the first rather than collide with them.
    #[test]
    fn a_name_somebody_holds_grows_until_it_is_free() {
        let mut registry = Registry::default();
        registry.join("ahmet", "MovieNight", sender());

        assert_eq!(registry.free_username("ahmet"), "ahmet_");
    }

    #[test]
    fn a_name_taken_in_another_room_still_collides() {
        let mut registry = Registry::default();
        registry.join("ahmet", "OtherRoom", sender());

        assert_eq!(
            registry.free_username("ahmet"),
            "ahmet_",
            "isolation hides rooms from users, not names from the server"
        );
    }

    #[test]
    fn names_collide_whatever_their_case() {
        let mut registry = Registry::default();
        registry.join("Ahmet", "MovieNight", sender());

        assert_eq!(registry.free_username("ahmet"), "ahmet_");
    }

    /// Upstream trims trailing underscores before growing again, so a room
    /// full of retries does not end up with ever longer tails.
    #[test]
    fn a_name_already_ending_in_underscore_is_trimmed_before_it_grows() {
        let mut registry = Registry::default();
        registry.join("ahmet", "MovieNight", sender());
        registry.join("ahmet_", "MovieNight", sender());

        assert_eq!(registry.free_username("ahmet_"), "ahmet__");
    }

    #[test]
    fn a_name_is_cut_to_the_length_we_advertise_before_it_is_checked() {
        let registry = Registry::default();

        assert_eq!(
            registry.free_username(&"a".repeat(40)).chars().count(),
            MAX_USERNAME_LENGTH,
            "the greeting claims a limit; the server has to keep it"
        );
    }
}
