//! Everything that speaks Syncplay: the wire protocol, the server, the sync
//! client that keeps a player in step with the room, the room monitor and the
//! client launcher.

pub mod client;
mod launcher;
mod monitor;
pub mod ping;
pub mod protocol;
mod server;

pub use client::{ClientConfig, ClientStatus, ClientTiming, ConnectionState, SyncClient};
pub use launcher::{find_client, find_player, ClientLauncher, MPV_KEY, SYNCPLAY_CLIENT_KEY};
pub use monitor::{
    MonitorConfig, RoomMonitor, RoomSnapshot, RoomView, WatchedFile, WatcherView, MONITOR_NICKNAME,
};
pub use protocol::hash_password;
pub use server::{NativeServer, ServerConfig, ServerController, ServerState};
