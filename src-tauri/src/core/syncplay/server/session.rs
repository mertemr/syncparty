//! One connection: the handshake, then the message loop, then leaving.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, RwLock};

use crate::core::error::Result;
use crate::core::syncplay::protocol::{
    self, ListEntry, ServerFeatures, ServerRoomList, ServerToClient, MAX_CHAT_MESSAGE_LENGTH,
};
use crate::core::syncplay::server::auth;
use crate::core::syncplay::server::registry::Registry;
use crate::core::syncplay::server::room::{Force, OpenFile, PlaybackState, StateUpdate};
use crate::core::syncplay::server::ServerConfig;

/// Bounded on purpose: a peer that has stopped reading is a peer that has
/// left, and buffering without limit would turn one stalled client into the
/// whole server's problem.
const OUTBOUND_CAPACITY: usize = 64;

/// Serves one connection to completion.
///
/// Takes a stream rather than an address so the same core can be fed by a
/// loopback socket today and a QUIC bi-stream later without learning that
/// anything happened.
pub async fn serve<S>(
    stream: S,
    registry: Arc<RwLock<Registry>>,
    config: Arc<ServerConfig>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let (outbound, mut inbound) = mpsc::channel::<String>(OUTBOUND_CAPACITY);

    // The only thing that touches the write half. Everything that wants to
    // send pushes a line in here instead, so no lock is ever held across a
    // write.
    let pump = tokio::spawn(async move {
        while let Some(line) = inbound.recv().await {
            if writer.write_all(line.as_bytes()).await.is_err() {
                break;
            }
        }
    });

    let mut session = Session {
        registry,
        config,
        outbound,
        username: None,
    };

    let result = session.run(reader).await;
    session.depart().await;

    // Drops this connection's last sender, which is what lets the pump finish
    // rather than wait for a line that is never coming.
    drop(session);
    let _ = pump.await;

    result
}

struct Session {
    registry: Arc<RwLock<Registry>>,
    config: Arc<ServerConfig>,
    outbound: mpsc::Sender<String>,
    /// `None` until the greeting is accepted. Once set, it is the name the
    /// registry knows, which is not always the one that was asked for.
    username: Option<String>,
}

impl Session {
    async fn run<R>(&mut self, reader: R) -> Result<()>
    where
        R: AsyncRead + Unpin,
    {
        let mut lines = BufReader::new(reader).lines();

        while let Some(line) = lines.next_line().await? {
            if !self.handle(&line).await {
                break;
            }
        }

        Ok(())
    }

    /// `false` closes the connection.
    async fn handle(&mut self, line: &str) -> bool {
        // Anything we cannot read is ignored rather than fatal. A newer client
        // sending a message we do not model must not end the party.
        let Ok(message) = serde_json::from_str::<serde_json::Map<String, Value>>(line.trim())
        else {
            return true;
        };

        for (command, payload) in message {
            match command.as_str() {
                "TLS" => self.send(ServerToClient::TlsRefusal).await,
                "Hello" => {
                    if !self.hello(&payload).await {
                        return false;
                    }
                }
                "List" => self.send_list().await,
                "State" => self.state(&payload).await,
                "Chat" => self.chat(&payload).await,
                "Set" => self.set(&payload).await,
                _ => {}
            }
        }

        true
    }

    /// `false` means the greeting was refused and the connection must close.
    async fn hello(&mut self, payload: &Value) -> bool {
        if self.username.is_some() {
            return true;
        }

        let wanted = payload["username"].as_str().map(str::trim).unwrap_or("");
        let room = payload["room"]["name"]
            .as_str()
            .map(str::trim)
            .unwrap_or("");
        if wanted.is_empty() || room.is_empty() {
            self.refuse("a greeting must name a user and a room").await;
            return false;
        }

        if !self.password_matches(payload) {
            self.refuse("the room password is wrong").await;
            return false;
        }

        // Upstream prefers `realversion` and falls back to `version`, then
        // echoes whichever it settled on straight back so an old client still
        // recognises the answer.
        let client_version = payload["realversion"]
            .as_str()
            .or_else(|| payload["version"].as_str())
            .unwrap_or_default()
            .to_owned();
        let features = payload
            .get("features")
            .cloned()
            .unwrap_or_else(|| json!({}));

        let username = {
            let mut registry = self.registry.write().await;
            let free = registry.free_username(wanted);
            registry.join(&free, room, self.outbound.clone());
            free
        };
        self.username = Some(username.clone());

        // The room hears about the arrival before the arrival hears anything,
        // which is the order upstream greets in.
        let announcement = ServerToClient::SetUser {
            username: username.clone(),
            room: room.to_owned(),
            file: None,
            event: Some(json!({
                "joined": true,
                "version": client_version,
                "features": features,
            })),
        };
        self.broadcast(room, Some(&username), announcement).await;

        self.send(ServerToClient::Hello {
            username,
            room: room.to_owned(),
            client_version,
            features: ServerFeatures::supported(),
        })
        .await;
        self.send_list().await;

        true
    }

    /// A server with no password configured lets anybody in, which is what an
    /// unset one does upstream.
    fn password_matches(&self, payload: &Value) -> bool {
        if self.config.password.is_empty() {
            return true;
        }

        let expected = protocol::hash_password(&self.config.password);
        payload["password"]
            .as_str()
            .is_some_and(|given| given == expected)
    }

    async fn refuse(&self, message: &str) {
        self.send(ServerToClient::Error {
            message: message.to_owned(),
        })
        .await;
    }

    async fn send(&self, message: ServerToClient) {
        let Ok(line) = message.to_line() else {
            return;
        };

        let _ = self.outbound.send(line).await;
    }

    /// Sends to everyone in `room`, skipping `except`.
    ///
    /// The senders are cloned out under the read lock and the lock is released
    /// before a single byte is written, so one slow peer cannot stall the
    /// registry for everybody else.
    async fn broadcast(&self, room: &str, except: Option<&str>, message: ServerToClient) {
        let Ok(line) = message.to_line() else {
            return;
        };

        let senders: Vec<mpsc::Sender<String>> = {
            let registry = self.registry.read().await;
            registry
                .room(room)
                .map(|room| {
                    room.users()
                        .iter()
                        .filter(|(name, _)| Some(name.as_str()) != except)
                        .map(|(_, user)| user.outbound.clone())
                        .collect()
                })
                .unwrap_or_default()
        };

        for sender in senders {
            let _ = sender.send(line.clone()).await;
        }
    }

    async fn send_list(&self) {
        let Some(username) = self.username.as_deref() else {
            return;
        };

        let rooms: ServerRoomList = {
            let registry = self.registry.read().await;
            registry
                .visible_list(username)
                .into_iter()
                .map(|(name, room)| {
                    let watchers = room
                        .users()
                        .iter()
                        .map(|(name, user)| {
                            (
                                name.clone(),
                                ListEntry {
                                    position: user.position.unwrap_or_default(),
                                    file: user
                                        .file
                                        .as_ref()
                                        .map(file_json)
                                        .unwrap_or_else(|| json!({})),
                                    controller: user.is_controller,
                                    is_ready: Some(user.is_ready),
                                    features: json!({}),
                                },
                            )
                        })
                        .collect();

                    (name.to_owned(), watchers)
                })
                .collect()
        };

        self.send(ServerToClient::List(rooms)).await;
    }

    /// One client's report of where it is.
    async fn state(&mut self, payload: &Value) {
        let Some(username) = self.username.clone() else {
            return;
        };

        let envelope = payload.get("ignoringOnTheFly");
        let server_ack = envelope.and_then(|envelope| envelope["server"].as_u64());
        let client_value = envelope.and_then(|envelope| envelope["client"].as_u64());

        let update = StateUpdate {
            position: payload["playstate"]["position"].as_f64(),
            paused: payload["playstate"]["paused"].as_bool(),
            do_seek: payload["playstate"]["doSeek"].as_bool().unwrap_or_default(),
        };

        let (room_name, force) = {
            let mut registry = self.registry.write().await;
            let Some(room) = registry.room_of_mut(&username) else {
                return;
            };

            if let Some(user) = room.user_mut(&username) {
                user.gate.observe(server_ack, client_value);
                // Whatever this says describes the world before the last force.
                if !user.gate.accepts_updates() {
                    return;
                }
            }

            let name = room.name().to_owned();
            // `message_age` is zero until there is a ping service to measure
            // it; `Room::apply` already knows what to do with a real one.
            (
                name,
                room.apply(&username, update, Duration::ZERO, Instant::now()),
            )
        };

        match force {
            Force::Nothing => {}
            Force::Broadcast(state) => {
                let forced = [Forced::from_state(&state, update_seek(payload))];
                self.send_forced(&room_name, None, &forced).await;
            }
            // Two messages, to the sender alone. Upstream puts the room's
            // position in both; the first differs only in echoing the pause
            // state they asked for, and exists so clients we did not write
            // keep working.
            Force::CorrectSender { echo, real } => {
                let forced = [
                    Forced {
                        position: real.position,
                        paused: echo.paused,
                        do_seek: false,
                        set_by: Some(username.clone()),
                    },
                    Forced::from_state(&real, true),
                ];
                self.send_forced(&room_name, Some(&username), &forced).await;
            }
        }
    }

    /// Renders one forced `State` per recipient and sends it.
    ///
    /// Rendering happens under the write lock because every copy is stamped
    /// with that recipient's own counter. Nothing is written until the lock is
    /// gone again.
    async fn send_forced(&self, room_name: &str, only: Option<&str>, forced: &[Forced]) {
        let outgoing: Vec<(mpsc::Sender<String>, Vec<String>)> = {
            let mut registry = self.registry.write().await;
            let Some(room) = registry.room_mut(room_name) else {
                return;
            };

            room.users_mut()
                .filter(|(name, _)| only.is_none_or(|wanted| wanted == name.as_str()))
                .map(|(_, user)| {
                    let lines = forced
                        .iter()
                        .filter_map(|state| {
                            user.gate.on_forced_send();
                            ServerToClient::State {
                                position: state.position,
                                paused: state.paused,
                                do_seek: state.do_seek,
                                set_by: state.set_by.clone(),
                                latency_calculation: timestamp(),
                                server_rtt: 0.0,
                                client_latency_calculation: None,
                                ignoring_on_the_fly: user.gate.take_envelope(),
                            }
                            .to_line()
                            .ok()
                        })
                        .collect();

                    (user.outbound.clone(), lines)
                })
                .collect()
        };

        for (sender, lines) in outgoing {
            for line in lines {
                let _ = sender.send(line).await;
            }
        }
    }

    async fn chat(&self, payload: &Value) {
        let (Some(username), Some(room)) = (self.username.clone(), self.room_name().await) else {
            return;
        };
        let Some(message) = payload.as_str() else {
            return;
        };

        let message = truncate(message, MAX_CHAT_MESSAGE_LENGTH);
        self.broadcast(&room, None, ServerToClient::Chat { username, message })
            .await;
    }

    async fn set(&mut self, payload: &Value) {
        let Some(settings) = payload.as_object() else {
            return;
        };

        for (setting, value) in settings {
            match setting.as_str() {
                "ready" => self.set_ready(value).await,
                "file" => self.set_file(value).await,
                "room" => self.set_room(value).await,
                "controllerAuth" => self.controller_auth(value).await,
                // Relayed, never interpreted. That is what lets a playlist UI
                // arrive later without this file changing again.
                "playlistChange" => self.relay_playlist(value, true).await,
                "playlistIndex" => self.relay_playlist(value, false).await,
                _ => {}
            }
        }
    }

    async fn set_ready(&self, value: &Value) {
        let (Some(username), Some(room)) = (self.username.clone(), self.room_name().await) else {
            return;
        };

        let is_ready = value["isReady"].as_bool();
        let manually_initiated = value["manuallyInitiated"].as_bool().unwrap_or_default();

        {
            let mut registry = self.registry.write().await;
            if let Some(user) = registry
                .room_of_mut(&username)
                .and_then(|room| room.user_mut(&username))
            {
                user.is_ready = is_ready.unwrap_or_default();
            }
        }

        // `setBy` stays empty: we advertise `setOthersReadiness: false`, so a
        // readiness change is always the sender's own.
        self.broadcast(
            &room,
            None,
            ServerToClient::SetReady {
                username,
                is_ready,
                manually_initiated,
                set_by: None,
            },
        )
        .await;
    }

    async fn set_file(&self, value: &Value) {
        let (Some(username), Some(room)) = (self.username.clone(), self.room_name().await) else {
            return;
        };

        let file = OpenFile {
            name: value["name"].as_str().unwrap_or_default().to_owned(),
            duration: value["duration"].as_f64(),
            size: value.get("size").cloned(),
        };

        {
            let mut registry = self.registry.write().await;
            if let Some(room) = registry.room_of_mut(&username) {
                room.set_file(&username, Some(file));
            }
        }

        self.broadcast(
            &room,
            None,
            ServerToClient::SetUser {
                username,
                room: room.clone(),
                file: Some(value.clone()),
                event: None,
            },
        )
        .await;
    }

    /// Moves this connection to another room.
    ///
    /// Both rooms are told. Isolation means the one being left cannot see the
    /// one being joined, so without the farewell it would go on showing a
    /// watcher who is not there any more.
    async fn set_room(&mut self, value: &Value) {
        let Some(username) = self.username.clone() else {
            return;
        };
        let wanted = value["name"].as_str().map(str::trim).unwrap_or_default();
        if wanted.is_empty() {
            return;
        }

        let Some(previous) = self.room_name().await else {
            return;
        };
        if previous == wanted {
            return;
        }

        self.announce(&previous, &username, json!({ "left": true }))
            .await;

        {
            let mut registry = self.registry.write().await;
            registry.move_to(&username, wanted);
        }

        self.announce(wanted, &username, json!({ "joined": true }))
            .await;
        self.send_list().await;
    }

    async fn controller_auth(&self, value: &Value) {
        let (Some(username), Some(room)) = (self.username.clone(), self.room_name().await) else {
            return;
        };

        let password = value["password"].as_str().unwrap_or_default();
        let target = value["room"].as_str().unwrap_or(&room);
        let success = auth::check_controlled_room(target, password, &self.config.salt);

        if success {
            let mut registry = self.registry.write().await;
            if let Some(room) = registry.room_of_mut(&username) {
                room.set_controller(&username, true);
            }
        }

        self.broadcast(
            &room,
            None,
            ServerToClient::SetControllerAuth {
                user: username,
                room: room.clone(),
                success,
            },
        )
        .await;
    }

    async fn relay_playlist(&self, value: &Value, is_change: bool) {
        let (Some(user), Some(room)) = (self.username.clone(), self.room_name().await) else {
            return;
        };

        let message = if is_change {
            ServerToClient::SetPlaylistChange {
                user,
                files: value.get("files").cloned().unwrap_or_else(|| json!([])),
            }
        } else {
            ServerToClient::SetPlaylistIndex {
                user,
                index: value.get("index").cloned().unwrap_or(Value::Null),
            }
        };

        self.broadcast(&room, None, message).await;
    }

    /// Tells `room` that `username` arrived or left.
    async fn announce(&self, room: &str, username: &str, event: Value) {
        self.broadcast(
            room,
            Some(username),
            ServerToClient::SetUser {
                username: username.to_owned(),
                room: room.to_owned(),
                file: None,
                event: Some(event),
            },
        )
        .await;
    }

    async fn room_name(&self) -> Option<String> {
        let username = self.username.as_deref()?;
        let registry = self.registry.read().await;

        registry
            .room_of(username)
            .map(|room| room.name().to_owned())
    }

    /// Takes this connection out of the registry and tells the room it has
    /// gone. Safe to call on a connection that never got past the greeting.
    async fn depart(&mut self) {
        let Some(username) = self.username.take() else {
            return;
        };

        let room = {
            let mut registry = self.registry.write().await;
            let room = registry
                .room_of(&username)
                .map(|room| room.name().to_owned());
            registry.leave(&username);
            room
        };

        let Some(room) = room else {
            return;
        };

        let farewell = ServerToClient::SetUser {
            username: username.clone(),
            room: room.clone(),
            file: None,
            event: Some(json!({ "left": true })),
        };
        self.broadcast(&room, Some(&username), farewell).await;
    }
}

/// One forced state, before a recipient's counter is stamped on it.
struct Forced {
    position: f64,
    paused: bool,
    do_seek: bool,
    set_by: Option<String>,
}

impl Forced {
    fn from_state(state: &PlaybackState, do_seek: bool) -> Self {
        Self {
            position: state.position,
            paused: state.paused,
            do_seek,
            set_by: state.set_by.clone(),
        }
    }
}

fn update_seek(payload: &Value) -> bool {
    payload["playstate"]["doSeek"].as_bool().unwrap_or_default()
}

/// Seconds since the epoch, which is what upstream's ping service sends and
/// what a client echoes back to work out its own latency.
fn timestamp() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs_f64())
        .unwrap_or_default()
}

/// Cut to the length the greeting claims, by characters rather than bytes.
fn truncate(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

/// A watcher's open file, as the wire carries it. Upstream sends `{}` for
/// somebody who has opened nothing, never a null.
fn file_json(file: &OpenFile) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("name".to_owned(), json!(file.name));
    if let Some(duration) = file.duration {
        map.insert("duration".to_owned(), json!(duration));
    }
    if let Some(size) = &file.size {
        map.insert("size".to_owned(), size.clone());
    }

    map.into()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

    use super::*;
    use crate::core::syncplay::protocol;
    use crate::core::syncplay::server::auth;

    fn test_config() -> Arc<ServerConfig> {
        Arc::new(ServerConfig {
            port: 0,
            password: "swordfish".to_owned(),
            salt: "PEPPER".to_owned(),
        })
    }

    /// Drives `serve` over an in-memory pipe and returns the client's half.
    /// No socket, no port, and no ordering between tests.
    fn connect(registry: Arc<RwLock<Registry>>) -> DuplexStream {
        let (client, server) = tokio::io::duplex(8192);
        tokio::spawn(serve(server, registry, test_config()));
        client
    }

    async fn write_line(client: &mut DuplexStream, line: &str) {
        client
            .write_all(format!("{line}\r\n").as_bytes())
            .await
            .expect("write");
    }

    /// Returns an empty string at end of stream, so a test about the
    /// connection closing reads naturally. A hang is a failure, not a wait.
    async fn read_line(client: &mut DuplexStream) -> String {
        let read = async {
            let mut line = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                match client.read(&mut byte).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) if byte[0] == b'\n' => break,
                    Ok(_) => line.push(byte[0]),
                }
            }
            String::from_utf8_lossy(&line).trim().to_owned()
        };

        tokio::time::timeout(Duration::from_secs(2), read)
            .await
            .expect("the server should have answered or closed by now")
    }

    fn hello_line(username: &str, room: &str, password: &str) -> String {
        serde_json::json!({
            "Hello": {
                "username": username,
                "password": protocol::hash_password(password),
                "room": { "name": room },
                "version": "1.2.255",
                "realversion": "1.7.5",
                "features": { "chat": true, "readiness": true },
            }
        })
        .to_string()
    }

    /// Greets a client and drains its `Hello` and `List`.
    async fn arrive(registry: &Arc<RwLock<Registry>>, name: &str, room: &str) -> DuplexStream {
        let mut client = connect(Arc::clone(registry));
        write_line(&mut client, &hello_line(name, room, "swordfish")).await;
        let _ = read_line(&mut client).await;
        let _ = read_line(&mut client).await;
        client
    }

    /// The refusal is the string `"false"`, not the boolean: the client tests
    /// it with a substring check that raises on anything else.
    #[tokio::test]
    async fn answers_the_tls_probe_before_anything_else() {
        let mut client = connect(Registry::shared());
        write_line(&mut client, r#"{"TLS":{"startTLS":"send"}}"#).await;

        let reply = read_line(&mut client).await;

        assert!(
            reply.contains(r#""startTLS":"false""#),
            "an unanswered probe leaves the client hanging with no error: {reply}"
        );
    }

    #[tokio::test]
    async fn a_correct_password_is_greeted_and_listed() {
        let mut client = connect(Registry::shared());
        write_line(&mut client, &hello_line("ahmet", "MovieNight", "swordfish")).await;

        let greeting = read_line(&mut client).await;
        assert!(greeting.contains(r#""Hello""#), "got {greeting}");

        let list = read_line(&mut client).await;
        assert!(
            list.contains("MovieNight"),
            "a new arrival is sent the room list, got {list}"
        );
    }

    #[tokio::test]
    async fn a_wrong_password_gets_an_error_and_the_connection_closes() {
        let mut client = connect(Registry::shared());
        write_line(&mut client, &hello_line("ahmet", "MovieNight", "wrong")).await;

        let reply = read_line(&mut client).await;
        assert!(reply.contains(r#""Error""#), "got {reply}");

        assert!(
            read_line(&mut client).await.is_empty(),
            "the connection must close rather than linger unauthenticated"
        );
    }

    #[tokio::test]
    async fn an_unparseable_line_is_ignored_rather_than_fatal() {
        let registry = Registry::shared();
        let mut client = arrive(&registry, "ahmet", "MovieNight").await;

        write_line(&mut client, "{ this is not json").await;
        write_line(&mut client, r#"{"List":null}"#).await;

        assert!(
            read_line(&mut client).await.contains("MovieNight"),
            "a newer client sending something we do not model must not break us"
        );
    }

    #[tokio::test]
    async fn a_join_is_announced_to_everyone_already_in_the_room() {
        let registry = Registry::shared();
        let mut first = arrive(&registry, "ahmet", "MovieNight").await;

        let mut second = connect(Arc::clone(&registry));
        write_line(
            &mut second,
            &hello_line("mehmet", "MovieNight", "swordfish"),
        )
        .await;

        let announcement = read_line(&mut first).await;
        assert!(
            announcement.contains("mehmet") && announcement.contains("joined"),
            "got {announcement}"
        );
    }

    /// Not in the plan, and the reason it has to be: `Registry::join` is keyed
    /// by name, so a second `ahmet` would take the first one's place and the
    /// first would simply stop existing.
    #[tokio::test]
    async fn a_second_arrival_with_a_taken_name_does_not_evict_the_first() {
        let registry = Registry::shared();
        let mut first = arrive(&registry, "ahmet", "MovieNight").await;

        let mut second = connect(Arc::clone(&registry));
        write_line(&mut second, &hello_line("ahmet", "MovieNight", "swordfish")).await;

        let greeting = read_line(&mut second).await;
        assert!(
            greeting.contains(r#""username":"ahmet_""#),
            "the newcomer is renamed rather than allowed to collide, got {greeting}"
        );

        let announcement = read_line(&mut first).await;
        assert!(
            announcement.contains("joined"),
            "the first is still connected and still hears about it, got {announcement}"
        );

        let guard = registry.read().await;
        assert_eq!(
            guard.room("MovieNight").expect("room").users().len(),
            2,
            "both are in the room"
        );
    }

    // --------------------------------------------------------- message loop

    fn state_line(position: f64, paused: bool) -> String {
        serde_json::json!({
            "State": { "playstate": { "position": position, "paused": paused } }
        })
        .to_string()
    }

    /// The same report, acknowledging a force this connection was sent.
    fn acked_state_line(position: f64, paused: bool, server: u64) -> String {
        serde_json::json!({
            "State": {
                "playstate": { "position": position, "paused": paused },
                "ignoringOnTheFly": { "server": server },
            }
        })
        .to_string()
    }

    /// Reads until a line contains `needle`, so a test is not coupled to how
    /// many unrelated messages happen to arrive first.
    async fn read_until(client: &mut DuplexStream, needle: &str) -> String {
        for _ in 0..20 {
            let line = read_line(client).await;
            assert!(
                !line.is_empty(),
                "the stream ended before {needle:?} arrived"
            );
            if line.contains(needle) {
                return line;
            }
        }

        panic!("no line containing {needle:?} arrived");
    }

    /// `None` when nothing arrives within a beat, which is how "the server
    /// said nothing at all" gets asserted.
    async fn silence(client: &mut DuplexStream) -> Option<String> {
        let mut byte = [0u8; 1];
        tokio::time::timeout(Duration::from_millis(500), client.read(&mut byte))
            .await
            .ok()
            .map(|read| format!("{read:?}"))
    }

    async fn two_in_a_room() -> (DuplexStream, DuplexStream) {
        two_in_room("MovieNight").await
    }

    async fn two_in_room(room: &str) -> (DuplexStream, DuplexStream) {
        let registry = Registry::shared();
        let ahmet = arrive(&registry, "ahmet", room).await;

        let mut mehmet = connect(Arc::clone(&registry));
        write_line(&mut mehmet, &hello_line("mehmet", room, "swordfish")).await;
        let _ = read_line(&mut mehmet).await;
        let _ = read_line(&mut mehmet).await;

        (ahmet, mehmet)
    }

    /// The plan wrote this as a single pause on a fresh room. That cannot
    /// force anything: a room opens paused, so `paused: true` is not a change.
    /// The room has to be playing first, which is also the only way a real
    /// party reaches a pause.
    #[tokio::test]
    async fn a_pause_reaches_the_other_watcher() {
        let (mut ahmet, mut mehmet) = two_in_a_room().await;

        write_line(&mut ahmet, &state_line(0.0, false)).await;
        let _ = read_until(&mut mehmet, "playstate").await;

        write_line(&mut ahmet, &acked_state_line(10.0, true, 1)).await;

        let forced = read_until(&mut mehmet, r#""paused":true"#).await;
        assert!(forced.contains(r#""setBy":"ahmet""#), "got {forced}");
    }

    #[tokio::test]
    async fn a_forced_state_carries_a_counter_the_client_must_acknowledge() {
        let (mut ahmet, mut mehmet) = two_in_a_room().await;

        write_line(&mut ahmet, &state_line(0.0, false)).await;

        let forced = read_until(&mut mehmet, "playstate").await;
        assert!(
            forced.contains(r#""ignoringOnTheFly":{"server":1}"#),
            "got {forced}"
        );
    }

    /// The whole point of the gate, and the part that fails quietly when it is
    /// wrong: while a force is unacknowledged, that connection's reports
    /// describe the world before it and are dropped rather than applied.
    #[tokio::test]
    async fn a_report_that_has_not_acknowledged_a_force_is_dropped() {
        let (mut ahmet, mut mehmet) = two_in_a_room().await;

        write_line(&mut ahmet, &state_line(0.0, false)).await;
        let _ = read_until(&mut mehmet, "playstate").await;
        let _ = read_until(&mut ahmet, "playstate").await;

        write_line(&mut mehmet, &state_line(10.0, true)).await;
        assert!(
            silence(&mut ahmet).await.is_none(),
            "a report from before the force must not move the room"
        );

        write_line(&mut mehmet, &acked_state_line(10.0, true, 1)).await;
        let forced = read_until(&mut ahmet, r#""paused":true"#).await;
        assert!(forced.contains(r#""setBy":"mehmet""#), "got {forced}");
    }

    #[tokio::test]
    async fn chat_is_relayed_with_its_sender() {
        let (mut ahmet, mut mehmet) = two_in_a_room().await;

        write_line(&mut ahmet, r#"{"Chat":"başlıyoruz"}"#).await;

        let relayed = read_until(&mut mehmet, "Chat").await;
        assert!(relayed.contains("başlıyoruz"), "got {relayed}");
        assert!(relayed.contains(r#""username":"ahmet""#), "got {relayed}");
    }

    #[tokio::test]
    async fn readiness_is_broadcast() {
        let (mut ahmet, mut mehmet) = two_in_a_room().await;

        write_line(
            &mut ahmet,
            r#"{"Set":{"ready":{"isReady":true,"manuallyInitiated":true}}}"#,
        )
        .await;

        let relayed = read_until(&mut mehmet, "ready").await;
        assert!(relayed.contains(r#""isReady":true"#), "got {relayed}");
        assert!(relayed.contains(r#""username":"ahmet""#), "got {relayed}");
    }

    #[tokio::test]
    async fn the_file_a_user_opens_shows_up_in_the_list() {
        let (mut ahmet, _mehmet) = two_in_a_room().await;

        write_line(
            &mut ahmet,
            r#"{"Set":{"file":{"name":"Film.mkv","duration":7200.0}}}"#,
        )
        .await;
        write_line(&mut ahmet, r#"{"List":null}"#).await;

        let list = read_until(&mut ahmet, "Film.mkv").await;
        assert!(list.contains("7200"), "got {list}");
    }

    #[tokio::test]
    async fn a_playlist_change_is_relayed_untouched() {
        let (mut ahmet, mut mehmet) = two_in_a_room().await;

        write_line(
            &mut ahmet,
            r#"{"Set":{"playlistChange":{"files":["one.mkv","two.mkv"]}}}"#,
        )
        .await;

        let relayed = read_until(&mut mehmet, "playlistChange").await;
        assert!(relayed.contains("two.mkv"), "got {relayed}");
        assert!(
            relayed.contains(r#""user":"ahmet""#),
            "the relay names who changed it even though the UI ignores playlists today: {relayed}"
        );
    }

    #[tokio::test]
    async fn the_right_operator_password_grants_control() {
        let room = auth::controlled_room_name("MovieNight", "AB-123-456", "PEPPER");
        let (mut ahmet, _mehmet) = two_in_room(&room).await;

        write_line(
            &mut ahmet,
            &serde_json::json!({
                "Set": { "controllerAuth": { "password": "AB-123-456", "room": room } }
            })
            .to_string(),
        )
        .await;

        let reply = read_until(&mut ahmet, "controllerAuth").await;
        assert!(reply.contains(r#""success":true"#), "got {reply}");
        assert!(reply.contains(r#""user":"ahmet""#), "got {reply}");
    }

    #[tokio::test]
    async fn a_wrong_operator_password_grants_nothing() {
        let room = auth::controlled_room_name("MovieNight", "AB-123-456", "PEPPER");
        let (mut ahmet, _mehmet) = two_in_room(&room).await;

        write_line(
            &mut ahmet,
            &serde_json::json!({
                "Set": { "controllerAuth": { "password": "AB-123-999", "room": room } }
            })
            .to_string(),
        )
        .await;

        let reply = read_until(&mut ahmet, "controllerAuth").await;
        assert!(reply.contains(r#""success":false"#), "got {reply}");
    }

    #[tokio::test]
    async fn a_departure_is_announced() {
        let (ahmet, mut mehmet) = two_in_a_room().await;
        drop(ahmet);

        let announcement = read_until(&mut mehmet, "left").await;
        assert!(announcement.contains("ahmet"), "got {announcement}");
    }

    /// Isolation means the room somebody leaves has to be told, or it goes on
    /// showing a watcher who is not there any more.
    #[tokio::test]
    async fn moving_to_another_room_is_announced_to_the_one_left_behind() {
        let (mut ahmet, mut mehmet) = two_in_a_room().await;

        write_line(&mut ahmet, r#"{"Set":{"room":{"name":"OtherRoom"}}}"#).await;

        let announcement = read_until(&mut mehmet, "left").await;
        assert!(announcement.contains("ahmet"), "got {announcement}");
    }
}
