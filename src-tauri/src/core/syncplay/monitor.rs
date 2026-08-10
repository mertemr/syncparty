//! A read-only Syncplay client that reports who is in the room.
//!
//! The PowerShell prototype counted TCP connections and guessed at names with
//! peer addresses, which told you an IP and a port number. Attaching a real
//! client instead gives the panel what the server already knows: nicknames,
//! the file each person has open, and whether they are ready.
//!
//! The trade-off is that the monitor is a participant, so it shows up in
//! everybody's user list under [`MONITOR_NICKNAME`]. Syncplay has no
//! observer role, so this is the price of the data; the host can turn it off.

use std::collections::HashMap;
use std::time::Duration;

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use ts_rs::TS;

use crate::core::error::{Result, SyncPartyError};
use crate::core::syncplay::protocol::{
    affects_room_view, ClientMessage, Hello, RoomList, ServerMessage,
};

/// The name the monitor appears under. Recognisable on purpose — someone
/// looking at the user list should be able to tell it is not a person.
pub const MONITOR_NICKNAME: &str = "syncparty-panel";

const RECONNECT_DELAY: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// What the room panel renders.
#[derive(Debug, Clone, Default, PartialEq, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RoomSnapshot {
    pub connected: bool,
    pub rooms: Vec<RoomView>,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct RoomView {
    pub name: String,
    pub watchers: Vec<WatcherView>,
    /// Backward-compatible summary for older clients. False only when the
    /// files are known to be incompatible; a room still loading is not
    /// reported as a mismatch.
    pub everyone_on_the_same_file: bool,
    /// A more useful compatibility signal than filenames alone. Different
    /// releases of the same film often have different names, while their
    /// durations still reveal that they are suitable for synchronized play.
    pub file_compatibility: FileCompatibility,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export, rename_all = "camelCase")]
#[serde(rename_all = "camelCase")]
pub enum FileCompatibility {
    Waiting,
    Exact,
    DurationMatch,
    Mismatch,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WatcherView {
    pub name: String,
    pub file: Option<WatchedFile>,
    pub is_ready: bool,
    pub is_controller: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WatchedFile {
    pub name: String,
    pub duration_seconds: Option<f64>,
}

impl RoomSnapshot {
    /// Converts a raw `List` payload into the panel's view, leaving out the
    /// monitor's own entry so it never appears in syncparty's own UI.
    pub fn from_list(list: &RoomList, exclude: &str) -> Self {
        let mut rooms: Vec<RoomView> = list
            .iter()
            .map(|(room_name, members)| RoomView::build(room_name, members, exclude))
            .filter(|room| !room.watchers.is_empty())
            .collect();

        // Stable order, otherwise the list reshuffles on every refresh.
        rooms.sort_by(|a, b| a.name.cmp(&b.name));

        Self {
            connected: true,
            rooms,
        }
    }

    pub fn disconnected() -> Self {
        Self {
            connected: false,
            rooms: Vec::new(),
        }
    }
}

impl RoomView {
    fn build(
        name: &str,
        members: &HashMap<String, crate::core::syncplay::protocol::UserEntry>,
        exclude: &str,
    ) -> Self {
        let mut watchers: Vec<WatcherView> = members
            .iter()
            .filter(|(username, _)| username.as_str() != exclude)
            .map(|(username, entry)| WatcherView {
                name: username.clone(),
                file: entry.file.name.clone().map(|file_name| WatchedFile {
                    name: file_name,
                    duration_seconds: entry.file.duration,
                }),
                is_ready: entry.is_ready.unwrap_or(false),
                is_controller: entry.controller,
            })
            .collect();

        watchers.sort_by(|a, b| a.name.cmp(&b.name));

        let file_compatibility = file_compatibility(&watchers);

        Self {
            everyone_on_the_same_file: file_compatibility != FileCompatibility::Mismatch,
            file_compatibility,
            name: name.to_owned(),
            watchers,
        }
    }
}

const DURATION_TOLERANCE_SECONDS: f64 = 2.0;

fn file_compatibility(watchers: &[WatcherView]) -> FileCompatibility {
    if watchers.is_empty() || watchers.iter().any(|watcher| watcher.file.is_none()) {
        return FileCompatibility::Waiting;
    }

    let files: Vec<&WatchedFile> = watchers
        .iter()
        .filter_map(|watcher| watcher.file.as_ref())
        .collect();

    let Some(first) = files.first() else {
        return FileCompatibility::Waiting;
    };

    if files
        .iter()
        .all(|file| file.name.eq_ignore_ascii_case(&first.name))
    {
        return FileCompatibility::Exact;
    }

    let durations: Option<Vec<f64>> = files.iter().map(|file| file.duration_seconds).collect();
    let Some(durations) = durations else {
        return FileCompatibility::Mismatch;
    };

    let shortest = durations.iter().copied().fold(f64::INFINITY, f64::min);
    let longest = durations.iter().copied().fold(f64::NEG_INFINITY, f64::max);

    if shortest.is_finite()
        && longest.is_finite()
        && longest - shortest <= DURATION_TOLERANCE_SECONDS
    {
        FileCompatibility::DurationMatch
    } else {
        FileCompatibility::Mismatch
    }
}

#[derive(Debug, Clone)]
pub struct MonitorConfig {
    pub host: String,
    pub port: u16,
    /// Plaintext; hashed on the way into the greeting.
    pub password: String,
    pub room: String,
}

/// Keeps a hidden client attached to the server and republishes room state.
///
/// State is exposed through a [`watch`] channel rather than a stream of
/// deltas: the panel only ever renders the latest snapshot, and a late
/// subscriber gets the current picture instead of having to replay history.
pub struct RoomMonitor {
    snapshot: watch::Receiver<RoomSnapshot>,
    task: JoinHandle<()>,
}

impl RoomMonitor {
    /// Starts monitoring. Returns immediately — connecting happens in the
    /// background and retries, so a server that is still booting is fine.
    pub fn start(config: MonitorConfig) -> Self {
        let (sender, receiver) = watch::channel(RoomSnapshot::disconnected());
        let task = tokio::spawn(run(config, sender));

        Self {
            snapshot: receiver,
            task,
        }
    }

    /// Latest room state, updated whenever the server says something changed.
    pub fn subscribe(&self) -> watch::Receiver<RoomSnapshot> {
        self.snapshot.clone()
    }

    pub fn snapshot(&self) -> RoomSnapshot {
        self.snapshot.borrow().clone()
    }
}

impl Drop for RoomMonitor {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Connects, and keeps reconnecting until the task is aborted.
async fn run(config: MonitorConfig, snapshot: watch::Sender<RoomSnapshot>) {
    loop {
        if let Err(error) = session(&config, &snapshot).await {
            tracing::debug!(%error, "room monitor session ended");
        }

        let _ = snapshot.send(RoomSnapshot::disconnected());
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

/// One connection's lifetime: greet, ask for the list, then react until the
/// socket closes.
async fn session(config: &MonitorConfig, snapshot: &watch::Sender<RoomSnapshot>) -> Result<()> {
    let address = format!("{}:{}", config.host, config.port);

    let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&address))
        .await
        .map_err(|_| SyncPartyError::MonitorFailed(format!("{address} did not answer")))?
        .map_err(|error| SyncPartyError::MonitorFailed(format!("{address}: {error}")))?;

    // Room state arrives in small bursts; Nagle would add latency for nothing.
    let _ = stream.set_nodelay(true);

    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();

    let hello = Hello::new(
        MONITOR_NICKNAME,
        &config.room,
        Some(config.password.as_str()),
    );
    send(&mut write_half, &ClientMessage::Hello(hello)).await?;

    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|error| SyncPartyError::MonitorFailed(error.to_string()))?
    {
        if line.trim().is_empty() {
            continue;
        }

        match ServerMessage::from_line(&line)? {
            // The greeting means the password was accepted; ask who is here.
            ServerMessage::Hello => {
                send(&mut write_half, &ClientMessage::ListRequest).await?;
            }

            ServerMessage::List(rooms) => {
                let _ = snapshot.send(RoomSnapshot::from_list(&rooms, MONITOR_NICKNAME));
            }

            // Rather than reconstructing state from each delta, ask for the
            // authoritative list again. Rooms hold a handful of people, so
            // the request is cheap and cannot drift out of sync.
            ServerMessage::Set(set) if affects_room_view(&set) => {
                send(&mut write_half, &ClientMessage::ListRequest).await?;
            }

            ServerMessage::State {
                latency_calculation,
                server_ignoring_on_the_fly,
            } => {
                send(
                    &mut write_half,
                    &ClientMessage::PingReply {
                        latency_calculation,
                        client_latency_calculation: unix_seconds(),
                        server_ignoring_on_the_fly,
                    },
                )
                .await?;
            }

            ServerMessage::Error { message } => {
                return Err(SyncPartyError::MonitorFailed(message));
            }

            ServerMessage::Set(_) | ServerMessage::Ignored => {}
        }
    }

    Ok(())
}

async fn send(writer: &mut tokio::net::tcp::OwnedWriteHalf, message: &ClientMessage) -> Result<()> {
    writer
        .write_all(message.to_line()?.as_bytes())
        .await
        .map_err(|error| SyncPartyError::MonitorFailed(error.to_string()))
}

fn unix_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::syncplay::protocol::ServerMessage;

    fn list_from(json: &str) -> RoomList {
        let ServerMessage::List(rooms) = ServerMessage::from_line(json).expect("parse") else {
            panic!("expected a list");
        };
        rooms
    }

    #[test]
    fn hides_the_monitor_from_its_own_panel() {
        let rooms = list_from(
            r#"{"List":{"MovieNight":{
                "ahmet":{"file":{"name":"Film.mkv"},"isReady":true,"controller":false},
                "syncparty-panel":{"file":{},"isReady":false,"controller":false}
            }}}"#,
        );

        let snapshot = RoomSnapshot::from_list(&rooms, MONITOR_NICKNAME);

        assert_eq!(snapshot.rooms.len(), 1);
        assert_eq!(snapshot.rooms[0].watchers.len(), 1);
        assert_eq!(snapshot.rooms[0].watchers[0].name, "ahmet");
    }

    #[test]
    fn drops_rooms_that_hold_nobody_but_the_monitor() {
        let rooms = list_from(
            r#"{"List":{"MovieNight":{"syncparty-panel":{"file":{},"isReady":false,"controller":false}}}}"#,
        );

        assert!(RoomSnapshot::from_list(&rooms, MONITOR_NICKNAME)
            .rooms
            .is_empty());
    }

    #[test]
    fn reads_names_readiness_and_duration() {
        let rooms = list_from(
            r#"{"List":{"MovieNight":{
                "ahmet":{"file":{"name":"Film.mkv","duration":7200.0},"isReady":true,"controller":true}
            }}}"#,
        );

        let watcher = &RoomSnapshot::from_list(&rooms, MONITOR_NICKNAME).rooms[0].watchers[0];

        assert_eq!(watcher.name, "ahmet");
        assert!(watcher.is_ready);
        assert!(watcher.is_controller);
        let file = watcher.file.as_ref().expect("file");
        assert_eq!(file.name, "Film.mkv");
        assert_eq!(file.duration_seconds, Some(7200.0));
    }

    #[test]
    fn flags_a_room_where_two_people_opened_different_files() {
        let rooms = list_from(
            r#"{"List":{"MovieNight":{
                "ahmet":{"file":{"name":"Film.mkv"},"isReady":true,"controller":false},
                "mehmet":{"file":{"name":"BaskaFilm.mkv"},"isReady":true,"controller":false}
            }}}"#,
        );

        assert!(
            !RoomSnapshot::from_list(&rooms, MONITOR_NICKNAME).rooms[0].everyone_on_the_same_file
        );
    }

    #[test]
    fn a_room_is_in_sync_when_the_open_files_match() {
        let rooms = list_from(
            r#"{"List":{"MovieNight":{
                "ahmet":{"file":{"name":"Film.mkv"},"isReady":true,"controller":false},
                "mehmet":{"file":{"name":"Film.mkv"},"isReady":false,"controller":false}
            }}}"#,
        );

        assert!(
            RoomSnapshot::from_list(&rooms, MONITOR_NICKNAME).rooms[0].everyone_on_the_same_file
        );
    }

    #[test]
    fn different_names_are_compatible_when_durations_match() {
        let rooms = list_from(
            r#"{"List":{"MovieNight":{
                "ahmet":{"file":{"name":"Film.1080p.mkv","duration":7200.0},"isReady":true,"controller":false},
                "mehmet":{"file":{"name":"Film.4k.webm","duration":7201.5},"isReady":true,"controller":false}
            }}}"#,
        );

        let room = &RoomSnapshot::from_list(&rooms, MONITOR_NICKNAME).rooms[0];
        assert_eq!(room.file_compatibility, FileCompatibility::DurationMatch);
        assert!(room.everyone_on_the_same_file);
    }

    #[test]
    fn durations_outside_the_tolerance_are_a_mismatch() {
        let rooms = list_from(
            r#"{"List":{"MovieNight":{
                "ahmet":{"file":{"name":"Film.mkv","duration":7200.0},"isReady":true,"controller":false},
                "mehmet":{"file":{"name":"Wrong.mkv","duration":7210.0},"isReady":true,"controller":false}
            }}}"#,
        );

        let room = &RoomSnapshot::from_list(&rooms, MONITOR_NICKNAME).rooms[0];
        assert_eq!(room.file_compatibility, FileCompatibility::Mismatch);
        assert!(!room.everyone_on_the_same_file);
    }

    #[test]
    fn somebody_who_has_opened_nothing_yet_is_not_a_mismatch() {
        let rooms = list_from(
            r#"{"List":{"MovieNight":{
                "ahmet":{"file":{"name":"Film.mkv"},"isReady":true,"controller":false},
                "mehmet":{"file":{},"isReady":false,"controller":false}
            }}}"#,
        );

        let room = &RoomSnapshot::from_list(&rooms, MONITOR_NICKNAME).rooms[0];
        assert_eq!(room.file_compatibility, FileCompatibility::Waiting);
        assert!(room.everyone_on_the_same_file);
        assert!(room.watchers.iter().any(|watcher| watcher.file.is_none()));
    }

    #[test]
    fn watchers_and_rooms_come_back_in_a_stable_order() {
        let rooms = list_from(
            r#"{"List":{
                "Zulu":{"mehmet":{"file":{},"isReady":false,"controller":false}},
                "Alpha":{"zeynep":{"file":{},"isReady":false,"controller":false},
                         "ahmet":{"file":{},"isReady":false,"controller":false}}
            }}"#,
        );

        let snapshot = RoomSnapshot::from_list(&rooms, MONITOR_NICKNAME);

        assert_eq!(
            snapshot
                .rooms
                .iter()
                .map(|r| r.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Alpha", "Zulu"]
        );
        assert_eq!(
            snapshot.rooms[0]
                .watchers
                .iter()
                .map(|w| w.name.as_str())
                .collect::<Vec<_>>(),
            vec!["ahmet", "zeynep"]
        );
    }

    #[test]
    fn a_disconnected_snapshot_is_empty_and_says_so() {
        let snapshot = RoomSnapshot::disconnected();

        assert!(!snapshot.connected);
        assert!(snapshot.rooms.is_empty());
    }
}
