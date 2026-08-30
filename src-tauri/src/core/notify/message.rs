//! The text syncparty posts to Discord.
//!
//! Written in the host's own language, because the people reading it are the
//! host's friends rather than the host.

use serde_json::{json, Value};

use crate::core::invite::Invite;

/// The "we're live" announcement, including the one-click join link.
pub fn party_ready(invite: &Invite, language: &str) -> String {
    if is_turkish(language) {
        format!(
            "🎬 **Film gecesi hazır!**\n\n\
             **Tek tıkla katıl:** {link}\n\
             **Davet kodu:** `{code}`\n\n\
             **Oda**\n\
             Oda adı: `{room}`\n\n\
             **İlk kez katılacaklar**\n\
             1. syncparty'yi kurun ve davet bağlantısına tıklayın — gerisini o halleder.\n\
             2. Hesap açmanız, ağ kurmanız veya port açmanız gerekmiyor.\n\n\
             Film dosyası herkeste yerel olarak bulunmalı; dosya internetten yayınlanmıyor.",
            link = invite.deep_link(),
            code = invite.encode(),
            room = invite.room,
        )
    } else {
        format!(
            "🎬 **Movie night is up!**\n\n\
             **One-click join:** {link}\n\
             **Invite code:** `{code}`\n\n\
             **Room**\n\
             Room name: `{room}`\n\n\
             **First time joining**\n\
             1. Install syncparty and open the invite link — it handles the rest.\n\
             2. There is no account to create, no network to join and no port to open.\n\n\
             Everyone needs their own copy of the file locally — nothing is streamed.",
            link = invite.deep_link(),
            code = invite.encode(),
            room = invite.room,
        )
    }
}

pub fn party_stopped(language: &str) -> String {
    if is_turkish(language) {
        "🛑 **Film gecesi sunucusu kapatıldı.** Görüşmek üzere!".to_owned()
    } else {
        "🛑 **Movie night server is down.** See you next time!".to_owned()
    }
}

pub fn webhook_test(language: &str) -> String {
    if is_turkish(language) {
        "✅ syncparty Discord bağlantısı çalışıyor.".to_owned()
    } else {
        "✅ syncparty is connected to this channel.".to_owned()
    }
}

/// `candidate_count` is announced rather than the titles themselves — the
/// channel message is a heads-up to go vote, not a spoiler of the options.
pub fn movie_vote_started(candidate_count: usize, language: &str) -> String {
    if is_turkish(language) {
        format!("🍿 **Film oylaması başladı!** {candidate_count} film arasından seçim yapılıyor.")
    } else {
        format!("🍿 **Movie vote is open!** Choosing between {candidate_count} movies.")
    }
}

/// One movie, reduced to what a channel card can show: what it is, when it
/// came out, and how it scored.
pub struct PosterCard<'a> {
    pub title: &'a str,
    pub poster: Option<&'a str>,
    pub release_date: Option<&'a str>,
    pub rating: f64,
}

impl PosterCard<'_> {
    fn line(&self) -> String {
        let year = self
            .release_date
            .and_then(|date| date.get(..4))
            .unwrap_or("—");
        format!("**{}** · {year} · ★ {:.1}", self.title, self.rating)
    }
}

/// Discord's accent bar, as the app's own magenta.
const EMBED_COLOUR: u32 = 0xE8_4C_9A;
const EMBED_COLOUR_WINNER: u32 = 0x4A_D9_95;

/// The ballot as a card: every candidate listed, with the first one's poster
/// as the thumbnail. A line of text saying "5 movies" told nobody which five,
/// which is the thing that decides whether anyone opens the app to vote.
pub fn movie_vote_started_card(candidates: &[PosterCard<'_>], language: &str) -> Value {
    let turkish = is_turkish(language);
    let listing: Vec<String> = candidates.iter().map(PosterCard::line).collect();

    let mut embed = json!({
        "title": if turkish { "🍿 Film oylaması başladı" } else { "🍿 Movie vote is open" },
        "description": listing.join("
    "),
        "color": EMBED_COLOUR,
        "footer": {
            "text": if turkish {
                format!("{} film arasından seçim yapılıyor", candidates.len())
            } else {
                format!("Choosing between {} movies", candidates.len())
            }
        },
    });

    // The first candidate's artwork, purely so the card is not a wall of
    // text — there is no "main" candidate before anyone has voted.
    if let Some(poster) = candidates.first().and_then(|card| card.poster) {
        embed["thumbnail"] = json!({ "url": poster });
    }

    json!({ "embeds": [embed] })
}

/// The winner, with its poster at full width — this is the one announcement
/// worth the space, and the only one people will scroll back to find.
pub fn movie_selected_card(card: &PosterCard<'_>, language: &str) -> Value {
    let turkish = is_turkish(language);

    let mut embed = json!({
        "title": if turkish { "🎬 Bu akşamın filmi" } else { "🎬 Tonight's movie" },
        "description": card.line(),
        "color": EMBED_COLOUR_WINNER,
    });

    if let Some(poster) = card.poster {
        embed["image"] = json!({ "url": poster });
    }

    json!({ "embeds": [embed] })
}

pub fn movie_vote_cancelled(language: &str) -> String {
    if is_turkish(language) {
        "🚫 **Film oylaması iptal edildi.**".to_owned()
    } else {
        "🚫 **The movie vote was cancelled.**".to_owned()
    }
}

pub fn movie_vote_completed(language: &str) -> String {
    if is_turkish(language) {
        "🗳️ **Film oylaması kapandı.**".to_owned()
    } else {
        "🗳️ **Voting has closed.**".to_owned()
    }
}

pub fn movie_selected(title: &str, language: &str) -> String {
    if is_turkish(language) {
        format!("🎬 **Bu akşamın filmi belli oldu: {title}**")
    } else {
        format!("🎬 **Tonight's movie is {title}!**")
    }
}

/// Matches `tr` and any regional variant such as `tr-TR`.
fn is_turkish(language: &str) -> bool {
    language.split(['-', '_']).next().unwrap_or(language) == "tr"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Invite {
        Invite {
            endpoint: iroh::SecretKey::generate().public().to_string(),
            password: "swordfish".to_owned(),
            room: "MovieNight".to_owned(),
        }
    }

    #[test]
    fn recognises_turkish_including_regional_tags() {
        assert!(is_turkish("tr"));
        assert!(is_turkish("tr-TR"));
        assert!(is_turkish("tr_TR"));
        assert!(!is_turkish("en"));
        assert!(!is_turkish("en-GB"));
    }

    #[test]
    fn the_announcement_carries_everything_a_guest_needs() {
        let invite = sample();

        for language in ["tr", "en"] {
            let message = party_ready(&invite, language);

            assert!(message.contains(&invite.deep_link()), "{language}");
            assert!(message.contains(&invite.encode()), "{language}");
            assert!(message.contains(&invite.room), "{language}");
        }
    }

    #[test]
    fn the_announcement_does_not_put_the_password_in_the_channel() {
        // It travels inside the code, which is the thing people paste into
        // syncparty. Printing it beside the code adds nothing and leaves the
        // party's one secret sitting in plain sight in a chat log.
        let invite = sample();

        for language in ["tr", "en"] {
            let message = party_ready(&invite, language);

            assert!(!message.contains(&invite.password), "{language}");
        }
    }

    #[test]
    fn falls_back_to_english_for_an_unknown_language() {
        assert_eq!(party_stopped("de"), party_stopped("en"));
    }

    #[test]
    fn the_movie_vote_started_message_names_the_candidate_count() {
        assert!(movie_vote_started(5, "en").contains('5'));
        assert!(movie_vote_started(5, "tr").contains('5'));
    }

    #[test]
    fn the_movie_selected_message_names_the_winner() {
        for language in ["tr", "en"] {
            assert!(movie_selected("Interstellar", language).contains("Interstellar"));
        }
    }
}
