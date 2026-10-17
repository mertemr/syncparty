//! Everything that speaks Syncplay's protocol: the wire format, the server,
//! the sync client that keeps a player in step with the room, and the room
//! monitor.

pub mod client;
mod monitor;
pub mod ping;
pub mod protocol;
mod server;

pub use client::{ClientConfig, ClientStatus, ClientTiming, ConnectionState, SyncClient};
pub use monitor::{
    MonitorConfig, RoomMonitor, RoomSnapshot, RoomView, WatchedFile, WatcherView, MONITOR_NICKNAME,
};
pub use protocol::hash_password;
pub use server::{NativeServer, ServerConfig, ServerController, ServerState, ServerTiming};
