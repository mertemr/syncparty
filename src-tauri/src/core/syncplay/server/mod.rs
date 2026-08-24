//! Starting and stopping the sync server.

mod auth;
mod ignore;
mod registry;
mod room;
mod session;

use std::fs::OpenOptions;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpListener;
use tokio::process::Child;
use tokio::sync::{Mutex, RwLock};
use tokio::task::{JoinHandle, JoinSet};
use ts_rs::TS;

use crate::core::error::{Result, SyncPartyError};
use crate::core::events::{AppEvent, EventBus};
use crate::core::paths::AppPaths;
use crate::core::process;
use crate::core::syncplay::server::registry::Registry;

/// How long to wait for the server to start listening before giving up.
const READY_TIMEOUT: Duration = Duration::from_secs(15);
const READY_POLL_INTERVAL: Duration = Duration::from_millis(250);

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

/// Owns the Syncplay server process.
///
/// A trait so the Python-backed implementation below can eventually be
/// replaced by a native one without anything above noticing.
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

/// Runs Syncplay out of the `uv`-managed virtual environment.
pub struct UvManagedServer {
    paths: AppPaths,
    bus: Arc<dyn EventBus>,
    running: Mutex<Option<RunningServer>>,
}

struct RunningServer {
    child: Child,
    port: u16,
}

impl UvManagedServer {
    pub fn new(paths: AppPaths, bus: Arc<dyn EventBus>) -> Self {
        Self {
            paths,
            bus,
            running: Mutex::new(None),
        }
    }

    /// Builds the server's argument list.
    ///
    /// Note what is *not* here: `--password` and `--salt`. Syncplay reads both
    /// from `SYNCPLAY_PASSWORD` and `SYNCPLAY_SALT`, so keeping them out of
    /// `argv` keeps them out of the process table, where any local program
    /// could otherwise read them.
    fn arguments(&self, config: &ServerConfig) -> Vec<String> {
        vec![
            "-u".to_owned(), // unbuffered, so log lines arrive as they happen
            self.paths
                .server_entrypoint()
                .to_string_lossy()
                .into_owned(),
            "--port".to_owned(),
            config.port.to_string(),
            "--isolate-rooms".to_owned(),
            "--ipv4-only".to_owned(),
            "--interface-ipv4".to_owned(),
            BIND_ADDRESS.to_string(),
        ]
    }

    /// Polls the listening socket until the server answers.
    ///
    /// Deliberately not a check for the welcome banner: that string is
    /// translated, so matching on it breaks the moment the machine is not in
    /// English. A successful connection means the same thing in every locale.
    ///
    /// The child is checked alongside the socket, because a port that answers
    /// is not proof that *our* server answered. A stale server left holding
    /// the port — which happens, since a force-killed syncparty cannot take
    /// its child down with it — would otherwise look like a clean start while
    /// the real child had already exited with "address in use".
    async fn await_ready(&self, config: &ServerConfig, child: &mut Child) -> Result<()> {
        let address = config.local_address();
        let deadline = tokio::time::Instant::now() + READY_TIMEOUT;

        while tokio::time::Instant::now() < deadline {
            if let Ok(Some(status)) = child.try_wait() {
                return Err(SyncPartyError::ServerStartFailed(format!(
                    "the server stopped immediately ({status}) — {address} may already be in use"
                )));
            }

            if tokio::net::TcpStream::connect(address).await.is_ok() {
                return Ok(());
            }

            tokio::time::sleep(READY_POLL_INTERVAL).await;
        }

        Err(SyncPartyError::ServerStartFailed(format!(
            "nothing was listening on {address} after {} seconds",
            READY_TIMEOUT.as_secs()
        )))
    }

    /// Forwards the child's output to the UI and the log file.
    fn pump_output(&self, child: &mut Child) {
        let log_path = self.paths.server_log();
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Each server run gets one fresh diagnostic log; stale output is more
        // misleading than useful when troubleshooting a new party.
        let _ = reset_log(&log_path);

        if let Some(stdout) = child.stdout.take() {
            spawn_reader(
                BufReader::new(stdout),
                Arc::clone(&self.bus),
                false,
                log_path.clone(),
            );
        }

        if let Some(stderr) = child.stderr.take() {
            spawn_reader(
                BufReader::new(stderr),
                Arc::clone(&self.bus),
                true,
                log_path,
            );
        }
    }
}

fn reset_log(path: &Path) -> std::io::Result<()> {
    std::fs::File::create(path).map(drop)
}

fn spawn_reader<R>(reader: BufReader<R>, bus: Arc<dyn EventBus>, is_error: bool, log_path: PathBuf)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = reader.lines();
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .ok();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(log) = &mut log {
                let stream = if is_error { "stderr" } else { "stdout" };
                let _ = writeln!(log, "[{stream}] {line}");
            }
            bus.publish(AppEvent::ServerLog { line, is_error });
        }
    });
}

#[async_trait]
impl ServerController for UvManagedServer {
    async fn start(&self, config: &ServerConfig) -> Result<()> {
        let mut running = self.running.lock().await;

        if running.is_some() {
            return Err(SyncPartyError::ServerAlreadyRunning);
        }

        let python = self.paths.server_python();
        if !python.is_file() {
            return Err(SyncPartyError::DependencyMissing(
                "Syncplay server runtime".to_owned(),
            ));
        }

        let mut child = process::spawnable(&python)
            .args(self.arguments(config))
            .current_dir(self.paths.syncplay_source_dir())
            .env("SYNCPLAY_PASSWORD", &config.password)
            .env("SYNCPLAY_SALT", &config.salt)
            // Syncplay prints non-ASCII in several languages; without this the
            // child dies on a UnicodeEncodeError when the console code page
            // cannot represent them.
            .env("PYTHONIOENCODING", "utf-8")
            // Windows does not tear down children with their parent, so
            // without this a normal shutdown can leave the server running and
            // holding the port. It does not help if syncparty is force-killed
            // — destructors do not run then — so an orphan is still possible.
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| SyncPartyError::ServerStartFailed(error.to_string()))?;

        self.pump_output(&mut child);

        // Readiness is settled before the child is handed over, so a failed
        // start leaves nothing behind for the next attempt to collide with.
        if let Err(error) = self.await_ready(config, &mut child).await {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(error);
        }

        *running = Some(RunningServer {
            child,
            port: config.port,
        });

        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        let mut running = self.running.lock().await;

        let Some(mut server) = running.take() else {
            return Ok(());
        };

        let _ = server.child.kill().await;
        let _ = server.child.wait().await;
        Ok(())
    }

    async fn state(&self) -> ServerState {
        match self.running.lock().await.as_ref() {
            Some(server) => ServerState::Running { port: server.port },
            None => ServerState::Stopped,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::events::NullEventBus;

    fn uv_server() -> UvManagedServer {
        UvManagedServer::new(
            AppPaths::rooted_at(std::env::temp_dir().join("syncparty-server-test")),
            Arc::new(NullEventBus),
        )
    }

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

    #[test]
    fn the_password_and_salt_never_reach_the_command_line() {
        let arguments = uv_server().arguments(&config());

        assert!(
            !arguments.iter().any(|a| a.contains("swordfish")),
            "the password must travel by environment variable"
        );
        assert!(!arguments.iter().any(|a| a.contains("PEPPER")));
        assert!(!arguments.iter().any(|a| a == "--password"));
        assert!(!arguments.iter().any(|a| a == "--salt"));
    }

    #[test]
    fn binds_only_to_loopback_so_nothing_on_the_network_can_reach_it() {
        let arguments = uv_server().arguments(&config());

        let index = arguments
            .iter()
            .position(|a| a == "--interface-ipv4")
            .expect("--interface-ipv4");
        assert_eq!(arguments[index + 1], "127.0.0.1");
        assert!(arguments.contains(&"--ipv4-only".to_owned()));
        assert!(
            !arguments.iter().any(|a| a == "0.0.0.0"),
            "binding to every interface would put the server on the internet"
        );
    }

    #[test]
    fn the_local_address_is_where_the_tunnel_should_connect() {
        assert_eq!(
            config().local_address(),
            "127.0.0.1:8999".parse::<SocketAddr>().expect("address")
        );
    }

    #[test]
    fn isolates_rooms_and_runs_python_unbuffered() {
        let arguments = uv_server().arguments(&config());

        assert!(arguments.contains(&"--isolate-rooms".to_owned()));
        assert_eq!(arguments[0], "-u");
    }

    #[test]
    fn starting_a_server_replaces_an_old_log() {
        let directory =
            std::env::temp_dir().join(format!("syncparty-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("directory");
        let log = directory.join("syncplay-server.log");
        std::fs::write(&log, "stale output").expect("seed log");

        reset_log(&log).expect("reset");

        assert_eq!(std::fs::read_to_string(log).expect("read log"), "");
    }
}
