//! The task that owns an mpv: reads what it says, keeps time for the
//! tracker, and notices when the process is gone.
//!
//! One task for all three, so that what mpv says, the deadlines running out
//! and the process exiting are dealt with in a single order, and `Closed` is
//! always the last thing a player's events carry.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::AsyncRead;
use tokio::sync::{mpsc, oneshot, watch, Notify};

use super::endpoint::Endpoint;
use super::tracker::{Effect, Tracker};
use super::transport::{LineReader, Process};
use crate::core::player::{CloseReason, PlayerEvent, PlayerSnapshot};

/// After mpv's socket closes, how long the process gets to exit by itself
/// before it is killed. A socket closed under a running mpv is a player
/// syncparty can no longer reach, and leaving it running would leave a film
/// on screen that nothing keeps in sync.
const EXIT_GRACE: Duration = Duration::from_secs(3);

/// After mpv exits, how long the socket gets to deliver what mpv wrote
/// before it went — the `end-file` that says the user quit, usually.
const DRAIN_GRACE: Duration = Duration::from_secs(1);

/// Why a command came to nothing.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum CommandError {
    /// mpv answered with an error, e.g. "property unavailable".
    Refused(String),
    Timeout,
    Disconnected,
}

pub(super) type Reply = std::result::Result<Value, CommandError>;

struct Waiting {
    effect: Effect,
    reply: oneshot::Sender<Reply>,
}

#[derive(Default)]
struct Requests {
    /// mpv echoes `request_id` back and reserves 0 for "no id", so ids start
    /// at 1.
    next_id: i64,
    waiting: HashMap<i64, Waiting>,
    /// Set once mpv is gone, under the same lock as `waiting`, so a request
    /// either lands in the map before it is emptied or is refused outright.
    closed: bool,
}

/// What the player handle and the connection task both reach.
pub(super) struct Shared {
    tracker: Mutex<Tracker>,
    requests: Mutex<Requests>,
    /// Taken when `Closed` is sent, which ends the stream: nothing can
    /// follow it, and a consumer reading until the end is not left waiting.
    events: Mutex<Option<mpsc::UnboundedSender<PlayerEvent>>>,
    pub(super) quitting: AtomicBool,
}

impl Shared {
    pub(super) fn new(events: mpsc::UnboundedSender<PlayerEvent>) -> Self {
        Self {
            tracker: Mutex::new(Tracker::new()),
            requests: Mutex::new(Requests {
                next_id: 1,
                ..Requests::default()
            }),
            events: Mutex::new(Some(events)),
            quitting: AtomicBool::new(false),
        }
    }

    pub(super) fn snapshot(&self) -> PlayerSnapshot {
        lock(&self.tracker).snapshot().clone()
    }

    /// Runs `change` on the tracker, then sends what it produced.
    ///
    /// Events go out under the lock, so two threads touching the tracker
    /// cannot reorder them. Loads are settled after the events, so a `load`
    /// call returns only once its `FileLoaded` is already on its way.
    pub(super) fn with_tracker<T>(&self, change: impl FnOnce(&mut Tracker) -> T) -> T {
        let mut tracker = lock(&self.tracker);
        let result = change(&mut tracker);
        let (events, settled) = tracker.take_output();
        if !events.is_empty() {
            if let Some(sender) = lock(&self.events).as_ref() {
                for event in events {
                    // Nobody listening is fine: the snapshot still works.
                    let _ = sender.send(event);
                }
            }
        }
        drop(tracker);

        for (waiter, outcome) in settled {
            let _ = waiter.send(outcome);
        }
        result
    }

    /// Registers a request and returns its id and where its reply will
    /// arrive, or `None` once mpv is gone.
    pub(super) fn register(&self, effect: Effect) -> Option<(i64, oneshot::Receiver<Reply>)> {
        let mut requests = lock(&self.requests);
        if requests.closed {
            return None;
        }
        let id = requests.next_id;
        requests.next_id += 1;
        let (reply, receiver) = oneshot::channel();
        requests.waiting.insert(id, Waiting { effect, reply });
        Some((id, receiver))
    }

    /// Forgets a request whose caller stopped waiting.
    pub(super) fn abandon(&self, id: i64) {
        lock(&self.requests).waiting.remove(&id);
    }

    /// One line from mpv.
    pub(super) fn handle_line(&self, line: &[u8], now: Instant) {
        // mpv passes on bytes that are not UTF-8 — a file name, typically —
        // and JSON cannot hold them. Replacing them keeps the rest of the
        // line, and the mirror compares paths in this lossy form throughout.
        let text = String::from_utf8_lossy(line);
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let message: Value = match serde_json::from_str(text) {
            Ok(message) => message,
            Err(error) => {
                tracing::debug!(%error, "ignored a line from mpv that is not JSON");
                return;
            }
        };

        if let Some(event) = message.get("event").and_then(Value::as_str) {
            if event == "log-message" {
                log_from_mpv(&message);
            }
            self.with_tracker(|tracker| tracker.on_event(&message, now));
            return;
        }

        let Some(error) = message.get("error").and_then(Value::as_str) else {
            return;
        };
        let id = message
            .get("request_id")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if id == 0 {
            // mpv could not read a line well enough to find its id.
            tracing::warn!(error, "mpv rejected a command line");
            return;
        }

        let waiting = lock(&self.requests).waiting.remove(&id);
        let Some(waiting) = waiting else {
            tracing::debug!(
                id,
                error,
                "mpv answered a request nobody waits for any more"
            );
            return;
        };

        let reply = if error == "success" {
            Ok(message.get("data").cloned().unwrap_or(Value::Null))
        } else {
            Err(CommandError::Refused(error.to_owned()))
        };
        self.with_tracker(|tracker| tracker.on_reply(waiting.effect, reply.as_ref().ok(), now));
        let _ = waiting.reply.send(reply);
    }

    /// mpv is gone: refuses further requests, fails the outstanding ones,
    /// and works out why it went.
    fn close(&self, code: Option<i32>) -> CloseReason {
        let abandoned = {
            let mut requests = lock(&self.requests);
            requests.closed = true;
            std::mem::take(&mut requests.waiting)
        };
        // Dropping the senders is what tells each caller it is disconnected.
        drop(abandoned);

        let quitting = self.quitting.load(Ordering::SeqCst);
        self.with_tracker(|tracker| {
            let reason = tracker.close_reason(quitting, code);
            tracker.on_closed();
            reason
        })
    }

    /// Sends the last event there will be, and ends the stream with it.
    fn finish(&self, reason: CloseReason) {
        if let Some(sender) = lock(&self.events).take() {
            let _ = sender.send(PlayerEvent::Closed(reason));
        }
    }
}

/// The connection task. Ends once the socket is closed and the process has
/// exited, after removing the endpoint and sending `Closed`.
pub(super) async fn run<R, P>(
    mut lines: LineReader<R>,
    mut process: P,
    shared: Arc<Shared>,
    endpoint: Endpoint,
    kill: Arc<Notify>,
    exited: watch::Sender<bool>,
) where
    R: AsyncRead + Unpin + Send,
    P: Process,
{
    let mut reading = true;
    let mut exit: Option<Option<i32>> = None;
    let mut give_up_at: Option<Instant> = None;

    while reading || exit.is_none() {
        let tick_at = shared.with_tracker(|tracker| tracker.next_deadline());

        tokio::select! {
            line = lines.next_line(), if reading => match line {
                Ok(Some(line)) => shared.handle_line(&line, Instant::now()),
                Ok(None) | Err(_) => {
                    reading = false;
                    if exit.is_none() {
                        give_up_at = Some(Instant::now() + EXIT_GRACE);
                    }
                }
            },
            code = process.wait(), if exit.is_none() => {
                exit = Some(code);
                if reading {
                    give_up_at = Some(Instant::now() + DRAIN_GRACE);
                }
            },
            () = kill.notified(), if exit.is_none() => process.kill(),
            () = sleep_until(tick_at), if tick_at.is_some() => {
                shared.with_tracker(|tracker| tracker.on_tick(Instant::now()));
            },
            () = sleep_until(give_up_at), if give_up_at.is_some() => {
                give_up_at = None;
                if exit.is_none() {
                    tracing::warn!("mpv closed its control socket but kept running; killing it");
                    process.kill();
                } else {
                    reading = false;
                }
            },
        }
    }

    let reason = shared.close(exit.flatten());
    endpoint.remove();
    shared.finish(reason);
    let _ = exited.send(true);
}

/// Sleeps until `deadline`; forever when there is none. The branch using it
/// is disabled then, but `select!` still builds the future.
async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => std::future::pending().await,
    }
}

/// mpv's own error messages go to syncparty's log, which is where someone
/// looking into "the film would not open" will look.
fn log_from_mpv(message: &Value) {
    let field = |name: &str| message.get(name).and_then(Value::as_str).unwrap_or("");
    tracing::warn!(
        level = field("level"),
        prefix = field("prefix"),
        "mpv: {}",
        field("text").trim_end()
    );
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic while holding the lock leaves plain data behind, which is
    // still the best picture there is of what mpv is doing.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
