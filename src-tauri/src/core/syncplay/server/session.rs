//! One connection: the handshake, then the message loop, then leaving.

use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, RwLock};

use crate::core::error::Result;
use crate::core::syncplay::protocol::{
    self, ListEntry, ServerFeatures, ServerRoomList, ServerToClient,
};
use crate::core::syncplay::server::registry::Registry;
use crate::core::syncplay::server::room::OpenFile;
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
}
