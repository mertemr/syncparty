//! The peer-to-peer transport a party runs over.
//!
//! syncparty used to require everyone to be on the same Tailscale tailnet.
//! That meant an account, a system service, an admin prompt, and the host
//! inviting each guest into their network before the evening could start — all
//! of it in service of one thing: a route between two machines behind NAT.
//!
//! iroh provides that route on its own. Every machine holds an ed25519 key
//! pair and the public half *is* its address, so there is nothing to look up
//! and nothing to sign into. Connections are QUIC, encrypted end to end, and
//! established by hole punching wherever the network allows it. Where it does
//! not, they fall back to a relay that forwards ciphertext it cannot read.
//!
//! What crosses this transport is not video. Everyone still plays their own
//! local copy of the file; only Syncplay's control messages travel, which is a
//! few hundred bytes a second even in a busy room.

mod tunnel;

use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointId, SecretKey};

use crate::core::config::{SecretKey as StoredSecret, SecretStore};
use crate::core::error::{Result, SyncPartyError};

pub use tunnel::{GuestTunnel, HostTunnel};

/// Identifies what is spoken over a syncparty connection.
///
/// Both sides must send the same string or the QUIC handshake is refused, so
/// this doubles as a cheap guard against a future protocol change talking to
/// an old build.
pub const ALPN: &[u8] = b"syncparty/syncplay/1";

/// How long to wait for the endpoint to work out how it is reachable.
///
/// Until this completes the endpoint has no home relay and has published
/// nothing, so an invite handed out before it would name an address no guest
/// could resolve.
const ONLINE_TIMEOUT: Duration = Duration::from_secs(30);

/// One machine's presence on the syncparty network.
pub struct PartyEndpoint {
    endpoint: Endpoint,
}

impl PartyEndpoint {
    /// Binds the endpoint a host accepts parties on.
    ///
    /// The key is generated once and kept, which is what makes an invite
    /// outlive a restart: the code names the endpoint, so a new key on every
    /// launch would silently invalidate every code already sent out. It is the
    /// same reasoning that keeps the Syncplay salt stable.
    pub async fn bind_hosting(secrets: &SecretStore) -> Result<Self> {
        let endpoint = Endpoint::builder(presets::N0)
            .secret_key(stored_key(secrets)?)
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .map_err(|error| SyncPartyError::EndpointBindFailed(error.to_string()))?;

        Ok(Self { endpoint })
    }

    /// Binds the endpoint a guest dials out from.
    ///
    /// Deliberately a throwaway key. A guest is never dialled, so a stable
    /// identity would buy nothing and would leave the same public key in every
    /// relay's sight from one movie night to the next.
    pub async fn bind_joining() -> Result<Self> {
        let endpoint = Endpoint::bind(presets::N0)
            .await
            .map_err(|error| SyncPartyError::EndpointBindFailed(error.to_string()))?;

        Ok(Self { endpoint })
    }

    /// This machine's address, in the form that goes into an invite.
    pub fn id(&self) -> EndpointId {
        self.endpoint.id()
    }

    pub fn inner(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Waits until the endpoint knows how it can be reached.
    ///
    /// Bounded, because the alternative is a host staring at "starting…"
    /// forever on a machine with no working internet connection.
    pub async fn wait_online(&self) -> Result<()> {
        tokio::time::timeout(ONLINE_TIMEOUT, self.endpoint.online())
            .await
            .map_err(|_| SyncPartyError::EndpointOffline)
    }

    /// Closes the endpoint, letting queued QUIC close frames go out first so
    /// the other side learns the party ended rather than timing out.
    pub async fn close(&self) {
        self.endpoint.close().await;
    }
}

/// Reads the host's endpoint key, creating and storing one on first use.
fn stored_key(secrets: &SecretStore) -> Result<SecretKey> {
    if let Some(existing) = secrets.get(StoredSecret::EndpointKey)? {
        if let Some(key) = decode_key(&existing) {
            return Ok(key);
        }
        // A key that cannot be parsed is worse than no key: it would fail on
        // every launch forever. Replacing it costs the old endpoint id, which
        // only invalidates invites that were already unusable.
        tracing::warn!("the stored endpoint key was unreadable; generating a new one");
    }

    let key = SecretKey::generate();
    secrets.set(StoredSecret::EndpointKey, &encode_key(&key))?;
    Ok(key)
}

/// The keychain stores strings, and a secret key is 32 bytes. base64url
/// rather than iroh's own hex encoding so the stored form does not depend on
/// a detail of iroh's `Display`, which is deliberately redacted anyway.
fn encode_key(key: &SecretKey) -> String {
    URL_SAFE_NO_PAD.encode(key.to_bytes())
}

fn decode_key(raw: &str) -> Option<SecretKey> {
    let bytes: [u8; 32] = URL_SAFE_NO_PAD.decode(raw).ok()?.try_into().ok()?;
    Some(SecretKey::from_bytes(&bytes))
}

/// This machine's endpoint id, if it has ever hosted.
///
/// Reads the key without binding anything, so diagnostics can report the
/// address guests would dial without needing a party or a network. Returns
/// `None` rather than creating a key: a guest that has never hosted does not
/// have one, and generating it here would be a surprising side effect of
/// opening a troubleshooting screen.
pub fn stored_endpoint_id(secrets: &SecretStore) -> Option<EndpointId> {
    let stored = secrets.get(StoredSecret::EndpointKey).ok().flatten()?;
    Some(decode_key(&stored)?.public())
}

/// Parses the endpoint id carried by an invite.
pub fn parse_endpoint_id(raw: &str) -> Result<EndpointId> {
    raw.parse::<EndpointId>()
        .map_err(|_| SyncPartyError::InvalidInvite("the code names an unreadable host".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_survives_the_trip_through_the_keychain() {
        let key = SecretKey::generate();

        let restored = decode_key(&encode_key(&key)).expect("decode");

        assert_eq!(restored.to_bytes(), key.to_bytes());
        assert_eq!(restored.public(), key.public());
    }

    #[test]
    fn stored_keys_do_not_need_quoting_in_a_url_or_a_chat_message() {
        let encoded = encode_key(&SecretKey::generate());

        assert!(encoded
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn a_damaged_stored_key_is_rejected_rather_than_panicking() {
        assert!(decode_key("not base64 at all !!").is_none());
        // Right alphabet, wrong length — the case a truncated keychain entry
        // produces, and the one `try_into` exists to catch.
        assert!(decode_key(&URL_SAFE_NO_PAD.encode([0_u8; 16])).is_none());
    }

    #[test]
    fn an_endpoint_id_round_trips_through_its_text_form() {
        let id = SecretKey::generate().public();

        assert_eq!(parse_endpoint_id(&id.to_string()).expect("parse"), id);
    }

    #[test]
    fn a_bad_endpoint_id_is_reported_as_a_bad_invite() {
        let error = parse_endpoint_id("nonsense").expect_err("not an endpoint id");

        assert_eq!(error.kind(), "invalid_invite");
    }
}
