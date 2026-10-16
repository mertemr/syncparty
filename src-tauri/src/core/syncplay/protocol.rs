//! The Syncplay wire protocol: newline-delimited JSON objects.
//!
//! Only the slice syncparty needs is modelled here: enough for the monitor to
//! observe a room, for the sync client to take part in one, and for the server
//! to host one. Every shape below was read off the Syncplay 1.7.x source
//! rather than guessed, because a near-miss here fails silently — the client
//! connects, sees nothing, and the film just never moves.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::core::error::{Result, SyncPartyError};

/// Sent as the client version. Syncplay clients report `1.2.255` for
/// compatibility with old servers and put the real version in `realversion`.
const PROTOCOL_VERSION: &str = "1.2.255";
const REAL_VERSION: &str = "1.7.5";

/// The limits a server reports in `Hello`, from Syncplay's `constants.py`.
/// Clients truncate against these, so they are part of the wire contract
/// rather than our own policy.
pub const MAX_CHAT_MESSAGE_LENGTH: usize = 150;
pub const MAX_USERNAME_LENGTH: usize = 16;
const MAX_ROOM_NAME_LENGTH: usize = 35;
const MAX_FILENAME_LENGTH: usize = 250;

/// Hashes a server password the way Syncplay does.
///
/// The server MD5-hashes whatever it was given at startup and compares that
/// against the digest in `Hello`, so a monitor sending the plaintext password
/// is simply rejected. Not a security measure — matching it is a protocol
/// requirement.
pub fn hash_password(plain: &str) -> String {
    use md5::{Digest, Md5};

    let mut hasher = Md5::new();
    hasher.update(plain.as_bytes());
    hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(32), |mut out, byte| {
            use std::fmt::Write;
            let _ = write!(out, "{byte:02x}");
            out
        })
}

// ---------------------------------------------------------------- outgoing

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RoomRef {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientFeatures {
    pub shared_playlists: bool,
    pub chat: bool,
    pub feature_list: bool,
    pub readiness: bool,
    pub managed_rooms: bool,
    pub persistent_rooms: bool,
    /// Which of upstream's two interfaces the client is. Purely informative —
    /// the server only echoes it to the room — so the observer leaves it out
    /// rather than pretend to be either.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_mode: Option<String>,
}

impl ClientFeatures {
    /// What a read-only observer supports: it reports readiness so it appears
    /// correctly in other clients, and opts out of everything that would let
    /// it change the room.
    pub fn observer() -> Self {
        Self {
            shared_playlists: false,
            chat: false,
            feature_list: true,
            readiness: true,
            managed_rooms: false,
            persistent_rooms: false,
            ui_mode: None,
        }
    }

    /// What the sync client supports.
    ///
    /// Playlists and managed rooms stay off because syncparty has no UI for
    /// either, and a client that claims a feature is sent its messages.
    /// Readiness is on because unpausing depends on it. Chat is claimed so the
    /// room treats this client like any other GUI client; incoming chat is
    /// simply not shown yet.
    pub fn participant() -> Self {
        Self {
            shared_playlists: false,
            chat: true,
            feature_list: true,
            readiness: true,
            managed_rooms: false,
            persistent_rooms: false,
            ui_mode: Some("GUI".to_owned()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Hello {
    pub username: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    pub room: RoomRef,
    pub version: String,
    pub realversion: String,
    pub features: ClientFeatures,
}

impl Hello {
    /// Builds the monitor's greeting, hashing `password` on the way in.
    pub fn new(username: &str, room: &str, password: Option<&str>) -> Self {
        Self::with_features(username, room, password, ClientFeatures::observer())
    }

    /// Builds the sync client's greeting.
    pub fn participant(username: &str, room: &str, password: Option<&str>) -> Self {
        Self::with_features(username, room, password, ClientFeatures::participant())
    }

    fn with_features(
        username: &str,
        room: &str,
        password: Option<&str>,
        features: ClientFeatures,
    ) -> Self {
        Self {
            username: username.to_owned(),
            password: password.filter(|p| !p.is_empty()).map(hash_password),
            room: RoomRef {
                name: room.to_owned(),
            },
            version: PROTOCOL_VERSION.to_owned(),
            realversion: REAL_VERSION.to_owned(),
            features,
        }
    }
}

/// Where the client's player is, as it goes out in a `State`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClientPlaystate {
    pub position: f64,
    pub paused: bool,
    /// Sent only when true. Upstream leaves the key out otherwise, and the
    /// server reads a missing key as "no seek".
    pub do_seek: bool,
}

/// One `State` from the sync client: a reply to the server's, or the report
/// of something the user did.
#[derive(Debug, Clone, PartialEq)]
pub struct ClientState {
    /// Absent while the client has nothing trustworthy to report — before it
    /// has applied the room's state, or while its own change is in flight.
    pub playstate: Option<ClientPlaystate>,
    /// The server's stamp, echoed verbatim so it can measure the round trip.
    pub latency_calculation: Option<f64>,
    /// This side's own stamp, which the server echoes back the same way.
    pub client_latency_calculation: f64,
    /// The last round trip this side measured, never negative on the wire.
    pub client_rtt: f64,
    pub ignoring_on_the_fly: IgnoringOnTheFly,
}

/// The file the client has open, as the room is told about it.
#[derive(Debug, Clone, PartialEq)]
pub struct FileReport {
    pub name: String,
    /// Zero when the player does not know, as upstream sends it.
    pub duration: f64,
    /// Zero when unknown; upstream treats a zero size as matching anything.
    pub size: u64,
}

/// A message syncparty sends to the server.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientMessage {
    Hello(Hello),
    /// `{"List": null}` — asks for every room and watcher.
    ListRequest,
    /// A ping reply carrying no playback state.
    ///
    /// Omitting `playstate` is what makes the monitor passive: the server
    /// treats a `None` paused flag as "no change", so nothing it sends can
    /// pause, unpause or seek anybody's film.
    PingReply {
        latency_calculation: Option<f64>,
        client_latency_calculation: f64,
        /// Acknowledges the server's forced state sequence without echoing
        /// any playback data back into the room.
        server_ignoring_on_the_fly: Option<u64>,
    },
    State(ClientState),
    /// `{"Set": {"ready": …}}`. `manually_initiated` is false when readiness
    /// follows from pausing or unpausing, which is how other clients tell a
    /// deliberate toggle from a side effect.
    SetReady {
        is_ready: bool,
        manually_initiated: bool,
    },
    SetFile(FileReport),
}

impl ClientMessage {
    /// Renders the message as one protocol line, newline included.
    pub fn to_line(&self) -> Result<String> {
        let value = match self {
            Self::Hello(hello) => serde_json::json!({ "Hello": hello }),
            Self::ListRequest => serde_json::json!({ "List": serde_json::Value::Null }),
            Self::PingReply {
                latency_calculation,
                client_latency_calculation,
                server_ignoring_on_the_fly,
            } => state_json(
                None,
                *latency_calculation,
                *client_latency_calculation,
                0.0,
                (*server_ignoring_on_the_fly, None),
            ),
            Self::State(state) => state_json(
                state.playstate,
                state.latency_calculation,
                state.client_latency_calculation,
                state.client_rtt,
                state.ignoring_on_the_fly,
            ),
            Self::SetReady {
                is_ready,
                manually_initiated,
            } => serde_json::json!({
                "Set": { "ready": { "isReady": is_ready, "manuallyInitiated": manually_initiated } }
            }),
            Self::SetFile(file) => serde_json::json!({
                "Set": { "file": { "name": file.name, "duration": file.duration, "size": file.size } }
            }),
        };

        Ok(format!("{}\r\n", serde_json::to_string(&value)?))
    }
}

/// Renders a client `State`, the way upstream's `sendState` builds it.
///
/// `ping` is always there, because the server measures the round trip from
/// it. Each counter half appears only while it is non-zero, and the envelope
/// only while either is.
fn state_json(
    playstate: Option<ClientPlaystate>,
    latency_calculation: Option<f64>,
    client_latency_calculation: f64,
    client_rtt: f64,
    (server, client): IgnoringOnTheFly,
) -> serde_json::Value {
    let mut state = serde_json::Map::new();

    if let Some(playstate) = playstate {
        let mut fields = serde_json::Map::new();
        fields.insert("position".to_owned(), playstate.position.into());
        fields.insert("paused".to_owned(), playstate.paused.into());
        if playstate.do_seek {
            fields.insert("doSeek".to_owned(), true.into());
        }
        state.insert("playstate".to_owned(), fields.into());
    }

    let mut ping = serde_json::Map::new();
    if let Some(value) = latency_calculation {
        ping.insert("latencyCalculation".to_owned(), value.into());
    }
    ping.insert(
        "clientLatencyCalculation".to_owned(),
        client_latency_calculation.into(),
    );
    ping.insert("clientRtt".to_owned(), client_rtt.max(0.0).into());
    state.insert("ping".to_owned(), ping.into());

    let mut envelope = serde_json::Map::new();
    if let Some(server) = server.filter(|value| *value != 0) {
        envelope.insert("server".to_owned(), server.into());
    }
    if let Some(client) = client.filter(|value| *value != 0) {
        envelope.insert("client".to_owned(), client.into());
    }
    if !envelope.is_empty() {
        state.insert("ignoringOnTheFly".to_owned(), envelope.into());
    }

    serde_json::json!({ "State": state })
}

// ---------------------------------------------------------------- outgoing

/// The `server`/`client` counter pair, exactly as `IgnoreGate::take_envelope`
/// hands it over, so wiring the two together is not a conversion.
pub type IgnoringOnTheFly = (Option<u64>, Option<u64>);

/// What this server tells clients it can do.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerFeatures {
    pub isolate_rooms: bool,
    pub readiness: bool,
    pub managed_rooms: bool,
    pub persistent_rooms: bool,
    pub chat: bool,
    pub set_others_readiness: bool,
    pub max_chat_message_length: usize,
    pub max_username_length: usize,
    pub max_room_name_length: usize,
    pub max_filename_length: usize,
}

impl ServerFeatures {
    /// What this server actually does, which is not the same as what exists.
    ///
    /// Claiming a feature we have not written does not fail loudly: the client
    /// simply uses it and nothing happens. So `persistentRooms` stays false
    /// because rooms live in memory, and `setOthersReadiness` stays false
    /// because marking somebody else ready is not implemented.
    pub fn supported() -> Self {
        Self {
            isolate_rooms: true,
            readiness: true,
            managed_rooms: true,
            persistent_rooms: false,
            chat: true,
            set_others_readiness: false,
            max_chat_message_length: MAX_CHAT_MESSAGE_LENGTH,
            max_username_length: MAX_USERNAME_LENGTH,
            max_room_name_length: MAX_ROOM_NAME_LENGTH,
            max_filename_length: MAX_FILENAME_LENGTH,
        }
    }
}

/// One watcher as they appear in a `List`.
#[derive(Debug, Clone, Serialize)]
pub struct ListEntry {
    pub position: f64,
    pub file: serde_json::Value,
    pub controller: bool,
    #[serde(rename = "isReady")]
    pub is_ready: Option<bool>,
    pub features: serde_json::Value,
}

/// `{ room name -> { username -> entry } }`, the shape a `List` goes out in.
pub type ServerRoomList = HashMap<String, HashMap<String, ListEntry>>;

/// A message the server writes to a client.
#[derive(Debug, Clone)]
pub enum ServerToClient {
    Hello {
        username: String,
        room: String,
        /// Echoed straight back from the client's own greeting. Upstream keeps
        /// this so a 1.2.x client still works against a newer server; our own
        /// version travels separately as `realversion`.
        client_version: String,
        features: ServerFeatures,
    },
    /// TLS is refused rather than implemented.
    TlsRefusal,
    SetUser {
        username: String,
        room: String,
        file: Option<serde_json::Value>,
        event: Option<serde_json::Value>,
    },
    SetReady {
        username: String,
        is_ready: Option<bool>,
        manually_initiated: bool,
        set_by: Option<String>,
    },
    SetControllerAuth {
        user: String,
        room: String,
        success: bool,
    },
    Chat {
        username: String,
        message: String,
    },
    List(ServerRoomList),
    /// Relayed verbatim with the sender's name attached. The server does not
    /// interpret playlists, which is what lets the UI grow one later without
    /// this file changing again.
    SetPlaylistChange {
        user: String,
        files: serde_json::Value,
    },
    SetPlaylistIndex {
        user: String,
        index: serde_json::Value,
    },
    State {
        position: f64,
        paused: bool,
        do_seek: bool,
        set_by: Option<String>,
        latency_calculation: f64,
        server_rtt: f64,
        client_latency_calculation: Option<f64>,
        ignoring_on_the_fly: Option<IgnoringOnTheFly>,
    },
    Error {
        message: String,
    },
}

impl ServerToClient {
    /// Serialises to one CRLF-terminated line, mirroring
    /// [`ClientMessage::to_line`].
    pub fn to_line(&self) -> Result<String> {
        let value = match self {
            Self::Hello {
                username,
                room,
                client_version,
                features,
            } => serde_json::json!({
                "Hello": {
                    "username": username,
                    "room": { "name": room },
                    // Their version, echoed. Upstream keeps this so a 1.2.x
                    // client still works against a newer server; ours travels
                    // separately.
                    "version": client_version,
                    "realversion": REAL_VERSION,
                    "features": features,
                    "motd": "",
                }
            }),

            // A string rather than a boolean. The client answers this with
            // `"false" in answer`, which raises on a boolean and takes the
            // client down with it.
            Self::TlsRefusal => serde_json::json!({ "TLS": { "startTLS": "false" } }),

            Self::SetUser {
                username,
                room,
                file,
                event,
            } => {
                let mut user = serde_json::Map::new();
                user.insert("room".to_owned(), serde_json::json!({ "name": room }));
                if let Some(file) = file {
                    user.insert("file".to_owned(), file.clone());
                }
                if let Some(event) = event {
                    user.insert("event".to_owned(), event.clone());
                }

                let mut users = serde_json::Map::new();
                users.insert(username.clone(), user.into());

                serde_json::json!({ "Set": { "user": users } })
            }

            Self::SetReady {
                username,
                is_ready,
                manually_initiated,
                set_by,
            } => {
                let mut ready = serde_json::Map::new();
                ready.insert("username".to_owned(), serde_json::json!(username));
                ready.insert("isReady".to_owned(), serde_json::json!(is_ready));
                ready.insert(
                    "manuallyInitiated".to_owned(),
                    serde_json::json!(manually_initiated),
                );
                // Present only when somebody else did it, which is how the
                // client tells "I went ready" from "I was made ready".
                if let Some(set_by) = set_by {
                    ready.insert("setBy".to_owned(), serde_json::json!(set_by));
                }

                serde_json::json!({ "Set": { "ready": ready } })
            }

            // `user`, not `username`. Upstream is not consistent between the
            // two across `Set` payloads.
            Self::SetControllerAuth {
                user,
                room,
                success,
            } => serde_json::json!({
                "Set": { "controllerAuth": { "user": user, "room": room, "success": success } }
            }),

            Self::Chat { username, message } => serde_json::json!({
                "Chat": { "username": username, "message": message }
            }),

            Self::List(rooms) => serde_json::json!({ "List": rooms }),

            Self::SetPlaylistChange { user, files } => serde_json::json!({
                "Set": { "playlistChange": { "user": user, "files": files } }
            }),

            Self::SetPlaylistIndex { user, index } => serde_json::json!({
                "Set": { "playlistIndex": { "user": user, "index": index } }
            }),

            Self::State {
                position,
                paused,
                do_seek,
                set_by,
                latency_calculation,
                server_rtt,
                client_latency_calculation,
                ignoring_on_the_fly,
            } => {
                let mut ping = serde_json::Map::new();
                ping.insert(
                    "latencyCalculation".to_owned(),
                    serde_json::json!(latency_calculation),
                );
                ping.insert("serverRtt".to_owned(), serde_json::json!(server_rtt));
                if let Some(value) = client_latency_calculation {
                    ping.insert(
                        "clientLatencyCalculation".to_owned(),
                        serde_json::json!(value),
                    );
                }

                let mut state = serde_json::Map::new();
                state.insert("ping".to_owned(), ping.into());
                state.insert(
                    "playstate".to_owned(),
                    serde_json::json!({
                        "position": position,
                        "paused": paused,
                        "doSeek": do_seek,
                        "setBy": set_by,
                    }),
                );

                // Each half appears only while it is in play, and the whole
                // object only while one of them is.
                if let Some((server, client)) = ignoring_on_the_fly {
                    let mut envelope = serde_json::Map::new();
                    if let Some(server) = server {
                        envelope.insert("server".to_owned(), serde_json::json!(server));
                    }
                    if let Some(client) = client {
                        envelope.insert("client".to_owned(), serde_json::json!(client));
                    }
                    state.insert("ignoringOnTheFly".to_owned(), envelope.into());
                }

                serde_json::json!({ "State": state })
            }

            Self::Error { message } => serde_json::json!({ "Error": { "message": message } }),
        };

        Ok(format!("{}\r\n", serde_json::to_string(&value)?))
    }
}

// ---------------------------------------------------------------- incoming

/// The file a watcher currently has open. Every field is optional because the
/// server sends `{}` for somebody who has not opened anything.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct FileInfo {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default)]
    pub size: Option<serde_json::Value>,
}

impl FileInfo {
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UserEntry {
    #[serde(default)]
    pub file: FileInfo,
    #[serde(default, rename = "isReady")]
    pub is_ready: Option<bool>,
    #[serde(default)]
    pub controller: bool,
    #[serde(default)]
    pub position: Option<f64>,
}

/// `{ room name -> { username -> entry } }`, exactly as `List` returns it.
pub type RoomList = HashMap<String, HashMap<String, UserEntry>>;

/// A `Set: user` payload announcing a join, part or move.
#[derive(Debug, Clone, Deserialize)]
pub struct UserUpdate {
    #[serde(default)]
    pub room: Option<RoomName>,
    /// Present when the user opened, changed or closed a file. An empty
    /// object means they have nothing open any more.
    #[serde(default)]
    pub file: Option<FileInfo>,
    #[serde(default)]
    pub event: Option<UserEvent>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoomName {
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UserEvent {
    #[serde(default)]
    pub joined: Option<serde_json::Value>,
    #[serde(default)]
    pub left: Option<serde_json::Value>,
}

/// A `Set: ready` payload: somebody's readiness changed.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadyUpdate {
    pub username: String,
    /// `None` is "never said", which upstream sends for everyone who joins.
    pub is_ready: Option<bool>,
    /// Upstream reads a missing flag as true.
    pub manually_initiated: bool,
    pub set_by: Option<String>,
}

/// The server's greeting, which confirms the password was accepted.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServerHello {
    /// The name the server knows this connection by. Not always the one asked
    /// for: a name already taken gets something appended.
    pub username: Option<String>,
    pub room: Option<String>,
    /// Whether the server tracks readiness. Absent from very old servers,
    /// which is read as yes because every server this talks to has it.
    pub readiness: Option<bool>,
}

/// The playback half of a server `State`.
#[derive(Debug, Clone, PartialEq)]
pub struct IncomingPlaystate {
    /// A missing position is read as zero, as upstream reads it; only an
    /// explicit `null` leaves it unknown.
    pub position: Option<f64>,
    pub paused: Option<bool>,
    pub do_seek: Option<bool>,
    /// Who the room's state comes from: the last person to pause or seek, or
    /// on a heartbeat the watcher the room is following.
    pub set_by: Option<String>,
}

/// The timing half of a server `State`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct IncomingPing {
    /// The server's stamp, to be echoed back untouched.
    pub latency_calculation: Option<f64>,
    pub server_rtt: Option<f64>,
    /// This client's own stamp coming back, which is what its round trip is
    /// measured from. Present once per client report at most.
    pub client_latency_calculation: Option<f64>,
}

/// The `ignoringOnTheFly` half of a server `State`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IgnoringEcho {
    /// The server's own counter, present after it forced a state.
    pub server: Option<u64>,
    /// The client's counter, echoed once to acknowledge a change it sent.
    pub client: Option<u64>,
}

/// A server `State`, every part optional because upstream treats every part
/// as optional.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IncomingState {
    pub playstate: Option<IncomingPlaystate>,
    pub ping: Option<IncomingPing>,
    pub ignoring_on_the_fly: Option<IgnoringEcho>,
}

impl IncomingState {
    fn from_value(payload: &serde_json::Value) -> Self {
        let playstate = payload.get("playstate").map(|playstate| IncomingPlaystate {
            position: match playstate.get("position") {
                None => Some(0.0),
                Some(position) => position.as_f64(),
            },
            paused: playstate.get("paused").and_then(serde_json::Value::as_bool),
            do_seek: playstate.get("doSeek").and_then(serde_json::Value::as_bool),
            set_by: playstate
                .get("setBy")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        });

        let ping = payload.get("ping").map(|ping| IncomingPing {
            latency_calculation: ping
                .get("latencyCalculation")
                .and_then(serde_json::Value::as_f64),
            server_rtt: ping.get("serverRtt").and_then(serde_json::Value::as_f64),
            client_latency_calculation: ping
                .get("clientLatencyCalculation")
                .and_then(serde_json::Value::as_f64),
        });

        let ignoring_on_the_fly = payload.get("ignoringOnTheFly").map(|ignore| IgnoringEcho {
            server: ignore.get("server").and_then(serde_json::Value::as_u64),
            client: ignore.get("client").and_then(serde_json::Value::as_u64),
        });

        Self {
            playstate,
            ping,
            ignoring_on_the_fly,
        }
    }

    pub fn latency_calculation(&self) -> Option<f64> {
        self.ping.and_then(|ping| ping.latency_calculation)
    }

    pub fn server_ignoring_on_the_fly(&self) -> Option<u64> {
        self.ignoring_on_the_fly.and_then(|ignore| ignore.server)
    }
}

/// A message received from the server.
#[derive(Debug, Clone)]
pub enum ServerMessage {
    Hello(ServerHello),
    List(RoomList),
    /// Any `Set`. The specific payloads that matter are pulled out below.
    Set(serde_json::Map<String, serde_json::Value>),
    State(IncomingState),
    Error {
        message: String,
    },
    /// Something syncparty does not model. Ignored rather than fatal, so a
    /// newer server cannot break the monitor or the client.
    Ignored,
}

impl ServerMessage {
    /// The first message on a line. Syncplay sends one per line, which is all
    /// the monitor needs.
    pub fn from_line(line: &str) -> Result<Self> {
        Self::all_from_line(line)
            .map(|messages| messages.into_iter().next().unwrap_or(Self::Ignored))
            .map_err(|error| SyncPartyError::MonitorFailed(format!("malformed message: {error}")))
    }

    /// Every message on a line, for a reader that must not miss one.
    ///
    /// Upstream handles every key of an object, so a line carrying two is
    /// read as two messages. Keys come back in alphabetical order, because
    /// that is the order `serde_json` keeps them in.
    pub fn all_from_line(line: &str) -> std::result::Result<Vec<Self>, serde_json::Error> {
        let value: serde_json::Value = serde_json::from_str(line.trim())?;

        let Some(object) = value.as_object() else {
            return Ok(vec![Self::Ignored]);
        };

        Ok(object
            .iter()
            .map(|(key, payload)| Self::from_value(key, payload))
            .collect())
    }

    fn from_value(key: &str, payload: &serde_json::Value) -> Self {
        match key {
            "Hello" => Self::Hello(ServerHello {
                username: payload
                    .get("username")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                room: payload
                    .get("room")
                    .and_then(|room| room.get("name"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                readiness: payload
                    .get("features")
                    .and_then(|features| features.get("readiness"))
                    .and_then(serde_json::Value::as_bool),
            }),
            "List" => serde_json::from_value(payload.clone())
                .map(Self::List)
                .unwrap_or(Self::Ignored),
            "Set" => payload
                .as_object()
                .cloned()
                .map(Self::Set)
                .unwrap_or(Self::Ignored),
            "State" => Self::State(IncomingState::from_value(payload)),
            "Error" => Self::Error {
                message: payload
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("the server rejected the connection")
                    .to_owned(),
            },
            _ => Self::Ignored,
        }
    }
}

/// Extracts the `user` half of a `Set`, if present.
pub fn user_updates(
    set: &serde_json::Map<String, serde_json::Value>,
) -> Option<HashMap<String, UserUpdate>> {
    serde_json::from_value(set.get("user")?.clone()).ok()
}

/// Extracts the `ready` half of a `Set`, if present and well formed.
pub fn ready_update(set: &serde_json::Map<String, serde_json::Value>) -> Option<ReadyUpdate> {
    let ready = set.get("ready")?;

    Some(ReadyUpdate {
        username: ready.get("username")?.as_str()?.to_owned(),
        is_ready: ready.get("isReady").and_then(serde_json::Value::as_bool),
        manually_initiated: ready
            .get("manuallyInitiated")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        set_by: ready
            .get("setBy")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    })
}

/// Extracts the room a `Set: room` moves this client to, if present.
pub fn room_change(set: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    set.get("room")?.get("name")?.as_str().map(str::to_owned)
}

/// True when this `Set` changes something the room panel displays.
///
/// Used to decide whether to re-request the list; ignoring irrelevant `Set`s
/// keeps a chatty room from causing a request per keystroke.
pub fn affects_room_view(set: &serde_json::Map<String, serde_json::Value>) -> bool {
    ["user", "ready", "room", "playlistChange", "playlistIndex"]
        .iter()
        .any(|key| set.contains_key(*key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_passwords_the_way_syncplay_does() {
        // Reference digests from `hashlib.md5(b"...").hexdigest()`, which is
        // literally the call Syncplay makes.
        assert_eq!(hash_password(""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hash_password("abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            hash_password("swordfish"),
            "15b29ffdce66e10527a65bc6d71ad94d"
        );
    }

    #[test]
    fn hello_hashes_the_password_and_omits_an_empty_one() {
        let with_password = Hello::new("panel", "MovieNight", Some("swordfish"));
        assert_eq!(
            with_password.password.as_deref(),
            Some(hash_password("swordfish").as_str())
        );

        let without = Hello::new("panel", "MovieNight", Some(""));
        assert!(
            without.password.is_none(),
            "an empty password must be left out entirely"
        );
        assert!(Hello::new("panel", "MovieNight", None).password.is_none());
    }

    #[test]
    fn a_list_request_serialises_to_a_null_payload() {
        let line = ClientMessage::ListRequest.to_line().expect("line");

        assert_eq!(line.trim(), r#"{"List":null}"#);
        assert!(line.ends_with("\r\n"));
    }

    #[test]
    fn a_ping_reply_never_carries_playback_state() {
        let line = ClientMessage::PingReply {
            latency_calculation: Some(12.5),
            client_latency_calculation: 99.0,
            server_ignoring_on_the_fly: Some(3),
        }
        .to_line()
        .expect("line");

        assert!(
            !line.contains("playstate"),
            "the monitor must not be able to pause or seek the room"
        );
        assert!(line.contains("latencyCalculation"));
        assert!(line.contains(r#""ignoringOnTheFly":{"server":3}"#));
    }

    #[test]
    fn parses_a_populated_list() {
        let message = ServerMessage::from_line(
            r#"{"List":{"MovieNight":{"ahmet":{"position":0,"file":{"name":"Film.mkv","duration":7200.0},"controller":false,"isReady":true,"features":{}},"mehmet":{"position":0,"file":{},"controller":false,"isReady":false,"features":{}}}}}"#,
        )
        .expect("parse");

        let ServerMessage::List(rooms) = message else {
            panic!("expected a list");
        };

        let watchers = &rooms["MovieNight"];
        assert_eq!(watchers.len(), 2);
        assert_eq!(watchers["ahmet"].file.name.as_deref(), Some("Film.mkv"));
        assert_eq!(watchers["ahmet"].is_ready, Some(true));
        assert!(
            watchers["mehmet"].file.is_empty(),
            "an empty file object means nothing is open"
        );
    }

    #[test]
    fn parses_join_and_leave_events() {
        let message = ServerMessage::from_line(
            r#"{"Set":{"user":{"ahmet":{"room":{"name":"MovieNight"},"event":{"joined":true}}}}}"#,
        )
        .expect("parse");

        let ServerMessage::Set(set) = message else {
            panic!("expected a set");
        };
        assert!(affects_room_view(&set));

        let updates = user_updates(&set).expect("user updates");
        let update = &updates["ahmet"];
        assert_eq!(
            update.room.as_ref().unwrap().name.as_deref(),
            Some("MovieNight")
        );
        assert!(update.event.as_ref().unwrap().joined.is_some());
        assert!(update.event.as_ref().unwrap().left.is_none());
    }

    #[test]
    fn reads_the_ping_out_of_a_state_message() {
        let message = ServerMessage::from_line(
            r#"{"State":{"ping":{"latencyCalculation":1234.5,"serverRtt":0},"playstate":{"position":10.0,"paused":true},"ignoringOnTheFly":{"server":4}}}"#,
        )
        .expect("parse");

        let ServerMessage::State(state) = message else {
            panic!("expected a state");
        };
        assert_eq!(state.latency_calculation(), Some(1234.5));
        assert_eq!(state.server_ignoring_on_the_fly(), Some(4));
    }

    #[test]
    fn reads_the_whole_of_a_server_state() {
        let message = ServerMessage::from_line(
            r#"{"State":{"ping":{"latencyCalculation":1792190198.36,"serverRtt":0.2524,"clientLatencyCalculation":1792190198.259},"playstate":{"position":0.63,"paused":false,"doSeek":null,"setBy":"alice"},"ignoringOnTheFly":{"server":1,"client":1}}}"#,
        )
        .expect("parse");

        let ServerMessage::State(state) = message else {
            panic!("expected a state");
        };
        assert_eq!(
            state.playstate,
            Some(IncomingPlaystate {
                position: Some(0.63),
                paused: Some(false),
                do_seek: None,
                set_by: Some("alice".to_owned()),
            })
        );
        assert_eq!(
            state.ping,
            Some(IncomingPing {
                latency_calculation: Some(1792190198.36),
                server_rtt: Some(0.2524),
                client_latency_calculation: Some(1792190198.259),
            })
        );
        assert_eq!(
            state.ignoring_on_the_fly,
            Some(IgnoringEcho {
                server: Some(1),
                client: Some(1),
            })
        );
    }

    /// Upstream defaults a missing position to zero but keeps an explicit
    /// `null` as unknown, and only the second stops the state being applied.
    #[test]
    fn a_missing_position_is_zero_and_a_null_one_is_unknown() {
        let ServerMessage::State(missing) =
            ServerMessage::from_line(r#"{"State":{"playstate":{"paused":true}}}"#).expect("parse")
        else {
            panic!("expected a state");
        };
        assert_eq!(missing.playstate.expect("playstate").position, Some(0.0));
        assert!(missing.ping.is_none());

        let ServerMessage::State(null) =
            ServerMessage::from_line(r#"{"State":{"playstate":{"position":null,"paused":true}}}"#)
                .expect("parse")
        else {
            panic!("expected a state");
        };
        assert_eq!(null.playstate.expect("playstate").position, None);
    }

    #[test]
    fn the_greeting_carries_the_name_the_server_settled_on() {
        let message = ServerMessage::from_line(
            r#"{"Hello":{"username":"ahmet_","room":{"name":"MovieNight"},"version":"1.2.255","realversion":"1.7.6","features":{"readiness":true},"motd":""}}"#,
        )
        .expect("parse");

        let ServerMessage::Hello(hello) = message else {
            panic!("expected a greeting");
        };
        assert_eq!(hello.username.as_deref(), Some("ahmet_"));
        assert_eq!(hello.room.as_deref(), Some("MovieNight"));
        assert_eq!(hello.readiness, Some(true));
    }

    #[test]
    fn a_line_with_two_messages_yields_both() {
        let messages = ServerMessage::all_from_line(
            r#"{"List":{},"State":{"ping":{"latencyCalculation":1}}}"#,
        )
        .expect("parse");

        assert_eq!(messages.len(), 2);
        assert!(matches!(messages[0], ServerMessage::List(_)));
        assert!(matches!(messages[1], ServerMessage::State(_)));
    }

    #[test]
    fn malformed_json_is_an_error_rather_than_a_message() {
        assert!(ServerMessage::all_from_line("{not json").is_err());
        assert!(ServerMessage::from_line("{not json").is_err());
    }

    #[test]
    fn reads_readiness_with_upstreams_defaults() {
        let ServerMessage::Set(set) =
            ServerMessage::from_line(r#"{"Set":{"ready":{"username":"ahmet","isReady":null}}}"#)
                .expect("parse")
        else {
            panic!("expected a set");
        };

        assert_eq!(
            ready_update(&set),
            Some(ReadyUpdate {
                username: "ahmet".to_owned(),
                is_ready: None,
                manually_initiated: true,
                set_by: None,
            })
        );
    }

    #[test]
    fn a_user_update_carries_the_file_they_opened() {
        let ServerMessage::Set(set) = ServerMessage::from_line(
            r#"{"Set":{"user":{"ahmet":{"room":{"name":"MovieNight"},"file":{"name":"Film.mkv","duration":7200.0,"size":12}}}}}"#,
        )
        .expect("parse") else {
            panic!("expected a set");
        };

        let updates = user_updates(&set).expect("user updates");
        let file = updates["ahmet"].file.as_ref().expect("file");
        assert_eq!(file.name.as_deref(), Some("Film.mkv"));
        assert_eq!(file.duration, Some(7200.0));
        assert!(updates["ahmet"].event.is_none());
    }

    #[test]
    fn a_room_change_names_the_new_room() {
        let ServerMessage::Set(set) =
            ServerMessage::from_line(r#"{"Set":{"room":{"name":"Elsewhere"}}}"#).expect("parse")
        else {
            panic!("expected a set");
        };

        assert_eq!(room_change(&set).as_deref(), Some("Elsewhere"));
    }

    // ------------------------------------------------------- client shapes

    fn client_json(message: ClientMessage) -> serde_json::Value {
        let line = message.to_line().expect("line");
        assert!(line.ends_with("\r\n"), "every message is CRLF terminated");
        serde_json::from_str(line.trim()).expect("json")
    }

    #[test]
    fn the_clients_greeting_claims_what_a_gui_client_would() {
        let value = client_json(ClientMessage::Hello(Hello::participant(
            "ahmet",
            "MovieNight",
            Some("swordfish"),
        )));
        let hello = &value["Hello"];

        assert_eq!(hello["username"], "ahmet");
        assert_eq!(hello["password"], hash_password("swordfish"));
        assert_eq!(hello["room"]["name"], "MovieNight");
        assert_eq!(hello["version"], "1.2.255");
        assert_eq!(
            hello["features"],
            serde_json::json!({
                "sharedPlaylists": false,
                "chat": true,
                "featureList": true,
                "readiness": true,
                "managedRooms": false,
                "persistentRooms": false,
                "uiMode": "GUI",
            })
        );
    }

    #[test]
    fn the_monitors_greeting_claims_no_interface() {
        let value = client_json(ClientMessage::Hello(Hello::new(
            "panel",
            "MovieNight",
            None,
        )));

        assert!(value["Hello"]["features"].get("uiMode").is_none());
        assert_eq!(value["Hello"]["features"]["chat"], false);
    }

    #[test]
    fn a_local_seek_carries_do_seek_and_the_clients_counter() {
        let value = client_json(ClientMessage::State(ClientState {
            playstate: Some(ClientPlaystate {
                position: 80.04,
                paused: false,
                do_seek: true,
            }),
            latency_calculation: None,
            client_latency_calculation: 1000.5,
            client_rtt: 0.0016,
            ignoring_on_the_fly: (None, Some(1)),
        }));
        let state = &value["State"];

        assert_eq!(
            state["playstate"],
            serde_json::json!({ "position": 80.04, "paused": false, "doSeek": true })
        );
        assert!(
            state["ping"].get("latencyCalculation").is_none(),
            "a change the user made answers nothing, so there is nothing to echo"
        );
        assert_eq!(state["ping"]["clientLatencyCalculation"], 1000.5);
        assert_eq!(state["ping"]["clientRtt"], 0.0016);
        assert_eq!(
            state["ignoringOnTheFly"],
            serde_json::json!({ "client": 1 })
        );
    }

    #[test]
    fn a_reply_without_a_seek_leaves_do_seek_out() {
        let value = client_json(ClientMessage::State(ClientState {
            playstate: Some(ClientPlaystate {
                position: 85.0,
                paused: true,
                do_seek: false,
            }),
            latency_calculation: Some(1234.5),
            client_latency_calculation: 1000.0,
            client_rtt: -0.5,
            ignoring_on_the_fly: (Some(1), None),
        }));
        let state = &value["State"];

        assert!(state["playstate"].get("doSeek").is_none());
        assert_eq!(state["ping"]["latencyCalculation"], 1234.5);
        assert_eq!(
            state["ping"]["clientRtt"], 0.0,
            "a clock step must not put a negative round trip on the wire"
        );
        assert_eq!(
            state["ignoringOnTheFly"],
            serde_json::json!({ "server": 1 })
        );
    }

    #[test]
    fn a_state_with_nothing_to_report_is_only_a_ping() {
        let value = client_json(ClientMessage::State(ClientState {
            playstate: None,
            latency_calculation: Some(1.0),
            client_latency_calculation: 2.0,
            client_rtt: 0.0,
            ignoring_on_the_fly: (None, None),
        }));

        assert!(value["State"].get("playstate").is_none());
        assert!(value["State"].get("ignoringOnTheFly").is_none());
        assert_eq!(value["State"]["ping"]["latencyCalculation"], 1.0);
    }

    #[test]
    fn readiness_and_file_go_out_as_settings() {
        let ready = client_json(ClientMessage::SetReady {
            is_ready: false,
            manually_initiated: false,
        });
        assert_eq!(
            ready,
            serde_json::json!({ "Set": { "ready": { "isReady": false, "manuallyInitiated": false } } })
        );

        let file = client_json(ClientMessage::SetFile(FileReport {
            name: "clip.mkv".to_owned(),
            duration: 120.007,
            size: 7362024,
        }));
        assert_eq!(
            file,
            serde_json::json!({
                "Set": { "file": { "name": "clip.mkv", "duration": 120.007, "size": 7362024 } }
            })
        );
    }

    #[test]
    fn surfaces_server_errors() {
        let message =
            ServerMessage::from_line(r#"{"Error":{"message":"Invalid password"}}"#).expect("parse");

        assert!(
            matches!(message, ServerMessage::Error { message } if message == "Invalid password")
        );
    }

    #[test]
    fn unknown_messages_are_ignored_rather_than_fatal() {
        let message = ServerMessage::from_line(r#"{"SomethingNew":{"a":1}}"#).expect("parse");

        assert!(matches!(message, ServerMessage::Ignored));
    }

    #[test]
    fn chat_alone_does_not_trigger_a_refresh() {
        let ServerMessage::Set(set) =
            ServerMessage::from_line(r#"{"Set":{"features":{"username":"a","features":{}}}}"#)
                .expect("parse")
        else {
            panic!("expected a set");
        };

        assert!(!affects_room_view(&set));
    }

    // ------------------------------------------------------- outgoing shapes
    //
    // Every expectation below was read off the pinned Syncplay 1.7.5 source
    // rather than reasoned about. A near-miss here does not fail loudly: the
    // client connects, sees nothing, and the room looks empty.

    fn parsed(message: ServerToClient) -> serde_json::Value {
        let line = message.to_line().expect("line");
        assert!(line.ends_with("\r\n"), "every message is CRLF terminated");
        serde_json::from_str(line.trim()).expect("json")
    }

    /// The client tests this value with `"false" in answer`, a substring test.
    /// A boolean would raise `TypeError` there and take the client down with
    /// it, so the string is not a stylistic choice.
    #[test]
    fn refuses_tls_rather_than_leaving_the_client_waiting() {
        let line = ServerToClient::TlsRefusal.to_line().expect("line");

        assert_eq!(line.trim(), r#"{"TLS":{"startTLS":"false"}}"#);
        assert!(line.ends_with("\r\n"), "every message is CRLF terminated");
    }

    #[test]
    fn a_forced_state_names_who_caused_it() {
        let value = parsed(ServerToClient::State {
            position: 42.0,
            paused: true,
            do_seek: true,
            set_by: Some("ahmet".to_owned()),
            latency_calculation: 1234.5,
            server_rtt: 0.0,
            client_latency_calculation: None,
            ignoring_on_the_fly: Some((Some(3), None)),
        });
        let playstate = &value["State"]["playstate"];

        assert_eq!(playstate["setBy"], "ahmet");
        assert_eq!(playstate["doSeek"], true);
        assert_eq!(playstate["paused"], true);
        assert_eq!(value["State"]["ignoringOnTheFly"]["server"], 3);
        assert!(
            value["State"]["ignoringOnTheFly"].get("client").is_none(),
            "a half that is not in play is left out rather than sent as zero"
        );
        assert_eq!(value["State"]["ping"]["latencyCalculation"], 1234.5);
        assert!(
            value["State"]["ping"]
                .get("clientLatencyCalculation")
                .is_none(),
            "nothing to echo, nothing sent"
        );
    }

    #[test]
    fn a_state_with_no_counters_carries_no_envelope_at_all() {
        let value = parsed(ServerToClient::State {
            position: 0.0,
            paused: true,
            do_seek: false,
            set_by: None,
            latency_calculation: 1.0,
            server_rtt: 0.0,
            client_latency_calculation: Some(9.0),
            ignoring_on_the_fly: None,
        });

        assert!(value["State"].get("ignoringOnTheFly").is_none());
        assert_eq!(
            value["State"]["playstate"]["setBy"],
            serde_json::Value::Null
        );
        assert_eq!(value["State"]["ping"]["clientLatencyCalculation"], 9.0);
    }

    #[test]
    fn the_greeting_reports_only_features_that_are_implemented() {
        let value = parsed(ServerToClient::Hello {
            username: "ahmet".to_owned(),
            room: "MovieNight".to_owned(),
            client_version: "1.2.255".to_owned(),
            features: ServerFeatures::supported(),
        });
        let features = &value["Hello"]["features"];

        assert_eq!(features["chat"], true);
        assert_eq!(features["readiness"], true);
        assert_eq!(features["managedRooms"], true);
        assert_eq!(
            features["isolateRooms"], true,
            "room isolation is not a flag here"
        );
        assert_eq!(
            features["persistentRooms"], false,
            "on-disk room persistence is out of scope and must not be claimed"
        );
        assert_eq!(
            features["setOthersReadiness"], false,
            "setting somebody else ready is not implemented"
        );
        assert_eq!(features["maxUsernameLength"], 16);
    }

    /// Upstream keeps `version` as whatever the client sent so an old client
    /// still works, and reports the server's own build as `realversion`.
    #[test]
    fn the_greeting_echoes_the_clients_version_and_names_the_room() {
        let value = parsed(ServerToClient::Hello {
            username: "ahmet".to_owned(),
            room: "MovieNight".to_owned(),
            client_version: "1.2.255".to_owned(),
            features: ServerFeatures::supported(),
        });

        assert_eq!(value["Hello"]["username"], "ahmet");
        assert_eq!(value["Hello"]["room"]["name"], "MovieNight");
        assert_eq!(value["Hello"]["version"], "1.2.255");
        assert_eq!(value["Hello"]["realversion"], REAL_VERSION);
        assert_eq!(value["Hello"]["motd"], "", "there is no message of the day");
    }

    #[test]
    fn chat_carries_the_sender_so_clients_do_not_have_to_guess() {
        let value = parsed(ServerToClient::Chat {
            username: "ahmet".to_owned(),
            message: "başlıyoruz".to_owned(),
        });

        assert_eq!(value["Chat"]["username"], "ahmet");
        assert_eq!(value["Chat"]["message"], "başlıyoruz");
    }

    #[test]
    fn an_error_is_a_message_and_nothing_else() {
        let value = parsed(ServerToClient::Error {
            message: "unknown command".to_owned(),
        });

        assert_eq!(value["Error"]["message"], "unknown command");
    }

    #[test]
    fn a_user_setting_nests_the_user_under_their_own_name() {
        let value = parsed(ServerToClient::SetUser {
            username: "ahmet".to_owned(),
            room: "MovieNight".to_owned(),
            file: None,
            event: Some(serde_json::json!({ "left": true })),
        });
        let user = &value["Set"]["user"]["ahmet"];

        assert_eq!(user["room"]["name"], "MovieNight");
        assert_eq!(user["event"]["left"], true);
        assert!(
            user.get("file").is_none(),
            "a part carries no file, and an absent one is omitted rather than null"
        );
    }

    #[test]
    fn readiness_names_who_set_it_only_when_somebody_else_did() {
        let alone = parsed(ServerToClient::SetReady {
            username: "ahmet".to_owned(),
            is_ready: Some(true),
            manually_initiated: true,
            set_by: None,
        });
        assert_eq!(alone["Set"]["ready"]["isReady"], true);
        assert_eq!(alone["Set"]["ready"]["manuallyInitiated"], true);
        assert!(alone["Set"]["ready"].get("setBy").is_none());

        let by_other = parsed(ServerToClient::SetReady {
            username: "ahmet".to_owned(),
            is_ready: Some(false),
            manually_initiated: false,
            set_by: Some("mehmet".to_owned()),
        });
        assert_eq!(by_other["Set"]["ready"]["setBy"], "mehmet");
    }

    /// `user`, not `username`. The two live side by side in `Set` payloads and
    /// upstream is not consistent between them.
    #[test]
    fn a_controller_auth_result_names_the_user_as_user() {
        let value = parsed(ServerToClient::SetControllerAuth {
            user: "ahmet".to_owned(),
            room: "+MovieNight:ABCDEF012345".to_owned(),
            success: true,
        });

        assert_eq!(value["Set"]["controllerAuth"]["user"], "ahmet");
        assert_eq!(
            value["Set"]["controllerAuth"]["room"],
            "+MovieNight:ABCDEF012345"
        );
        assert_eq!(value["Set"]["controllerAuth"]["success"], true);
    }

    #[test]
    fn a_list_nests_watchers_under_their_room() {
        let mut rooms = ServerRoomList::new();
        rooms.insert(
            "MovieNight".to_owned(),
            HashMap::from([(
                "ahmet".to_owned(),
                ListEntry {
                    position: 95.0,
                    file: serde_json::json!({ "name": "Film.mkv" }),
                    controller: false,
                    is_ready: Some(true),
                    features: serde_json::json!({}),
                },
            )]),
        );

        let value = parsed(ServerToClient::List(rooms));
        let entry = &value["List"]["MovieNight"]["ahmet"];

        assert_eq!(entry["position"], 95.0);
        assert_eq!(entry["file"]["name"], "Film.mkv");
        assert_eq!(entry["isReady"], true);
        assert_eq!(entry["controller"], false);
    }

    #[test]
    fn a_playlist_change_names_who_made_it() {
        let value = parsed(ServerToClient::SetPlaylistChange {
            user: "ahmet".to_owned(),
            files: serde_json::json!(["one.mkv", "two.mkv"]),
        });

        assert_eq!(value["Set"]["playlistChange"]["user"], "ahmet");
        assert_eq!(
            value["Set"]["playlistChange"]["files"][1], "two.mkv",
            "the list is relayed as it arrived"
        );
    }

    #[test]
    fn a_playlist_index_names_who_moved_it() {
        let value = parsed(ServerToClient::SetPlaylistIndex {
            user: "ahmet".to_owned(),
            index: serde_json::json!(2),
        });

        assert_eq!(value["Set"]["playlistIndex"]["user"], "ahmet");
        assert_eq!(value["Set"]["playlistIndex"]["index"], 2);
    }
}
