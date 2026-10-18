//! What mpv is doing, worked out from what it says.
//!
//! mpv pushes property changes and events, and nothing in them says who
//! caused a change: a pause from the space bar and one syncparty asked for
//! arrive as the same line. So this keeps two things side by side — the
//! mirror the sync client reads, and a list of what syncparty asked for and
//! by when it should show up — and matches every change against the list.
//!
//! It does no I/O and is handed the time, so every ordering mpv has been
//! seen to produce can be replayed in a test without a socket or a clock.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::oneshot;

use crate::core::player::{CloseReason, LoadedFile, Origin, PlayerEvent, PlayerSnapshot};

/// The properties observed, in the order of their observation ids, which
/// start at 1.
pub(super) const OBSERVED: [&str; 11] = [
    "pause",
    "time-pos",
    "duration",
    "path",
    "filename",
    "file-size",
    "speed",
    "seeking",
    "eof-reached",
    "idle-active",
    "paused-for-cache",
];

/// How long a pause or speed change syncparty asked for has to show up.
/// mpv reports it within milliseconds; the margin is for a busy machine.
/// When it does not show up at all — mpv silently ignores an unpause at the
/// end of a file — the mirror goes back to what mpv last said.
const PAUSE_PATIENCE: Duration = Duration::from_secs(1);
const SPEED_PATIENCE: Duration = Duration::from_secs(1);

/// How long a seek has to land. Seeking a large file over a slow disk takes
/// a while, and mpv never reports a seek that failed.
const SEEK_PATIENCE: Duration = Duration::from_secs(3);

/// An unexplained pause is held back this long in case mpv is about to say
/// the file has ended. When a seek runs past the end mpv sends the pause
/// first, with the end of the file in the same millisecond.
const EOF_PAUSE_HOLD: Duration = Duration::from_millis(250);

/// After mpv says the file has ended, how long its own pause there is waited
/// for. It came 108–140 ms later on mpv 0.41.
const EOF_PAUSE_WAIT: Duration = Duration::from_millis(400);

/// A seek landed where it was aimed when mpv reports a position this close.
/// It reports the exact target, so the tolerance only has to absorb a seek
/// clamped to the last frame.
const SEEK_TOLERANCE: f64 = 0.5;

const SPEED_TOLERANCE: f64 = 1e-3;

/// How a load ended, for the `load` call waiting on it.
pub(super) type LoadOutcome = std::result::Result<(), LoadFailure>;

/// Why a load did not end with the file playing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LoadFailure {
    /// mpv could not open it, or it was closed or replaced first.
    Failed(String),
    /// mpv opened it as a playlist and has gone on to what that lists,
    /// which is not the film that was asked for. The caller stops it there.
    Playlist,
}

/// What a playlist of ours is reported as when it fails to be a film.
pub(super) const NOT_A_FILM: &str = "it is a playlist, not a film";

/// What a command does to the mirror once mpv has accepted it.
///
/// Applied by the connection task as the reply is read, so it lands in the
/// order mpv said things: after anything mpv reported before accepting the
/// command, before anything it reported after.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Effect {
    None,
    Pause(bool),
    Speed(f64),
    Seek(u64),
    Load(u64),
}

struct Expected<T> {
    value: T,
    deadline: Instant,
    /// mpv has accepted the command, so the value has been set — even if this
    /// client never hears it, because something set it again first.
    accepted: bool,
}

struct ExpectedSeek {
    id: u64,
    /// Clamped into the file, which is where mpv will report landing.
    target: f64,
    /// mpv has accepted the command. Positions reported before that are
    /// real; positions reported after it, until the seek lands, are frames
    /// left over from before it.
    accepted: bool,
    deadline: Instant,
}

/// What `loadfile` said about the playlist entry it created.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Entry {
    /// No reply yet.
    Unanswered,
    /// mpv before 0.33 or so accepts `loadfile` without naming the entry.
    Unnamed,
    Named(i64),
}

struct PendingLoad {
    generation: u64,
    entry: Entry,
    waiter: oneshot::Sender<LoadOutcome>,
}

/// Where the current file is between `start-file` and playing.
///
/// The `Option<u64>` is the generation of syncparty's load the file
/// belongs to, `None` when someone else opened it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Idle,
    Starting(Option<u64>),
    Opened(Option<u64>),
    Playing,
    /// Something a playlist of ours listed, which mpv went on to by itself
    /// and is being stopped. Never announced: as far as the party is
    /// concerned, nothing was opened.
    Refused,
}

/// The end of the file, which mpv announces in two lines whose order varies.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Eof {
    Clear,
    /// mpv paused without being asked and without saying the file ended.
    /// Held until the deadline in case it is about to.
    HeldPause(Instant),
    /// mpv said the file ended while playing; its pause there is due.
    AwaitingPause(Instant),
    Reported,
}

/// The last thing mpv said about each property, whatever the mirror shows.
struct Reported {
    paused: bool,
    speed: f64,
    time_pos: Option<f64>,
    time_pos_at: Option<Instant>,
    duration: Option<f64>,
    path: Option<String>,
    filename: Option<String>,
    size: Option<u64>,
}

pub(super) struct Tracker {
    snapshot: PlayerSnapshot,
    reported: Reported,
    /// One bit per observed property, set once its first value arrived. The
    /// first value is mpv's answer to "observe": the state it is in, not a
    /// change anybody made.
    seeded: u32,
    phase: Phase,
    load: Option<PendingLoad>,
    pauses: Vec<Expected<bool>>,
    speeds: Vec<Expected<f64>>,
    seeks: Vec<ExpectedSeek>,
    next_id: u64,
    eof: Eof,
    /// A `FileLoaded` went out and no `Unloaded` since. `Unloaded` waits for
    /// mpv to go idle, which is what tells a file closed apart from a file
    /// replaced by another — including one replaced by a file that then
    /// failed to open, which leaves mpv idle with nothing open all the same.
    announced: bool,
    /// A playlist of ours was refused, so whatever mpv starts next without
    /// being asked comes from that playlist. Lasts until mpv is idle.
    refusing: bool,
    last_end_reason: Option<String>,
    events: Vec<PlayerEvent>,
    settled: Vec<(oneshot::Sender<LoadOutcome>, LoadOutcome)>,
}

pub(super) type Settled = Vec<(oneshot::Sender<LoadOutcome>, LoadOutcome)>;

impl Tracker {
    pub(super) fn new() -> Self {
        let snapshot = PlayerSnapshot::default();
        Self {
            reported: Reported {
                paused: snapshot.paused,
                speed: snapshot.speed,
                time_pos: None,
                time_pos_at: None,
                duration: None,
                path: None,
                filename: None,
                size: None,
            },
            snapshot,
            seeded: 0,
            phase: Phase::Idle,
            load: None,
            pauses: Vec::new(),
            speeds: Vec::new(),
            seeks: Vec::new(),
            next_id: 1,
            eof: Eof::Clear,
            announced: false,
            refusing: false,
            last_end_reason: None,
            events: Vec::new(),
            settled: Vec::new(),
        }
    }

    pub(super) fn snapshot(&self) -> &PlayerSnapshot {
        &self.snapshot
    }

    /// The events to emit and the loads to settle since the last call, in
    /// that order.
    pub(super) fn take_output(&mut self) -> (Vec<PlayerEvent>, Settled) {
        (
            std::mem::take(&mut self.events),
            std::mem::take(&mut self.settled),
        )
    }

    // What syncparty is about to ask for. Each is noted before the command
    // is written: the reader runs alongside, and mpv can answer before the
    // writer has even returned.

    /// Notes that a pause change will be syncparty's. `false` when it would
    /// change nothing — mpv sends no event for a value it already has, so an
    /// expectation would never be met.
    pub(super) fn expect_pause(&mut self, paused: bool, now: Instant) -> bool {
        if self.snapshot.paused == paused {
            return false;
        }
        self.pauses.push(Expected {
            value: paused,
            deadline: now + PAUSE_PATIENCE,
            accepted: false,
        });
        true
    }

    pub(super) fn expect_speed(&mut self, speed: f64, now: Instant) -> bool {
        if (self.snapshot.speed - speed).abs() < SPEED_TOLERANCE {
            return false;
        }
        self.speeds.push(Expected {
            value: speed,
            deadline: now + SPEED_PATIENCE,
            accepted: false,
        });
        true
    }

    /// Notes a seek, or `None` with nothing loaded: mpv refuses a seek before
    /// the file has loaded and the seek is simply lost.
    pub(super) fn expect_seek(&mut self, target: f64, now: Instant) -> Option<u64> {
        if !self.snapshot.is_loaded() {
            return None;
        }
        let target = match self.snapshot.duration {
            Some(duration) if duration > 0.0 => target.min(duration),
            _ => target,
        }
        .max(0.0);

        let id = self.fresh_id();
        self.seeks.push(ExpectedSeek {
            id,
            target,
            accepted: false,
            deadline: now + SEEK_PATIENCE,
        });
        Some(id)
    }

    /// Notes a load, and who to tell how it went. A load still in flight is
    /// superseded: only the newest file is the one syncparty wants open.
    pub(super) fn expect_load(&mut self, waiter: oneshot::Sender<LoadOutcome>) -> u64 {
        let generation = self.fresh_id();
        let previous = self.load.replace(PendingLoad {
            generation,
            entry: Entry::Unanswered,
            waiter,
        });
        if let Some(previous) = previous {
            self.settled.push((
                previous.waiter,
                Err(LoadFailure::Failed(
                    "another file was opened before this one started playing".to_owned(),
                )),
            ));
        }
        generation
    }

    /// Stops waiting for a load the caller gave up on, so that a file
    /// starting long after is not credited to it.
    pub(super) fn forget_load(&mut self, generation: u64) {
        if self
            .load
            .as_ref()
            .is_some_and(|load| load.generation == generation)
        {
            self.load = None;
        }
    }

    /// mpv answered a command: `data` is the reply's data when it accepted
    /// it, `None` when it refused.
    ///
    /// Acceptance moves the mirror to the commanded value straight away. The
    /// property change follows within milliseconds, but the sync client
    /// looks at the mirror the moment the command returns, and a correction
    /// it just made must not look like the user undoing it.
    pub(super) fn on_reply(&mut self, effect: Effect, data: Option<&Value>, now: Instant) {
        let accepted = data.is_some();

        match effect {
            Effect::None => {}
            Effect::Pause(paused) => {
                if accepted {
                    // Replies come in order, so this answers the oldest
                    // command for this value still waiting on one.
                    if let Some(expected) = self
                        .pauses
                        .iter_mut()
                        .find(|expected| expected.value == paused && !expected.accepted)
                    {
                        expected.accepted = true;
                    }
                    self.mirror(now, |snapshot| snapshot.paused = paused);
                } else {
                    withdraw(&mut self.pauses, |value| *value == paused);
                    if self.pauses.is_empty() {
                        let reported = self.reported.paused;
                        self.mirror(now, |snapshot| snapshot.paused = reported);
                    }
                }
            }
            Effect::Speed(speed) => {
                if accepted {
                    self.mirror_speed(speed, now);
                } else {
                    withdraw(&mut self.speeds, |value| {
                        (*value - speed).abs() < SPEED_TOLERANCE
                    });
                    if self.speeds.is_empty() {
                        self.mirror_speed(self.reported.speed, now);
                    }
                }
            }
            Effect::Seek(id) => {
                if !accepted {
                    self.seeks.retain(|seek| seek.id != id);
                } else if let Some(seek) = self.seeks.iter_mut().find(|seek| seek.id == id) {
                    seek.accepted = true;
                    self.snapshot.time_pos = Some(seek.target);
                    self.snapshot.time_pos_at = Some(now);
                }
            }
            Effect::Load(generation) => {
                let Some(load) = self
                    .load
                    .as_mut()
                    .filter(|load| load.generation == generation)
                else {
                    return;
                };
                match data {
                    Some(data) => {
                        load.entry = data
                            .get("playlist_entry_id")
                            .and_then(Value::as_i64)
                            .map_or(Entry::Unnamed, Entry::Named);
                    }
                    // The caller has the refusal from the reply itself.
                    None => self.load = None,
                }
            }
        }
    }

    /// One event line from mpv.
    pub(super) fn on_event(&mut self, message: &Value, now: Instant) {
        let Some(event) = message.get("event").and_then(Value::as_str) else {
            return;
        };

        match event {
            "property-change" => {
                if let Some(name) = message.get("name").and_then(Value::as_str) {
                    // An unavailable property comes without `data`.
                    let data = message.get("data").filter(|data| !data.is_null());
                    self.on_property(name, data, now);
                }
            }
            "start-file" => {
                let entry = message.get("playlist_entry_id").and_then(Value::as_i64);
                self.on_start_file(entry, now);
            }
            "file-loaded" => self.on_file_loaded(),
            "playback-restart" => self.on_playback_restart(now),
            "end-file" => {
                let reason = message
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let error = message.get("file_error").and_then(Value::as_str);
                self.on_end_file(reason, error, now);
            }
            // Deprecated, but still sent; `idle-active` says the same.
            "idle" => self.on_idle(),
            _ => {}
        }
    }

    /// Lets expectations and held events run out. Called at
    /// [`Tracker::next_deadline`].
    pub(super) fn on_tick(&mut self, now: Instant) {
        // A change of ours mpv accepted and never reported, with mpv still
        // saying the opposite: within the time mpv takes to tell this client
        // anything, somebody set it back — the room resumed and the user
        // paused in the same instant — and mpv, which reports only the latest
        // value, had nothing new to say. Ours did happen and theirs is a
        // change too. At the end of the file mpv accepts an unpause and does
        // nothing, which is the one time silence means just that.
        let reported = self.reported.paused;
        let overtaken = self.pauses.iter().rposition(|expected| {
            expected.deadline <= now && expected.accepted && expected.value != reported
        });
        let overtaken = overtaken.filter(|_| !self.snapshot.eof_reached);
        if let Some(index) = overtaken {
            let ours = self.pauses[index].value;
            self.pauses.drain(..=index);
            self.events.push(PlayerEvent::PauseChanged {
                paused: ours,
                origin: Origin::Ours,
            });
            self.external_pause(reported, now);
        }

        let expired = expire(&mut self.pauses, now);
        if (expired || overtaken.is_some()) && self.pauses.is_empty() {
            self.mirror(now, |snapshot| snapshot.paused = reported);
        }
        if expire(&mut self.speeds, now) && self.speeds.is_empty() {
            self.mirror_speed(self.reported.speed, now);
        }

        let was_landing = self.seeks.iter().any(|seek| seek.accepted);
        self.seeks.retain(|seek| seek.deadline > now);
        if was_landing && !self.seeks.iter().any(|seek| seek.accepted) {
            self.snapshot.time_pos = self.reported.time_pos;
            self.snapshot.time_pos_at = self.reported.time_pos_at;
        }

        match self.eof {
            Eof::HeldPause(deadline) if deadline <= now => self.release_held_pause(),
            Eof::AwaitingPause(deadline) if deadline <= now => self.end_of_file(),
            _ => {}
        }
    }

    /// When [`Tracker::on_tick`] next has something to do.
    pub(super) fn next_deadline(&self) -> Option<Instant> {
        let eof = match self.eof {
            Eof::HeldPause(deadline) | Eof::AwaitingPause(deadline) => Some(deadline),
            Eof::Clear | Eof::Reported => None,
        };

        self.pauses
            .iter()
            .map(|expected| expected.deadline)
            .chain(self.speeds.iter().map(|expected| expected.deadline))
            .chain(self.seeks.iter().map(|seek| seek.deadline))
            .chain(eof)
            .min()
    }

    /// Why mpv went away, once it has.
    ///
    /// Closing the window and pressing `q` both run mpv's `quit`, which ends
    /// the file with reason "quit" and exits with 0. Anything else that ends
    /// the process is a crash as far as the party is concerned.
    pub(super) fn close_reason(&self, quitting: bool, code: Option<i32>) -> CloseReason {
        if quitting {
            CloseReason::QuitRequested
        } else if self.last_end_reason.as_deref() == Some("quit") || code == Some(0) {
            CloseReason::UserQuit
        } else {
            CloseReason::Crashed(code)
        }
    }

    pub(super) fn on_closed(&mut self) {
        if let Some(load) = self.load.take() {
            self.settled.push((
                load.waiter,
                Err(LoadFailure::Failed(
                    "mpv closed before the file started playing".to_owned(),
                )),
            ));
        }
        self.pauses.clear();
        self.speeds.clear();
        self.seeks.clear();
        // A pause still held goes unreported: the player it happened in is
        // gone, and `Closed` is the last word on it.
        self.eof = Eof::Clear;
    }

    fn on_property(&mut self, name: &str, data: Option<&Value>, now: Instant) {
        let Some(index) = OBSERVED.iter().position(|observed| *observed == name) else {
            return;
        };
        let seed = self.seeded & (1 << index) == 0;
        self.seeded |= 1 << index;

        let flag = data.and_then(Value::as_bool).unwrap_or(false);
        let number = data.and_then(Value::as_f64);
        let text = data.and_then(Value::as_str).map(str::to_owned);

        match name {
            "pause" => {
                if let Some(paused) = data.and_then(Value::as_bool) {
                    self.on_pause(paused, seed, now);
                }
            }
            "time-pos" => self.on_time_pos(number, now),
            "duration" => {
                self.reported.duration = number;
                self.snapshot.duration = number;
                if let Some(file) = self.snapshot.file.as_mut() {
                    file.duration = number;
                }
            }
            "path" => self.reported.path = text,
            "filename" => self.reported.filename = text,
            "file-size" => self.reported.size = data.and_then(Value::as_u64),
            "speed" => {
                if let Some(speed) = number {
                    self.on_speed(speed, seed, now);
                }
            }
            "seeking" => self.mirror(now, |snapshot| snapshot.seeking = flag),
            "eof-reached" => self.on_eof_reached(flag, seed, now),
            "idle-active" => {
                if flag {
                    self.on_idle();
                }
            }
            "paused-for-cache" => self.mirror(now, |snapshot| snapshot.buffering = flag),
            _ => {}
        }
    }

    fn on_pause(&mut self, paused: bool, seed: bool, now: Instant) {
        let changed = paused != self.reported.paused;
        self.reported.paused = paused;

        if seed {
            self.mirror(now, |snapshot| snapshot.paused = paused);
            return;
        }

        // Playing again, whoever made it so, means the pause being held was
        // not the end of the file. It goes out first, because it came first:
        // left to its deadline it would arrive after this, and the last word
        // would be "paused" about an mpv that is playing.
        if !paused {
            self.release_held_pause();
        }

        if let Some(index) = self
            .pauses
            .iter()
            .position(|expected| expected.value == paused && now <= expected.deadline)
        {
            // Ours, and so is anything asked for before it: mpv reports only
            // the latest value when two changes come close together.
            self.pauses.drain(..=index);
            if self.pauses.is_empty() {
                self.mirror(now, |snapshot| snapshot.paused = paused);
            }
            self.events.push(PlayerEvent::PauseChanged {
                paused,
                origin: Origin::Ours,
            });
            if paused && matches!(self.eof, Eof::AwaitingPause(_)) {
                self.end_of_file();
            }
            return;
        }

        // mpv tells a client only the latest value of what it observes. A
        // value that contradicts a change of ours mpv has already accepted
        // means something set it again before this client heard about ours —
        // the user pausing in the same instant the room resumed, typically.
        // Ours did happen, and what is reported now is somebody else's, even
        // though it matches the last value this client was told.
        let changed = match self
            .pauses
            .iter()
            .rposition(|expected| expected.accepted && expected.value != paused)
        {
            Some(index) => {
                let ours = self.pauses[index].value;
                self.pauses.drain(..=index);
                self.events.push(PlayerEvent::PauseChanged {
                    paused: ours,
                    origin: Origin::Ours,
                });
                true
            }
            None => changed,
        };

        // While something syncparty asked for is still due, the mirror keeps
        // showing it; mpv applied that command after whatever this was.
        if self.pauses.is_empty() {
            self.mirror(now, |snapshot| snapshot.paused = paused);
        }
        if !changed {
            return;
        }

        self.external_pause(paused, now);
    }

    /// A pause change that was not syncparty's. A pause is held a moment
    /// first, in case it is the end of the file arriving in the other order.
    fn external_pause(&mut self, paused: bool, now: Instant) {
        if paused {
            match self.eof {
                Eof::AwaitingPause(_) => self.end_of_file(),
                // Still the pause the end of the file brought.
                Eof::Reported => {}
                _ if self.snapshot.eof_reached => self.end_of_file(),
                _ => self.eof = Eof::HeldPause(now + EOF_PAUSE_HOLD),
            }
        } else {
            self.events.push(PlayerEvent::PauseChanged {
                paused: false,
                origin: Origin::External,
            });
        }
    }

    fn on_speed(&mut self, speed: f64, seed: bool, now: Instant) {
        let changed = (speed - self.reported.speed).abs() >= SPEED_TOLERANCE;
        self.reported.speed = speed;

        if seed {
            self.mirror_speed(speed, now);
            return;
        }

        if let Some(index) = self.speeds.iter().position(|expected| {
            (expected.value - speed).abs() < SPEED_TOLERANCE && now <= expected.deadline
        }) {
            self.speeds.drain(..=index);
            if self.speeds.is_empty() {
                self.mirror_speed(speed, now);
            }
            self.events.push(PlayerEvent::SpeedChanged {
                speed,
                origin: Origin::Ours,
            });
            return;
        }

        if self.speeds.is_empty() {
            self.mirror_speed(speed, now);
        }
        if changed {
            self.events.push(PlayerEvent::SpeedChanged {
                speed,
                origin: Origin::External,
            });
        }
    }

    fn on_time_pos(&mut self, position: Option<f64>, now: Instant) {
        self.reported.time_pos = position;
        self.reported.time_pos_at = Some(now);

        let mut landing = self
            .seeks
            .iter()
            .filter(|seek| seek.accepted)
            .map(|seek| seek.target)
            .peekable();
        if landing.peek().is_some() {
            let near = position.is_some_and(|position| {
                landing.any(|target| (position - target).abs() < SEEK_TOLERANCE)
            });
            if !near {
                // A frame decoded before the seek took effect. Showing it
                // would move the mirror back to where the film was, which
                // looks exactly like the user seeking back.
                return;
            }
        }

        self.snapshot.time_pos = position;
        self.snapshot.time_pos_at = position.map(|_| now);
    }

    fn on_eof_reached(&mut self, reached: bool, seed: bool, now: Instant) {
        self.mirror(now, |snapshot| snapshot.eof_reached = reached);
        if seed {
            return;
        }

        if reached {
            match self.eof {
                Eof::HeldPause(_) => self.end_of_file(),
                // Already paused, so no pause will follow.
                Eof::Clear if self.reported.paused => self.end_of_file(),
                Eof::Clear => self.eof = Eof::AwaitingPause(now + EOF_PAUSE_WAIT),
                Eof::AwaitingPause(_) | Eof::Reported => {}
            }
        } else if !matches!(self.eof, Eof::HeldPause(_)) {
            self.eof = Eof::Clear;
        }
    }

    fn on_start_file(&mut self, entry: Option<i64>, now: Instant) {
        let ours = match &self.load {
            Some(load) => {
                let matches = match (load.entry, entry) {
                    (Entry::Named(ours), Some(theirs)) => ours == theirs,
                    // Older mpv names neither the entry nor the file
                    // starting; what follows our answered load is ours.
                    (Entry::Named(_) | Entry::Unnamed, _) => true,
                    // mpv answers a command before it reports what the
                    // command did, so a file starting before the answer was
                    // opened by someone else.
                    (Entry::Unanswered, _) => false,
                };
                matches.then_some(load.generation)
            }
            None => None,
        };

        // A pause made just before is still a pause, whatever opens next.
        self.release_held_pause();

        self.phase = if ours.is_none() && self.refusing {
            Phase::Refused
        } else {
            self.refusing = false;
            Phase::Starting(ours)
        };
        self.eof = Eof::Clear;
        self.seeks.clear();
        self.mirror(now, |snapshot| snapshot.file = None);
    }

    fn on_file_loaded(&mut self) {
        self.phase = match self.phase {
            Phase::Starting(ours) => Phase::Opened(ours),
            Phase::Idle => Phase::Opened(None),
            other => other,
        };
    }

    /// `playback-restart` ends both a load and a seek. Only at the first one
    /// after `file-loaded` are the duration and the size known.
    fn on_playback_restart(&mut self, now: Instant) {
        match self.phase {
            Phase::Opened(ours) => self.finish_load(ours, now),
            Phase::Playing => {
                let Some(position) = self.reported.time_pos else {
                    return;
                };
                let origin = self.settle_seeks(position);
                self.snapshot.time_pos = Some(position);
                self.snapshot.time_pos_at = Some(now);
                self.events.push(PlayerEvent::Seeked { position, origin });
            }
            Phase::Idle | Phase::Starting(_) | Phase::Refused => {}
        }
    }

    fn finish_load(&mut self, ours: Option<u64>, now: Instant) {
        let path = self.reported.path.clone().unwrap_or_default();
        let name = self
            .reported
            .filename
            .clone()
            .or_else(|| {
                Path::new(&path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_default();
        let file = LoadedFile {
            path,
            name,
            duration: self.reported.duration,
            size: self.reported.size,
        };

        self.phase = Phase::Playing;
        let position = self.reported.time_pos;
        let loaded = file.clone();
        self.mirror(now, |snapshot| {
            snapshot.duration = loaded.duration;
            snapshot.file = Some(loaded);
            snapshot.time_pos = position;
            snapshot.time_pos_at = position.map(|_| now);
        });

        let origin = if ours.is_some() {
            Origin::Ours
        } else {
            Origin::External
        };
        self.events.push(PlayerEvent::FileLoaded { file, origin });
        self.announced = true;
        self.settle_load(ours, Ok(()));
    }

    /// Which seek a `playback-restart` finished.
    ///
    /// mpv merges seeks that come close together and restarts once for all
    /// of them, so restarts and seek commands do not pair up one to one. The
    /// position does: a restart where one of ours was aimed is ours, and
    /// takes every older one with it. One anywhere else was somebody else's,
    /// and mpv has dealt with every seek of ours it had accepted by then.
    fn settle_seeks(&mut self, position: f64) -> Origin {
        let landed = self
            .seeks
            .iter()
            .rposition(|seek| seek.accepted && (position - seek.target).abs() < SEEK_TOLERANCE);

        match landed {
            Some(index) => {
                self.seeks.drain(..=index);
                Origin::Ours
            }
            None => {
                self.seeks.retain(|seek| !seek.accepted);
                Origin::External
            }
        }
    }

    fn on_end_file(&mut self, reason: &str, error: Option<&str>, now: Instant) {
        self.last_end_reason = Some(reason.to_owned());

        // The process is on its way out; `Closed` says the rest.
        if reason == "quit" {
            return;
        }
        self.release_held_pause();

        if reason == "redirect" {
            self.on_redirect();
            return;
        }

        let ours = match self.phase {
            Phase::Starting(ours) | Phase::Opened(ours) => ours,
            Phase::Idle | Phase::Playing | Phase::Refused => None,
        };

        // A refused file failing or being stopped is not news either.
        if self.phase != Phase::Refused {
            if reason == "error" {
                let error = error.unwrap_or("mpv could not play it").to_owned();
                self.events.push(PlayerEvent::FileFailed {
                    error: error.clone(),
                });
                self.settle_load(ours, Err(LoadFailure::Failed(error)));
            } else {
                self.settle_load(
                    ours,
                    Err(LoadFailure::Failed(
                        "the file was closed before it started playing".to_owned(),
                    )),
                );
            }
        }

        self.phase = Phase::Idle;
        self.eof = Eof::Clear;
        self.seeks.clear();
        self.mirror(now, |snapshot| snapshot.file = None);
    }

    /// The file turned out to be a playlist, and mpv is about to start what
    /// it lists.
    ///
    /// For a file of ours that is the end of it: it is not a film, and what
    /// a playlist lists can be anything, URLs included — exactly what `load`
    /// keeps away from mpv. `--access-references=no` stops mpv from getting
    /// this far at all; this is for an mpv or a demuxer that does not honour
    /// it, so that a playlist fails at once — not after the load's timeout,
    /// with what it listed playing meanwhile as somebody else's file.
    ///
    /// A playlist somebody else opened is their business, and plays.
    fn on_redirect(&mut self) {
        let Phase::Starting(Some(generation)) = self.phase else {
            return;
        };

        self.events.push(PlayerEvent::FileFailed {
            error: NOT_A_FILM.to_owned(),
        });
        self.settle_load(Some(generation), Err(LoadFailure::Playlist));
        self.phase = Phase::Idle;
        self.refusing = true;
    }

    fn on_idle(&mut self) {
        self.refusing = false;
        if std::mem::take(&mut self.announced) {
            self.events.push(PlayerEvent::Unloaded);
        }
    }

    /// Reports an unexplained pause held back for the end of the file, now
    /// that something the end of the file cannot explain has happened.
    /// Called before that something is reported, so the events keep mpv's
    /// order.
    fn release_held_pause(&mut self) {
        if matches!(self.eof, Eof::HeldPause(_)) {
            self.eof = Eof::Clear;
            self.events.push(PlayerEvent::PauseChanged {
                paused: true,
                origin: Origin::External,
            });
        }
    }

    fn end_of_file(&mut self) {
        self.eof = Eof::Reported;
        self.events.push(PlayerEvent::EndOfFile);
    }

    fn settle_load(&mut self, generation: Option<u64>, outcome: LoadOutcome) {
        let Some(generation) = generation else {
            return;
        };
        if self
            .load
            .as_ref()
            .is_some_and(|load| load.generation == generation)
        {
            if let Some(load) = self.load.take() {
                self.settled.push((load.waiter, outcome));
            }
        }
    }

    /// Changes the mirror, keeping the position estimate continuous.
    ///
    /// The estimate runs on from the last reported position while the film
    /// plays. When it stops, the estimate is pinned where it got to; when it
    /// starts again, it runs on from now — not from a report made before a
    /// pause, which would count the whole pause as playing time.
    fn mirror(&mut self, now: Instant, change: impl FnOnce(&mut PlayerSnapshot)) {
        let was_still = standing_still(&self.snapshot);
        let estimate = self.snapshot.position(now);
        let before = (self.snapshot.time_pos, self.snapshot.time_pos_at);

        change(&mut self.snapshot);

        let position_untouched = (self.snapshot.time_pos, self.snapshot.time_pos_at) == before;
        if !position_untouched || self.snapshot.time_pos.is_none() {
            return;
        }
        match (was_still, standing_still(&self.snapshot)) {
            (false, true) => {
                self.snapshot.time_pos = estimate;
                self.snapshot.time_pos_at = Some(now);
            }
            (true, false) => self.snapshot.time_pos_at = Some(now),
            _ => {}
        }
    }

    /// A new speed counts from now, not from the last position report.
    fn mirror_speed(&mut self, speed: f64, now: Instant) {
        if !standing_still(&self.snapshot) {
            if let Some(estimate) = self.snapshot.position(now) {
                self.snapshot.time_pos = Some(estimate);
                self.snapshot.time_pos_at = Some(now);
            }
        }
        self.snapshot.speed = speed;
    }

    fn fresh_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

/// The same test [`PlayerSnapshot::position`] makes.
fn standing_still(snapshot: &PlayerSnapshot) -> bool {
    snapshot.paused
        || snapshot.seeking
        || snapshot.eof_reached
        || snapshot.buffering
        || snapshot.file.is_none()
}

/// Drops expectations whose time is up; `true` if any were.
fn expire<T>(expectations: &mut Vec<Expected<T>>, now: Instant) -> bool {
    let before = expectations.len();
    expectations.retain(|expected| expected.deadline > now);
    expectations.len() != before
}

/// Takes back the newest expectation a refused command made.
fn withdraw<T>(expectations: &mut Vec<Expected<T>>, made_by: impl Fn(&T) -> bool) {
    if let Some(index) = expectations
        .iter()
        .rposition(|expected| made_by(&expected.value))
    {
        expectations.remove(index);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const DURATION: f64 = 20.0;

    fn property(tracker: &mut Tracker, name: &str, data: Value, at: Instant) {
        tracker.on_event(
            &json!({ "event": "property-change", "name": name, "data": data }),
            at,
        );
    }

    fn event(tracker: &mut Tracker, message: Value, at: Instant) {
        tracker.on_event(&message, at);
    }

    fn events(tracker: &mut Tracker) -> Vec<PlayerEvent> {
        tracker.take_output().0
    }

    /// A tracker with every property seeded as a freshly started
    /// `mpv --pause --idle` reports them.
    fn seeded(at: Instant) -> Tracker {
        let mut tracker = Tracker::new();
        for name in OBSERVED {
            let data = match name {
                "pause" | "idle-active" => json!(true),
                "speed" => json!(1.0),
                _ => Value::Null,
            };
            property(&mut tracker, name, data, at);
        }
        assert!(events(&mut tracker).is_empty(), "seeds are not changes");
        tracker
    }

    /// A tracker with a file of ours loaded, paused at `position`.
    fn loaded(at: Instant, position: f64) -> Tracker {
        let mut tracker = seeded(at);
        let (waiter, _outcome) = oneshot::channel();
        let generation = tracker.expect_load(waiter);
        tracker.on_reply(
            Effect::Load(generation),
            Some(&json!({ "playlist_entry_id": 1 })),
            at,
        );
        event(
            &mut tracker,
            json!({ "event": "start-file", "playlist_entry_id": 1 }),
            at,
        );
        property(&mut tracker, "path", json!("/films/Film.mkv"), at);
        property(&mut tracker, "filename", json!("Film.mkv"), at);
        property(&mut tracker, "idle-active", json!(false), at);
        event(&mut tracker, json!({ "event": "file-loaded" }), at);
        property(&mut tracker, "duration", json!(DURATION), at);
        property(&mut tracker, "file-size", json!(86_898), at);
        property(&mut tracker, "time-pos", json!(position), at);
        event(&mut tracker, json!({ "event": "playback-restart" }), at);

        let loaded = events(&mut tracker);
        assert!(
            matches!(
                loaded.as_slice(),
                [PlayerEvent::FileLoaded {
                    origin: Origin::Ours,
                    ..
                }]
            ),
            "{loaded:?}"
        );
        tracker
    }

    /// The same, playing.
    fn playing(at: Instant, position: f64) -> Tracker {
        let mut tracker = loaded(at, position);
        property(&mut tracker, "pause", json!(false), at);
        events(&mut tracker);
        tracker
    }

    #[test]
    fn an_unpause_mpv_ignores_gives_way_to_what_mpv_last_said() {
        // At the end of a file mpv accepts an unpause and does nothing.
        let at = Instant::now();
        let mut tracker = loaded(at, 19.96);
        property(&mut tracker, "eof-reached", json!(true), at);
        assert_eq!(events(&mut tracker), [PlayerEvent::EndOfFile]);

        assert!(tracker.expect_pause(false, at));
        tracker.on_reply(Effect::Pause(false), Some(&Value::Null), at);
        assert!(!tracker.snapshot().paused, "accepted, so mirrored at once");

        let later = at + PAUSE_PATIENCE + Duration::from_millis(1);
        assert!(tracker
            .next_deadline()
            .is_some_and(|deadline| deadline <= later));
        tracker.on_tick(later);

        assert!(tracker.snapshot().paused);
        assert!(events(&mut tracker).is_empty());
    }

    /// mpv reports only the latest value. A resume of ours that it accepted,
    /// overtaken by the user pausing before this client heard of the resume,
    /// arrives as "paused" — the value this client last saw — and is still
    /// two changes: ours, then the user's.
    #[test]
    fn a_change_of_ours_overtaken_before_it_was_reported_is_still_two_changes() {
        let at = Instant::now();
        let mut tracker = loaded(at, 10.0);

        assert!(tracker.expect_pause(false, at));
        tracker.on_reply(Effect::Pause(false), Some(&Value::Null), at);
        let overtaken = at + Duration::from_millis(5);
        property(&mut tracker, "pause", json!(true), overtaken);

        assert_eq!(
            events(&mut tracker),
            [PlayerEvent::PauseChanged {
                paused: false,
                origin: Origin::Ours
            }]
        );
        assert!(tracker.snapshot().paused, "the mirror follows mpv at once");

        // The user's pause, held a moment in case it is the end of the file.
        tracker.on_tick(overtaken + EOF_PAUSE_HOLD + Duration::from_millis(1));
        assert_eq!(
            events(&mut tracker),
            [PlayerEvent::PauseChanged {
                paused: true,
                origin: Origin::External
            }]
        );
    }

    /// The same race, closer still: the user's pause lands before mpv has
    /// told this client anything, so the value it last reported never
    /// changed and no event comes at all. An accepted resume running out
    /// against a paused mpv, away from the end of the file, is that race.
    #[test]
    fn a_change_of_ours_overtaken_without_a_word_from_mpv_is_still_two_changes() {
        let at = Instant::now();
        let mut tracker = loaded(at, 10.0);

        assert!(tracker.expect_pause(false, at));
        tracker.on_reply(Effect::Pause(false), Some(&Value::Null), at);
        let later = at + PAUSE_PATIENCE + Duration::from_millis(1);
        tracker.on_tick(later);

        assert_eq!(
            events(&mut tracker),
            [PlayerEvent::PauseChanged {
                paused: false,
                origin: Origin::Ours
            }]
        );
        assert!(tracker.snapshot().paused);

        tracker.on_tick(later + EOF_PAUSE_HOLD + Duration::from_millis(1));
        assert_eq!(
            events(&mut tracker),
            [PlayerEvent::PauseChanged {
                paused: true,
                origin: Origin::External
            }]
        );
    }

    #[test]
    fn resuming_does_not_count_the_time_spent_paused() {
        let at = Instant::now();
        let mut tracker = loaded(at, 10.0);
        let resumed = at + Duration::from_secs(60);

        assert!(tracker.expect_pause(false, resumed));
        tracker.on_reply(Effect::Pause(false), Some(&Value::Null), resumed);

        let position = |later: Duration| {
            tracker
                .snapshot()
                .position(resumed + later)
                .expect("position")
        };
        assert!((position(Duration::ZERO) - 10.0).abs() < 1e-9);
        assert!((position(Duration::from_secs(1)) - 11.0).abs() < 1e-9);
    }

    #[test]
    fn pausing_pins_the_estimate_where_it_had_got_to() {
        let at = Instant::now();
        let mut tracker = playing(at, 10.0);
        let paused_at = at + Duration::from_millis(30);

        property(&mut tracker, "pause", json!(true), paused_at);

        let position = tracker
            .snapshot()
            .position(paused_at + Duration::from_secs(5))
            .expect("position");
        assert!((position - 10.03).abs() < 1e-6, "{position}");
    }

    #[test]
    fn the_end_of_the_file_is_reported_even_if_mpv_never_pauses_there() {
        let at = Instant::now();
        let mut tracker = playing(at, 19.9);

        property(&mut tracker, "eof-reached", json!(true), at);
        assert!(events(&mut tracker).is_empty(), "its pause is due first");

        tracker.on_tick(at + EOF_PAUSE_WAIT);
        assert_eq!(events(&mut tracker), [PlayerEvent::EndOfFile]);
    }

    #[test]
    fn an_unexplained_pause_is_reported_once_the_end_of_the_file_fails_to_follow() {
        let at = Instant::now();
        let mut tracker = playing(at, 5.0);

        property(&mut tracker, "pause", json!(true), at);
        assert!(
            events(&mut tracker).is_empty(),
            "held for the end of the file"
        );
        assert!(tracker.snapshot().paused, "the mirror is not held back");

        tracker.on_tick(at + EOF_PAUSE_HOLD);
        assert_eq!(
            events(&mut tracker),
            [PlayerEvent::PauseChanged {
                paused: true,
                origin: Origin::External
            }]
        );
    }

    #[test]
    fn a_pause_and_unpause_inside_the_hold_are_both_reported_in_order() {
        let at = Instant::now();
        let mut tracker = playing(at, 5.0);

        property(&mut tracker, "pause", json!(true), at);
        property(
            &mut tracker,
            "pause",
            json!(false),
            at + Duration::from_millis(50),
        );

        assert_eq!(
            events(&mut tracker),
            [
                PlayerEvent::PauseChanged {
                    paused: true,
                    origin: Origin::External
                },
                PlayerEvent::PauseChanged {
                    paused: false,
                    origin: Origin::External
                },
            ]
        );
        assert_eq!(tracker.next_deadline(), None);
    }

    #[test]
    fn a_held_pause_is_reported_before_our_resume_that_overtakes_it() {
        // The user pauses, and within the hold the room's state has the
        // client resume the player. mpv's history is pause, then play.
        let at = Instant::now();
        let mut tracker = playing(at, 5.0);

        property(&mut tracker, "pause", json!(true), at);
        assert!(events(&mut tracker).is_empty(), "held");

        let resumed = at + Duration::from_millis(80);
        assert!(tracker.expect_pause(false, resumed));
        tracker.on_reply(Effect::Pause(false), Some(&Value::Null), resumed);
        property(&mut tracker, "pause", json!(false), resumed);
        tracker.on_tick(at + EOF_PAUSE_HOLD + Duration::from_millis(1));

        assert_eq!(
            events(&mut tracker),
            [
                PlayerEvent::PauseChanged {
                    paused: true,
                    origin: Origin::External
                },
                PlayerEvent::PauseChanged {
                    paused: false,
                    origin: Origin::Ours
                },
            ],
            "the last word is mpv's real state"
        );
        assert!(!tracker.snapshot().paused);
        assert_eq!(tracker.next_deadline(), None, "nothing left held");
    }

    #[test]
    fn a_held_pause_is_reported_before_the_next_file_starts() {
        let at = Instant::now();
        let mut tracker = playing(at, 5.0);

        property(&mut tracker, "pause", json!(true), at);
        event(
            &mut tracker,
            json!({ "event": "end-file", "reason": "stop" }),
            at,
        );
        event(
            &mut tracker,
            json!({ "event": "start-file", "playlist_entry_id": 2 }),
            at,
        );

        assert_eq!(
            events(&mut tracker),
            [PlayerEvent::PauseChanged {
                paused: true,
                origin: Origin::External
            }]
        );
        assert_eq!(tracker.next_deadline(), None);
    }

    #[test]
    fn a_playlist_of_ours_fails_at_once_and_what_it_lists_is_never_announced() {
        let at = Instant::now();
        let mut tracker = seeded(at);
        let (waiter, mut outcome) = oneshot::channel();
        let generation = tracker.expect_load(waiter);
        tracker.on_reply(
            Effect::Load(generation),
            Some(&json!({ "playlist_entry_id": 1 })),
            at,
        );

        // What mpv 0.41 sends for a playlist when references are followed.
        event(
            &mut tracker,
            json!({ "event": "start-file", "playlist_entry_id": 1 }),
            at,
        );
        event(
            &mut tracker,
            json!({
                "event": "end-file",
                "reason": "redirect",
                "playlist_entry_id": 1,
                "playlist_insert_id": 2,
                "playlist_insert_num_entries": 2
            }),
            at,
        );
        let (emitted, settled) = tracker.take_output();
        assert_eq!(
            emitted,
            [PlayerEvent::FileFailed {
                error: NOT_A_FILM.to_owned()
            }]
        );
        for (waiter, result) in settled {
            let _ = waiter.send(result);
        }
        assert_eq!(outcome.try_recv(), Ok(Err(LoadFailure::Playlist)));

        // mpv goes on to the first entry, which fails, and then the next,
        // which opens before the stop syncparty sends lands.
        event(
            &mut tracker,
            json!({ "event": "start-file", "playlist_entry_id": 2 }),
            at,
        );
        property(&mut tracker, "path", json!("http://example.com/x"), at);
        event(
            &mut tracker,
            json!({ "event": "end-file", "reason": "error", "file_error": "loading failed" }),
            at,
        );
        event(
            &mut tracker,
            json!({ "event": "start-file", "playlist_entry_id": 3 }),
            at,
        );
        property(&mut tracker, "path", json!("/films/Listed.mkv"), at);
        event(&mut tracker, json!({ "event": "file-loaded" }), at);
        event(&mut tracker, json!({ "event": "playback-restart" }), at);
        event(
            &mut tracker,
            json!({ "event": "end-file", "reason": "stop" }),
            at,
        );
        property(&mut tracker, "idle-active", json!(true), at);

        assert!(events(&mut tracker).is_empty());
        assert!(!tracker.snapshot().is_loaded());

        // Idle again, so a file opened later is judged on its own.
        event(
            &mut tracker,
            json!({ "event": "start-file", "playlist_entry_id": 9 }),
            at,
        );
        event(&mut tracker, json!({ "event": "file-loaded" }), at);
        event(&mut tracker, json!({ "event": "playback-restart" }), at);
        assert!(matches!(
            events(&mut tracker).as_slice(),
            [PlayerEvent::FileLoaded {
                origin: Origin::External,
                ..
            }]
        ));
    }

    #[test]
    fn a_playlist_someone_else_opened_plays_as_theirs() {
        let at = Instant::now();
        let mut tracker = seeded(at);

        event(
            &mut tracker,
            json!({ "event": "start-file", "playlist_entry_id": 1 }),
            at,
        );
        event(
            &mut tracker,
            json!({ "event": "end-file", "reason": "redirect", "playlist_entry_id": 1 }),
            at,
        );
        event(
            &mut tracker,
            json!({ "event": "start-file", "playlist_entry_id": 2 }),
            at,
        );
        property(&mut tracker, "path", json!("/films/Listed.mkv"), at);
        event(&mut tracker, json!({ "event": "file-loaded" }), at);
        event(&mut tracker, json!({ "event": "playback-restart" }), at);

        let played = events(&mut tracker);
        assert!(
            matches!(
                played.as_slice(),
                [PlayerEvent::FileLoaded { file, origin: Origin::External }] if file.name == "Listed.mkv"
            ),
            "{played:?}"
        );
    }

    #[test]
    fn a_failed_load_over_a_playing_file_still_unloads_the_old_one() {
        let at = Instant::now();
        let mut tracker = playing(at, 5.0);
        let (waiter, _outcome) = oneshot::channel();
        let generation = tracker.expect_load(waiter);
        tracker.on_reply(
            Effect::Load(generation),
            Some(&json!({ "playlist_entry_id": 2 })),
            at,
        );

        // What mpv 0.41 sends when a broken file replaces a playing one.
        event(
            &mut tracker,
            json!({ "event": "end-file", "reason": "stop", "playlist_entry_id": 1 }),
            at,
        );
        event(
            &mut tracker,
            json!({ "event": "start-file", "playlist_entry_id": 2 }),
            at,
        );
        event(
            &mut tracker,
            json!({
                "event": "end-file",
                "reason": "error",
                "playlist_entry_id": 2,
                "file_error": "unrecognized file format"
            }),
            at,
        );
        event(&mut tracker, json!({ "event": "idle" }), at);
        property(&mut tracker, "idle-active", json!(true), at);

        assert_eq!(
            events(&mut tracker),
            [
                PlayerEvent::FileFailed {
                    error: "unrecognized file format".to_owned()
                },
                PlayerEvent::Unloaded,
            ]
        );
        assert!(!tracker.snapshot().is_loaded());
    }

    #[test]
    fn a_file_that_fails_with_nothing_open_before_it_unloads_nothing() {
        let at = Instant::now();
        let mut tracker = seeded(at);

        event(
            &mut tracker,
            json!({ "event": "start-file", "playlist_entry_id": 1 }),
            at,
        );
        event(
            &mut tracker,
            json!({ "event": "end-file", "reason": "error", "file_error": "loading failed" }),
            at,
        );
        property(&mut tracker, "idle-active", json!(true), at);

        assert_eq!(
            events(&mut tracker),
            [PlayerEvent::FileFailed {
                error: "loading failed".to_owned()
            }]
        );
    }

    #[test]
    fn two_quick_pause_changes_of_ours_stay_ours_when_mpv_reports_only_the_last() {
        let at = Instant::now();
        let mut tracker = playing(at, 5.0);

        assert!(tracker.expect_pause(true, at));
        tracker.on_reply(Effect::Pause(true), Some(&Value::Null), at);
        assert!(tracker.expect_pause(false, at));
        tracker.on_reply(Effect::Pause(false), Some(&Value::Null), at);
        property(&mut tracker, "pause", json!(false), at);

        assert_eq!(
            events(&mut tracker),
            [PlayerEvent::PauseChanged {
                paused: false,
                origin: Origin::Ours
            }]
        );
        assert_eq!(tracker.next_deadline(), None, "both expectations met");
    }

    #[test]
    fn a_seek_merged_with_someone_elses_is_theirs_and_frees_the_mirror() {
        let at = Instant::now();
        let mut tracker = playing(at, 5.0);

        let id = tracker.expect_seek(10.0, at).expect("loaded");
        tracker.on_reply(Effect::Seek(id), Some(&Value::Null), at);
        property(&mut tracker, "time-pos", json!(15.0), at);
        assert_eq!(
            tracker.snapshot().time_pos,
            Some(10.0),
            "held at our target"
        );

        event(&mut tracker, json!({ "event": "playback-restart" }), at);
        assert_eq!(
            events(&mut tracker),
            [PlayerEvent::Seeked {
                position: 15.0,
                origin: Origin::External
            }]
        );
        assert_eq!(tracker.snapshot().time_pos, Some(15.0));

        property(&mut tracker, "time-pos", json!(15.04), at);
        assert_eq!(tracker.snapshot().time_pos, Some(15.04));
    }

    #[test]
    fn a_seek_past_the_end_is_aimed_at_the_end() {
        let at = Instant::now();
        let mut tracker = playing(at, 5.0);

        let id = tracker.expect_seek(25.0, at).expect("loaded");
        tracker.on_reply(Effect::Seek(id), Some(&Value::Null), at);
        assert_eq!(tracker.snapshot().time_pos, Some(DURATION));

        property(&mut tracker, "time-pos", json!(19.96), at);
        event(&mut tracker, json!({ "event": "playback-restart" }), at);
        assert_eq!(
            events(&mut tracker),
            [PlayerEvent::Seeked {
                position: 19.96,
                origin: Origin::Ours
            }]
        );
    }

    #[test]
    fn a_seek_that_never_lands_lets_go_of_the_mirror() {
        let at = Instant::now();
        let mut tracker = playing(at, 5.0);

        let id = tracker.expect_seek(10.0, at).expect("loaded");
        tracker.on_reply(Effect::Seek(id), Some(&Value::Null), at);
        property(&mut tracker, "time-pos", json!(5.04), at);
        assert_eq!(tracker.snapshot().time_pos, Some(10.0));

        tracker.on_tick(at + SEEK_PATIENCE);
        assert_eq!(tracker.snapshot().time_pos, Some(5.04));
    }

    #[test]
    fn seeking_with_nothing_loaded_is_refused_before_it_reaches_mpv() {
        let mut tracker = seeded(Instant::now());

        assert_eq!(tracker.expect_seek(10.0, Instant::now()), None);
    }

    #[test]
    fn a_closed_file_is_unloaded_only_once_mpv_is_idle() {
        let at = Instant::now();
        let mut tracker = playing(at, 5.0);

        event(
            &mut tracker,
            json!({ "event": "end-file", "reason": "stop" }),
            at,
        );
        assert!(!tracker.snapshot().is_loaded());
        assert!(
            events(&mut tracker).is_empty(),
            "it may be replaced instead"
        );

        property(&mut tracker, "idle-active", json!(true), at);
        event(&mut tracker, json!({ "event": "idle" }), at);
        assert_eq!(events(&mut tracker), [PlayerEvent::Unloaded]);
    }

    #[test]
    fn a_file_replaced_by_another_is_not_unloaded() {
        let at = Instant::now();
        let mut tracker = playing(at, 5.0);

        event(
            &mut tracker,
            json!({ "event": "end-file", "reason": "stop" }),
            at,
        );
        event(
            &mut tracker,
            json!({ "event": "start-file", "playlist_entry_id": 2 }),
            at,
        );
        property(&mut tracker, "path", json!("/films/Other.mkv"), at);
        property(&mut tracker, "filename", json!("Other.mkv"), at);
        event(&mut tracker, json!({ "event": "file-loaded" }), at);
        event(&mut tracker, json!({ "event": "playback-restart" }), at);

        let replaced = events(&mut tracker);
        assert!(
            matches!(
                replaced.as_slice(),
                [PlayerEvent::FileLoaded { file, origin: Origin::External }] if file.name == "Other.mkv"
            ),
            "{replaced:?}"
        );
    }

    #[test]
    fn a_file_mpv_names_without_a_filename_is_named_after_its_path() {
        let at = Instant::now();
        let mut tracker = seeded(at);

        event(&mut tracker, json!({ "event": "start-file" }), at);
        property(&mut tracker, "path", json!("/films/Film.mkv"), at);
        event(&mut tracker, json!({ "event": "file-loaded" }), at);
        event(&mut tracker, json!({ "event": "playback-restart" }), at);

        let loaded = events(&mut tracker);
        assert!(
            matches!(
                loaded.as_slice(),
                [PlayerEvent::FileLoaded { file, .. }] if file.name == "Film.mkv"
            ),
            "{loaded:?}"
        );
    }

    #[test]
    fn an_old_mpv_that_names_no_playlist_entry_still_has_its_loads_credited() {
        let at = Instant::now();
        let mut tracker = seeded(at);
        let (waiter, mut outcome) = oneshot::channel();
        let generation = tracker.expect_load(waiter);

        tracker.on_reply(Effect::Load(generation), Some(&Value::Null), at);
        event(&mut tracker, json!({ "event": "start-file" }), at);
        property(&mut tracker, "path", json!("/films/Film.mkv"), at);
        event(&mut tracker, json!({ "event": "file-loaded" }), at);
        event(&mut tracker, json!({ "event": "playback-restart" }), at);

        let (emitted, settled) = tracker.take_output();
        assert!(matches!(
            emitted.as_slice(),
            [PlayerEvent::FileLoaded {
                origin: Origin::Ours,
                ..
            }]
        ));
        for (waiter, result) in settled {
            let _ = waiter.send(result);
        }
        assert_eq!(outcome.try_recv(), Ok(Ok(())));
    }

    #[test]
    fn the_way_mpv_went_away_decides_the_close_reason() {
        let tracker = Tracker::new();
        assert_eq!(
            tracker.close_reason(true, Some(0)),
            CloseReason::QuitRequested
        );
        assert_eq!(tracker.close_reason(false, Some(0)), CloseReason::UserQuit);
        assert_eq!(
            tracker.close_reason(false, Some(4)),
            CloseReason::Crashed(Some(4))
        );
        assert_eq!(
            tracker.close_reason(false, None),
            CloseReason::Crashed(None)
        );

        let mut quit = Tracker::new();
        event(
            &mut quit,
            json!({ "event": "end-file", "reason": "quit" }),
            Instant::now(),
        );
        assert_eq!(quit.close_reason(false, None), CloseReason::UserQuit);
    }
}
