//! How a guest gets from this machine to the host's sync server.
//!
//! In a real party that is a QUIC tunnel: a throwaway endpoint, a connection
//! to the host it names, and a loopback port at the near end for the sync
//! client to dial. It is behind a trait for one reason — so the session's
//! tests can stand a loopback server in for the host and exercise joining,
//! leaving and resuming without the internet.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;

use crate::core::error::Result;
use crate::core::invite::Invite;
use crate::core::net::{ControlChannel, GuestTunnel, PartyEndpoint, TransportReport};

/// Opens a route to the host an invite names.
#[async_trait]
pub(crate) trait Dialer: Send + Sync {
    /// `control` receives whatever the host sends down the control channel.
    async fn dial(
        &self,
        invite: &Invite,
        control: Arc<dyn ControlChannel>,
    ) -> Result<Box<dyn Route>>;
}

/// An open route, held for as long as the guest is in the party.
#[async_trait]
pub(crate) trait Route: Send + Sync {
    /// Where the sync client connects. Always on loopback.
    fn local_addr(&self) -> SocketAddr;

    /// Whether the route still reaches the host. A route that has died stays
    /// dead — the sync client behind it retries forever and gets nowhere —
    /// so the only way back into the party is a new one.
    fn is_alive(&self) -> bool;

    /// How this machine is placed on the network, and how the route to the
    /// host is carried.
    fn report(&self) -> TransportReport;

    /// Sends `bytes` to the host down the control channel.
    async fn send_control(&self, bytes: &[u8]) -> Result<()>;

    /// Closes the route, telling the host rather than letting it time out.
    async fn close(self: Box<Self>);
}

/// The real thing: a tunnel over the syncparty network.
pub(crate) struct TunnelDialer;

#[async_trait]
impl Dialer for TunnelDialer {
    async fn dial(
        &self,
        invite: &Invite,
        control: Arc<dyn ControlChannel>,
    ) -> Result<Box<dyn Route>> {
        let host = invite.endpoint_id()?;

        let endpoint = PartyEndpoint::bind_joining().await?;
        match GuestTunnel::open(endpoint.inner().clone(), host, control).await {
            Ok(tunnel) => Ok(Box::new(Tunnel { tunnel, endpoint })),
            Err(error) => {
                endpoint.close().await;
                Err(error)
            }
        }
    }
}

/// Both halves have to outlive the call that created them. The tunnel is the
/// route the sync client's connection actually runs through, and the endpoint
/// owns the QUIC state underneath it, so dropping either mid-film disconnects
/// the guest.
struct Tunnel {
    tunnel: GuestTunnel,
    endpoint: PartyEndpoint,
}

#[async_trait]
impl Route for Tunnel {
    fn local_addr(&self) -> SocketAddr {
        self.tunnel.local_addr()
    }

    fn is_alive(&self) -> bool {
        self.tunnel.is_alive()
    }

    fn report(&self) -> TransportReport {
        self.endpoint.report(vec![self.tunnel.host_path()])
    }

    async fn send_control(&self, bytes: &[u8]) -> Result<()> {
        self.tunnel.send_control(bytes).await
    }

    async fn close(self: Box<Self>) {
        let Self { tunnel, endpoint } = *self;
        drop(tunnel);
        endpoint.close().await;
    }
}
