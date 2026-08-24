//! Starting and stopping the sync server.

mod auth;
mod ignore;
mod registry;
mod room;
mod session;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, RwLock};
use tokio::task::{JoinHandle, JoinSet};
use ts_rs::TS;

use crate::core::error::{Result, SyncPartyError};
use crate::core::events::{AppEvent, EventBus};
use crate::core::syncplay::server::registry::Registry;

/// How long the accept loop waits after a failed accept. A transient failure
/// should not end the party, but a permanent one would spin at full speed.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// The address the Syncplay server listens on.
///
/// Loopback, always, and not configurable. Guests arrive through the host's
/// tunnel rather than by dialling this port, so there is no reason for it to
/// be visible on any network interface — not the local one and certainly not
/// the internet. Previously this was the machine's Tailscale address, which
/// was already narrow but still reachable by every peer on that tailnet.
const BIND_ADDRESS: Ipv4Addr = Ipv4Addr::LOCALHOST;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub port: u16,
    /// Plaintext. Reaches the server through the environment, never `argv`.
    pub password: String,
    /// Stable across restarts, or every room operator password breaks.
    pub salt: String,
}

impl ServerConfig {
    /// Where the tunnel and the host's own Syncplay client should connect.
    pub fn local_address(&self) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(BIND_ADDRESS), self.port)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum ServerState {
    Stopped,
    Running { port: u16 },
}

/// Owns the sync server.
///
/// A trait so the implementation behind it can change without anything above
/// noticing — which is exactly what happened: it used to spawn Python.
#[async_trait]
pub trait ServerController: Send + Sync {
    async fn start(&self, config: &ServerConfig) -> Result<()>;

    /// Stops the server and nothing else.
    ///
    /// Explicitly *not* the transport: the endpoint and the tunnel are the
    /// session's to tear down, and stopping a party must not disturb anything
    /// else the process is doing.
    async fn stop(&self) -> Result<()>;

    async fn state(&self) -> ServerState;
}

/// Runs the sync server in this process.
///
/// Takes no `AppPaths`: unlike the Python-backed implementation it has no
/// interpreter to find and no child whose output needs a log file. What it
/// used to learn by watching stdout it now simply knows.
pub struct NativeServer {
    bus: Arc<dyn EventBus>,
    running: Mutex<Option<RunningNativeServer>>,
}

struct RunningNativeServer {
    port: u16,
    /// Aborting this drops the listener and, with it, every live connection.
    accept: JoinHandle<()>,
}

impl NativeServer {
    pub fn new(bus: Arc<dyn EventBus>) -> Self {
        Self {
            bus,
            running: Mutex::new(None),
        }
    }
}

#[async_trait]
impl ServerController for NativeServer {
    /// Binds and returns.
    ///
    /// There is no readiness to wait for: the listener either binds or it does
    /// not, and the answer arrives now rather than after a poll loop discovers
    /// that a child died at startup.
    async fn start(&self, config: &ServerConfig) -> Result<()> {
        let mut running = self.running.lock().await;

        if running.is_some() {
            return Err(SyncPartyError::ServerAlreadyRunning);
        }

        let address = config.local_address();
        let listener = TcpListener::bind(address).await.map_err(|error| {
            SyncPartyError::ServerStartFailed(format!("could not listen on {address}: {error}"))
        })?;

        self.bus.publish(AppEvent::ServerLog {
            line: format!("sync server listening on {address}"),
            is_error: false,
        });

        let port = config.port;
        let accept = tokio::spawn(accept_loop(
            listener,
            Registry::shared(),
            Arc::new(config.clone()),
            Arc::clone(&self.bus),
        ));

        *running = Some(RunningNativeServer { port, accept });

        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        let mut running = self.running.lock().await;

        let Some(server) = running.take() else {
            return Ok(());
        };

        server.accept.abort();
        // Awaited rather than left to wind down on its own: until the task is
        // really gone it is still holding the port, and the next start would
        // fail on an address that looks in use.
        let _ = server.accept.await;

        self.bus.publish(AppEvent::ServerLog {
            line: "sync server stopped".to_owned(),
            is_error: false,
        });

        Ok(())
    }

    async fn state(&self) -> ServerState {
        match self.running.lock().await.as_ref() {
            Some(server) => ServerState::Running { port: server.port },
            None => ServerState::Stopped,
        }
    }
}

/// Accepts connections until it is aborted.
///
/// The connections are owned here rather than detached, because that is what
/// makes stopping mean something: aborting this task drops the `JoinSet`, and
/// a dropped `JoinSet` aborts everything still in it. Detached tasks would
/// leave guests connected to a party that had ended.
async fn accept_loop(
    listener: TcpListener,
    registry: Arc<RwLock<Registry>>,
    config: Arc<ServerConfig>,
    bus: Arc<dyn EventBus>,
) {
    let mut sessions = JoinSet::new();

    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    let registry = Arc::clone(&registry);
                    let config = Arc::clone(&config);
                    let bus = Arc::clone(&bus);

                    sessions.spawn(async move {
                        if let Err(error) = session::serve(stream, registry, config, Arc::clone(&bus)).await {
                            bus.publish(AppEvent::ServerLog {
                                line: format!("{peer} disconnected: {error}"),
                                is_error: true,
                            });
                        }
                    });
                }
                Err(error) => {
                    bus.publish(AppEvent::ServerLog {
                        line: format!("could not accept a connection: {error}"),
                        is_error: true,
                    });
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                }
            },
            // Reaped so a long party does not accumulate finished tasks.
            Some(_) = sessions.join_next(), if !sessions.is_empty() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::events::NullEventBus;

    fn config() -> ServerConfig {
        ServerConfig {
            port: 8999,
            password: "swordfish".to_owned(),
            salt: "PEPPER".to_owned(),
        }
    }

    fn server() -> NativeServer {
        NativeServer::new(Arc::new(NullEventBus))
    }

    /// A port the kernel has just confirmed is free, so tests can run in
    /// parallel. A fixed one would mean whichever test bound it first broke
    /// every other.
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("a free port")
            .local_addr()
            .expect("address")
            .port()
    }

    fn config_on(port: u16) -> ServerConfig {
        ServerConfig {
            port,
            password: "swordfish".to_owned(),
            salt: "PEPPER".to_owned(),
        }
    }

    #[tokio::test]
    async fn a_fresh_controller_reports_itself_stopped() {
        assert_eq!(server().state().await, ServerState::Stopped);
    }

    #[tokio::test]
    async fn stopping_something_that_never_started_is_not_an_error() {
        assert!(server().stop().await.is_ok());
    }

    #[tokio::test]
    async fn starting_twice_is_refused() {
        let server = server();
        let config = config_on(free_port());
        server.start(&config).await.expect("first start");

        assert!(matches!(
            server.start(&config).await,
            Err(SyncPartyError::ServerAlreadyRunning)
        ));

        server.stop().await.expect("stop");
    }

    #[tokio::test]
    async fn a_started_server_reports_the_port_it_is_on() {
        let server = server();
        let config = config_on(free_port());
        server.start(&config).await.expect("start");

        assert_eq!(
            server.state().await,
            ServerState::Running { port: config.port }
        );

        server.stop().await.expect("stop");
    }

    #[tokio::test]
    async fn a_started_server_accepts_a_connection_on_loopback() {
        let server = server();
        let config = config_on(free_port());
        server.start(&config).await.expect("start");

        assert!(
            tokio::net::TcpStream::connect(config.local_address())
                .await
                .is_ok(),
            "the port is listening the moment start returns"
        );

        server.stop().await.expect("stop");
    }

    /// There is no readiness to poll for any more, so a bind failure is
    /// immediate rather than something a fifteen-second loop discovers.
    #[tokio::test]
    async fn a_port_already_in_use_fails_immediately_rather_than_after_a_timeout() {
        let config = config_on(free_port());
        let _squatter = tokio::net::TcpListener::bind(config.local_address())
            .await
            .expect("squatter");

        let started = tokio::time::timeout(Duration::from_secs(1), server().start(&config))
            .await
            .expect("start must not sit in a fifteen-second poll loop");

        assert!(started.is_err());
    }

    #[tokio::test]
    async fn stopping_closes_the_listener() {
        let server = server();
        let config = config_on(free_port());
        server.start(&config).await.expect("start");
        server.stop().await.expect("stop");

        assert!(
            tokio::net::TcpStream::connect(config.local_address())
                .await
                .is_err(),
            "a stopped server must not still be holding the port"
        );
    }

    /// Loopback, and not configurable. Guests arrive through the host's
    /// tunnel, so there is no interface this should be visible on.
    #[test]
    fn the_local_address_is_loopback_and_is_where_the_tunnel_connects() {
        assert_eq!(
            config().local_address(),
            "127.0.0.1:8999".parse::<SocketAddr>().expect("address")
        );
    }
}
