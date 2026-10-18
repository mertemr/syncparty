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

use serde_json::{json, Value};
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

/// How the connection task tells a hung mpv from a quiet one.
///
/// A paused mpv says nothing at all, and a hung one says nothing either —
/// while the mirror, last told "playing", keeps estimating a film that is
/// frozen on screen. So mpv is asked something cheap every so often, and an
/// mpv that has said nothing through `hung_after` worth of asking is taken
/// for hung and killed. The party then hears `Closed(Crashed)`, the contract's
/// word for a player that is no use any more, and can offer to reopen it.
///
/// Counted in unanswered probes rather than read off the clock, so a laptop
/// waking from sleep — whose clock may have run on while mpv could not
/// answer — is not mistaken for a minute of silence: the first probe after
/// waking is answered, and the count starts again.
///
/// The estimate is not frozen while probes go unanswered. Upstream Syncplay
/// keeps extrapolating an mpv that stops answering until it gives up on it
/// at the same threshold, and a position frozen by the driver would be sent
/// to the room as a player falling behind, rewinding everyone to one that is
/// merely stuck.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Liveness {
    pub(super) probe_every: Duration,
    pub(super) hung_after: Duration,
}

impl Liveness {
    /// How many probes in a row may go by with nothing heard.
    fn patience(&self) -> u32 {
        let probes = self.hung_after.as_secs_f64() / self.probe_every.as_secs_f64().max(1e-3);
        // `as` saturates, so an absurd `hung_after` still gives a count.
        (probes.ceil() as u32).max(1)
    }
}

impl Default for Liveness {
    /// Upstream's `MPV_UNRESPONSIVE_THRESHOLD` is 60 s; asking every 5 s
    /// with that as the limit is what the driver spec settled on.
    fn default() -> Self {
        Self {
            probe_every: Duration::from_secs(5),
            hung_after: Duration::from_secs(60),
        }
    }
}

/// What a probe asks: something every mpv has and answers at once.
pub(super) const PROBE: [&str; 2] = ["get_property", "pid"];

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

    /// Registers `command` and queues it for the writer. The reply arrives
    /// on the receiver; `None` once mpv is gone or the writer has stopped.
    pub(super) fn send(
        &self,
        writer: &mpsc::UnboundedSender<Vec<u8>>,
        command: &Value,
        effect: Effect,
    ) -> Option<(i64, oneshot::Receiver<Reply>)> {
        let (id, reply) = self.register(effect)?;

        // serde_json never writes a raw newline, which is what mpv frames on.
        let mut line = json!({ "command": command, "request_id": id }).to_string();
        line.push('\n');

        if writer.send(line.into_bytes()).is_err() {
            self.abandon(id);
            return None;
        }
        Some((id, reply))
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

/// Everything the connection task is handed besides the socket and the
/// process.
pub(super) struct Wiring {
    pub(super) shared: Arc<Shared>,
    pub(super) endpoint: Endpoint,
    /// For the probes. The task holding a sender keeps the writer alive as
    /// long as the task runs, which is no longer than mpv does.
    pub(super) writer: mpsc::UnboundedSender<Vec<u8>>,
    pub(super) kill: Arc<Notify>,
    pub(super) exited: watch::Sender<bool>,
    pub(super) liveness: Liveness,
}

/// The connection task. Ends once the socket is closed and the process has
/// exited, after removing the endpoint and sending `Closed`.
pub(super) async fn run<R, P>(mut lines: LineReader<R>, mut process: P, wiring: Wiring)
where
    R: AsyncRead + Unpin + Send,
    P: Process,
{
    let Wiring {
        shared,
        endpoint,
        writer,
        kill,
        exited,
        liveness,
    } = wiring;

    let mut reading = true;
    let mut exit: Option<Option<i32>> = None;
    let mut give_up_at: Option<Instant> = None;

    let mut probe_at = Some(Instant::now() + liveness.probe_every);
    let mut probe: Option<i64> = None;
    let mut heard = true;
    let mut unanswered = 0_u32;

    while reading || exit.is_none() {
        let tick_at = shared.with_tracker(|tracker| tracker.next_deadline());

        tokio::select! {
            line = lines.next_line(), if reading => match line {
                Ok(Some(line)) => {
                    heard = true;
                    shared.handle_line(&line, Instant::now());
                }
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
            () = sleep_until(probe_at), if probe_at.is_some() && reading && exit.is_none() => {
                unanswered = if std::mem::take(&mut heard) { 0 } else { unanswered + 1 };
                if unanswered >= liveness.patience() {
                    tracing::warn!(
                        "mpv has not answered for {} s; killing it",
                        liveness.hung_after.as_secs()
                    );
                    probe_at = None;
                    process.kill();
                } else {
                    // Only the newest probe is worth an answer; an older one
                    // still waiting would sit in the map until mpv went.
                    if let Some(previous) = probe.take() {
                        shared.abandon(previous);
                    }
                    probe = shared
                        .send(&writer, &json!(PROBE), Effect::None)
                        .map(|(id, _reply)| id);
                    probe_at = Some(Instant::now() + liveness.probe_every);
                }
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
