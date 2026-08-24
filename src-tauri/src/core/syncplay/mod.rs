//! Everything that speaks Syncplay: the wire protocol, the server process,
//! the room monitor and the client launcher.

mod launcher;
mod monitor;
pub mod protocol;
mod server;

pub use launcher::{find_client, find_player, ClientLauncher, MPV_KEY, SYNCPLAY_CLIENT_KEY};
pub use monitor::{
    MonitorConfig, RoomMonitor, RoomSnapshot, RoomView, WatchedFile, WatcherView, MONITOR_NICKNAME,
};
pub use protocol::hash_password;
pub use server::{NativeServer, ServerConfig, ServerController, ServerState, UvManagedServer};
