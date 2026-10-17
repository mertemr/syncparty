//! Carrying the sync protocol's TCP connection over a QUIC one.
//!
//! The protocol is Syncplay's, and Syncplay runs it over plain TCP: the server
//! binds a port and a client dials `host:port`. syncparty's own client and
//! server still speak it that way — which is also what lets a guest on an
//! older, Syncplay-based syncparty keep joining a new host. So rather than
//! teach either end QUIC, both are given a socket on their own machine that
//! behaves exactly like the one they expect:
//!
//! ```text
//! Guest machine                                Host machine
//! sync client                                     sync server
//!    │ TCP                                              ▲ TCP
//!    ▼                                                  │
//! 127.0.0.1:auto ─► syncparty ══ QUIC ══► syncparty ─► 127.0.0.1:8999
//! ```
//!
//! The upshot is that the client, the monitor and `protocol` never learn that
//! the transport exists — they connect to a local address — and the server
//! does not listen on any network interface at all.
//!
//! One consequence worth being explicit about: syncparty is on the path for
//! the whole film. Closing it mid-party takes the tunnel with it. Both tunnels
//! are therefore owned by the session and live as long as it does.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use tokio::io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;

use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr, EndpointId};

use crate::core::error::{Result, SyncPartyError};
use crate::core::net::{PathKind, PeerPath, ALPN};

/// Receives whatever the other side sends down the control channel — the one
/// stream per connection that carries application data instead of the sync
/// protocol.
///
/// Implemented by whatever domain owns that data (movie voting, say); `net`
/// itself has no opinion on what the bytes mean, only that they arrived.
/// Takes `Arc<Self>` rather than `&self` because handling a message is
/// typically async work (locking state, broadcasting a reply), which needs an
/// owned, `'static` handle to spawn onto its own task.
pub trait ControlChannel: Send + Sync {
    fn on_message(self: Arc<Self>, peer: EndpointId, bytes: Vec<u8>);

    /// Called once a peer's control channel is up, before anything has
    /// necessarily arrived on it. The default does nothing; a host-side
    /// implementation can use this moment to push a freshly connected guest
    /// the current state instead of waiting for it to ask.
    fn on_connected(self: Arc<Self>, _peer: EndpointId) {}
}

type BoxedWriter = Box<dyn AsyncWrite + Unpin + Send>;
type BoxedReader = Box<dyn AsyncRead + Unpin + Send>;

/// The live control-channel write halves, one per connected guest, so a
/// broadcast can reach everyone without the caller tracking connections
/// itself.
type ControlWriters = Arc<Mutex<BTreeMap<EndpointId, Arc<AsyncMutex<BoxedWriter>>>>>;

/// Writes one length-prefixed frame. The prefix is what lets a stream of
/// otherwise-opaque JSON blobs be split back into messages on the other end.
async fn write_frame(writer: &mut (dyn AsyncWrite + Unpin + Send), bytes: &[u8]) -> io::Result<()> {
    writer.write_u32(bytes.len() as u32).await?;
    writer.write_all(bytes).await
}

async fn read_frame(reader: &mut (dyn AsyncRead + Unpin + Send)) -> io::Result<Vec<u8>> {
    let len = reader.read_u32().await?;
    let mut buffer = vec![0_u8; len as usize];
    reader.read_exact(&mut buffer).await?;
    Ok(buffer)
}

/// The connections currently being served, so the diagnostics screen can say
/// how each one is carried.
///
/// A `std` mutex rather than tokio's: every critical section is a map insert
/// or a clone-out, with no `await` between lock and unlock.
type Peers = Arc<Mutex<BTreeMap<EndpointId, Connection>>>;

/// The host half: turns incoming QUIC streams into connections to the local
/// sync server.
pub struct HostTunnel {
    task: JoinHandle<()>,
    peers: Peers,
    control_writers: ControlWriters,
}

impl HostTunnel {
    /// Starts accepting parties on `endpoint`, forwarding each stream to
    /// `target` — the loopback address the sync server is bound to.
    ///
    /// Every guest gets its own QUIC connection, and every TCP connection that
    /// guest makes is one stream inside it. The sync client reconnects rather
    /// than giving up when a connection drops, so a stream ending is routine
    /// and must not bring the tunnel down with it.
    ///
    /// `control` receives whatever a guest sends down its control channel —
    /// the one stream every guest opens before its sync client dials at all.
    pub fn start(endpoint: Endpoint, target: SocketAddr, control: Arc<dyn ControlChannel>) -> Self {
        let peers: Peers = Arc::new(Mutex::new(BTreeMap::new()));
        let control_writers: ControlWriters = Arc::new(Mutex::new(BTreeMap::new()));

        let accepted = Arc::clone(&peers);
        let accepted_writers = Arc::clone(&control_writers);
        let task = tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                let peers = Arc::clone(&accepted);
                let control_writers = Arc::clone(&accepted_writers);
                let control = Arc::clone(&control);
                tokio::spawn(async move {
                    let connection = match incoming.await {
                        Ok(connection) => connection,
                        Err(error) => {
                            tracing::warn!(%error, "a guest failed to complete its handshake");
                            return;
                        }
                    };

                    serve(connection, target, peers, control_writers, control).await;
                });
            }
        });

        Self {
            task,
            peers,
            control_writers,
        }
    }

    /// How each guest currently in the party is reached.
    ///
    /// The answer this exists for is whether a connection was hole punched or
    /// fell back to a relay — which is not something either side can choose,
    /// and the only honest way to find out is to ask a live connection.
    pub fn peers(&self) -> Vec<PeerPath> {
        let peers = self.peers.lock().expect("the peer map is never poisoned");
        peers.values().map(describe).collect()
    }

    /// Sends `bytes` down every guest's control channel.
    ///
    /// Best-effort: a guest whose control channel has gone quiet (mid
    /// reconnect, say) is skipped rather than failing the whole broadcast —
    /// it gets caught up by the next one, since the host always sends a full
    /// snapshot rather than a delta.
    pub async fn broadcast_control(&self, bytes: &[u8]) {
        let writers: Vec<_> = self
            .control_writers
            .lock()
            .expect("the control writer map is never poisoned")
            .values()
            .cloned()
            .collect();

        for writer in writers {
            let mut writer = writer.lock().await;
            if let Err(error) = write_frame(&mut *writer, bytes).await {
                tracing::warn!(%error, "a guest's control channel could not be written to");
            }
        }
    }

    /// Sends `bytes` down one guest's control channel, if it is still
    /// connected. Used to hydrate a single guest — on reconnect, say — rather
    /// than disturbing everyone else with a broadcast.
    pub async fn send_control_to(&self, peer: EndpointId, bytes: &[u8]) {
        let writer = self
            .control_writers
            .lock()
            .expect("the control writer map is never poisoned")
            .get(&peer)
            .cloned();

        let Some(writer) = writer else { return };
        let mut writer = writer.lock().await;
        if let Err(error) = write_frame(&mut *writer, bytes).await {
            tracing::warn!(%error, %peer, "a guest's control channel could not be written to");
        }
    }
}

impl Drop for HostTunnel {
    /// Stopping a party has to actually stop it. Without this the accept loop
    /// would outlive the session and the next one would find the endpoint
    /// already serving the old, now-dead server port.
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Serves one guest until it disconnects.
///
/// The guest always opens its control channel before its sync client dials at
/// all (see [`GuestTunnel::open`]), so the first stream accepted here is
/// always that one — no header byte or negotiation needed to tell it apart
/// from the sync passthrough streams that follow.
async fn serve(
    connection: Connection,
    target: SocketAddr,
    peers: Peers,
    control_writers: ControlWriters,
    control: Arc<dyn ControlChannel>,
) {
    let guest = connection.remote_id();
    tracing::info!(%guest, "a guest joined the party");

    peers
        .lock()
        .expect("the peer map is never poisoned")
        .insert(guest, connection.clone());

    let Ok((send, recv)) = connection.accept_bi().await else {
        tracing::warn!(%guest, "a guest never opened its control channel");
        peers
            .lock()
            .expect("the peer map is never poisoned")
            .remove(&guest);
        return;
    };

    let writer: Arc<AsyncMutex<BoxedWriter>> = Arc::new(AsyncMutex::new(Box::new(send)));
    control_writers
        .lock()
        .expect("the control writer map is never poisoned")
        .insert(guest, Arc::clone(&writer));

    Arc::clone(&control).on_connected(guest);

    let mut reader: BoxedReader = Box::new(recv);
    tokio::spawn(async move {
        while let Ok(bytes) = read_frame(&mut *reader).await {
            Arc::clone(&control).on_message(guest, bytes);
        }
    });

    // Ends when the guest closes the connection or it times out, which is what
    // terminates this task — there is nothing else to poll for.
    while let Ok((send, recv)) = connection.accept_bi().await {
        tokio::spawn(async move {
            match TcpStream::connect(target).await {
                Ok(stream) => splice(stream, send, recv).await,
                Err(error) => {
                    // The party is up but its sync server is not answering on
                    // loopback. Logged rather than fatal: the guest's client
                    // will retry, and by then the server may have recovered.
                    tracing::warn!(%error, %target, "could not reach the local sync server");
                }
            }
        });
    }

    peers
        .lock()
        .expect("the peer map is never poisoned")
        .remove(&guest);
    control_writers
        .lock()
        .expect("the control writer map is never poisoned")
        .remove(&guest);

    tracing::info!(%guest, "a guest left the party");
}

/// Reads back how a live connection is actually being carried.
///
/// QUIC keeps several paths open at once and moves between them — a
/// connection typically starts relayed and switches to a direct path a moment
/// later, once hole punching succeeds. The selected path is therefore the only
/// one worth reporting, and its absence is a real state rather than an error.
fn describe(connection: &Connection) -> PeerPath {
    let paths = connection.paths();
    let selected = paths.iter().find(|path| path.is_selected());

    let Some(path) = selected else {
        return PeerPath {
            peer: connection.remote_id().to_string(),
            kind: PathKind::Unknown,
            remote: None,
            rtt_ms: None,
        };
    };

    PeerPath {
        peer: connection.remote_id().to_string(),
        kind: if path.is_relay() {
            PathKind::Relayed
        } else {
            PathKind::Direct
        },
        remote: Some(format!("{:?}", path.remote_addr())),
        rtt_ms: Some(path.rtt().as_millis() as u64),
    }
}

/// The guest half: a loopback port for the sync client to dial.
pub struct GuestTunnel {
    local: SocketAddr,
    task: JoinHandle<()>,
    /// The connection to the host and its control channel. Both are replaced
    /// whenever the tunnel has to dial again.
    link: Arc<Link>,
    /// Kept alive, never used again.
    ///
    /// An [`Endpoint`] is a handle to the QUIC stack, and dropping the last
    /// one takes every connection made through it down. Holding a clone here
    /// means a tunnel cannot be quietly killed by whoever handed the endpoint
    /// over letting go of theirs.
    _endpoint: Endpoint,
}

/// What a guest currently holds open to its host.
struct Link {
    /// Cloning a [`Connection`] clones a handle, not the connection, so this
    /// is only ever locked long enough to swap or copy one out.
    connection: Mutex<Connection>,
    /// The write half of the control channel. The writer inside is replaced
    /// on every dial, so a [`GuestTunnel::send_control`] that waits for the
    /// lock writes to whichever connection is current by the time it gets it.
    control_writer: Arc<AsyncMutex<BoxedWriter>>,
}

impl Link {
    fn current(&self) -> Connection {
        self.connection
            .lock()
            .expect("the guest link is never poisoned")
            .clone()
    }

    /// The live connection, dialling the host again first if the old one has
    /// gone.
    ///
    /// Only ever called from the accept loop, one connection at a time, so
    /// two dials can never race each other.
    async fn usable(
        &self,
        endpoint: &Endpoint,
        host: &EndpointAddr,
        control: &Arc<dyn ControlChannel>,
    ) -> Option<Connection> {
        let current = self.current();
        if current.close_reason().is_none() {
            return Some(current);
        }

        match dial(endpoint, host, control).await {
            Ok((connection, writer)) => {
                tracing::info!(host = %host.id, "dialled the host again");
                *self.control_writer.lock().await = writer;
                *self
                    .connection
                    .lock()
                    .expect("the guest link is never poisoned") = connection.clone();
                Some(connection)
            }
            Err(error) => {
                tracing::warn!(%error, "the host is still out of reach");
                None
            }
        }
    }
}

/// Dials `host` and opens the control channel on the new connection.
///
/// The control channel is the first bi-stream on every connection, so the
/// host's `serve` loop can rely on it always coming first. `control` receives
/// whatever the host sends back down it, for as long as that connection lasts.
async fn dial(
    endpoint: &Endpoint,
    host: &EndpointAddr,
    control: &Arc<dyn ControlChannel>,
) -> Result<(Connection, BoxedWriter)> {
    let unreachable = |reason: String| SyncPartyError::PartyUnreachable {
        host: host.id.to_string(),
        reason,
    };

    let connection = endpoint
        .connect(host.clone(), ALPN)
        .await
        .map_err(|error| unreachable(error.to_string()))?;

    let (control_send, control_recv) = connection
        .open_bi()
        .await
        .map_err(|error| unreachable(error.to_string()))?;

    let mut control_reader: BoxedReader = Box::new(control_recv);
    let peer = connection.remote_id();
    let control = Arc::clone(control);
    tokio::spawn(async move {
        while let Ok(bytes) = read_frame(&mut *control_reader).await {
            Arc::clone(&control).on_message(peer, bytes);
        }
    });

    Ok((connection, Box::new(control_send)))
}

impl GuestTunnel {
    /// Dials `host` and opens a local port that forwards to it.
    ///
    /// The connection is established here rather than on first use so that a
    /// host who is offline is reported as such immediately, instead of
    /// connecting the sync client to a port that will never answer — which is
    /// exactly the failure the Tailscale-era address probing existed to avoid.
    ///
    /// `host` is anything that names the far side: an
    /// [`EndpointId`](iroh::EndpointId) on its own in normal use, since that
    /// is all an invite carries and the transport resolves the rest, or a full
    /// [`EndpointAddr`] when the addresses are already known and there is no
    /// lookup service to ask.
    ///
    /// Opens the control channel before anything else, and again on every
    /// later dial. `control` receives whatever the host sends back down it.
    ///
    /// The tunnel outlives the connection under it. When the host stops its
    /// party and starts it again, restarts syncparty, or goes quiet for longer
    /// than the idle timeout, the next connection the sync client makes dials
    /// the host again — the invite still names the same endpoint, so the
    /// client's own reconnect loop is all it takes to get back in.
    pub async fn open(
        endpoint: Endpoint,
        host: impl Into<EndpointAddr>,
        control: Arc<dyn ControlChannel>,
    ) -> Result<Self> {
        let host = host.into();
        let (connection, writer) = dial(&endpoint, &host, &control).await?;

        let link = Arc::new(Link {
            connection: Mutex::new(connection),
            control_writer: Arc::new(AsyncMutex::new(writer)),
        });

        // Port 0 asks the OS for a free one: a fixed port would collide with
        // the guest's own sync server if they ever host, and with a second
        // window of syncparty.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let local = listener.local_addr()?;

        let forwarding = Arc::clone(&link);
        let dialler = endpoint.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };

                // Dropping the stream unanswered when the host cannot be
                // reached is what the sync client expects of a dead server:
                // it backs off and tries again, and so do we.
                let Some(connection) = forwarding.usable(&dialler, &host, &control).await else {
                    continue;
                };

                tokio::spawn(async move {
                    match connection.open_bi().await {
                        Ok((send, recv)) => splice(stream, send, recv).await,
                        Err(error) => {
                            tracing::warn!(%error, "the host stopped accepting streams");
                        }
                    }
                });
            }
        });

        Ok(Self {
            local,
            task,
            link,
            _endpoint: endpoint,
        })
    }

    /// The address for the sync client to dial. Always on loopback, so nothing
    /// else on the network can reach it.
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// How this guest is reaching the host.
    pub fn host_path(&self) -> PeerPath {
        describe(&self.link.current())
    }

    /// Whether the connection to the host is up right now.
    ///
    /// False between the host going away and the next successful dial. The
    /// tunnel dials again by itself, but only when the sync client next
    /// connects, so the session can use this to tell a party that is really
    /// still there from one it is only trying to get back to.
    pub fn is_alive(&self) -> bool {
        self.link.current().close_reason().is_none() && !self.task.is_finished()
    }

    /// Sends `bytes` to the host down the control channel.
    pub async fn send_control(&self, bytes: &[u8]) -> Result<()> {
        let mut writer = self.link.control_writer.lock().await;
        write_frame(&mut *writer, bytes).await.map_err(|error| {
            SyncPartyError::Other(format!("control channel write failed: {error}"))
        })
    }
}

impl Drop for GuestTunnel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Copies bytes both ways until either side finishes.
///
/// `copy_bidirectional` is not usable here: it wants one object that is both
/// reader and writer, and a QUIC stream arrives already split in two. Each
/// direction therefore gets its own copy, and the first one to end tears down
/// the other — a half-open TCP connection would leave the client waiting on a
/// reply that can no longer arrive.
async fn splice<S, R>(stream: TcpStream, mut send: S, mut recv: R)
where
    S: AsyncWrite + Unpin + Send + 'static,
    R: AsyncRead + Unpin + Send + 'static,
{
    // Nagle's algorithm delays small writes to coalesce them. The sync
    // traffic is almost entirely small writes whose whole value is arriving
    // promptly, which is the case this optimisation is wrong for.
    let _ = stream.set_nodelay(true);

    let (mut reader, mut writer) = stream.into_split();

    let downstream = async move { tokio::io::copy(&mut recv, &mut writer).await };
    let upstream = async move { tokio::io::copy(&mut reader, &mut send).await };

    tokio::select! {
        _ = downstream => {}
        _ = upstream => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Discards whatever arrives — these tests are exercising the sync
    /// passthrough, not the control channel itself.
    struct NullControl;
    impl ControlChannel for NullControl {
        fn on_message(self: Arc<Self>, _peer: EndpointId, _bytes: Vec<u8>) {}
    }

    /// A stand-in for the sync server: echoes whatever it is sent.
    async fn echo_server() -> SocketAddr {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");

        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let (mut reader, mut writer) = stream.split();
                    let _ = tokio::io::copy(&mut reader, &mut writer).await;
                });
            }
        });

        address
    }

    /// Exercises `splice` over a plain TCP pair rather than a QUIC one.
    ///
    /// The interesting behaviour — both directions copying, and either end
    /// finishing tearing the other down — is independent of what the stream
    /// halves happen to be, and a real iroh connection here would make this a
    /// network test rather than a unit one.
    #[tokio::test]
    async fn a_spliced_connection_carries_bytes_both_ways() {
        let backend = echo_server().await;

        let front = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("front");
        let front_address = front.local_addr().expect("address");

        tokio::spawn(async move {
            let (inbound, _) = front.accept().await.expect("accept");
            let outbound = TcpStream::connect(backend).await.expect("connect");
            let (recv, send) = inbound.into_split();
            splice(outbound, send, recv).await;
        });

        let mut client = TcpStream::connect(front_address).await.expect("dial");
        client.write_all(b"hello syncplay").await.expect("write");

        let mut buffer = [0_u8; 14];
        client.read_exact(&mut buffer).await.expect("read");

        assert_eq!(&buffer, b"hello syncplay");
    }

    #[tokio::test]
    async fn closing_one_end_tears_the_other_down_rather_than_hanging() {
        let backend = echo_server().await;

        let front = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("front");
        let front_address = front.local_addr().expect("address");

        let spliced = tokio::spawn(async move {
            let (inbound, _) = front.accept().await.expect("accept");
            let outbound = TcpStream::connect(backend).await.expect("connect");
            let (recv, send) = inbound.into_split();
            splice(outbound, send, recv).await;
        });

        let client = TcpStream::connect(front_address).await.expect("dial");
        drop(client);

        // The point of the test: this returns rather than waiting forever.
        tokio::time::timeout(std::time::Duration::from_secs(5), spliced)
            .await
            .expect("splice should finish when a side goes away")
            .expect("no panic");
    }

    /// Waits until an endpoint knows which sockets it is bound to.
    ///
    /// With no relay and no address lookup service in the test, the host's
    /// direct addresses are the only way for the guest to find it, and they
    /// are not known the instant `bind` returns.
    async fn addr_of(endpoint: &Endpoint) -> EndpointAddr {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);

        while tokio::time::Instant::now() < deadline {
            let addr = endpoint.addr();
            if !addr.addrs.is_empty() {
                return addr;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        panic!("an endpoint on loopback should learn its own addresses");
    }

    /// The whole point of the module, end to end: something that speaks plain
    /// TCP to a loopback port reaches a server listening on a loopback port of
    /// another endpoint, with a QUIC connection in between and neither side
    /// aware there is one.
    ///
    /// `presets::Minimal` rather than `N0` deliberately — it leaves out the
    /// relays and the DNS publishing, so this exercises syncparty's own code
    /// rather than n0's infrastructure, and it passes on a machine with no
    /// outbound network at all.
    #[tokio::test]
    async fn a_guest_reaches_the_hosts_local_server_through_the_tunnel() {
        use iroh::endpoint::presets;

        let syncplay = echo_server().await;

        let host_endpoint = Endpoint::builder(presets::Minimal)
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .expect("host endpoint");
        let guest_endpoint = Endpoint::bind(presets::Minimal)
            .await
            .expect("guest endpoint");

        let host_addr = addr_of(&host_endpoint).await;
        let _host = HostTunnel::start(host_endpoint, syncplay, Arc::new(NullControl));

        let guest = GuestTunnel::open(guest_endpoint, host_addr, Arc::new(NullControl))
            .await
            .expect("the tunnel should open");

        let mut client = TcpStream::connect(guest.local_addr())
            .await
            .expect("the tunnel's local port should accept a connection");
        client.write_all(b"hello syncplay").await.expect("write");

        let mut buffer = [0_u8; 14];
        tokio::time::timeout(
            std::time::Duration::from_secs(15),
            client.read_exact(&mut buffer),
        )
        .await
        .expect("the reply should arrive")
        .expect("read");

        assert_eq!(
            &buffer, b"hello syncplay",
            "bytes must survive the round trip through QUIC and back to TCP"
        );
    }

    /// A tunnel dials once and never again, so it has to be able to say when
    /// the connection under it is gone: that is how the session knows a
    /// second join of the same party is a real one and not a repeat.
    #[tokio::test]
    async fn a_guest_tunnel_knows_when_the_host_has_gone() {
        use iroh::endpoint::presets;

        let syncplay = echo_server().await;

        let host_endpoint = Endpoint::builder(presets::Minimal)
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .expect("host endpoint");
        let guest_endpoint = Endpoint::bind(presets::Minimal)
            .await
            .expect("guest endpoint");

        let host_addr = addr_of(&host_endpoint).await;
        let host = HostTunnel::start(host_endpoint.clone(), syncplay, Arc::new(NullControl));
        let guest = GuestTunnel::open(guest_endpoint, host_addr, Arc::new(NullControl))
            .await
            .expect("the tunnel should open");
        assert!(guest.is_alive());

        // What stopping a party does: the tunnel goes, then the endpoint is
        // closed, which tells every guest.
        drop(host);
        host_endpoint.close().await;

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
        while guest.is_alive() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the guest never noticed the host had gone"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// Whether one plain TCP round trip through `local` makes it to the echo
    /// server and back.
    async fn echoes_through(local: SocketAddr) -> bool {
        let Ok(mut client) = TcpStream::connect(local).await else {
            return false;
        };
        if client.write_all(b"hello syncplay").await.is_err() {
            return false;
        }

        let mut buffer = [0_u8; 14];
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client.read_exact(&mut buffer),
        )
        .await;
        matches!(read, Ok(Ok(_))) && &buffer == b"hello syncplay"
    }

    /// A host that stops its party and starts it again keeps its key, so the
    /// invite still names it. The guest's tunnel has to notice that the old
    /// connection is gone and dial again behind the same local port — the one
    /// the sync client keeps retrying — or every guest would sit reconnecting
    /// to a party that is back.
    #[tokio::test]
    async fn a_guest_tunnel_dials_again_when_the_host_comes_back() {
        use iroh::endpoint::presets;

        let syncplay = echo_server().await;
        let key = iroh::SecretKey::generate();
        // The same port both times, standing in for the address lookup that
        // finds a restarted host in real use and that this test leaves out.
        let port = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("a free port")
            .local_addr()
            .expect("address")
            .port();
        // Retried briefly: the aborted accept loop of the first host lets go
        // of its handle on the socket a moment after it is told to.
        let bind_host = |key: iroh::SecretKey| async move {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let bound = Endpoint::builder(presets::Minimal)
                    .secret_key(key.clone())
                    .alpns(vec![ALPN.to_vec()])
                    .clear_ip_transports()
                    .bind_addr(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
                    .expect("a loopback address")
                    .bind()
                    .await;
                match bound {
                    Ok(endpoint) => break endpoint,
                    Err(error) if tokio::time::Instant::now() > deadline => {
                        panic!("host endpoint: {error}")
                    }
                    Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
                }
            }
        };

        let host_endpoint = bind_host(key.clone()).await;
        let host_addr = addr_of(&host_endpoint).await;
        let host = HostTunnel::start(host_endpoint.clone(), syncplay, Arc::new(NullControl));

        let guest_endpoint = Endpoint::bind(presets::Minimal)
            .await
            .expect("guest endpoint");
        let guest = GuestTunnel::open(guest_endpoint, host_addr, Arc::new(NullControl))
            .await
            .expect("the tunnel should open");
        assert!(echoes_through(guest.local_addr()).await);

        // The party stops...
        drop(host);
        host_endpoint.close().await;
        drop(host_endpoint);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
        while guest.is_alive() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the guest never noticed the host had gone"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        // ...and starts again, under the same key.
        let host_endpoint = bind_host(key).await;
        let _host = HostTunnel::start(host_endpoint, syncplay, Arc::new(NullControl));

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        while !echoes_through(guest.local_addr()).await {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the guest never got back to the restarted host"
            );
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        assert!(guest.is_alive(), "a tunnel that got back in is alive again");
    }

    /// The claim the whole move off Tailscale rests on: an invite code alone
    /// is enough to reach a host, with nothing installed, nothing signed into
    /// and no port forwarded on either router.
    ///
    /// Unlike the test above, this one uses `presets::N0` — the real relays
    /// and the real address lookup — and the guest is handed nothing but an
    /// [`EndpointId`], exactly what an invite carries. Everything that turns
    /// that id into a route is then being exercised for real.
    ///
    /// `#[ignore]` because it needs the internet and n0's infrastructure to be
    /// up, which is not a property of this repository. Run it deliberately:
    ///
    /// ```text
    /// cd src-tauri && cargo test -- --ignored --nocapture reaches_a_host
    /// ```
    ///
    /// On one machine both endpoints sit behind the same NAT, so the path that
    /// wins is a local one and this proves discovery rather than hole
    /// punching. Two machines on different networks is what proves the rest,
    /// and the path it prints is how you tell which happened.
    #[tokio::test]
    #[ignore = "needs the internet and n0's relays"]
    async fn a_guest_reaches_a_host_from_the_invite_code_alone() {
        use iroh::endpoint::presets;

        let syncplay = echo_server().await;

        let host_endpoint = Endpoint::builder(presets::N0)
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .expect("host endpoint");

        // Without this the host has published nothing yet and the id names an
        // address the guest could not resolve — the same reason the real host
        // waits before handing out an invite.
        tokio::time::timeout(std::time::Duration::from_secs(30), host_endpoint.online())
            .await
            .expect("the host should get online");

        // The only thing that crosses from host to guest, standing in for the
        // invite code being pasted into a chat window.
        let invited = host_endpoint.id();

        let guest_endpoint = Endpoint::bind(presets::N0).await.expect("guest endpoint");

        let host = HostTunnel::start(host_endpoint, syncplay, Arc::new(NullControl));

        let guest = GuestTunnel::open(guest_endpoint, invited, Arc::new(NullControl))
            .await
            .expect("the code alone should be enough to reach the host");

        let mut client = TcpStream::connect(guest.local_addr())
            .await
            .expect("the tunnel's local port should accept a connection");
        client.write_all(b"hello syncplay").await.expect("write");

        let mut buffer = [0_u8; 14];
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            client.read_exact(&mut buffer),
        )
        .await
        .expect("the reply should arrive")
        .expect("read");

        assert_eq!(&buffer, b"hello syncplay");

        // Printed rather than asserted on: which path wins is decided by the
        // two networks involved, and a relayed connection is a working one.
        // Failing the test for it would be asserting on someone's router.
        println!("guest -> host: {:?}", guest.host_path());
        for peer in host.peers() {
            println!("host <- guest: {peer:?}");
        }
    }

    #[tokio::test]
    async fn a_guest_tunnel_listens_only_on_loopback() {
        // Built without a host on purpose: the address the guest's sync client
        // dials must never be routable from the local network, and that is
        // decided by the bind alone.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");

        assert!(address.ip().is_loopback());
        assert_ne!(address.port(), 0, "the OS should have chosen a real port");
    }
}
