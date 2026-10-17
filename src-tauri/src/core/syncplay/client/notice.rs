//! What the sync client tells the viewer over the picture.
//!
//! The engine decides that something is worth saying and hands back a
//! [`Notice`]; turning it into words happens at the edge, where the language
//! setting is. That keeps the engine's tests about what happened rather than
//! about phrasing.
//!
//! Slowing down to catch up is deliberately never announced: it happens
//! often, it is meant to go unnoticed, and upstream keeps it off the screen
//! for the same reason.

use std::time::Duration;

/// How long a notice stays up. Upstream's `OSD_DURATION`.
pub const NOTICE_DURATION: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    Connected,
    Reconnecting,
    Joined {
        user: String,
    },
    Left {
        user: String,
    },
    /// `user` is `None` when the server did not say who.
    Paused {
        user: Option<String>,
        at: f64,
    },
    Unpaused {
        user: Option<String>,
    },
    Seeked {
        user: Option<String>,
        from: f64,
        to: f64,
    },
    Rewound {
        user: Option<String>,
    },
    FastForwarded {
        user: Option<String>,
    },
    /// The unpause was turned into "ready" because somebody else is not.
    ReadyToUnpause,
    FileDiffers {
        user: String,
    },
}

impl Notice {
    pub fn render(&self, language: &str) -> String {
        if is_turkish(language) {
            self.turkish()
        } else {
            self.english()
        }
    }

    fn english(&self) -> String {
        let who = |user: &Option<String>| user.clone().unwrap_or_else(|| "Someone".to_owned());

        match self {
            Self::Connected => "Connected to the party".to_owned(),
            Self::Reconnecting => "Lost the connection to the party, reconnecting…".to_owned(),
            Self::Joined { user } => format!("{user} joined"),
            Self::Left { user } => format!("{user} left"),
            Self::Paused { user, at } => format!("{} paused at {}", who(user), format_time(*at)),
            Self::Unpaused { user } => format!("{} unpaused", who(user)),
            Self::Seeked { user, from, to } => format!(
                "{} jumped from {} to {}",
                who(user),
                format_time(*from),
                format_time(*to)
            ),
            Self::Rewound { user } => {
                format!("Rewound due to a time difference with {}", who(user))
            }
            Self::FastForwarded { user } => {
                format!("Fast-forwarded due to a time difference with {}", who(user))
            }
            Self::ReadyToUnpause => {
                "You are now set as ready — unpause again to unpause".to_owned()
            }
            Self::FileDiffers { user } => format!("Your file differs from {user}'s"),
        }
    }

    fn turkish(&self) -> String {
        let who = |user: &Option<String>| user.clone().unwrap_or_else(|| "Biri".to_owned());

        match self {
            Self::Connected => "Partiye bağlanıldı".to_owned(),
            Self::Reconnecting => "Partiyle bağlantı koptu, yeniden bağlanılıyor…".to_owned(),
            Self::Joined { user } => format!("{user} katıldı"),
            Self::Left { user } => format!("{user} ayrıldı"),
            Self::Paused { user, at } => {
                format!("{} {} konumunda duraklattı", who(user), format_time(*at))
            }
            Self::Unpaused { user } => format!("{} devam ettirdi", who(user)),
            Self::Seeked { user, from, to } => format!(
                "{} {} konumundan {} konumuna atladı",
                who(user),
                format_time(*from),
                format_time(*to)
            ),
            Self::Rewound { user } => {
                format!("{} ile zaman farkı nedeniyle geri sarıldı", who(user))
            }
            Self::FastForwarded { user } => {
                format!("{} ile zaman farkı nedeniyle ileri sarıldı", who(user))
            }
            Self::ReadyToUnpause => {
                "Artık hazırsınız — oynatmak için yeniden devam ettirin".to_owned()
            }
            Self::FileDiffers { user } => {
                format!("Dosyanız {user} kullanıcısının dosyasından farklı")
            }
        }
    }
}

/// Matches `tr` and any regional variant such as `tr-TR`, the same rule the
/// Discord messages follow.
fn is_turkish(language: &str) -> bool {
    language.split(['-', '_']).next().unwrap_or(language) == "tr"
}

/// `mm:ss`, or `hh:mm:ss` from an hour on, rounded to the second the way
/// upstream rounds it.
fn format_time(seconds: f64) -> String {
    let sign = if seconds < 0.0 { "-" } else { "" };
    let total = seconds.abs().round() as u64;
    let (hours, minutes, seconds) = (total / 3600, total % 3600 / 60, total % 60);

    if hours > 0 {
        format!("{sign}{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{sign}{minutes:02}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_read_as_minutes_until_there_is_an_hour() {
        assert_eq!(format_time(0.0), "00:00");
        assert_eq!(format_time(85.4), "01:25");
        assert_eq!(format_time(59.6), "01:00");
        assert_eq!(format_time(3725.0), "01:02:05");
        assert_eq!(format_time(-5.0), "-00:05");
    }

    #[test]
    fn turkish_is_chosen_for_any_turkish_locale() {
        let notice = Notice::Unpaused {
            user: Some("ahmet".to_owned()),
        };

        assert_eq!(notice.render("tr"), "ahmet devam ettirdi");
        assert_eq!(notice.render("tr-TR"), "ahmet devam ettirdi");
        assert_eq!(notice.render("en"), "ahmet unpaused");
        assert_eq!(notice.render(""), "ahmet unpaused");
    }

    #[test]
    fn a_pause_says_who_and_where() {
        let notice = Notice::Paused {
            user: Some("ahmet".to_owned()),
            at: 85.0,
        };

        assert_eq!(notice.render("en"), "ahmet paused at 01:25");
        assert_eq!(notice.render("tr"), "ahmet 01:25 konumunda duraklattı");
    }

    #[test]
    fn a_jump_says_from_where_to_where() {
        let notice = Notice::Seeked {
            user: Some("ahmet".to_owned()),
            from: 10.0,
            to: 50.0,
        };

        assert_eq!(notice.render("en"), "ahmet jumped from 00:10 to 00:50");
        assert_eq!(
            notice.render("tr"),
            "ahmet 00:10 konumundan 00:50 konumuna atladı"
        );
    }

    #[test]
    fn nobody_in_particular_is_somebody() {
        let notice = Notice::Rewound { user: None };

        assert_eq!(
            notice.render("en"),
            "Rewound due to a time difference with Someone"
        );
        assert_eq!(
            notice.render("tr"),
            "Biri ile zaman farkı nedeniyle geri sarıldı"
        );
    }

    #[test]
    fn every_notice_has_both_languages() {
        let user = Some("ahmet".to_owned());
        let notices = [
            Notice::Connected,
            Notice::Reconnecting,
            Notice::Joined {
                user: "ahmet".to_owned(),
            },
            Notice::Left {
                user: "ahmet".to_owned(),
            },
            Notice::Paused {
                user: user.clone(),
                at: 1.0,
            },
            Notice::Unpaused { user: user.clone() },
            Notice::Seeked {
                user: user.clone(),
                from: 1.0,
                to: 2.0,
            },
            Notice::Rewound { user: user.clone() },
            Notice::FastForwarded { user },
            Notice::ReadyToUnpause,
            Notice::FileDiffers {
                user: "ahmet".to_owned(),
            },
        ];

        for notice in notices {
            let english = notice.render("en");
            let turkish = notice.render("tr");
            assert!(!english.is_empty() && !turkish.is_empty(), "{notice:?}");
            assert_ne!(english, turkish, "{notice:?} is not translated");
        }
    }
}
