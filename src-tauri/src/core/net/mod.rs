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

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointId, SecretKey, TransportAddr, Watcher};
use serde::Serialize;
use ts_rs::TS;

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

/// The address range carriers hand out when they have run out of IPv4 and put
/// their subscribers behind a shared NAT.
///
/// A machine here has no public address of its own and cannot be reached by
/// forwarding a port on its router, because the router is not the thing doing
/// the translating. It is the case syncparty has to survive rather than
/// diagnose away, so this is reported and not treated as a fault.
const CARRIER_GRADE_NAT: (Ipv4Addr, u8) = (Ipv4Addr::new(100, 64, 0, 0), 10);

/// How a live connection to one peer is carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum PathKind {
    /// Hole punched: packets travel machine to machine, with no port forwarded
    /// on either end.
    Direct,
    /// Forwarded by a relay, which is where a network that refuses to be hole
    /// punched ends up. Slower, still end-to-end encrypted.
    Relayed,
    /// Connected, but QUIC has not settled on a path yet. Normal for the first
    /// moment of a connection.
    Unknown,
}

/// One live connection, and how it turned out to be carried.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PeerPath {
    pub peer: String,
    pub kind: PathKind,
    /// The selected path's far address, as iroh reports it.
    pub remote: Option<String>,
    pub rtt_ms: Option<u64>,
}

/// One relay this endpoint has been assigned, and whether it is usable.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RelayHealth {
    pub url: String,
    pub connected: bool,
    pub last_error: Option<String>,
}

/// What the transport can say about itself right now.
///
/// Assembled from a live endpoint, so every field is measured rather than
/// configured — which is the point. Nothing here is read from settings, and
/// there is no address for the user to have got wrong.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct TransportReport {
    /// This machine's address on the syncparty network.
    pub endpoint_id: String,
    /// The addresses iroh worked out for this machine, including any public
    /// one a relay observed. Not a setting — these are discovered.
    pub addresses: Vec<String>,
    /// Whether a discovered address falls inside the carrier-grade NAT range.
    ///
    /// `None` when no public IPv4 was discovered at all, which is a different
    /// statement from "not behind CGNAT" and must not be shown as one.
    pub behind_carrier_nat: Option<bool>,
    pub relays: Vec<RelayHealth>,
    /// Live connections. Empty when no party is running, which is why the
    /// screen has to say so rather than reading it as "nobody connected".
    pub peers: Vec<PeerPath>,
}

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

    /// A snapshot of how this endpoint is placed on the network.
    ///
    /// `peers` is left to the caller: an endpoint does not know which of its
    /// connections belong to a party, and the tunnels that do are the ones
    /// holding them.
    pub fn report(&self, peers: Vec<PeerPath>) -> TransportReport {
        let addr = self.endpoint.addr();

        let addresses: Vec<String> = addr
            .addrs
            .iter()
            .map(|address| match address {
                TransportAddr::Ip(socket) => socket.to_string(),
                TransportAddr::Relay(url) => url.to_string(),
                other => format!("{other:?}"),
            })
            .collect();

        let relays = self
            .endpoint
            .home_relay_status()
            .get()
            .iter()
            .map(|relay| RelayHealth {
                url: relay.url().to_string(),
                connected: relay.is_connected(),
                last_error: relay.last_error().map(|error| error.to_string()),
            })
            .collect();

        TransportReport {
            endpoint_id: self.endpoint.id().to_string(),
            addresses,
            behind_carrier_nat: carrier_nat_verdict(&addr.addrs),
            relays,
            peers,
        }
    }
}

/// Whether any routable address discovered for this machine is a carrier-grade
/// NAT one.
///
/// Private and link-local addresses are skipped: every machine has those, and
/// they say nothing about how the internet sees it. Returning `None` when
/// nothing routable was discovered keeps "we could not tell" distinct from
/// "no, you have a real address".
fn carrier_nat_verdict<'a>(addrs: impl IntoIterator<Item = &'a TransportAddr>) -> Option<bool> {
    let mut verdict = None;

    for address in addrs {
        let TransportAddr::Ip(socket) = address else {
            continue;
        };
        let IpAddr::V4(ip) = socket.ip() else {
            continue;
        };
        if ip.is_private() || ip.is_loopback() || ip.is_link_local() {
            continue;
        }

        // Any carrier-NAT address settles it: the machine is behind one, even
        // if some other interface has a public address.
        if in_carrier_nat_range(ip) {
            return Some(true);
        }
        verdict = Some(false);
    }

    verdict
}

fn in_carrier_nat_range(ip: Ipv4Addr) -> bool {
    let (base, prefix) = CARRIER_GRADE_NAT;
    let mask = u32::MAX << (32 - prefix);

    u32::from(ip) & mask == u32::from(base) & mask
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

    fn ip(address: &str) -> TransportAddr {
        TransportAddr::Ip(format!("{address}:11204").parse().expect("address"))
    }

    #[test]
    fn the_carrier_nat_range_is_recognised_at_both_ends() {
        assert!(in_carrier_nat_range(Ipv4Addr::new(100, 64, 0, 0)));
        assert!(in_carrier_nat_range(Ipv4Addr::new(100, 127, 255, 255)));

        // The addresses either side of the range. 100.63 and 100.128 are
        // ordinary public space, and treating them as CGNAT would tell someone
        // their connection is doomed when it is not.
        assert!(!in_carrier_nat_range(Ipv4Addr::new(100, 63, 255, 255)));
        assert!(!in_carrier_nat_range(Ipv4Addr::new(100, 128, 0, 0)));
    }

    #[test]
    fn a_carrier_nat_address_is_reported_even_alongside_a_public_one() {
        let verdict = carrier_nat_verdict(&[ip("100.90.1.1"), ip("203.0.113.7")]);

        assert_eq!(verdict, Some(true));
    }

    #[test]
    fn a_public_address_on_its_own_clears_the_machine() {
        assert_eq!(carrier_nat_verdict(&[ip("203.0.113.7")]), Some(false));
    }

    /// The distinction the UI depends on: "we could not tell" must not render
    /// as "no". A machine that has only found its LAN address has not been
    /// cleared of anything.
    #[test]
    fn local_addresses_alone_are_not_a_verdict() {
        let verdict = carrier_nat_verdict(&[
            ip("192.168.1.42"),
            ip("10.0.0.5"),
            ip("127.0.0.1"),
            ip("169.254.3.9"),
            TransportAddr::Relay("https://relay.example".parse().expect("url")),
        ]);

        assert_eq!(verdict, None);
    }

    /// What this machine actually looks like from the outside.
    ///
    /// Exercises the path the diagnostics screen takes when no party is
    /// running — bind, wait for the relays, read back — against the real
    /// infrastructure, and prints the verdict. Not an assertion: the answer
    /// depends on whoever's network it runs on, and every answer is valid.
    ///
    /// ```text
    /// cargo test --manifest-path src-tauri/Cargo.toml -- --ignored --nocapture this_machine
    /// ```
    #[tokio::test]
    #[ignore = "needs the internet and n0's relays"]
    async fn what_the_network_looks_like_from_this_machine() {
        let endpoint = PartyEndpoint::bind_joining().await.expect("bind");
        endpoint.wait_online().await.expect("get online");

        let report = endpoint.report(Vec::new());
        endpoint.close().await;

        println!("addresses:  {:?}", report.addresses);
        println!("carrier nat: {:?}", report.behind_carrier_nat);
        for relay in &report.relays {
            println!("relay:      {} connected={}", relay.url, relay.connected);
        }
    }

    #[test]
    fn a_bad_endpoint_id_is_reported_as_a_bad_invite() {
        let error = parse_endpoint_id("nonsense").expect_err("not an endpoint id");

        assert_eq!(error.kind(), "invalid_invite");
    }
}
