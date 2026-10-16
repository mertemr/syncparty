//! syncparty's own Syncplay client: one connection to the party's server, and
//! the player it keeps in step with the room.
//!
//! It used to be the Syncplay desktop client doing this, launched as a
//! separate program. Now it is this module, in two halves:
//!
//! - `engine`, upstream's synchronisation algorithm with no I/O in it, which
//!   decides what to tell the server and what to tell the player;
//! - `connection`, the task that owns the socket and the player and carries
//!   those decisions out — reconnecting when the connection drops, and giving
//!   up only when the server turns this client away.
//!
//! [`SyncClient`] is the handle the rest of the app holds. Everything it
//! exposes is a snapshot or a request; the task does the work.

mod connection;
mod engine;
mod notice;
#[cfg(test)]
mod tests;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::core::error::{Result, SyncPartyError};
use crate::core::player::{Player, PlayerEvent};
use crate::core::syncplay::RoomSnapshot;

use connection::Command;

/// Where to connect and who to be.
#[derive(Clone)]
pub struct ClientConfig {
    /// Loopback: the server itself for the host, the near end of the tunnel
    /// for a guest.
    pub address: SocketAddr,
    /// The nickname from settings. The server may hand back a different one.
    pub username: String,
    pub room: String,
    /// Plaintext; only its MD5 digest goes into the greeting.
    pub password: String,
    /// The `language` setting, which picks the language of on-screen notices.
    pub language: String,
    pub timing: ClientTiming,
}

/// Never prints the password, so a config can be logged.
impl std::fmt::Debug for ClientConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientConfig")
            .field("address", &self.address)
            .field("username", &self.username)
            .field("room", &self.room)
            .field("password", &"<redacted>")
            .field("language", &self.language)
            .field("timing", &self.timing)
            .finish()
    }
}

/// Every interval the client keeps, injectable so tests do not take minutes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClientTiming {
    pub connect_timeout: Duration,
    /// Silence from the server for this long means the connection is dead.
    /// Upstream's `PROTOCOL_TIMEOUT`, and the same figure the server uses.
    pub protocol_timeout: Duration,
    /// How often the player is looked at. Upstream's `PLAYER_ASK_DELAY`.
    pub player_poll: Duration,
    /// Retry `n` waits `reconnect_base · 2^min(n, 5)`.
    pub reconnect_base: Duration,
    /// How long after a file opens its position is not believed. Upstream's
    /// `MPV_NEWFILE_IGNORE_TIME`.
    pub load_settle: Duration,
}

impl Default for ClientTiming {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            protocol_timeout: Duration::from_millis(12_500),
            player_poll: Duration::from_millis(100),
            reconnect_base: Duration::from_millis(100),
            load_settle: Duration::from_secs(1),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionState {
    /// Not greeted yet on any connection.
    Connecting,
    Connected,
    /// A connection that was up dropped, and is being retried. `attempt`
    /// counts from 1 and starts again after each successful greeting.
    Reconnecting {
        attempt: u32,
    },
    /// The server turned this client away. Not retried.
    Rejected {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClientStatus {
    pub connection: ConnectionState,
    /// As the server assigned it, once greeted.
    pub username: Option<String>,
    pub ready: bool,
    /// From the server's `List`, this client included and the room monitor
    /// left out. Disconnected until the first list arrives.
    pub room: RoomSnapshot,
    pub player_attached: bool,
}

impl ClientStatus {
    fn starting() -> Self {
        Self {
            connection: ConnectionState::Connecting,
            username: None,
            ready: false,
            room: RoomSnapshot::disconnected(),
            player_attached: false,
        }
    }
}

/// A running client. Dropping it stops the task and closes the socket.
pub struct SyncClient {
    status: watch::Receiver<ClientStatus>,
    commands: mpsc::UnboundedSender<Command>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl SyncClient {
    /// Spawns the connection task and returns at once; connecting happens in
    /// the background.
    pub fn start(config: ClientConfig) -> Self {
        let (status_sender, status) = watch::channel(ClientStatus::starting());
        let (commands, receiver) = mpsc::unbounded_channel();
        let task = tokio::spawn(connection::run(config, status_sender, receiver));

        Self {
            status,
            commands,
            task: Mutex::new(Some(task)),
        }
    }

    pub fn status(&self) -> ClientStatus {
        self.status.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<ClientStatus> {
        self.status.clone()
    }

    /// Resolves once greeted (Ok) or refused (`SyncRejected`), or with
    /// `SyncFailed` after `timeout`.
    pub async fn wait_connected(&self, timeout: Duration) -> Result<()> {
        let mut status = self.status.clone();

        let outcome = tokio::time::timeout(timeout, async {
            loop {
                match &status.borrow_and_update().connection {
                    ConnectionState::Connected => return Ok(()),
                    ConnectionState::Rejected { message } => {
                        return Err(SyncPartyError::SyncRejected(message.clone()))
                    }
                    ConnectionState::Connecting | ConnectionState::Reconnecting { .. } => {}
                }

                if status.changed().await.is_err() {
                    return Err(SyncPartyError::SyncFailed(
                        "the sync client has stopped".to_owned(),
                    ));
                }
            }
        })
        .await;

        outcome.unwrap_or_else(|_| {
            Err(SyncPartyError::SyncFailed(format!(
                "the party did not answer within {} seconds",
                timeout.as_secs_f64().ceil()
            )))
        })
    }

    /// Hands the client a player and that player's event stream. Replaces
    /// any previous player, which is forgotten rather than closed.
    pub fn attach_player(
        &self,
        player: Arc<dyn Player>,
        events: mpsc::UnboundedReceiver<PlayerEvent>,
    ) {
        let _ = self.commands.send(Command::Attach(player, events));
    }

    /// Forgets the player. The connection stays up and reports no playstate
    /// until another one is attached.
    pub fn detach_player(&self) {
        let _ = self.commands.send(Command::Detach);
    }

    /// The user's own readiness toggle.
    pub fn set_ready(&self, ready: bool) {
        let _ = self.commands.send(Command::SetReady(ready));
    }

    /// Closes the connection and ends the task. Idempotent.
    pub async fn stop(&self) {
        let task = self
            .task
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(task) = task else {
            return;
        };

        let _ = self.commands.send(Command::Stop);
        let _ = task.await;
    }
}

impl Drop for SyncClient {
    fn drop(&mut self) {
        let task = self
            .task
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(task) = task {
            task.abort();
        }
    }
}
