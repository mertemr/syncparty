//! The connection task: the socket, the player, and the clock the engine is
//! not allowed to have.
//!
//! One task owns everything mutable — the engine, the connection and the
//! player — and reacts to whichever of them speaks next: a line from the
//! server, an event from the player, the tenth-of-a-second poll, a request
//! from the app, or a timer. Nothing is shared, so nothing is locked.
//!
//! Player commands are carried out before the task listens again. The engine
//! has already moved its model of the player when it asks for a command, and
//! the next poll must find the player where the model says it is, or the
//! engine's own correction would look like the user seeking.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use crate::core::player::{Player, PlayerEvent, PlayerEvents};
use crate::core::syncplay::ping::epoch_seconds;
use crate::core::syncplay::protocol::{ClientMessage, ServerMessage};

use super::engine::{Engine, EngineConfig, PlayerCommand, Step, Tuning};
use super::notice::{Notice, NOTICE_DURATION};
use super::{ClientConfig, ClientStatus, ConnectionState};

/// What the [`super::SyncClient`] handle asks of the task.
pub(super) enum Command {
    Attach(Arc<dyn Player>, PlayerEvents),
    Detach,
    SetReady(bool),
    Stop,
}

/// A line longer than this is not a protocol message. Generous next to
/// upstream's 16 KiB, so a big room list is never mistaken for an attack.
const MAX_LINE_LENGTH: usize = 1 << 20;

/// A player that takes longer than this to accept a command has stopped
/// answering. Its own close event will follow; the connection must not wait.
const PLAYER_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

/// Backoff stops doubling after this many attempts.
const MAX_BACKOFF_EXPONENT: u32 = 5;

enum Inbound {
    Line(String),
    Closed(String),
}

/// One TCP connection: a reader task and a writer task, both ended when this
/// is dropped, which is what closes the socket.
struct Link {
    inbound: mpsc::UnboundedReceiver<Inbound>,
    outbound: mpsc::UnboundedSender<String>,
    tasks: [JoinHandle<()>; 2],
    /// When the server last sent a `State` — or when the connection opened,
    /// before it has. The liveness timeout runs from here.
    last_state: Instant,
}

impl Link {
    fn open(stream: TcpStream, write_timeout: Duration) -> Self {
        // States are small and latency is the whole point; Nagle would only
        // hold them back.
        let _ = stream.set_nodelay(true);
        let (read, write) = stream.into_split();

        let (inbound_sender, inbound) = mpsc::unbounded_channel();
        let (outbound, outbound_receiver) = mpsc::unbounded_channel();
        let reader = tokio::spawn(read_lines(read, inbound_sender.clone()));
        let writer = tokio::spawn(write_lines(
            write,
            outbound_receiver,
            inbound_sender,
            write_timeout,
        ));

        Self {
            inbound,
            outbound,
            tasks: [reader, writer],
            last_state: Instant::now(),
        }
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

struct Attached {
    player: Arc<dyn Player>,
    events: PlayerEvents,
}

type Connecting = Pin<Box<dyn Future<Output = Result<TcpStream, String>> + Send>>;

struct Driver {
    config: ClientConfig,
    engine: Engine,
    status: watch::Sender<ClientStatus>,
    connection: ConnectionState,
    link: Option<Link>,
    connecting: Option<Connecting>,
    retry_at: Option<Instant>,
    attempt: u32,
    ever_greeted: bool,
    attached: Option<Attached>,
}

pub(super) async fn run(
    config: ClientConfig,
    status: watch::Sender<ClientStatus>,
    mut commands: mpsc::UnboundedReceiver<Command>,
) {
    let mut poll = tokio::time::interval(config.timing.player_poll);
    poll.set_missed_tick_behavior(MissedTickBehavior::Delay);

    let mut driver = Driver::new(config, status);
    driver.start_connecting();

    loop {
        let deadline = driver.deadline();

        tokio::select! {
            command = commands.recv() => match command {
                None | Some(Command::Stop) => break,
                Some(command) => driver.command(command).await,
            },
            result = until_connected(driver.connecting.as_mut()) => {
                driver.connect_finished(result).await;
            }
            inbound = next_inbound(driver.link.as_mut()) => driver.inbound(inbound).await,
            event = next_event(driver.attached.as_mut()) => driver.player_event(event).await,
            _ = poll.tick(), if driver.attached.is_some() => driver.poll().await,
            () = sleep_until(deadline) => driver.timer_fired().await,
        }

        driver.publish();
    }

    // Dropping the driver drops the link, which closes the socket.
}

impl Driver {
    fn new(config: ClientConfig, status: watch::Sender<ClientStatus>) -> Self {
        let engine = Engine::new(EngineConfig {
            username: config.username.clone(),
            room: config.room.clone(),
            password: config.password.clone(),
            load_settle: config.timing.load_settle,
            tuning: Tuning::default(),
        });

        Self {
            config,
            engine,
            status,
            connection: ConnectionState::Connecting,
            link: None,
            connecting: None,
            retry_at: None,
            attempt: 0,
            ever_greeted: false,
            attached: None,
        }
    }

    /// The next time a timer branch has something to check.
    fn deadline(&self) -> Option<Instant> {
        let liveness = self
            .link
            .as_ref()
            .map(|link| link.last_state + self.config.timing.protocol_timeout);

        match (liveness, self.retry_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn start_connecting(&mut self) {
        let address = self.config.address;
        let timeout = self.config.timing.connect_timeout;

        self.connecting = Some(Box::pin(async move {
            match tokio::time::timeout(timeout, TcpStream::connect(address)).await {
                Ok(Ok(stream)) => Ok(stream),
                Ok(Err(error)) => Err(format!("{address}: {error}")),
                Err(_) => Err(format!("{address} did not answer")),
            }
        }));
    }

    async fn connect_finished(&mut self, result: Result<TcpStream, String>) {
        self.connecting = None;

        match result {
            Ok(stream) => {
                self.link = Some(Link::open(stream, self.config.timing.protocol_timeout));
                let step = self.engine.connected();
                self.execute(step).await;
            }
            Err(reason) => {
                tracing::debug!(%reason, "could not connect to the sync server");
                self.schedule_retry().await;
            }
        }
    }

    async fn inbound(&mut self, inbound: Inbound) {
        match inbound {
            Inbound::Line(line) => self.line(&line).await,
            Inbound::Closed(reason) => self.lost(&reason).await,
        }
    }

    async fn line(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }

        // Upstream drops the connection on anything it cannot read, and stops
        // reconnecting too. A message from a newer server is no reason to end
        // the film, so it is skipped instead.
        let messages = match ServerMessage::all_from_line(line) {
            Ok(messages) => messages,
            Err(error) => {
                tracing::debug!(%error, "ignoring a line from the sync server that is not JSON");
                return;
            }
        };

        for message in messages {
            let now = Instant::now();

            match &message {
                ServerMessage::Error { message } => {
                    self.rejected(message.clone());
                    return;
                }
                ServerMessage::State(_) => {
                    if let Some(link) = self.link.as_mut() {
                        link.last_state = now;
                    }
                }
                ServerMessage::Hello(_) => {
                    self.connection = ConnectionState::Connected;
                    self.attempt = 0;
                    self.ever_greeted = true;
                }
                // Not logged with its contents: chat and file names travel in
                // these, and neither belongs in a log.
                ServerMessage::Ignored => {
                    tracing::debug!("ignoring a message the sync client does not model");
                }
                ServerMessage::List(_) | ServerMessage::Set(_) => {}
            }

            let step = self.engine.receive(message, now, epoch_seconds());
            self.execute(step).await;
        }
    }

    /// The server turned this client away. Trying again would be turned away
    /// again, so this is the end of the connection for good.
    fn rejected(&mut self, message: String) {
        tracing::info!(%message, "the sync server refused this client");

        self.link = None;
        self.connecting = None;
        self.retry_at = None;
        self.engine.disconnected();
        self.connection = ConnectionState::Rejected { message };
    }

    async fn lost(&mut self, reason: &str) {
        if self.link.take().is_none() {
            return;
        }

        tracing::info!(%reason, "lost the connection to the sync server");
        self.engine.disconnected();
        self.schedule_retry().await;
    }

    /// Waits `base · 2^min(n, 5)` before attempt `n`, as upstream does —
    /// 0.2, 0.4, 0.8, 1.6, 3.2 and then 3.2 seconds forever with the default
    /// base.
    async fn schedule_retry(&mut self) {
        if matches!(self.connection, ConnectionState::Rejected { .. }) {
            return;
        }

        self.attempt = self.attempt.saturating_add(1);
        let delay =
            self.config.timing.reconnect_base * 2u32.pow(self.attempt.min(MAX_BACKOFF_EXPONENT));
        self.retry_at = Some(Instant::now() + delay);

        if self.ever_greeted {
            self.connection = ConnectionState::Reconnecting {
                attempt: self.attempt,
            };
            self.execute(Step {
                notices: vec![Notice::Reconnecting],
                ..Step::default()
            })
            .await;
        }
    }

    async fn timer_fired(&mut self) {
        let now = Instant::now();

        let silent_since = self.link.as_ref().map(|link| link.last_state);
        if let Some(since) = silent_since {
            if now.saturating_duration_since(since) >= self.config.timing.protocol_timeout {
                self.lost("the server stopped sending its state").await;
            }
        }

        if self.link.is_none()
            && self.connecting.is_none()
            && self.retry_at.is_some_and(|at| now >= at)
        {
            self.retry_at = None;
            self.start_connecting();
        }
    }

    async fn command(&mut self, command: Command) {
        match command {
            Command::Attach(player, events) => {
                let snapshot = player.snapshot();
                self.attached = Some(Attached { player, events });
                let step = self
                    .engine
                    .attach(&snapshot, Instant::now(), epoch_seconds());
                self.execute(step).await;
            }
            Command::Detach => self.detach(),
            Command::SetReady(ready) => {
                let step = self.engine.set_ready(ready);
                self.execute(step).await;
            }
            // Handled by the loop, which has to stop listening to do it.
            Command::Stop => {}
        }
    }

    fn detach(&mut self) {
        self.attached = None;
        self.engine.detach();
    }

    async fn player_event(&mut self, event: Option<PlayerEvent>) {
        match event {
            // A stream that ends without saying why ended with the player.
            None | Some(PlayerEvent::Closed(_)) => {
                tracing::info!(?event, "the player has gone; the connection stays up");
                self.detach();
            }
            Some(PlayerEvent::FileLoaded { file, .. }) => {
                let step = self
                    .engine
                    .file_loaded(&file, Instant::now(), epoch_seconds());
                self.execute(step).await;
            }
            Some(PlayerEvent::Unloaded) => self.engine.file_unloaded(),
            // The poll would find these within a tenth of a second anyway;
            // looking now just tells the room sooner. Thresholds, not the
            // event's origin, decide what the user did.
            Some(PlayerEvent::PauseChanged { .. })
            | Some(PlayerEvent::Seeked { .. })
            | Some(PlayerEvent::EndOfFile) => self.poll().await,
            Some(PlayerEvent::FileFailed { error }) => {
                tracing::info!(%error, "the player could not open a file");
            }
            Some(PlayerEvent::SpeedChanged { .. }) => {}
        }
    }

    async fn poll(&mut self) {
        let Some(attached) = self.attached.as_ref() else {
            return;
        };

        let snapshot = attached.player.snapshot();
        let step = self.engine.poll(&snapshot, Instant::now(), epoch_seconds());
        self.execute(step).await;
    }

    /// Carries out a step: messages first, so a reply is never held up by
    /// the player, then player commands in order, then notices.
    async fn execute(&mut self, step: Step) {
        for message in step.send {
            self.send(&message);
        }

        let Some(player) = self
            .attached
            .as_ref()
            .map(|attached| attached.player.clone())
        else {
            return;
        };

        for command in step.player {
            let outcome = match command {
                PlayerCommand::SetPaused(paused) => {
                    tokio::time::timeout(PLAYER_COMMAND_TIMEOUT, player.set_paused(paused)).await
                }
                PlayerCommand::Seek(position) => {
                    tokio::time::timeout(PLAYER_COMMAND_TIMEOUT, player.seek(position)).await
                }
                PlayerCommand::SetSpeed(speed) => {
                    tokio::time::timeout(PLAYER_COMMAND_TIMEOUT, player.set_speed(speed)).await
                }
            };
            log_player_failure(&format!("{command:?}"), outcome);
        }

        for notice in step.notices {
            let text = notice.render(&self.config.language);
            let outcome = tokio::time::timeout(
                PLAYER_COMMAND_TIMEOUT,
                player.show_text(&text, NOTICE_DURATION),
            )
            .await;
            log_player_failure("show_text", outcome);
        }
    }

    fn send(&self, message: &ClientMessage) {
        let Some(link) = self.link.as_ref() else {
            return;
        };

        match message.to_line() {
            // A writer that has gone has also reported the connection closed.
            Ok(line) => {
                let _ = link.outbound.send(line);
            }
            Err(error) => tracing::warn!(%error, "could not render a message for the server"),
        }
    }

    fn publish(&self) {
        let status = ClientStatus {
            connection: self.connection.clone(),
            username: self.engine.username().map(str::to_owned),
            ready: self.engine.ready(),
            room: self.engine.room().clone(),
            player_attached: self.attached.is_some(),
        };

        self.status.send_if_modified(|current| {
            if *current == status {
                return false;
            }
            *current = status;
            true
        });
    }
}

fn log_player_failure(
    what: &str,
    outcome: Result<crate::core::error::Result<()>, tokio::time::error::Elapsed>,
) {
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::debug!(%error, what, "the player refused a command"),
        Err(_) => tracing::warn!(what, "the player did not answer a command in time"),
    }
}

async fn until_connected(connecting: Option<&mut Connecting>) -> Result<TcpStream, String> {
    match connecting {
        Some(connecting) => connecting.await,
        None => std::future::pending().await,
    }
}

async fn next_inbound(link: Option<&mut Link>) -> Inbound {
    match link {
        Some(link) => link
            .inbound
            .recv()
            .await
            .unwrap_or_else(|| Inbound::Closed("the connection ended".to_owned())),
        None => std::future::pending().await,
    }
}

async fn next_event(attached: Option<&mut Attached>) -> Option<PlayerEvent> {
    match attached {
        Some(attached) => attached.events.recv().await,
        None => std::future::pending().await,
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => std::future::pending().await,
    }
}

/// Splits the stream into lines, accepting a bare `\n` as well as upstream's
/// `\r\n`, and reports how it ended.
async fn read_lines(read: OwnedReadHalf, inbound: mpsc::UnboundedSender<Inbound>) {
    let mut reader = BufReader::new(read);
    let mut line = Vec::new();

    let reason = loop {
        let available = match reader.fill_buf().await {
            Ok(available) => available,
            Err(error) => break error.to_string(),
        };
        if available.is_empty() {
            break "the server closed the connection".to_owned();
        }

        let (consumed, complete) = match available.iter().position(|byte| *byte == b'\n') {
            Some(end) => {
                line.extend_from_slice(&available[..end]);
                (end + 1, true)
            }
            None => {
                line.extend_from_slice(available);
                (available.len(), false)
            }
        };
        reader.consume(consumed);

        if !complete {
            if line.len() > MAX_LINE_LENGTH {
                break "the server sent a line too long to be a message".to_owned();
            }
            continue;
        }

        let text = String::from_utf8_lossy(&line);
        let text = text.strip_suffix('\r').unwrap_or(&text).to_owned();
        line.clear();
        if inbound.send(Inbound::Line(text)).is_err() {
            return;
        }
    };

    let _ = inbound.send(Inbound::Closed(reason));
}

async fn write_lines(
    mut write: OwnedWriteHalf,
    mut outbound: mpsc::UnboundedReceiver<String>,
    inbound: mpsc::UnboundedSender<Inbound>,
    timeout: Duration,
) {
    while let Some(line) = outbound.recv().await {
        let reason = match tokio::time::timeout(timeout, write.write_all(line.as_bytes())).await {
            Ok(Ok(())) => continue,
            Ok(Err(error)) => error.to_string(),
            Err(_) => "the server stopped reading".to_owned(),
        };

        let _ = inbound.send(Inbound::Closed(reason));
        return;
    }
}
