//! Watching a party on this machine: one sync client and the player it keeps
//! in step.
//!
//! Host and guest watch the same way — the only difference is where the sync
//! client connects. A host's goes straight to its own server on loopback; a
//! guest's goes to the near end of the tunnel to the host.
//!
//! These are the types the UI sees. They are deliberately coarse: whether the
//! connection is up, whether the player is open and what it is playing. The
//! position is not here, because it changes ten times a second and the UI has
//! nothing to do with it.

use serde::Serialize;
use ts_rs::TS;

use crate::core::syncplay::WatchedFile;

/// Everything the watch card needs.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WatchState {
    pub role: WatchRole,
    pub room: String,
    /// The name the server knows this machine by. Not always the nickname in
    /// settings: a name already taken in the room gets something appended.
    /// `None` until the server has answered.
    pub username: Option<String>,
    pub connection: WatchConnection,
    pub player: PlayerStatus,
    pub ready: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum WatchRole {
    Host,
    Guest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum WatchConnection {
    Connecting,
    Connected,
    /// The connection dropped and is being retried. `attempt` counts from 1.
    Reconnecting {
        attempt: u32,
    },
    /// The server turned this client away — a wrong password, usually. Not
    /// retried: trying the same thing again would be turned away again.
    Rejected {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlayerStatus {
    Starting,
    /// Open, with nothing loaded yet.
    Idle,
    Loaded {
        file: WatchedFile,
        paused: bool,
    },
    /// The user closed it. The party goes on; it can be reopened.
    Closed,
    Failed {
        message: String,
    },
}
