//! Every room, and who is in which.
//!
//! Room isolation is structural here rather than a filter applied at the edge:
//! [`Registry::visible_list`] can only ever return the caller's own room, so
//! there is no code path that could forget to apply it. The server is always
//! started with isolation on today, and this makes that the only shape it has.

use std::collections::HashMap;
use std::sync::Arc;

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
    pub fn move_to(&mut self, user: &str, room: &str) {
        let Some(carried) = self.detach(user) else {
            return;
        };

        self.room_or_create(room).insert(carried);
        self.where_is.insert(user.to_owned(), room.to_owned());
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

        registry.move_to("ahmet", "OtherRoom");

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
        user.is_ready = true;

        registry.move_to("ahmet", "OtherRoom");

        let moved = registry
            .room("OtherRoom")
            .expect("room")
            .users()
            .get("ahmet")
            .expect("user");
        assert_eq!(moved.file.as_ref().expect("file").name, "Film.mkv");
        assert!(moved.is_ready);
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
        registry.move_to("nobody", "OtherRoom");

        assert!(registry.room("OtherRoom").is_none());
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
