//! The Syncplay wire protocol: newline-delimited JSON objects.
//!
//! Only the slice syncparty needs to observe a room is modelled here. Every
//! shape below was read off the Syncplay 1.7.x source rather than guessed,
//! because a near-miss here fails silently — the monitor connects, sees
//! nothing, and the panel just looks empty.

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

#[derive(Debug, Clone, Serialize)]
pub struct RoomRef {
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientFeatures {
    pub shared_playlists: bool,
    pub chat: bool,
    pub feature_list: bool,
    pub readiness: bool,
    pub managed_rooms: bool,
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
        }
    }
}

#[derive(Debug, Clone, Serialize)]
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
    /// Builds the greeting, hashing `password` on the way in.
    pub fn new(username: &str, room: &str, password: Option<&str>) -> Self {
        Self {
            username: username.to_owned(),
            password: password.filter(|p| !p.is_empty()).map(hash_password),
            room: RoomRef {
                name: room.to_owned(),
            },
            version: PROTOCOL_VERSION.to_owned(),
            realversion: REAL_VERSION.to_owned(),
            features: ClientFeatures::observer(),
        }
    }
}

/// A message syncparty sends to the server.
#[derive(Debug, Clone)]
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
            } => {
                let mut ping = serde_json::Map::new();
                if let Some(value) = latency_calculation {
                    ping.insert("latencyCalculation".to_owned(), (*value).into());
                }
                ping.insert(
                    "clientLatencyCalculation".to_owned(),
                    (*client_latency_calculation).into(),
                );
                ping.insert("clientRtt".to_owned(), 0.into());

                let mut state = serde_json::Map::new();
                state.insert("ping".to_owned(), ping.into());
                if let Some(sequence) = server_ignoring_on_the_fly {
                    state.insert(
                        "ignoringOnTheFly".to_owned(),
                        serde_json::json!({ "server": sequence }),
                    );
                }

                serde_json::json!({ "State": state })
            }
        };

        Ok(format!("{}\r\n", serde_json::to_string(&value)?))
    }
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

#[derive(Debug, Clone, Deserialize)]
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

/// A message received from the server.
#[derive(Debug, Clone)]
pub enum ServerMessage {
    /// The greeting that confirms the password was accepted.
    Hello,
    List(RoomList),
    /// Any `Set`. The specific payloads that matter are pulled out below.
    Set(serde_json::Map<String, serde_json::Value>),
    State {
        latency_calculation: Option<f64>,
        server_ignoring_on_the_fly: Option<u64>,
    },
    Error {
        message: String,
    },
    /// Something syncparty does not model. Ignored rather than fatal, so a
    /// newer server cannot break the monitor.
    Ignored,
}

impl ServerMessage {
    pub fn from_line(line: &str) -> Result<Self> {
        let value: serde_json::Value = serde_json::from_str(line.trim()).map_err(|error| {
            SyncPartyError::MonitorFailed(format!("malformed message: {error}"))
        })?;

        let Some(object) = value.as_object() else {
            return Ok(Self::Ignored);
        };

        // Syncplay sends one key per message.
        let Some((key, payload)) = object.iter().next() else {
            return Ok(Self::Ignored);
        };

        Ok(match key.as_str() {
            "Hello" => Self::Hello,
            "List" => serde_json::from_value(payload.clone())
                .map(Self::List)
                .unwrap_or(Self::Ignored),
            "Set" => payload
                .as_object()
                .cloned()
                .map(Self::Set)
                .unwrap_or(Self::Ignored),
            "State" => Self::State {
                latency_calculation: payload
                    .get("ping")
                    .and_then(|ping| ping.get("latencyCalculation"))
                    .and_then(serde_json::Value::as_f64),
                server_ignoring_on_the_fly: payload
                    .get("ignoringOnTheFly")
                    .and_then(|ignore| ignore.get("server"))
                    .and_then(serde_json::Value::as_u64),
            },
            "Error" => Self::Error {
                message: payload
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("the server rejected the connection")
                    .to_owned(),
            },
            _ => Self::Ignored,
        })
    }
}

/// Extracts the `user` half of a `Set`, if present.
pub fn user_updates(
    set: &serde_json::Map<String, serde_json::Value>,
) -> Option<HashMap<String, UserUpdate>> {
    serde_json::from_value(set.get("user")?.clone()).ok()
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

        assert!(matches!(
            message,
            ServerMessage::State {
                latency_calculation: Some(value),
                server_ignoring_on_the_fly: Some(4),
            } if (value - 1234.5).abs() < f64::EPSILON
        ));
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
