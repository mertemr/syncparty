//! Passwords for controlled rooms.
//!
//! Syncplay never stores an operator password. A controlled room's *name*
//! carries a hash of the room, the server salt and the password, so checking a
//! password means recomputing the name and comparing. That is the mechanism the
//! README's warning about the salt is protecting: a new salt does not produce
//! an error, it produces a different name, and every operator password already
//! in circulation quietly stops working.
//!
//! The chain below is `RoomPasswordProvider._computeRoomHash` from the Syncplay
//! 1.7.x source, and every expected value in the tests was produced by running
//! that function rather than reasoned about here.

use sha1::Sha1;
use sha2::{Digest, Sha256};

/// Characters of the hash that end a controlled room name.
const HASH_LENGTH: usize = 12;

/// Builds the controlled room name for a password, which is also how a
/// password is checked — there is nothing else to compare against.
///
/// Nothing in production calls this yet: syncparty names its own rooms and has
/// no flow for turning one into a controlled room. It is the other half of
/// `check_controlled_room` and the only way to produce a name that function
/// will accept, so it stays.
#[allow(dead_code)]
pub fn controlled_room_name(room: &str, password: &str, salt: &str) -> String {
    format!("+{room}:{}", compute_room_hash(room, password, salt))
}

/// Whether a room name has the shape only this module produces.
pub fn is_controlled_room(room: &str) -> bool {
    split_controlled(room).is_some()
}

/// Whether `password` opens `room`.
///
/// A malformed password is simply refused. Syncplay raises on one, but there is
/// nothing here a caller could do differently: the answer to "may this user
/// control the room" is no either way.
pub fn check_controlled_room(room: &str, password: &str, salt: &str) -> bool {
    if !password_has_expected_shape(password) {
        return false;
    }

    let Some((base, hash)) = split_controlled(room) else {
        return false;
    };

    compute_room_hash(base, password, salt) == hash
}

/// Splits `+<name>:<hash>` into its two halves, or `None` if it is not one.
///
/// The regex upstream is `^\+(.*):(\w{12})$`, whose greedy `(.*)` puts the
/// separator at the *last* colon leaving twelve word characters behind it. An
/// earlier colon can never match instead, because the tail would then contain a
/// colon, which is not a word character.
fn split_controlled(room: &str) -> Option<(&str, &str)> {
    let rest = room.strip_prefix('+')?;
    let separator = rest.rfind(':')?;
    let (base, hash) = (&rest[..separator], &rest[separator + 1..]);

    let looks_like_a_hash =
        hash.chars().count() == HASH_LENGTH && hash.chars().all(is_word_character);

    looks_like_a_hash.then_some((base, hash))
}

/// Python's `\w`, which is Unicode-aware for `str` patterns.
fn is_word_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// Upstream's `PASSWORD_REGEX`, `[A-Z]{2}-\d{3}-\d{3}`, applied with
/// `re.match` — anchored at the start only, so trailing text is tolerated.
fn password_has_expected_shape(password: &str) -> bool {
    let mut characters = password.chars();
    let mut next_is = |expected: fn(char) -> bool| characters.next().is_some_and(expected);

    next_is(|c| c.is_ascii_uppercase())
        && next_is(|c| c.is_ascii_uppercase())
        && next_is(|c| c == '-')
        && next_is(|c| c.is_ascii_digit())
        && next_is(|c| c.is_ascii_digit())
        && next_is(|c| c.is_ascii_digit())
        && next_is(|c| c == '-')
        && next_is(|c| c.is_ascii_digit())
        && next_is(|c| c.is_ascii_digit())
        && next_is(|c| c.is_ascii_digit())
}

/// The hash chain, in the order upstream performs it.
///
/// The subtle step is the third argument to the SHA-1: upstream rebinds `salt`
/// to its own hex digest before that line, so what is concatenated there is the
/// *hashed* salt, not the one passed in. Using the raw salt produces a
/// perfectly plausible hash that no Syncplay client will ever agree with.
fn compute_room_hash(room: &str, password: &str, salt: &str) -> String {
    let salt = sha256_hex(salt.as_bytes());
    let provisional = sha256_hex(&[room.as_bytes(), salt.as_bytes()].concat());

    let full = sha1_hex(&[provisional.as_bytes(), salt.as_bytes(), password.as_bytes()].concat());

    // Hex is ASCII, so slicing by bytes is slicing by characters here.
    full[..HASH_LENGTH].to_uppercase()
}

fn sha256_hex(input: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input);
    to_hex(&hasher.finalize())
}

fn sha1_hex(input: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(input);
    to_hex(&hasher.finalize())
}

fn to_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            use std::fmt::Write;
            let _ = write!(out, "{byte:02x}");
            out
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference names from `RoomPasswordProvider.getControlledRoomName` in the
    /// pinned 1.7.5 checkout, captured by running it. These are the whole point
    /// of the module: an implementation that disagrees with them is one that
    /// silently invalidates operator passwords people already hold.
    #[test]
    fn derives_the_controlled_room_name_the_way_syncplay_does() {
        assert_eq!(
            controlled_room_name("MovieNight", "AB-123-456", "PEPPER"),
            "+MovieNight:41FE629E81EC"
        );
        assert_eq!(
            controlled_room_name("MovieNight", "AB-123-457", "PEPPER"),
            "+MovieNight:D8190FD38205"
        );
        assert_eq!(
            controlled_room_name("MovieNight", "AB-123-456", "OTHERSALT"),
            "+MovieNight:3C678103C6A8"
        );
        assert_eq!(
            controlled_room_name("", "AB-123-456", "PEPPER"),
            "+:2D2066EFB84C"
        );
    }

    /// The room name is hashed as UTF-8. A room called something Turkish is not
    /// an edge case here, it is Tuesday, and encoding it any other way would
    /// break exactly the users this app was written for.
    #[test]
    fn hashes_a_non_ascii_room_name_as_utf8() {
        assert_eq!(
            controlled_room_name("film odası", "ZZ-999-000", "PEPPER"),
            "+film odası:E135E1B0A5EE"
        );
    }

    #[test]
    fn recognises_a_controlled_room_by_its_shape() {
        assert!(is_controlled_room("+MovieNight:ABCDEF012345"));
        assert!(!is_controlled_room("MovieNight"));
        assert!(
            !is_controlled_room("+MovieNight:TOOSHORT"),
            "the hash is exactly twelve characters"
        );
        assert!(
            !is_controlled_room("+MovieNight"),
            "a name with no separator is not controlled"
        );
    }

    /// Greedy `(.*)` upstream means the separator is the last colon, so a room
    /// whose own name contains one still resolves.
    #[test]
    fn a_room_name_may_itself_contain_a_colon() {
        let name = controlled_room_name("part 1: the beginning", "AB-123-456", "PEPPER");

        assert!(is_controlled_room(&name));
        assert!(check_controlled_room(&name, "AB-123-456", "PEPPER"));
    }

    #[test]
    fn accepts_only_the_password_the_room_name_was_built_from() {
        let room = controlled_room_name("MovieNight", "AB-123-456", "PEPPER");

        assert!(check_controlled_room(&room, "AB-123-456", "PEPPER"));
        assert!(!check_controlled_room(&room, "AB-123-457", "PEPPER"));
        assert!(
            !check_controlled_room(&room, "AB-123-456", "OTHERSALT"),
            "a different salt must not open a room"
        );
    }

    #[test]
    fn refuses_a_password_that_is_not_shaped_like_one() {
        let room = controlled_room_name("MovieNight", "AB-123-456", "PEPPER");

        assert!(!check_controlled_room(&room, "swordfish", "PEPPER"));
        assert!(!check_controlled_room(&room, "", "PEPPER"));
        assert!(!check_controlled_room(&room, "ab-123-456", "PEPPER"));
    }

    #[test]
    fn an_uncontrolled_room_is_never_opened_by_any_password() {
        assert!(!check_controlled_room("MovieNight", "AB-123-456", "PEPPER"));
    }
}
