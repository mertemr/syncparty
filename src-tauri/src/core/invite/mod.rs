//! Turning a party into one string a guest can act on.
//!
//! Everything a guest needs travels as a single token, so nobody has to copy
//! several values out of a chat message and retype them into a connection
//! dialog. The same token doubles as a `syncparty://` link, which opens the
//! app already filled in.
//!
//! Since the move off Tailscale an invite is markedly smaller. The host used
//! to have to advertise every address that might reach it — a masqueraded
//! share address, its own tailnet IP, its MagicDNS name — because which one
//! worked depended on which tailnet the guest happened to be on, something the
//! host could not know. An iroh endpoint id has no such problem: it is the
//! address everywhere, from every network, and it does not change when the
//! host's connection does. The port went the same way, since guests reach the
//! Syncplay server through the host's tunnel rather than by dialling it.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use iroh::EndpointId;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::core::error::{Result, SyncPartyError};
use crate::core::net;

/// URI scheme registered with the OS for one-click joining.
pub const DEEP_LINK_SCHEME: &str = "syncparty";

/// Token prefix. Versioned so a future format can be told apart from this one
/// instead of failing with a confusing parse error.
const TOKEN_PREFIX: &str = "SP2.";

/// The prefix used while parties ran over Tailscale. Recognised only so those
/// codes can be turned down with an explanation rather than "not valid".
const LEGACY_TOKEN_PREFIX: &str = "SP1.";

/// A party, in the form a guest receives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Invite {
    /// The host's iroh endpoint id — its public key, written out.
    ///
    /// This is the whole address. There is no host name, no IP and no port,
    /// because the transport resolves the id to whatever route works right
    /// now: a direct connection when hole punching succeeds, a relay when it
    /// does not.
    pub endpoint: String,
    pub password: String,
    pub room: String,
}

/// The on-the-wire payload. Keys are abbreviated because the whole thing ends
/// up base64-encoded in a chat message, and a shorter token is a friendlier
/// one to paste.
#[derive(Serialize, Deserialize)]
struct Payload {
    v: u8,
    e: String,
    pw: String,
    r: String,
}

impl Invite {
    /// Encodes the invite as a `SP2.…` token.
    pub fn encode(&self) -> String {
        let payload = Payload {
            v: 2,
            e: self.endpoint.clone(),
            pw: self.password.clone(),
            r: self.room.clone(),
        };

        // Serialising a struct we own cannot fail.
        let json = serde_json::to_vec(&payload).unwrap_or_default();
        format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(json))
    }

    /// Parses a token, a deep link, or a chat message containing either.
    ///
    /// Guests paste whatever they have — the bare token, the full link, the
    /// line of surrounding text — so this accepts all of it rather than
    /// asking them to trim it first.
    pub fn decode(input: &str) -> Result<Self> {
        let Some(token) = extract_token(input, TOKEN_PREFIX) else {
            // Worth telling apart: an old code is not a typo, and the guest
            // cannot fix it by pasting more carefully.
            if extract_token(input, LEGACY_TOKEN_PREFIX).is_some() {
                return Err(SyncPartyError::InvalidInvite(
                    "this code is from a version of syncparty that ran over Tailscale — ask the \
                     host for a new one"
                        .to_owned(),
                ));
            }

            return Err(SyncPartyError::InvalidInvite(
                "no invite code found".to_owned(),
            ));
        };

        let encoded = token.trim_start_matches(TOKEN_PREFIX);
        let json = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
            SyncPartyError::InvalidInvite("the code is not valid base64".to_owned())
        })?;

        let payload: Payload = serde_json::from_slice(&json)
            .map_err(|_| SyncPartyError::InvalidInvite("the code is damaged".to_owned()))?;

        if payload.v != 2 {
            return Err(SyncPartyError::InvalidInvite(format!(
                "this code was made by a newer version of syncparty (format {})",
                payload.v
            )));
        }

        if payload.e.is_empty() || payload.r.is_empty() {
            return Err(SyncPartyError::InvalidInvite(
                "the code is missing a host or a room".to_owned(),
            ));
        }

        let invite = Self {
            endpoint: payload.e,
            password: payload.pw,
            room: payload.r,
        };

        // Checked here rather than at dial time so a mistyped or truncated
        // code is rejected while the guest still has the chat message open,
        // instead of after they press Join.
        invite.endpoint_id()?;

        Ok(invite)
    }

    /// The host's endpoint id, parsed.
    pub fn endpoint_id(&self) -> Result<EndpointId> {
        net::parse_endpoint_id(&self.endpoint)
    }

    /// The clickable form: `syncparty://join/SP2.…`.
    pub fn deep_link(&self) -> String {
        format!("{DEEP_LINK_SCHEME}://join/{}", self.encode())
    }

    /// A short, human-comparable form of the host's address.
    ///
    /// Only for display — reading a full endpoint id aloud is hopeless, but
    /// the first characters are enough for two people to confirm over voice
    /// chat that they are talking about the same party.
    pub fn short_endpoint(&self) -> String {
        self.endpoint.chars().take(8).collect()
    }
}

/// Finds a token with the given prefix anywhere in `input`.
fn extract_token<'a>(input: &'a str, prefix: &str) -> Option<&'a str> {
    let start = input.find(prefix)?;
    let rest = &input[start..];

    // base64url plus the prefix's dot; anything else ends the token.
    let end = rest
        .char_indices()
        .position(|(index, character)| {
            index >= prefix.len()
                && !(character.is_ascii_alphanumeric() || character == '-' || character == '_')
        })
        .unwrap_or(rest.len());

    Some(&rest[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real endpoint id, so the parse check in `decode` is exercised rather
    /// than sidestepped by a placeholder that could never be dialled.
    fn endpoint() -> String {
        iroh::SecretKey::generate().public().to_string()
    }

    fn sample() -> Invite {
        Invite {
            endpoint: endpoint(),
            password: "swordfish".to_owned(),
            room: "MovieNight".to_owned(),
        }
    }

    #[test]
    fn survives_a_round_trip() {
        let invite = sample();

        assert_eq!(Invite::decode(&invite.encode()).expect("decode"), invite);
    }

    #[test]
    fn tokens_are_url_safe_so_they_survive_chat_apps() {
        let token = sample().encode();

        assert!(token.starts_with(TOKEN_PREFIX));
        assert!(token
            .trim_start_matches(TOKEN_PREFIX)
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn decodes_a_deep_link() {
        let invite = sample();

        assert_eq!(Invite::decode(&invite.deep_link()).expect("decode"), invite);
    }

    #[test]
    fn digs_the_token_out_of_a_chat_message() {
        let invite = sample();
        let message = format!(
            "hey everyone, film starts at 9 — join with {} (bring snacks)",
            invite.encode()
        );

        assert_eq!(Invite::decode(&message).expect("decode"), invite);
    }

    #[test]
    fn deep_links_use_the_registered_scheme() {
        assert!(sample().deep_link().starts_with("syncparty://join/SP2."));
    }

    #[test]
    fn rejects_input_with_no_code_in_it() {
        let error = Invite::decode("good evening").expect_err("no code");

        assert_eq!(error.kind(), "invalid_invite");
    }

    #[test]
    fn rejects_a_corrupted_code() {
        let error = Invite::decode("SP2.notrealbase64payload").expect_err("damaged");

        assert_eq!(error.kind(), "invalid_invite");
    }

    #[test]
    fn a_tailscale_era_code_says_so_instead_of_looking_like_a_typo() {
        // A real token from the last Tailscale release. Someone re-pasting an
        // old chat message must be told the format changed, not that they
        // copied it wrong.
        let legacy = "SP1.eyJ2IjoxLCJoIjoiMTAwLjEyNy4xNjcuNTYiLCJwIjo4OTk5LCJwdyI6IjdQWEpCQjZIWDQzaEoyYWpZWiIsInIiOiJNb3ZpZU5pZ2h0In0";

        let error = Invite::decode(legacy).expect_err("SP1 is no longer usable");

        assert_eq!(error.kind(), "invalid_invite");
        assert!(error.to_string().contains("Tailscale"), "{error}");
    }

    #[test]
    fn rejects_a_future_format_with_a_useful_message() {
        let payload = serde_json::to_vec(&Payload {
            v: 99,
            e: endpoint(),
            pw: String::new(),
            r: "room".to_owned(),
        })
        .expect("encode");
        let token = format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(payload));

        let error = Invite::decode(&token).expect_err("future format");
        assert!(error.to_string().contains("newer version"));
    }

    #[test]
    fn rejects_a_code_whose_endpoint_is_not_a_real_address() {
        // The failure a truncated paste produces. Caught at decode time so the
        // guest is told before they press Join, not after.
        let payload = serde_json::to_vec(&Payload {
            v: 2,
            e: "definitely-not-a-key".to_owned(),
            pw: String::new(),
            r: "room".to_owned(),
        })
        .expect("encode");
        let token = format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(payload));

        assert_eq!(
            Invite::decode(&token).expect_err("bad endpoint").kind(),
            "invalid_invite"
        );
    }

    #[test]
    fn rejects_a_code_missing_its_room() {
        let payload = serde_json::to_vec(&Payload {
            v: 2,
            e: endpoint(),
            pw: String::new(),
            r: String::new(),
        })
        .expect("encode");
        let token = format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(payload));

        assert!(Invite::decode(&token).is_err());
    }

    #[test]
    fn an_empty_password_round_trips() {
        let invite = Invite {
            password: String::new(),
            ..sample()
        };

        assert_eq!(Invite::decode(&invite.encode()).expect("decode"), invite);
    }

    #[test]
    fn the_endpoint_id_parses_back_to_the_key_it_came_from() {
        let key = iroh::SecretKey::generate();
        let invite = Invite {
            endpoint: key.public().to_string(),
            ..sample()
        };

        assert_eq!(invite.endpoint_id().expect("parse"), key.public());
    }

    #[test]
    fn the_short_form_is_readable_aloud_without_being_the_whole_key() {
        let invite = sample();
        let short = invite.short_endpoint();

        assert_eq!(short.len(), 8);
        assert!(invite.endpoint.starts_with(&short));
    }
}
