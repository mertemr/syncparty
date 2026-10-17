//! One connection: the handshake, then the message loop, then leaving.
//!
//! The loop does three things at once: it reads lines, it ticks this
//! connection's heartbeat, and before the greeting it watches a deadline. The
//! heartbeat is what a Syncplay client lives on — it hears where the room is
//! only from a `State`, and treats twelve and a half seconds without one as a
//! dead connection — so it runs whether or not anything else is happening.

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::sync::{mpsc, RwLock};
use tokio::time::{Interval, MissedTickBehavior};

use crate::core::error::Result;
use crate::core::events::{AppEvent, EventBus};
use crate::core::syncplay::protocol::{
    self, ListEntry, ServerFeatures, ServerRoomList, ServerToClient, MAX_CHAT_MESSAGE_LENGTH,
};
use crate::core::syncplay::server::auth;
use crate::core::syncplay::server::registry::Registry;
use crate::core::syncplay::server::room::{Force, OpenFile, Playstate, Room, StateUpdate};
use crate::core::syncplay::server::{stamp, ServerConfig};

/// Bounded on purpose: a peer that has stopped reading is a peer that has
/// left, and buffering without limit would turn one stalled client into the
/// whole server's problem.
const OUTBOUND_CAPACITY: usize = 64;

/// The longest line accepted, terminator included, before the greeting and
/// after it.
///
/// Twisted's `LineReceiver.MAX_LENGTH`, which upstream inherits and applies to
/// every line, so no Syncplay client sends a longer one. A line that never
/// ends would otherwise be memory without limit: from anybody at all until
/// the password has been checked, and after that from anybody holding an
/// invite, which is enough to take the host's whole party down.
const LINE_LIMIT: usize = 16 * 1024;

/// Serves one connection to completion.
///
/// Takes a stream rather than an address so the same core can be fed by a
/// loopback socket today and a QUIC bi-stream later without learning that
/// anything happened.
pub async fn serve<S>(
    stream: S,
    registry: Arc<RwLock<Registry>>,
    config: Arc<ServerConfig>,
    bus: Arc<dyn EventBus>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (reader, writer) = tokio::io::split(stream);
    let (outbound, inbound) = mpsc::channel::<String>(OUTBOUND_CAPACITY);
    let pump = tokio::spawn(pump(writer, inbound, config.timing.protocol_timeout));

    let mut session = Session {
        registry,
        config,
        bus,
        outbound,
        username: None,
        heartbeat: None,
    };

    let ending = session.run(reader).await;
    session.depart().await;

    // Drops this connection's last sender, which is what lets the pump finish
    // rather than wait for a line that is never coming.
    drop(session);

    match ending {
        // Whatever is still queued — a refusal, say — is worth delivering.
        Ok(Ending::Closed) => {
            let _ = pump.await;
            Ok(())
        }
        // Not to a peer that has stopped listening: waiting for it to drain
        // would hold the socket open for as long as the write timeout.
        Ok(Ending::Dropped) | Err(_) => {
            pump.abort();
            let _ = pump.await;
            ending.map(|_| ())
        }
    }
}

/// The only thing that touches the write half. Everything that wants to send
/// pushes a line in here instead, so no lock is ever held across a write.
///
/// A write that takes longer than `patience` means the peer has stopped
/// reading. The pump gives up on it, which closes the queue, and the session
/// notices at its next heartbeat.
async fn pump<W>(mut writer: W, mut inbound: mpsc::Receiver<String>, patience: Duration)
where
    W: AsyncWrite + Unpin,
{
    while let Some(line) = inbound.recv().await {
        let written = tokio::time::timeout(patience, writer.write_all(line.as_bytes())).await;
        if !matches!(written, Ok(Ok(()))) {
            break;
        }
    }
}

/// Why the message loop stopped.
enum Ending {
    /// The peer went, or the greeting was refused. Anything still queued is
    /// flushed.
    Closed,
    /// The server dropped the peer, which is not listening any more or never
    /// properly started. Nothing is flushed.
    Dropped,
}

/// What woke the message loop.
enum Event {
    Line(Option<Line>),
    Heartbeat,
    HelloOverdue,
}

enum Line {
    Text(String),
    TooLong,
}

struct Session {
    registry: Arc<RwLock<Registry>>,
    config: Arc<ServerConfig>,
    bus: Arc<dyn EventBus>,
    outbound: mpsc::Sender<String>,
    /// `None` until the greeting is accepted. Once set, it is the name the
    /// registry knows, which is not always the one that was asked for.
    username: Option<String>,
    /// This connection's own timer, as upstream gives every watcher one.
    /// `None` until the greeting: a connection that is in no room has no room
    /// to report.
    heartbeat: Option<Interval>,
}

impl Session {
    async fn run<R>(&mut self, reader: R) -> Result<Ending>
    where
        R: AsyncRead + Unpin,
    {
        let mut reader = BufReader::new(reader);
        let mut pending = Vec::new();
        let hello_deadline = tokio::time::sleep(self.config.timing.hello_deadline);
        tokio::pin!(hello_deadline);

        loop {
            let greeted = self.username.is_some();

            let event = tokio::select! {
                line = read_line(&mut reader, &mut pending, LINE_LIMIT) => Event::Line(line?),
                () = next_tick(&mut self.heartbeat) => Event::Heartbeat,
                () = &mut hello_deadline, if !greeted => Event::HelloOverdue,
            };

            match event {
                Event::Line(None) => return Ok(Ending::Closed),
                Event::Line(Some(Line::TooLong)) => {
                    self.log(match &self.username {
                        Some(username) => format!("dropped {username}, who sent an overlong line"),
                        None => "closed a connection that sent an overlong line before greeting"
                            .to_owned(),
                    });
                    return Ok(Ending::Dropped);
                }
                Event::Line(Some(Line::Text(line))) => {
                    if !self.handle(&line).await {
                        return Ok(Ending::Closed);
                    }
                }
                Event::Heartbeat => {
                    if !self.heartbeat().await {
                        return Ok(Ending::Dropped);
                    }
                }
                Event::HelloOverdue => {
                    self.log("closed a connection that never greeted".to_owned());
                    return Ok(Ending::Dropped);
                }
            }
        }
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
                "TLS" => self.send(ServerToClient::TlsRefusal),
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
            self.refuse("a greeting must name a user and a room");
            return false;
        }

        if !self.password_matches(payload) {
            self.refuse("the room password is wrong");
            return false;
        }

        let room = truncate(room, ServerFeatures::supported().max_room_name_length);

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
        let now = Instant::now();

        // One lock for the whole greeting, so nothing anybody else sends can
        // reach the newcomer before its own `Hello` does.
        let username = {
            let mut registry = self.registry.write().await;
            let username = registry.free_username(wanted);
            registry.join(&username, &room, self.outbound.clone());

            if let Some(joined) = registry.room_mut(&room) {
                // The room hears about the arrival before the arrival hears
                // anything, which is the order upstream greets in.
                joined.broadcast(
                    Some(&username),
                    &user_event(
                        &username,
                        &room,
                        json!({
                            "joined": true,
                            "version": client_version,
                            "features": features,
                        }),
                    ),
                );
                // Before the `Hello`, as upstream sends it. A Syncplay client
                // answers its greeting with the readiness it has stored, so
                // this is what puts a reconnecting guest back to
                // `readyAtStart` instead of keeping what it was last time.
                announce_readiness(joined, &username);
                joined.send_to(
                    &username,
                    &ServerToClient::Hello {
                        username: username.clone(),
                        room: room.clone(),
                        client_version,
                        features: ServerFeatures::supported(),
                    },
                );
            }

            deliver_list(&mut registry, &username);
            username
        };

        self.log(format!("{username} joined {room}"));
        self.username = Some(username);
        self.heartbeat = Some(heartbeat_from(
            now + self.config.timing.first_state_delay,
            self.config.timing.state_interval,
        ));

        true
    }

    /// One tick of this connection's timer: tell it where the room is, unless
    /// it has gone. `false` closes the connection.
    ///
    /// Gone means one of two things. Silence: nothing the room accepted for
    /// longer than the protocol timeout, which is how a peer that vanished
    /// without closing its socket is noticed. Or lagging: a line for it could
    /// not be queued, so it has stopped reading, and nothing it is sent from
    /// here on would arrive in order anyway.
    async fn heartbeat(&self) -> bool {
        let Some(username) = self.username.as_deref() else {
            return true;
        };
        let timeout = self.config.timing.protocol_timeout;
        let now = Instant::now();

        let gone = {
            let mut registry = self.registry.write().await;
            let Some(room) = registry.room_of_mut(username) else {
                return true;
            };

            let position = room.sample(now);
            let state = Playstate::of(room.playback(), position, false);
            let Some(user) = room.user_mut(username) else {
                return true;
            };

            if user.is_silent(timeout, now) {
                Some("has not answered")
            } else {
                user.send_state(&state, false, now);
                (user.lagging || user.outbound.is_closed()).then_some("stopped reading")
            }
        };

        match gone {
            Some(reason) => {
                self.log(format!("dropped {username}, who {reason}"));
                false
            }
            None => true,
        }
    }

    /// Server-side diagnostics, which used to be the Python child's stdout.
    fn log(&self, line: String) {
        self.bus.publish(AppEvent::ServerLog {
            line,
            is_error: false,
        });
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

    fn refuse(&self, message: &str) {
        self.send(ServerToClient::Error {
            message: message.to_owned(),
        });
    }

    /// Straight onto this connection's own queue, for the few replies that
    /// come before it is in any room. Everything after the greeting goes
    /// through the registry instead, under its lock, so it is ordered with
    /// everything else this connection is sent.
    fn send(&self, message: ServerToClient) {
        if let Ok(line) = message.to_line() {
            let _ = self.outbound.try_send(line);
        }
    }

    /// Runs `act` on this connection's room under the registry's write lock.
    ///
    /// Every send in here is queued without waiting, so holding the lock
    /// across them costs nothing; and holding it is what keeps the order the
    /// room decided things in the same order every recipient hears them in.
    async fn in_room<T>(&self, act: impl FnOnce(&mut Room, &str) -> T) -> Option<T> {
        let username = self.username.as_deref()?;
        let mut registry = self.registry.write().await;
        let room = registry.room_of_mut(username)?;

        Some(act(room, username))
    }

    async fn send_list(&self) {
        let Some(username) = self.username.as_deref() else {
            return;
        };

        deliver_list(&mut *self.registry.write().await, username);
    }

    /// One client's report of where it is.
    async fn state(&self, payload: &Value) {
        let envelope = payload.get("ignoringOnTheFly");
        let server_ack = envelope.and_then(|envelope| envelope["server"].as_u64());
        let client_value = envelope.and_then(|envelope| envelope["client"].as_u64());

        let update = StateUpdate {
            position: payload["playstate"]["position"].as_f64(),
            paused: payload["playstate"]["paused"].as_bool(),
            do_seek: payload["playstate"]["doSeek"].as_bool().unwrap_or_default(),
        };
        let ping = payload.get("ping");
        let now = Instant::now();

        self.in_room(|room, username| {
            let Some(user) = room.user_mut(username) else {
                return;
            };

            user.gate.observe(server_ack, client_value);

            // Read before the gate is consulted, as upstream reads it: a report
            // that describes the world before a force still measured a real
            // round trip, and still carries a stamp to hand back.
            if let Some(ping) = ping {
                user.ping.receive(
                    ping["latencyCalculation"].as_f64(),
                    ping["clientRtt"].as_f64().unwrap_or_default(),
                    stamp(now),
                );
                user.client_latency = ping["clientLatencyCalculation"]
                    .as_f64()
                    .filter(|stamp| *stamp != 0.0)
                    .map(|stamp| (stamp, now));
            }

            // Whatever this says describes the world before the last force.
            if !user.gate.accepts_updates() {
                return;
            }

            // Answering at all is what keeps a connection alive, with or
            // without a playstate: the room monitor never sends one.
            user.last_seen = now;
            let message_age =
                Duration::try_from_secs_f64(user.ping.forward_delay()).unwrap_or_default();

            match room.apply(username, update, message_age, now) {
                Force::Nothing => {}
                Force::Broadcast(state) => {
                    for (_, user) in room.users_mut() {
                        user.send_state(&state, true, now);
                    }
                }
                Force::CorrectSender { echo, real } => {
                    if let Some(user) = room.user_mut(username) {
                        user.send_state(&echo, true, now);
                        user.send_state(&real, true, now);
                    }
                }
            }
        })
        .await;
    }

    async fn chat(&self, payload: &Value) {
        let Some(message) = payload.as_str() else {
            return;
        };

        let message = truncate(message, MAX_CHAT_MESSAGE_LENGTH);
        self.in_room(|room, username| {
            room.broadcast(
                None,
                &ServerToClient::Chat {
                    username: username.to_owned(),
                    message,
                },
            );
        })
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
        let named = value.get("username").and_then(Value::as_str);
        let is_ready = value["isReady"].as_bool();
        let manually_initiated = value["manuallyInitiated"].as_bool().unwrap_or_default();

        self.in_room(|room, username| {
            // We advertise `setOthersReadiness: false`, so marking somebody
            // else is not something this server does. It must not land on the
            // sender instead, which is what reading past the name used to do.
            if named.is_some_and(|named| named != username) {
                return;
            }

            if let Some(user) = room.user_mut(username) {
                user.is_ready = is_ready;
            }

            room.broadcast(
                None,
                &ServerToClient::SetReady {
                    username: username.to_owned(),
                    is_ready,
                    manually_initiated,
                    set_by: None,
                },
            );
        })
        .await;
    }

    async fn set_file(&self, value: &Value) {
        let file = opened_file(value);

        // What goes back out is the client's own object, so fields we do not
        // model survive, with only the name cut to what we advertise. A file
        // that is not one goes out as `{}`, which is how upstream writes
        // "nothing open".
        let wire = match &file {
            Some(file) => {
                let mut wire = value.clone();
                wire["name"] = json!(file.name);
                wire
            }
            None => json!({}),
        };

        self.in_room(|room, username| {
            room.set_file(username, file);
            room.broadcast(
                None,
                &ServerToClient::SetUser {
                    username: username.to_owned(),
                    room: room.name().to_owned(),
                    file: Some(wire),
                    event: None,
                },
            );
        })
        .await;
    }

    /// Moves this connection to another room.
    ///
    /// Both rooms are told. Isolation means the one being left cannot see the
    /// one being joined, so without the farewell it would go on showing a
    /// watcher who is not there any more.
    ///
    /// The mover is the one who needs telling most. Nothing else would tell
    /// them where this room is — their next heartbeat is a second away and
    /// would not make them seek — so they get a forced `State` with a seek,
    /// as upstream sends one.
    async fn set_room(&mut self, value: &Value) {
        let Some(username) = self.username.clone() else {
            return;
        };
        let wanted = value["name"].as_str().map(str::trim).unwrap_or_default();
        if wanted.is_empty() {
            return;
        }
        let wanted = truncate(wanted, ServerFeatures::supported().max_room_name_length);
        let now = Instant::now();

        let previous = {
            let mut registry = self.registry.write().await;
            let Some(previous) = registry
                .room_of(&username)
                .map(|room| room.name().to_owned())
            else {
                return;
            };
            if previous == wanted {
                return;
            }

            if let Some(left) = registry.room_mut(&previous) {
                left.broadcast(
                    Some(&username),
                    &user_event(&username, &previous, json!({ "left": true })),
                );
            }

            // The position they were seeded with, not a second sample: with
            // them in the room, that could follow them and name them as
            // having set it, and a client ignores a seek it made itself.
            let position = registry.move_to(&username, &wanted, now);

            if let (Some(position), Some(room)) = (position, registry.room_mut(&wanted)) {
                let state = Playstate::of(room.playback(), position, true);
                let file = room
                    .users()
                    .get(&username)
                    .and_then(|user| user.file.as_ref())
                    .map(file_json);

                if let Some(user) = room.user_mut(&username) {
                    user.send_state(&state, true, now);
                }

                room.broadcast(
                    Some(&username),
                    &user_event(&username, &wanted, json!({ "joined": true })),
                );
                if let Some(file) = file {
                    room.broadcast(
                        None,
                        &ServerToClient::SetUser {
                            username: username.clone(),
                            room: wanted.clone(),
                            file: Some(file),
                            event: None,
                        },
                    );
                }
                announce_readiness(room, &username);
            }

            deliver_list(&mut registry, &username);
            previous
        };

        // Restarted rather than left running, as upstream restarts it: the
        // next heartbeat is a whole interval after the State that just went.
        self.heartbeat = Some(heartbeat_from(
            now + self.config.timing.state_interval,
            self.config.timing.state_interval,
        ));
        self.log(format!("{username} moved from {previous} to {wanted}"));
    }

    async fn controller_auth(&self, value: &Value) {
        let password = value["password"].as_str().unwrap_or_default();
        let target = value["room"].as_str();
        let salt = &self.config.salt;

        self.in_room(|room, username| {
            // Only ever the room they are in. Upstream checks whichever room
            // the message names and, on success, makes the sender a
            // controller of the room they are actually in — a room that
            // password was never checked against.
            let names_this_room = target.is_none_or(|target| target == room.name());
            let success =
                names_this_room && auth::check_controlled_room(room.name(), password, salt);

            if success {
                room.set_controller(username, true);
            }

            room.broadcast(
                None,
                &ServerToClient::SetControllerAuth {
                    user: username.to_owned(),
                    room: room.name().to_owned(),
                    success,
                },
            );
        })
        .await;
    }

    async fn relay_playlist(&self, value: &Value, is_change: bool) {
        self.in_room(|room, username| {
            let user = username.to_owned();
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

            room.broadcast(None, &message);
        })
        .await;
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

            if let Some(left) = room.as_deref().and_then(|name| registry.room_mut(name)) {
                let farewell = user_event(&username, left.name(), json!({ "left": true }));
                left.broadcast(None, &farewell);
            }

            room
        };

        if let Some(room) = room {
            self.log(format!("{username} left {room}"));
        }
    }
}

/// Reads one line, without its terminator. `None` at the end of the stream.
///
/// Cancel safe, which is what lets it sit in a `select!` beside the
/// heartbeat: whatever part of a line has arrived stays in `pending` when the
/// future is dropped, and the next call carries on from there. A line that
/// reaches `limit` without ending is reported rather than read on.
async fn read_line<R>(
    reader: &mut R,
    pending: &mut Vec<u8>,
    limit: usize,
) -> io::Result<Option<Line>>
where
    R: AsyncBufRead + Unpin,
{
    let room = limit.saturating_sub(pending.len()) as u64;
    (&mut *reader).take(room).read_until(b'\n', pending).await?;

    if pending.last() == Some(&b'\n') {
        let mut bytes = std::mem::take(pending);
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
        return text(bytes).map(Some);
    }

    if pending.len() >= limit {
        return Ok(Some(Line::TooLong));
    }

    // The end of the stream. Whatever arrived without a terminator is still
    // a line, the way `Lines` treats it.
    if pending.is_empty() {
        return Ok(None);
    }

    text(std::mem::take(pending)).map(Some)
}

/// Bad UTF-8 ends the connection, as it does upstream: a peer sending it is
/// not speaking this protocol at all.
fn text(bytes: Vec<u8>) -> io::Result<Line> {
    String::from_utf8(bytes)
        .map(Line::Text)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Waits for this connection's next heartbeat, or forever before it has a
/// timer at all.
async fn next_tick(heartbeat: &mut Option<Interval>) {
    match heartbeat {
        Some(interval) => {
            interval.tick().await;
        }
        None => std::future::pending().await,
    }
}

/// A heartbeat whose first tick is at `start`.
///
/// A late tick is not made up for with a burst: a client that is sent three
/// States at once replies three times, for nothing.
fn heartbeat_from(start: Instant, period: Duration) -> Interval {
    let period = period.max(Duration::from_millis(1));
    let mut interval = tokio::time::interval_at(tokio::time::Instant::from_std(start), period);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    interval
}

/// Tells `room` how ready `username` is, the newcomer included.
///
/// On arrival that is `null`: nobody knows yet, and a client that kept its
/// own idea from a previous connection should hear that the room does not
/// share it. On a room switch it is whatever they carried with them.
fn announce_readiness(room: &mut Room, username: &str) {
    let is_ready = room.users().get(username).and_then(|user| user.is_ready);

    room.broadcast(
        None,
        &ServerToClient::SetReady {
            username: username.to_owned(),
            is_ready,
            manually_initiated: false,
            set_by: None,
        },
    );
}

/// Sends `username` the room list they may see. Under the caller's lock, so
/// it lands in order with everything else sent to them.
fn deliver_list(registry: &mut Registry, username: &str) {
    let rooms: ServerRoomList = registry
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
                            is_ready: user.is_ready,
                            features: json!({}),
                        },
                    )
                })
                .collect();

            (name.to_owned(), watchers)
        })
        .collect();

    if let Some(room) = registry.room_of_mut(username) {
        room.send_to(username, &ServerToClient::List(rooms));
    }
}

/// `Set user` announcing that `username` arrived in or left `room`.
fn user_event(username: &str, room: &str, event: Value) -> ServerToClient {
    ServerToClient::SetUser {
        username: username.to_owned(),
        room: room.to_owned(),
        file: None,
        event: Some(event),
    }
}

/// The file a `Set file` describes, or `None` when it describes nothing.
///
/// `{}` and `null` are how a client says it has closed what it had open. Kept
/// as a nameless file, that would still count as having playback, and a
/// watcher with nothing open would go on being sampled at a stale position.
fn opened_file(value: &Value) -> Option<OpenFile> {
    let name = value
        .get("name")?
        .as_str()
        .filter(|name| !name.is_empty())?;

    Some(OpenFile {
        name: truncate(name, ServerFeatures::supported().max_filename_length),
        duration: value["duration"].as_f64(),
        size: value.get("size").cloned(),
    })
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

    use serde_json::Value;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

    use super::*;
    use crate::core::events::NullEventBus;
    use crate::core::syncplay::protocol;
    use crate::core::syncplay::server::auth;
    use crate::core::syncplay::server::ServerTiming;

    /// No heartbeat and no timeout within any test's lifetime. Every test that
    /// is not about the timer runs on this, so what it reads is only what it
    /// caused.
    fn quiet() -> ServerTiming {
        let never = Duration::from_secs(3600);
        ServerTiming {
            first_state_delay: never,
            state_interval: never,
            protocol_timeout: never,
            hello_deadline: Duration::from_secs(10),
        }
    }

    /// Heartbeats every few tens of milliseconds, and nobody timed out while a
    /// test is looking.
    fn brisk() -> ServerTiming {
        ServerTiming {
            first_state_delay: Duration::from_millis(20),
            state_interval: Duration::from_millis(50),
            protocol_timeout: Duration::from_secs(30),
            hello_deadline: Duration::from_secs(10),
        }
    }

    fn test_config(timing: ServerTiming) -> Arc<ServerConfig> {
        Arc::new(ServerConfig {
            port: 0,
            password: "swordfish".to_owned(),
            salt: "PEPPER".to_owned(),
            timing,
        })
    }

    /// Drives `serve` over an in-memory pipe and returns the client's half.
    /// No socket, no port, and no ordering between tests.
    fn connect(registry: Arc<RwLock<Registry>>) -> DuplexStream {
        connect_with(registry, quiet())
    }

    fn connect_with(registry: Arc<RwLock<Registry>>, timing: ServerTiming) -> DuplexStream {
        connect_through(registry, timing, 8192)
    }

    /// Over a pipe that holds at most `buffer` bytes each way, for the tests
    /// about a peer that stops reading.
    fn connect_through(
        registry: Arc<RwLock<Registry>>,
        timing: ServerTiming,
        buffer: usize,
    ) -> DuplexStream {
        let (client, server) = tokio::io::duplex(buffer);
        tokio::spawn(serve(
            server,
            registry,
            test_config(timing),
            Arc::new(NullEventBus),
        ));
        client
    }

    async fn write_line<W: AsyncWrite + Unpin>(client: &mut W, line: &str) {
        client
            .write_all(format!("{line}\r\n").as_bytes())
            .await
            .expect("write");
    }

    /// Returns an empty string at end of stream, so a test about the
    /// connection closing reads naturally. A hang is a failure, not a wait.
    async fn read_line<R: AsyncRead + Unpin>(client: &mut R) -> String {
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

    fn parse(line: &str) -> Value {
        serde_json::from_str(line).unwrap_or_else(|_| panic!("not JSON: {line}"))
    }

    fn state_of(line: &str) -> Value {
        parse(line)["State"].clone()
    }

    fn hello_line(username: &str, room: &str, password: &str) -> String {
        json!({
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

    /// Greets a client and reads up to its `List`, the last thing a greeting
    /// sends.
    async fn arrive(registry: &Arc<RwLock<Registry>>, name: &str, room: &str) -> DuplexStream {
        arrive_with(registry, name, room, quiet()).await
    }

    async fn arrive_with(
        registry: &Arc<RwLock<Registry>>,
        name: &str,
        room: &str,
        timing: ServerTiming,
    ) -> DuplexStream {
        let mut client = connect_with(Arc::clone(registry), timing);
        write_line(&mut client, &hello_line(name, room, "swordfish")).await;
        let _ = read_until(&mut client, r#""List""#).await;
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

        let greeting = read_until(&mut client, r#""Hello""#).await;
        assert!(greeting.contains("MovieNight"), "got {greeting}");

        let list = read_until(&mut client, r#""List""#).await;
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

    /// A connection that never greets holds a socket and a task for nothing,
    /// and upstream would hold them forever.
    #[tokio::test]
    async fn a_connection_that_never_greets_is_closed() {
        let timing = ServerTiming {
            hello_deadline: Duration::from_millis(100),
            ..quiet()
        };
        let mut client = connect_with(Registry::shared(), timing);

        assert!(
            read_line(&mut client).await.is_empty(),
            "the server should have hung up on a silent stranger"
        );
    }

    #[tokio::test]
    async fn the_greeting_deadline_means_nothing_once_greeted() {
        let timing = ServerTiming {
            hello_deadline: Duration::from_millis(100),
            ..quiet()
        };
        let registry = Registry::shared();
        let mut client = arrive_with(&registry, "ahmet", "MovieNight", timing).await;

        tokio::time::sleep(Duration::from_millis(250)).await;
        write_line(&mut client, r#"{"List":null}"#).await;

        assert!(read_line(&mut client).await.contains(r#""List""#));
    }

    #[tokio::test]
    async fn an_overlong_line_before_the_greeting_closes_the_connection() {
        let (mut read, mut write) = tokio::io::split(connect(Registry::shared()));
        tokio::spawn(async move {
            // Ignored: once the server hangs up, the rest has nowhere to go.
            let _ = write.write_all(&[b'a'; 64 * 1024]).await;
        });

        assert!(
            read_line(&mut read).await.is_empty(),
            "a stranger may not make the server buffer without limit"
        );
    }

    /// The limit is upstream's, which is generous: anything under it is read
    /// whole, however long.
    #[tokio::test]
    async fn a_long_line_within_the_limit_is_read_like_any_other() {
        let registry = Registry::shared();
        let mut ahmet = arrive(&registry, "ahmet", "MovieNight").await;

        let chat = json!({ "Chat": "a".repeat(15 * 1024) }).to_string();
        write_line(&mut ahmet, &chat).await;

        let relayed = parse(&read_until(&mut ahmet, "Chat").await);
        assert_eq!(
            relayed["Chat"]["message"]
                .as_str()
                .map(|message| message.chars().count()),
            Some(MAX_CHAT_MESSAGE_LENGTH)
        );
    }

    /// The password is in every invite, so being greeted proves little. One
    /// guest sending a line that never ends must not be able to make the host
    /// buffer it until the whole party falls over.
    #[tokio::test]
    async fn an_overlong_line_after_the_greeting_drops_the_connection_too() {
        let (ahmet, mut mehmet) = two_in_a_room().await;
        let (mut read, mut write) = tokio::io::split(ahmet);
        tokio::spawn(async move {
            let chat = json!({ "Chat": "a".repeat(32 * 1024) }).to_string();
            // Ignored: once the server hangs up, the rest has nowhere to go.
            let _ = write.write_all(format!("{chat}\r\n").as_bytes()).await;
        });

        let left = read_until(&mut mehmet, "left").await;
        assert!(
            left.contains("ahmet"),
            "the room is told he has gone: {left}"
        );
        assert!(hung_up(&mut read).await, "and his connection is closed");
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

        let greeting = read_until(&mut second, r#""Hello""#).await;
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

    /// Upstream cuts both, and a client that believed the advertised limits
    /// would otherwise disagree with the server about what a room is called.
    #[tokio::test]
    async fn names_are_cut_to_the_lengths_the_greeting_advertises() {
        let registry = Registry::shared();
        let mut client = connect(Arc::clone(&registry));
        write_line(
            &mut client,
            &hello_line("ahmet", &"r".repeat(50), "swordfish"),
        )
        .await;

        let greeting = parse(&read_until(&mut client, "Hello").await);
        let room = greeting["Hello"]["room"]["name"].as_str().expect("room");
        assert_eq!(room, "r".repeat(35));
        let _ = read_until(&mut client, r#""List""#).await;

        let open = json!({ "Set": { "file": { "name": "f".repeat(300), "duration": 60.0 } } });
        write_line(&mut client, &open.to_string()).await;

        let announced = parse(&read_until(&mut client, r#""file""#).await);
        let name = &announced["Set"]["user"]["ahmet"]["file"]["name"];
        assert_eq!(name.as_str().map(str::len), Some(250), "got {name}");

        write_line(&mut client, r#"{"List":null}"#).await;
        let list = parse(&read_until(&mut client, r#""List""#).await);
        let listed = &list["List"][room]["ahmet"]["file"]["name"];
        assert_eq!(listed.as_str().map(str::len), Some(250), "got {listed}");
    }

    // --------------------------------------------------------- message loop

    fn state_line(position: f64, paused: bool) -> String {
        json!({
            "State": { "playstate": { "position": position, "paused": paused } }
        })
        .to_string()
    }

    /// The same report, acknowledging a force this connection was sent.
    fn acked_state_line(position: f64, paused: bool, server: u64) -> String {
        json!({
            "State": {
                "playstate": { "position": position, "paused": paused },
                "ignoringOnTheFly": { "server": server },
            }
        })
        .to_string()
    }

    const OPEN_FILM: &str = r#"{"Set":{"file":{"name":"Film.mkv","duration":7200.0}}}"#;

    /// Reads until a line contains `needle`, so a test is not coupled to how
    /// many unrelated messages — heartbeats, mostly — happen to arrive first.
    async fn read_until<R: AsyncRead + Unpin>(client: &mut R, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);

        while Instant::now() < deadline {
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

    /// The reply a client owes every `State`: the ping echoed and, if the
    /// State was forced, the counter acknowledged. No playstate, like the room
    /// monitor's.
    fn reply_to(state: &Value) -> String {
        let mut reply = json!({
            "ping": {
                "latencyCalculation": state["ping"]["latencyCalculation"],
                "clientRtt": 0,
            }
        });
        if let Some(server) = state["ignoringOnTheFly"]["server"].as_u64() {
            reply["ignoringOnTheFly"] = json!({ "server": server });
        }

        json!({ "State": reply }).to_string()
    }

    /// Like `read_until`, but answering every `State` on the way, as a live
    /// client does.
    async fn answer_until(client: &mut DuplexStream, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);

        while Instant::now() < deadline {
            let line = read_line(client).await;
            assert!(
                !line.is_empty(),
                "the stream ended before {needle:?} arrived"
            );
            if line.contains(r#"{"State""#) {
                write_line(client, &reply_to(&state_of(&line))).await;
            }
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

    /// Whether the server closes this connection, reading past whatever was
    /// still on its way.
    async fn hung_up<R: AsyncRead + Unpin>(client: &mut R) -> bool {
        let mut rest = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), client.read_to_end(&mut rest))
            .await
            .is_ok()
    }

    async fn two_in_a_room() -> (DuplexStream, DuplexStream) {
        two_in_room("MovieNight").await
    }

    async fn two_in_room(room: &str) -> (DuplexStream, DuplexStream) {
        pair(&Registry::shared(), room, quiet()).await
    }

    /// ahmet, then mehmet, with ahmet's news of mehmet's arrival already read.
    async fn pair(
        registry: &Arc<RwLock<Registry>>,
        room: &str,
        timing: ServerTiming,
    ) -> (DuplexStream, DuplexStream) {
        let mut ahmet = arrive_with(registry, "ahmet", room, timing).await;
        let mehmet = arrive_with(registry, "mehmet", room, timing).await;
        let _ = read_until(&mut ahmet, r#""ready""#).await;

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

    /// Upstream starts everybody at "unknown" and says so to the whole room,
    /// the newcomer included: a client that remembers being ready from an
    /// earlier connection hears that the room does not.
    ///
    /// The newcomer hears it *before* its `Hello`. A Syncplay client answers
    /// the greeting with its stored readiness, or `readyAtStart` when it has
    /// none, so this is what puts a reconnecting guest back to not ready.
    /// Arriving after, it is overwritten by the readiness the client just
    /// reported from its previous connection.
    #[tokio::test]
    async fn an_arrival_is_announced_as_not_yet_known_to_be_ready() {
        let registry = Registry::shared();
        let mut ahmet = arrive(&registry, "ahmet", "MovieNight").await;

        let mut mehmet = connect(Arc::clone(&registry));
        write_line(
            &mut mehmet,
            &hello_line("mehmet", "MovieNight", "swordfish"),
        )
        .await;

        let _ = read_until(&mut ahmet, "joined").await;
        let ready = parse(&read_line(&mut ahmet).await);
        let ready = &ready["Set"]["ready"];
        assert_eq!(ready["username"], "mehmet", "got {ready}");
        assert!(ready["isReady"].is_null(), "got {ready}");
        assert_eq!(ready["manuallyInitiated"], false, "got {ready}");

        let own = parse(&read_line(&mut mehmet).await);
        let own = &own["Set"]["ready"];
        assert_eq!(own["username"], "mehmet", "the first thing he hears: {own}");
        assert!(own["isReady"].is_null(), "got {own}");
        let greeting = read_line(&mut mehmet).await;
        assert!(greeting.contains(r#""Hello""#), "and then: {greeting}");

        let list = parse(&read_until(&mut mehmet, r#""List""#).await);
        for name in ["ahmet", "mehmet"] {
            assert!(
                list["List"]["MovieNight"][name]["isReady"].is_null(),
                "nobody has said yet: {list}"
            );
        }
    }

    /// We advertise `setOthersReadiness: false`. Naming somebody else used to
    /// change the *sender's* readiness instead, which is worse than nothing.
    #[tokio::test]
    async fn readiness_naming_somebody_else_changes_nobody() {
        let (mut ahmet, _mehmet) = two_in_a_room().await;

        write_line(
            &mut ahmet,
            r#"{"Set":{"ready":{"username":"mehmet","isReady":true,"manuallyInitiated":true}}}"#,
        )
        .await;
        write_line(&mut ahmet, r#"{"List":null}"#).await;

        let next = read_line(&mut ahmet).await;
        assert!(
            next.contains(r#""List""#),
            "no readiness change may have been announced first: {next}"
        );
        let list = parse(&next);
        for name in ["ahmet", "mehmet"] {
            assert!(
                list["List"]["MovieNight"][name]["isReady"].is_null(),
                "{name} was changed: {list}"
            );
        }
    }

    #[tokio::test]
    async fn the_file_a_user_opens_shows_up_in_the_list() {
        let (mut ahmet, _mehmet) = two_in_a_room().await;

        write_line(&mut ahmet, OPEN_FILM).await;
        write_line(&mut ahmet, r#"{"List":null}"#).await;

        let list = read_until(&mut ahmet, "Film.mkv").await;
        assert!(list.contains("7200"), "got {list}");
    }

    /// `{}` is how a client says it closed its file. Stored as a file with no
    /// name, it would go on counting as playback and keep being sampled.
    #[tokio::test]
    async fn an_empty_file_closes_what_was_open() {
        let registry = Registry::shared();
        let mut ahmet = arrive(&registry, "ahmet", "MovieNight").await;

        for closed in [r#"{"Set":{"file":{}}}"#, r#"{"Set":{"file":null}}"#] {
            write_line(&mut ahmet, OPEN_FILM).await;
            let _ = read_until(&mut ahmet, "Film.mkv").await;

            write_line(&mut ahmet, closed).await;
            let announced = parse(&read_until(&mut ahmet, r#""file""#).await);
            assert_eq!(announced["Set"]["user"]["ahmet"]["file"], json!({}));

            write_line(&mut ahmet, r#"{"List":null}"#).await;
            let list = parse(&read_until(&mut ahmet, r#""List""#).await);
            assert_eq!(
                list["List"]["MovieNight"]["ahmet"]["file"],
                json!({}),
                "after {closed}"
            );

            let guard = registry.read().await;
            let room = guard.room("MovieNight").expect("room");
            assert!(room.users()["ahmet"].file.is_none(), "after {closed}");
        }
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

        write_line(&mut ahmet, &controller_auth_line(&room, "AB-123-456")).await;

        let reply = read_until(&mut ahmet, "controllerAuth").await;
        assert!(reply.contains(r#""success":true"#), "got {reply}");
        assert!(reply.contains(r#""user":"ahmet""#), "got {reply}");
    }

    #[tokio::test]
    async fn a_wrong_operator_password_grants_nothing() {
        let room = auth::controlled_room_name("MovieNight", "AB-123-456", "PEPPER");
        let (mut ahmet, _mehmet) = two_in_room(&room).await;

        write_line(&mut ahmet, &controller_auth_line(&room, "AB-123-999")).await;

        let reply = read_until(&mut ahmet, "controllerAuth").await;
        assert!(reply.contains(r#""success":false"#), "got {reply}");
    }

    /// The right password for a *different* room. Upstream checks the room the
    /// message names and then crowns the sender in the room they are in.
    #[tokio::test]
    async fn an_operator_password_for_another_room_grants_nothing_here() {
        let elsewhere = auth::controlled_room_name("OtherRoom", "AB-123-456", "PEPPER");
        let (mut ahmet, _mehmet) = two_in_a_room().await;

        write_line(&mut ahmet, &controller_auth_line(&elsewhere, "AB-123-456")).await;

        let reply = read_until(&mut ahmet, "controllerAuth").await;
        assert!(reply.contains(r#""success":false"#), "got {reply}");

        write_line(&mut ahmet, r#"{"List":null}"#).await;
        let list = parse(&read_until(&mut ahmet, r#""List""#).await);
        assert_eq!(list["List"]["MovieNight"]["ahmet"]["controller"], false);
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

    /// Nothing else would tell the mover where the new room is: a heartbeat
    /// does not make a client seek.
    #[tokio::test]
    async fn a_room_switch_sends_the_mover_where_the_new_room_is() {
        let registry = Registry::shared();
        let mut ayse = arrive(&registry, "ayse", "OtherRoom").await;
        write_line(&mut ayse, &state_line(500.0, false)).await;
        let _ = read_until(&mut ayse, r#""server":1"#).await;
        write_line(&mut ayse, &acked_state_line(500.0, false, 1)).await;

        let mut ahmet = arrive(&registry, "ahmet", "MovieNight").await;
        write_line(&mut ahmet, r#"{"Set":{"room":{"name":"OtherRoom"}}}"#).await;

        let forced = state_of(&read_until(&mut ahmet, "State").await);
        let playstate = &forced["playstate"];
        assert_eq!(playstate["doSeek"], true, "got {forced}");
        assert_eq!(playstate["paused"], false, "got {forced}");
        let position = playstate["position"].as_f64().expect("position");
        assert!((500.0..502.0).contains(&position), "got {forced}");
        assert_eq!(
            forced["ignoringOnTheFly"]["server"], 1,
            "forced, so it must be acknowledged: {forced}"
        );

        let joined = read_until(&mut ayse, "joined").await;
        assert!(joined.contains("ahmet"), "got {joined}");

        let list = parse(&read_until(&mut ahmet, r#""List""#).await);
        assert!(list["List"]["OtherRoom"]["ayse"].is_object(), "got {list}");
    }

    /// The room being joined holds somebody with nothing open, as the host's
    /// room always does: the room monitor never opens a file. Once the mover
    /// is in, they are its only candidate, and a State rendered from a second
    /// sample would name them — and a client ignores a seek it is told it made
    /// itself, so they would stay on the old film's position.
    #[tokio::test]
    async fn a_room_switch_never_names_the_mover_as_who_set_the_room() {
        let registry = Registry::shared();
        let _monitor = arrive(&registry, "monitor", "OtherRoom").await;

        let mut ahmet = arrive(&registry, "ahmet", "MovieNight").await;
        write_line(&mut ahmet, OPEN_FILM).await;
        write_line(&mut ahmet, &state_line(3000.0, false)).await;
        let _ = read_until(&mut ahmet, r#""server":1"#).await;
        write_line(&mut ahmet, &acked_state_line(3000.0, false, 1)).await;

        // Long enough that the room he moves to is due a fresh sample.
        tokio::time::sleep(Duration::from_millis(1200)).await;
        write_line(&mut ahmet, r#"{"Set":{"room":{"name":"OtherRoom"}}}"#).await;

        let forced = state_of(&read_until(&mut ahmet, r#""doSeek":true"#).await);
        let playstate = &forced["playstate"];
        assert_ne!(
            playstate["setBy"], "ahmet",
            "he would ignore the seek: {forced}"
        );
        assert_eq!(playstate["position"], 0.0, "got {forced}");
        assert_eq!(playstate["paused"], true, "got {forced}");
    }

    /// A switch cuts the name as a greeting does, or the mover and the room
    /// he meant would be two different rooms. And the room he joins hears how
    /// ready he is, since he brings that with him.
    #[tokio::test]
    async fn a_switch_cuts_the_room_name_and_tells_the_new_room_how_ready_the_mover_is() {
        let registry = Registry::shared();
        let cut = "r".repeat(35);
        let mut ayse = arrive(&registry, "ayse", &cut).await;

        let mut ahmet = arrive(&registry, "ahmet", "MovieNight").await;
        write_line(
            &mut ahmet,
            r#"{"Set":{"ready":{"isReady":true,"manuallyInitiated":true}}}"#,
        )
        .await;
        let _ = read_until(&mut ahmet, r#""isReady":true"#).await;

        let switch = json!({ "Set": { "room": { "name": "r".repeat(50) } } });
        write_line(&mut ahmet, &switch.to_string()).await;

        let joined = parse(&read_until(&mut ayse, "joined").await);
        assert_eq!(
            joined["Set"]["user"]["ahmet"]["room"]["name"],
            cut.as_str(),
            "got {joined}"
        );
        let ready = parse(&read_until(&mut ayse, r#""ready""#).await);
        assert_eq!(ready["Set"]["ready"]["username"], "ahmet", "got {ready}");
        assert_eq!(ready["Set"]["ready"]["isReady"], true, "got {ready}");

        let list = parse(&read_until(&mut ahmet, r#""List""#).await);
        let room = &list["List"][cut.as_str()];
        assert!(room["ayse"].is_object(), "the room ayse is in: {list}");
        assert_eq!(room["ahmet"]["isReady"], true, "got {list}");
    }

    /// The privilege escalation: an operator of one controlled room switching
    /// into another and finding themselves in charge of it.
    #[tokio::test]
    async fn switching_rooms_leaves_controller_status_behind() {
        let first = auth::controlled_room_name("MovieNight", "AB-123-456", "PEPPER");
        let second = auth::controlled_room_name("OtherRoom", "AB-123-456", "PEPPER");
        let registry = Registry::shared();
        let mut ahmet = arrive(&registry, "ahmet", &first).await;

        write_line(&mut ahmet, &controller_auth_line(&first, "AB-123-456")).await;
        let granted = read_until(&mut ahmet, "controllerAuth").await;
        assert!(granted.contains(r#""success":true"#), "got {granted}");

        write_line(
            &mut ahmet,
            &json!({ "Set": { "room": { "name": second } } }).to_string(),
        )
        .await;
        let list = parse(&read_until(&mut ahmet, r#""List""#).await);
        assert_eq!(list["List"][second.as_str()]["ahmet"]["controller"], false);

        // Starting the film is a request here, not a decision: he is corrected
        // rather than obeyed. The 1 acknowledges the switch's own force.
        write_line(&mut ahmet, &acked_state_line(0.0, false, 1)).await;
        let _echo = state_of(&read_line(&mut ahmet).await);
        let real = state_of(&read_line(&mut ahmet).await);
        assert_eq!(real["playstate"]["doSeek"], true, "got {real}");
        assert_eq!(
            real["playstate"]["paused"], true,
            "the room he moved to stays paused: {real}"
        );
    }

    fn controller_auth_line(room: &str, password: &str) -> String {
        json!({
            "Set": { "controllerAuth": { "password": password, "room": room } }
        })
        .to_string()
    }

    /// A correction has to carry where the room *is*, not the last position
    /// recorded against it. Those are the same thing only for the first second
    /// after somebody moves it; after that a playing room has gone on without
    /// anybody writing it down, and handing back the stale number would drag
    /// the watcher backwards.
    #[tokio::test]
    async fn a_correction_carries_where_the_room_is_now_not_what_was_last_recorded() {
        let room = auth::controlled_room_name("MovieNight", "AB-123-456", "PEPPER");
        let (mut ahmet, mut mehmet) = two_in_room(&room).await;

        write_line(&mut mehmet, &controller_auth_line(&room, "AB-123-456")).await;
        let _ = read_until(&mut mehmet, "controllerAuth").await;

        // The controller starts the room playing from zero.
        write_line(&mut mehmet, &state_line(0.0, false)).await;
        let _ = read_until(&mut mehmet, "playstate").await;
        let _ = read_until(&mut ahmet, "playstate").await;

        tokio::time::sleep(Duration::from_millis(1200)).await;

        // ahmet may not move this room, so he is corrected instead.
        write_line(&mut ahmet, &acked_state_line(600.0, true, 1)).await;

        let correction = read_until(&mut ahmet, "playstate").await;
        let value: Value = serde_json::from_str(&correction).expect("json");
        let position = value["State"]["playstate"]["position"]
            .as_f64()
            .expect("position");

        assert!(
            position > 1.0,
            "the room has been playing for over a second: {correction}"
        );
    }

    // ----------------------------------------------------------- heartbeat

    #[tokio::test]
    async fn the_first_heartbeat_follows_the_greeting_and_the_rest_keep_time() {
        let timing = ServerTiming {
            first_state_delay: Duration::from_millis(100),
            state_interval: Duration::from_millis(300),
            ..brisk()
        };
        let mut client = connect_with(Registry::shared(), timing);
        write_line(&mut client, &hello_line("ahmet", "MovieNight", "swordfish")).await;

        let _ = read_until(&mut client, "Hello").await;
        let greeted = Instant::now();
        let first = state_of(&read_until(&mut client, r#"{"State""#).await);
        let first_at = Instant::now();
        let _ = read_until(&mut client, r#"{"State""#).await;
        let second_at = Instant::now();

        let delay = first_at - greeted;
        assert!(
            (Duration::from_millis(50)..=Duration::from_millis(250)).contains(&delay),
            "the first State is a tenth of a second after the greeting: {delay:?}"
        );
        let gap = second_at - first_at;
        assert!(
            (Duration::from_millis(200)..=Duration::from_millis(450)).contains(&gap),
            "and the next one an interval after that: {gap:?}"
        );

        // A heartbeat reports; it commands nothing.
        assert_eq!(first["playstate"]["doSeek"], false, "got {first}");
        assert_eq!(first["playstate"]["paused"], true, "got {first}");
        assert!(first.get("ignoringOnTheFly").is_none(), "got {first}");
        assert_eq!(first["ping"]["serverRtt"], 0.0, "nothing measured yet");
        assert!(first["ping"].get("clientLatencyCalculation").is_none());
        assert!(first["ping"]["latencyCalculation"]
            .as_f64()
            .is_some_and(|stamp| stamp > 1.0e9));
    }

    /// The client has not yet heard where the room went, so nothing may tell
    /// it the room is anywhere else until it says it has.
    #[tokio::test]
    async fn a_heartbeat_waits_while_a_force_is_unacknowledged() {
        let registry = Registry::shared();
        let (mut ahmet, mut mehmet) = pair(&registry, "MovieNight", brisk()).await;

        write_line(&mut ahmet, &state_line(0.0, false)).await;
        let _ = read_until(&mut mehmet, r#""server":1"#).await;

        assert!(
            silence(&mut mehmet).await.is_none(),
            "ten heartbeats' worth of time, and not one of them may go out"
        );

        write_line(
            &mut mehmet,
            r#"{"State":{"ignoringOnTheFly":{"server":1}}}"#,
        )
        .await;
        let resumed = state_of(&read_until(&mut mehmet, r#"{"State""#).await);
        assert!(resumed.get("ignoringOnTheFly").is_none(), "got {resumed}");
        assert_eq!(resumed["playstate"]["paused"], false, "got {resumed}");
    }

    /// A client waiting for its own counter ignores the room. When nothing it
    /// did forced a State, a heartbeat is the only thing that can hand the
    /// counter back.
    #[tokio::test]
    async fn a_heartbeat_hands_the_clients_own_counter_back_to_it() {
        let registry = Registry::shared();
        let mut ahmet = arrive_with(&registry, "ahmet", "MovieNight", brisk()).await;

        // A change of its own that moves nothing: the room is already there.
        write_line(
            &mut ahmet,
            r#"{"State":{"playstate":{"position":0.0,"paused":true},"ignoringOnTheFly":{"client":1}}}"#,
        )
        .await;

        let echoed = state_of(&read_until(&mut ahmet, r#""client":1"#).await);
        assert_eq!(
            echoed["ignoringOnTheFly"],
            json!({ "client": 1 }),
            "a heartbeat, not a force, carries it"
        );

        let after = state_of(&read_until(&mut ahmet, r#"{"State""#).await);
        assert!(
            after.get("ignoringOnTheFly").is_none(),
            "echoed exactly once: {after}"
        );
    }

    #[tokio::test]
    async fn the_round_trip_is_measured_and_the_clients_stamp_handed_back() {
        let timing = ServerTiming {
            state_interval: Duration::from_millis(200),
            ..brisk()
        };
        let registry = Registry::shared();
        let mut ahmet = arrive_with(&registry, "ahmet", "MovieNight", timing).await;

        let first = state_of(&read_until(&mut ahmet, r#"{"State""#).await);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let reply = json!({
            "State": {
                "ping": {
                    "latencyCalculation": first["ping"]["latencyCalculation"],
                    "clientLatencyCalculation": 1000.0,
                    "clientRtt": 0.0,
                }
            }
        });
        write_line(&mut ahmet, &reply.to_string()).await;

        let measured = state_of(&read_until(&mut ahmet, "clientLatencyCalculation").await);
        let rtt = measured["ping"]["serverRtt"].as_f64().expect("serverRtt");
        assert!(
            (0.04..1.0).contains(&rtt),
            "about the fifty milliseconds the reply was held back: {rtt}"
        );
        let handed_back = measured["ping"]["clientLatencyCalculation"]
            .as_f64()
            .expect("stamp");
        assert!(
            (1000.0..1000.5).contains(&handed_back),
            "the client's own stamp, plus the time the server sat on it: {handed_back}"
        );

        let next = state_of(&read_until(&mut ahmet, r#"{"State""#).await);
        assert!(
            next["ping"].get("clientLatencyCalculation").is_none(),
            "once per client report: {next}"
        );
        assert_eq!(
            next["ping"]["serverRtt"], measured["ping"]["serverRtt"],
            "the last round trip measured, until there is another"
        );
    }

    /// A peer that vanished without closing its socket. Left in, it would sit
    /// in the room frozen, and the room would follow it.
    #[tokio::test]
    async fn a_connection_that_stops_answering_is_dropped() {
        let timing = ServerTiming {
            protocol_timeout: Duration::from_millis(400),
            ..brisk()
        };
        let registry = Registry::shared();
        let (mut ahmet, mut mehmet) = pair(&registry, "MovieNight", timing).await;

        // ahmet answers every State, as the room monitor does; mehmet never
        // answers anything.
        let left = answer_until(&mut ahmet, "left").await;
        assert!(left.contains("mehmet"), "got {left}");
        assert!(hung_up(&mut mehmet).await, "and his connection is closed");

        let answered_until = Instant::now() + Duration::from_millis(800);
        while Instant::now() < answered_until {
            let _ = answer_until(&mut ahmet, r#"{"State""#).await;
        }
        let guard = registry.read().await;
        assert!(
            guard
                .room("MovieNight")
                .expect("room")
                .users()
                .contains_key("ahmet"),
            "answering is what keeps a connection, playstate or not"
        );
    }

    /// One peer that stops reading used to stall every session that sent it
    /// anything, since each send waited for room in its queue.
    #[tokio::test]
    async fn a_peer_that_stops_reading_is_disconnected_rather_than_waited_for() {
        let registry = Registry::shared();
        let (mut ahmet, mut mehmet) = pair(&registry, "MovieNight", brisk()).await;

        // Far more than fits between mehmet and the server, none of it read.
        // Ignored: the server may hang up before the last of it is written.
        let _ = mehmet
            .write_all("{\"List\":null}\r\n".repeat(400).as_bytes())
            .await;

        let left = answer_until(&mut ahmet, "left").await;
        assert!(left.contains("mehmet"), "got {left}");
        assert!(
            hung_up(&mut mehmet).await,
            "the server hangs up rather than waiting for him to read"
        );
    }

    /// Upstream's timer keeps its own time: a force in between does not push
    /// the next heartbeat back, whether the force came from this connection's
    /// own report or from somebody else's.
    #[tokio::test]
    async fn a_forced_state_does_not_put_the_next_heartbeat_back() {
        let timing = ServerTiming {
            first_state_delay: Duration::from_millis(100),
            state_interval: Duration::from_millis(400),
            ..brisk()
        };

        for own in [true, false] {
            let registry = Registry::shared();
            let (mut ahmet, mut mehmet) = pair(&registry, "MovieNight", timing).await;
            let _ = read_until(&mut ahmet, r#"{"State""#).await;
            let beat = Instant::now();

            tokio::time::sleep(Duration::from_millis(200)).await;
            let forcer = if own { &mut ahmet } else { &mut mehmet };
            write_line(forcer, &state_line(0.0, false)).await;
            let _ = read_until(&mut ahmet, r#""server":1"#).await;
            write_line(&mut ahmet, r#"{"State":{"ignoringOnTheFly":{"server":1}}}"#).await;

            let next = state_of(&read_until(&mut ahmet, r#"{"State""#).await);
            let gap = beat.elapsed();
            assert!(next.get("ignoringOnTheFly").is_none(), "got {next}");
            assert!(
                (Duration::from_millis(250)..=Duration::from_millis(520)).contains(&gap),
                "one interval after the last heartbeat, not after the force \
                 (own report: {own}): {gap:?}"
            );
        }
    }

    /// A report has been on its way for the forward delay by the time it
    /// arrives, and a playing film has moved on by that much since. Measuring
    /// the delay is for nothing unless the room hears about it.
    #[tokio::test]
    async fn a_playing_report_is_carried_forward_by_how_long_it_took_to_arrive() {
        let registry = Registry::shared();
        let mut ahmet = arrive_with(&registry, "ahmet", "MovieNight", brisk()).await;

        let heartbeat = state_of(&read_until(&mut ahmet, r#"{"State""#).await);
        // Held back, so the round trip is a fifth of a second at least, and
        // the client claims none of it for its own leg.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let report = json!({
            "State": {
                "ping": {
                    "latencyCalculation": heartbeat["ping"]["latencyCalculation"],
                    "clientRtt": 0.0,
                },
                "playstate": { "position": 100.0, "paused": false },
            }
        });
        write_line(&mut ahmet, &report.to_string()).await;

        let forced = state_of(&read_until(&mut ahmet, r#""server":1"#).await);
        let carried = forced["playstate"]["position"].as_f64().expect("position") - 100.0;
        assert!(
            (0.25..2.0).contains(&carried),
            "half the average round trip plus the part the client did not \
             account for, about three tenths of a second: {forced}"
        );
    }

    /// A peer that stops reading fills the pipe long before it fills its
    /// queue. The write that cannot finish is what gives up on it then; the
    /// queue alone would take more than sixty heartbeats to notice.
    #[tokio::test]
    async fn a_write_that_cannot_finish_gives_up_on_the_peer() {
        let timing = ServerTiming {
            protocol_timeout: Duration::from_millis(300),
            ..brisk()
        };
        let registry = Registry::shared();
        let mut ahmet = arrive(&registry, "ahmet", "MovieNight").await;

        // Room for barely one line, and none of it is ever read.
        let mut mehmet = connect_through(Arc::clone(&registry), timing, 64);
        write_line(
            &mut mehmet,
            &hello_line("mehmet", "MovieNight", "swordfish"),
        )
        .await;
        // He goes on answering, so it is not silence that ends him.
        let answering = tokio::spawn(async move {
            while mehmet.write_all(b"{\"State\":{}}\r\n").await.is_ok() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });

        let left =
            tokio::time::timeout(Duration::from_millis(1500), read_until(&mut ahmet, "left"))
                .await
                .expect("the write timeout, not a full queue, should have ended him by now");
        assert!(left.contains("mehmet"), "got {left}");
        answering.abort();
    }

    /// Everybody else corrects towards the watcher furthest behind, which only
    /// works if the heartbeat names them — clients never correct against
    /// themselves.
    #[tokio::test]
    async fn a_heartbeat_follows_the_slowest_watcher_and_names_them() {
        let timing = ServerTiming {
            state_interval: Duration::from_millis(100),
            ..brisk()
        };
        let registry = Registry::shared();
        let (mut ahmet, mut mehmet) = pair(&registry, "MovieNight", timing).await;
        write_line(&mut ahmet, OPEN_FILM).await;
        write_line(&mut mehmet, OPEN_FILM).await;

        write_line(&mut ahmet, &state_line(100.0, false)).await;
        let _ = read_until(&mut ahmet, r#""server":1"#).await;
        let _ = read_until(&mut mehmet, r#""server":1"#).await;
        write_line(&mut ahmet, &acked_state_line(100.0, false, 1)).await;
        write_line(&mut mehmet, &acked_state_line(90.0, false, 1)).await;

        let followed = state_of(&read_until(&mut ahmet, r#""setBy":"mehmet""#).await);
        let position = followed["playstate"]["position"]
            .as_f64()
            .expect("position");
        assert!(
            (90.0..93.0).contains(&position),
            "mehmet's position, carried forward from when he reported it: {followed}"
        );
    }

    /// The rewind a heartbeat would cause without it: mehmet's stored report is
    /// from before the seek, his newer ones are dropped until he acknowledges
    /// it, and a second later the room would follow him back.
    #[tokio::test]
    async fn nobody_is_sent_back_after_a_seek() {
        let timing = ServerTiming {
            state_interval: Duration::from_millis(100),
            ..brisk()
        };
        let registry = Registry::shared();
        let (mut ahmet, mut mehmet) = pair(&registry, "MovieNight", timing).await;
        write_line(&mut ahmet, OPEN_FILM).await;
        write_line(&mut mehmet, OPEN_FILM).await;

        write_line(&mut ahmet, &state_line(10.0, false)).await;
        let _ = read_until(&mut ahmet, r#""server":1"#).await;
        let _ = read_until(&mut mehmet, r#""server":1"#).await;
        write_line(&mut ahmet, &acked_state_line(10.0, false, 1)).await;
        write_line(&mut mehmet, &acked_state_line(10.0, false, 1)).await;

        let seek = json!({
            "State": { "playstate": { "position": 600.0, "paused": false, "doSeek": true } }
        });
        write_line(&mut ahmet, &seek.to_string()).await;
        // The counter starts again from one: the first force was acknowledged.
        let _ = read_until(&mut ahmet, r#""doSeek":true"#).await;
        let sought = Instant::now();
        write_line(&mut ahmet, &acked_state_line(600.0, false, 1)).await;
        // Still where he was, and not yet acknowledging: dropped.
        write_line(&mut mehmet, &state_line(11.0, false)).await;

        while sought.elapsed() < Duration::from_millis(1500) {
            let heartbeat = state_of(&read_until(&mut ahmet, r#"{"State""#).await);
            let position = heartbeat["playstate"]["position"]
                .as_f64()
                .expect("position");
            assert!(
                position > 599.0,
                "the room went to 600 and must not come back: {heartbeat}"
            );
        }
    }
}
