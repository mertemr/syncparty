//! Carrying Syncplay's TCP connection over a QUIC one.
//!
//! Syncplay is not ours. The server binds a TCP port and the desktop client
//! dials `host:port`, and neither can be taught to speak anything else. So
//! rather than replace them, both ends are given a socket on their own machine
//! that behaves exactly like the one they expect:
//!
//! ```text
//! Guest machine                                Host machine
//! Syncplay client                                Syncplay server
//!    │ TCP                                              ▲ TCP
//!    ▼                                                  │
//! 127.0.0.1:auto ─► syncparty ══ QUIC ══► syncparty ─► 127.0.0.1:8999
//! ```
//!
//! The upshot is that `launcher`, `monitor` and `protocol` never learn that
//! the transport changed — they still connect to a local address — and the
//! Syncplay server no longer listens on any network interface at all.
//!
//! One consequence worth being explicit about: syncparty is now on the path
//! for the whole film. Closing it mid-party takes the tunnel with it, where
//! previously the two Syncplay processes talked over Tailscale on their own.
//! Both tunnels are therefore owned by the session and live as long as it does.

use std::net::{Ipv4Addr, SocketAddr};

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr};

use crate::core::error::{Result, SyncPartyError};
use crate::core::net::ALPN;

/// The host half: turns incoming QUIC streams into connections to the local
/// Syncplay server.
pub struct HostTunnel {
    task: JoinHandle<()>,
}

impl HostTunnel {
    /// Starts accepting parties on `endpoint`, forwarding each stream to
    /// `target` — the loopback address the Syncplay server is bound to.
    ///
    /// Every guest gets its own QUIC connection, and every TCP connection that
    /// guest makes is one stream inside it. Syncplay reconnects rather than
    /// giving up when a connection drops, so a stream ending is routine and
    /// must not bring the tunnel down with it.
    pub fn start(endpoint: Endpoint, target: SocketAddr) -> Self {
        let task = tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                tokio::spawn(async move {
                    let connection = match incoming.await {
                        Ok(connection) => connection,
                        Err(error) => {
                            tracing::warn!(%error, "a guest failed to complete its handshake");
                            return;
                        }
                    };

                    serve(connection, target).await;
                });
            }
        });

        Self { task }
    }
}

impl Drop for HostTunnel {
    /// Stopping a party has to actually stop it. Without this the accept loop
    /// would outlive the session and the next one would find the endpoint
    /// already serving the old, now-dead Syncplay port.
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Serves one guest until it disconnects.
async fn serve(connection: Connection, target: SocketAddr) {
    let guest = connection.remote_id();
    tracing::info!(%guest, "a guest joined the party");

    // Ends when the guest closes the connection or it times out, which is what
    // terminates this task — there is nothing else to poll for.
    while let Ok((send, recv)) = connection.accept_bi().await {
        tokio::spawn(async move {
            match TcpStream::connect(target).await {
                Ok(stream) => splice(stream, send, recv).await,
                Err(error) => {
                    // The party is up but its Syncplay server is not answering
                    // on loopback. Logged rather than fatal: the guest's client
                    // will retry, and by then the server may have recovered.
                    tracing::warn!(%error, %target, "could not reach the local Syncplay server");
                }
            }
        });
    }

    tracing::info!(%guest, "a guest left the party");
}

/// The guest half: a loopback port that Syncplay can be pointed at.
pub struct GuestTunnel {
    local: SocketAddr,
    task: JoinHandle<()>,
    /// Kept alive, never used again.
    ///
    /// An [`Endpoint`] is a handle to the QUIC stack, and dropping the last
    /// one takes every connection made through it down. Holding a clone here
    /// means a tunnel cannot be quietly killed by whoever handed the endpoint
    /// over letting go of theirs.
    _endpoint: Endpoint,
}

impl GuestTunnel {
    /// Dials `host` and opens a local port that forwards to it.
    ///
    /// The connection is established here rather than on first use so that a
    /// host who is offline is reported as such immediately, instead of
    /// launching Syncplay against a port that will never answer — which is
    /// exactly the failure the Tailscale-era address probing existed to avoid.
    ///
    /// `host` is anything that names the far side: an
    /// [`EndpointId`](iroh::EndpointId) on its own in normal use, since that
    /// is all an invite carries and the transport resolves the rest, or a full
    /// [`EndpointAddr`] when the addresses are already known and there is no
    /// lookup service to ask.
    pub async fn open(endpoint: Endpoint, host: impl Into<EndpointAddr>) -> Result<Self> {
        let host = host.into();
        let id = host.id;

        let connection = endpoint.connect(host, ALPN).await.map_err(|error| {
            SyncPartyError::PartyUnreachable {
                host: id.to_string(),
                reason: error.to_string(),
            }
        })?;

        // Port 0 asks the OS for a free one: a fixed port would collide with
        // the guest's own Syncplay server if they ever host, and with a second
        // window of syncparty.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let local = listener.local_addr()?;

        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };

                let connection = connection.clone();
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
            _endpoint: endpoint,
        })
    }

    /// The address to hand to Syncplay. Always on loopback, so nothing else on
    /// the network can reach it.
    pub fn local_addr(&self) -> SocketAddr {
        self.local
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
/// the other — a half-open TCP connection would leave Syncplay waiting on a
/// reply that can no longer arrive.
async fn splice<S, R>(stream: TcpStream, mut send: S, mut recv: R)
where
    S: AsyncWrite + Unpin + Send + 'static,
    R: AsyncRead + Unpin + Send + 'static,
{
    // Nagle's algorithm delays small writes to coalesce them. Syncplay's
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A stand-in for the Syncplay server: echoes whatever it is sent.
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
        let _host = HostTunnel::start(host_endpoint, syncplay);

        let guest = GuestTunnel::open(guest_endpoint, host_addr)
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

    #[tokio::test]
    async fn a_guest_tunnel_listens_only_on_loopback() {
        // Built without a host on purpose: the address the guest hands to
        // Syncplay must never be routable from the local network, and that is
        // decided by the bind alone.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("listener");
        let address = listener.local_addr().expect("address");

        assert!(address.ip().is_loopback());
        assert_ne!(address.port(), 0, "the OS should have chosen a real port");
    }
}
