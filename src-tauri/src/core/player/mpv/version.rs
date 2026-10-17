//! Which mpv is at the other end, from what its `mpv-version` says.
//!
//! The property rather than `mpv --version`: it needs no second process, and
//! mpv.net answers it like any mpv while its `--version` says something else.

/// The oldest mpv syncparty drives. 0.33 is where `loadfile` and every
/// `start-file` began naming their playlist entry, which is what tells a
/// film syncparty opened apart from one dropped on the window; the oldest
/// distributions still supported ship newer (Ubuntu 22.04 has 0.34, Debian
/// 12 has 0.35). Older ones are refused with their version rather than left
/// to fail at the first option or reply they do not understand.
pub(super) const MINIMUM: (u32, u32) = (0, 33);

/// The major and minor version in what `mpv-version` says — "mpv v0.41.0",
/// "mpv 0.35.1-dirty", "mpv v0.38.0-575-g6d7f8b2c4" — or `None` for anything
/// else, such as a git build named after a date.
pub(super) fn parse(text: &str) -> Option<(u32, u32)> {
    let rest = text.trim().strip_prefix("mpv")?.trim_start();
    let rest = rest.strip_prefix('v').unwrap_or(rest);

    let (major, rest) = leading_number(rest)?;
    let (minor, _) = leading_number(rest.strip_prefix('.')?)?;
    Some((major, minor))
}

/// Whether mpv says it is older than syncparty can drive. An unreadable
/// version is not: refusing it would refuse an mpv that may well be new.
pub(super) fn too_old(text: &str) -> bool {
    parse(text).is_some_and(|found| found < MINIMUM)
}

fn leading_number(text: &str) -> Option<(u32, &str)> {
    let end = text
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(text.len());
    let number = text[..end].parse().ok()?;
    Some((number, &text[end..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_distribution_and_git_builds_all_give_their_version() {
        assert_eq!(parse("mpv v0.41.0"), Some((0, 41)));
        assert_eq!(parse("mpv 0.35.1-dirty"), Some((0, 35)));
        assert_eq!(parse("mpv v0.38.0-575-g6d7f8b2c4"), Some((0, 38)));
        assert_eq!(
            parse("mpv 0.32.0+git20200301.e9f5d05+dfsg-1"),
            Some((0, 32))
        );
        assert_eq!(parse("mpv 1.2"), Some((1, 2)));
    }

    #[test]
    fn a_version_that_cannot_be_read_is_none() {
        for unreadable in [
            "",
            "mpv",
            "mpv git-2019-05-01",
            "mpv 0",
            "mpv v.41",
            "vlc 3.0.21",
        ] {
            assert_eq!(parse(unreadable), None, "{unreadable}");
        }
    }

    #[test]
    fn only_a_version_known_to_be_below_the_floor_is_too_old() {
        assert!(too_old("mpv 0.32.0"));
        assert!(too_old("mpv v0.29.1"));
        assert!(!too_old("mpv 0.33.0"));
        assert!(!too_old("mpv v0.41.0"));
        assert!(!too_old("mpv v1.0.0"));
        assert!(!too_old("mpv git-2019-05-01"));
    }
}
