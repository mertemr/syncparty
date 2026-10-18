//! Syncplay's client algorithm, with the sockets and the clock taken out.
//!
//! This is a port of upstream 1.7.6's `SyncplayClient` and the client half of
//! its protocol, kept deliberately close to the original: two peers that
//! disagree about when to rewind, or about which counter acknowledges which
//! change, end up fighting over the room. Where this differs it says so.
//!
//! Nothing here does I/O. Every entry point takes the time it should act at,
//! and hands back a [`Step`]: the protocol messages to send, the commands to
//! give the player and the notices to show. The connection task carries those
//! out. That split is what lets the decision tree be tested to the threshold
//! without a socket or a player anywhere.
//!
//! Two upstream behaviours are fixed rather than ported, because both make a
//! joiner disturb a room that was fine without them:
//!
//! - Upstream starts out believing the room is playing from zero and reports
//!   that straight away, so the first answer from a new client can unpause
//!   everyone. Here the room is assumed paused, and nothing about playback is
//!   reported until a state from the server has actually been applied to a
//!   player.
//! - Upstream seeks a newly opened file to the room's position but leaves its
//!   pause state alone, and mpv starts paused, so somebody opening the film in
//!   a playing room pauses the whole room a second later. Here the file is
//!   brought to the room's pause state as well. A file too short to reach
//!   the room's position, which mpv parks on its last frame, paused, is not
//!   taken to have paused or rewound anything either.

use std::time::{Duration, Instant};

use crate::core::player::{LoadedFile, PlayerSnapshot};
use crate::core::syncplay::monitor::{
    file_compatibility, FileCompatibility, RoomSnapshot, MONITOR_NICKNAME,
};
use crate::core::syncplay::ping::PingService;
use crate::core::syncplay::protocol::{
    affects_room_view, ready_update, room_change, user_updates, ClientMessage, ClientPlaystate,
    ClientState, FileReport, Hello, IncomingState, ReadyUpdate, RoomList, ServerHello,
    ServerMessage, UserEntry, UserUpdate,
};

use super::notice::Notice;

/// A position change bigger than this, against both the player's last report
/// and the room, is the user seeking. Upstream's `SEEK_THRESHOLD`.
const SEEK_THRESHOLD: f64 = 1.0;
const SLOWDOWN_RATE: f64 = 0.95;
/// Back to normal speed once this close — or ahead the other way.
const SLOWDOWN_RESET_THRESHOLD: f64 = 0.1;
/// Fast-forwarding lands this far past the room, so it does not land behind.
const FASTFORWARD_EXTRA_TIME: f64 = 0.25;
/// Behind by more than this starts the fast-forward clock.
const FASTFORWARD_BEHIND_THRESHOLD: f64 = 1.75;
/// After fast-forwarding, the clock is held off this long.
const FASTFORWARD_RESET_THRESHOLD: Duration = Duration::from_secs(3);
/// A pause this close to the end of a film this long is the film ending.
/// Upstream's playlist thresholds, which are what decide it there.
const END_OF_FILE_MINIMUM_LENGTH: f64 = 10.0;
const END_OF_FILE_THRESHOLD: f64 = 5.0;
/// A seek to within this of the end of the file, or past it, leaves the
/// player on the last frame: the room is somewhere this file does not reach.
const PAST_THE_END_MARGIN: f64 = SEEK_THRESHOLD;

/// The knobs upstream exposes as settings. syncparty has no UI for them, so
/// they stay at upstream's defaults; they exist so the branches those
/// defaults switch off can still be tested.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Tuning {
    pub rewind_threshold: f64,
    pub fastforward_threshold: f64,
    pub slowdown_threshold: f64,
    pub rewind_on_desync: bool,
    pub slow_on_desync: bool,
    pub fastforward_on_desync: bool,
    /// Report the room's position instead of the player's, so this client
    /// never drags the room back to itself. Off upstream, which is also what
    /// keeps fast-forwarding off.
    pub dont_slow_down_with_me: bool,
    pub ready_at_start: bool,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            rewind_threshold: 4.0,
            fastforward_threshold: 5.0,
            slowdown_threshold: 1.5,
            rewind_on_desync: true,
            slow_on_desync: true,
            fastforward_on_desync: true,
            dont_slow_down_with_me: false,
            ready_at_start: false,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct EngineConfig {
    pub username: String,
    pub room: String,
    pub password: String,
    /// How long after a file opens its position is not believed. Upstream's
    /// `MPV_NEWFILE_IGNORE_TIME`.
    pub load_settle: Duration,
    pub tuning: Tuning,
}

/// Something the player should be told to do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum PlayerCommand {
    SetPaused(bool),
    Seek(f64),
    SetSpeed(f64),
}

/// What one input to the engine calls for, in the order it should happen.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Step {
    pub send: Vec<ClientMessage>,
    pub player: Vec<PlayerCommand>,
    pub notices: Vec<Notice>,
}

/// The file this client has open, and when it opened.
#[derive(Debug, Clone)]
struct OpenFile {
    /// What the player calls it, which is how a snapshot showing some other
    /// file is told apart from one showing this one.
    path: String,
    report: FileReport,
    loaded_at: Instant,
}

/// Whose file a mismatch notice was last shown for, so it is shown once per
/// pair of files rather than on every refresh of the list.
type FileNoticeKey = (String, String, String);

pub(crate) struct Engine {
    config: EngineConfig,
    /// The name to greet with: the configured one, then whatever the server
    /// settled on, as upstream adopts it.
    username: String,
    room: String,
    /// The name the server knows this client by, once it has said.
    me: Option<String>,

    // Per connection.
    logged: bool,
    server_readiness: bool,
    client_ignoring: u64,
    server_ignoring: u64,
    ping: PingService,

    // The room, as last applied. `None` until a server state has been
    // applied to a player on this connection.
    global_paused: bool,
    global_position: f64,
    last_global_update: Option<Instant>,

    // The player, as last polled.
    player_attached: bool,
    file: Option<OpenFile>,
    player_paused: bool,
    player_position: f64,
    last_player_update: Option<Instant>,
    speed_changed: bool,
    behind_first_detected: Option<Instant>,
    position_before_last_seek: f64,
    /// The player sits on the last frame of its file — sent there by a seek
    /// to the end or beyond, or run out by playing. mpv parks there, paused,
    /// and ignores any unpause, so nothing it reports from there is the
    /// user's doing.
    parked_at_the_end: bool,

    // Readiness and the room list.
    ready: Option<bool>,
    rooms: RoomList,
    have_list: bool,
    snapshot: RoomSnapshot,
    file_notice: Option<FileNoticeKey>,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Self {
        // Upstream falls back to the username for a room nobody named.
        let room = if config.room.trim().is_empty() {
            config.username.clone()
        } else {
            config.room.clone()
        };

        Self {
            username: config.username.clone(),
            room,
            config,
            me: None,
            logged: false,
            server_readiness: true,
            client_ignoring: 0,
            server_ignoring: 0,
            ping: PingService::new(),
            global_paused: true,
            global_position: 0.0,
            last_global_update: None,
            player_attached: false,
            file: None,
            player_paused: true,
            player_position: 0.0,
            last_player_update: None,
            speed_changed: false,
            behind_first_detected: None,
            position_before_last_seek: 0.0,
            parked_at_the_end: false,
            ready: None,
            rooms: RoomList::new(),
            have_list: false,
            snapshot: RoomSnapshot::disconnected(),
            file_notice: None,
        }
    }

    /// The name the server assigned, once it has greeted this client.
    pub fn username(&self) -> Option<&str> {
        self.me.as_deref()
    }

    pub fn ready(&self) -> bool {
        self.ready.unwrap_or(false)
    }

    pub fn room(&self) -> &RoomSnapshot {
        &self.snapshot
    }

    // ---------------------------------------------------------- connection

    /// A connection is up: everything that belonged to the last one is gone,
    /// and the greeting goes out first.
    pub fn connected(&mut self) -> Step {
        self.reset_connection();

        Step {
            send: vec![ClientMessage::Hello(Hello::participant(
                &self.username,
                &self.room,
                Some(&self.config.password),
            ))],
            ..Step::default()
        }
    }

    /// The connection is gone. The player and the file are kept, so the next
    /// connection picks up where this one left off; the room list is not,
    /// because nothing vouches for it any more.
    pub fn disconnected(&mut self) {
        self.reset_connection();
        self.rooms.clear();
        self.have_list = false;
        self.file_notice = None;
        self.refresh_snapshot();
    }

    /// Each connection gets new counters and a new ping service, as each
    /// gets a new protocol object upstream. The room's state is forgotten
    /// too: until the new connection's first state is applied, this client
    /// has nothing it should tell the room about playback.
    fn reset_connection(&mut self) {
        self.logged = false;
        self.client_ignoring = 0;
        self.server_ignoring = 0;
        self.ping = PingService::new();
        self.last_global_update = None;
    }

    pub fn receive(&mut self, message: ServerMessage, now: Instant, epoch: f64) -> Step {
        let mut step = Step::default();

        match message {
            ServerMessage::Hello(hello) => self.greeted(hello, &mut step),
            ServerMessage::List(rooms) => self.list(rooms, &mut step),
            ServerMessage::Set(set) => self.set(&set, &mut step),
            ServerMessage::State(state) => self.state(state, now, epoch, &mut step),
            // A refusal ends the connection, which is the connection task's
            // business rather than the algorithm's.
            ServerMessage::Error { .. } | ServerMessage::Ignored => {}
        }

        step
    }

    fn greeted(&mut self, hello: ServerHello, step: &mut Step) {
        if let Some(username) = hello.username.filter(|name| !name.is_empty()) {
            self.username = username;
        }
        if let Some(room) = hello.room.filter(|room| !room.is_empty()) {
            self.room = room;
        }
        self.me = Some(self.username.clone());
        self.server_readiness = hello.readiness.unwrap_or(true);
        self.logged = true;

        step.notices.push(Notice::Connected);
        step.send.push(ClientMessage::SetReady {
            is_ready: self.ready.unwrap_or(self.config.tuning.ready_at_start),
            manually_initiated: false,
        });
        if let Some(file) = &self.file {
            step.send.push(ClientMessage::SetFile(file.report.clone()));
        }
        // Asked for whether or not there is a file to announce: the list is
        // also what the room panel and readiness are built from.
        step.send.push(ClientMessage::ListRequest);
    }

    // ---------------------------------------------------------- the room

    fn list(&mut self, rooms: RoomList, step: &mut Step) {
        self.rooms = rooms;
        self.have_list = true;
        if let Some(entry) = self.me.clone().and_then(|me| self.entry(&me).cloned()) {
            self.ready = entry.is_ready;
        }

        self.refresh_snapshot();
        self.check_files(step);
    }

    fn set(&mut self, set: &serde_json::Map<String, serde_json::Value>, step: &mut Step) {
        if let Some(updates) = user_updates(set) {
            for (username, update) in updates {
                self.user_update(username, update, step);
            }
        }
        if let Some(update) = ready_update(set) {
            self.ready_update(update);
        }
        if let Some(room) = room_change(set) {
            self.room = room;
        }

        // The deltas above keep readiness and the notices current; the list
        // is still asked for again, because rebuilding from deltas is how a
        // view drifts out of sync, and rooms are small.
        if affects_room_view(set) && self.logged {
            step.send.push(ClientMessage::ListRequest);
        }

        self.refresh_snapshot();
        self.check_files(step);
    }

    fn user_update(&mut self, username: String, update: UserUpdate, step: &mut Step) {
        let is_me = self.me.as_deref() == Some(username.as_str());
        let room = update.room.and_then(|room| room.name);
        let event = update.event.unwrap_or_default();

        if event.joined.is_some() {
            if is_me {
                return;
            }
            let room = room.unwrap_or_else(|| self.room.clone());
            self.remove_user(&username);
            if room == self.room {
                step.notices.push(Notice::Joined {
                    user: username.clone(),
                });
            }
            self.rooms.entry(room).or_default().insert(
                username,
                UserEntry {
                    file: update.file.unwrap_or_default(),
                    ..UserEntry::default()
                },
            );
        } else if event.left.is_some() {
            let was_here = self.room_of(&username) == Some(self.room.clone());
            self.remove_user(&username);
            if was_here && !is_me {
                step.notices.push(Notice::Left { user: username });
            }
        } else {
            // A move or a file change. An empty file object closes the file.
            let current = self.room_of(&username);
            let mut entry = self.remove_user(&username).unwrap_or_default();
            if let Some(file) = update.file {
                entry.file = file;
            }
            let room = room.or(current).unwrap_or_else(|| self.room.clone());
            self.rooms.entry(room).or_default().insert(username, entry);
        }
    }

    fn ready_update(&mut self, update: ReadyUpdate) {
        if self.me.as_deref() == Some(update.username.as_str()) {
            self.ready = update.is_ready;
        }
        if let Some(entry) = self
            .rooms
            .values_mut()
            .find_map(|members| members.get_mut(&update.username))
        {
            entry.is_ready = update.is_ready;
        }
    }

    fn room_of(&self, username: &str) -> Option<String> {
        self.rooms
            .iter()
            .find(|(_, members)| members.contains_key(username))
            .map(|(room, _)| room.clone())
    }

    fn entry(&self, username: &str) -> Option<&UserEntry> {
        self.rooms
            .values()
            .find_map(|members| members.get(username))
    }

    fn remove_user(&mut self, username: &str) -> Option<UserEntry> {
        let removed = self
            .rooms
            .values_mut()
            .find_map(|members| members.remove(username));
        self.rooms.retain(|_, members| !members.is_empty());
        removed
    }

    fn refresh_snapshot(&mut self) {
        self.snapshot = if self.have_list {
            RoomSnapshot::from_list(&self.rooms, MONITOR_NICKNAME)
        } else {
            RoomSnapshot::disconnected()
        };
    }

    /// Warns, once per pair of files, when somebody in the room has a file
    /// that is not the same film as this client's.
    fn check_files(&mut self, step: &mut Step) {
        let Some(me) = self.me.as_deref() else {
            return;
        };
        let Some(room) = self
            .snapshot
            .rooms
            .iter()
            .find(|room| room.name == self.room)
        else {
            self.file_notice = None;
            return;
        };
        if room.file_compatibility != FileCompatibility::Mismatch {
            self.file_notice = None;
            return;
        }
        let Some(mine) = room.watchers.iter().find(|watcher| watcher.name == me) else {
            return;
        };
        let Some(my_file) = mine.file.as_ref() else {
            return;
        };

        let other = room.watchers.iter().find(|watcher| {
            watcher.name != me
                && file_compatibility(&[mine.clone(), (*watcher).clone()])
                    == FileCompatibility::Mismatch
        });
        let Some((other, other_file)) =
            other.and_then(|other| other.file.as_ref().map(|file| (other, file)))
        else {
            return;
        };

        let key = (
            other.name.clone(),
            other_file.name.clone(),
            my_file.name.clone(),
        );
        if self.file_notice.as_ref() != Some(&key) {
            step.notices.push(Notice::FileDiffers {
                user: other.name.clone(),
            });
            self.file_notice = Some(key);
        }
    }

    // ---------------------------------------------------------- readiness

    /// The user's own toggle. Before the server has greeted this client it
    /// is only remembered, and goes out with the greeting.
    pub fn set_ready(&mut self, ready: bool) -> Step {
        let mut step = Step::default();

        if !self.logged {
            self.ready = Some(ready);
        } else if self.ready != Some(ready) {
            step.send.push(ClientMessage::SetReady {
                is_ready: ready,
                manually_initiated: true,
            });
        }

        step
    }

    /// Upstream's `changeReadyState`: tells the server, if this is a change.
    /// The local value only moves when the server echoes it back.
    fn change_ready_state(&mut self, ready: bool, manually_initiated: bool, step: &mut Step) {
        if !self.server_readiness || !self.logged || self.ready == Some(ready) {
            return;
        }

        step.send.push(ClientMessage::SetReady {
            is_ready: ready,
            manually_initiated,
        });
    }

    /// Whether this client may unpause without asking: it is ready already,
    /// or everybody else in the room is (upstream's `IfOthersReady`).
    ///
    /// Somebody with no file open never blocks, and neither does somebody who
    /// has never said either way — only an explicit "not ready" does.
    fn instaplay_conditions_met(&self) -> bool {
        if self.ready == Some(true) {
            return true;
        }

        let me = self.me.as_deref();
        self.rooms
            .get(&self.room)
            .map(|members| {
                members
                    .iter()
                    .filter(|(name, _)| Some(name.as_str()) != me)
                    .all(|(_, entry)| !(entry.file.name.is_some() && entry.is_ready == Some(false)))
            })
            .unwrap_or(true)
    }

    /// Upstream's `_toggleReady`, reduced to the branches that apply without
    /// managed rooms, playlists or the music override. Returns whether the
    /// pause change should still go to the room.
    fn toggle_ready(&mut self, paused: bool, step: &mut Step) -> bool {
        if !paused && !self.instaplay_conditions_met() {
            // Somebody is not ready: the unpause becomes "I am ready", and
            // the player goes back to waiting.
            step.player.push(PlayerCommand::SetPaused(true));
            self.player_paused = true;
            self.change_ready_state(true, true, step);
            step.notices.push(Notice::ReadyToUnpause);
            return false;
        }

        // Upstream reads this back through `getPlayerPaused()`, which is the
        // value just stored — except in the one poll before the player model
        // has a timestamp, where it falls back to the room and inverts the
        // answer. The intent, unpaused means ready, is used directly.
        self.change_ready_state(!paused, false, step);
        true
    }

    // ---------------------------------------------------------- the player

    /// A player is attached. It starts from nothing: until a server state has
    /// been applied to it, this client reports no playstate.
    pub fn attach(&mut self, snapshot: &PlayerSnapshot, now: Instant, epoch: f64) -> Step {
        self.reset_player();
        self.player_attached = true;

        match &snapshot.file {
            Some(file) => self.file_loaded(file, now, epoch),
            None => Step::default(),
        }
    }

    /// The player is gone. The connection stays up, and says nothing about
    /// playback until another player is attached and has caught up.
    pub fn detach(&mut self) {
        self.reset_player();
        self.player_attached = false;
    }

    fn reset_player(&mut self) {
        self.file = None;
        self.player_paused = true;
        self.player_position = 0.0;
        self.last_player_update = None;
        self.speed_changed = false;
        self.behind_first_detected = None;
        self.position_before_last_seek = 0.0;
        self.parked_at_the_end = false;
        self.last_global_update = None;
    }

    /// The player finished opening a file.
    ///
    /// The room hears about it, and the player is brought to the room's
    /// position and pause state straight away — the second half being one of
    /// the two fixes this port makes, so opening the film never pauses or
    /// rewinds a room that is already watching.
    ///
    /// A file too short to reach the room's position is still sent there:
    /// mpv parks it on its last frame, and the room's state stands in for it
    /// until something brings it back into the file. Leaving it at its start
    /// instead would read as this client seeking the room back to the start.
    pub fn file_loaded(&mut self, file: &LoadedFile, now: Instant, epoch: f64) -> Step {
        let mut step = Step::default();
        if !self.player_attached {
            return step;
        }

        let report = FileReport {
            name: file.name.clone(),
            duration: file.duration.unwrap_or(0.0),
            size: file.size.unwrap_or(0),
        };
        self.file = Some(OpenFile {
            path: file.path.clone(),
            report: report.clone(),
            loaded_at: now,
        });
        self.parked_at_the_end = false;

        if self.last_global_update.is_some() {
            let position = self.global_position_at(now);
            self.set_position(position, now, epoch, &mut step);
        }
        let paused = self.global_paused_now();
        self.set_paused(paused, now, &mut step);

        if self.logged {
            step.send.push(ClientMessage::SetFile(report));
            step.send.push(ClientMessage::ListRequest);
        }

        step
    }

    /// The player closed its file and is idle again — or failed to open
    /// one, which closes whatever it had open first.
    pub fn file_unloaded(&mut self) {
        self.file = None;
        self.parked_at_the_end = false;
    }

    /// One look at the player, upstream's `updatePlayerStatus`.
    ///
    /// Run ten times a second. A change the user made is told apart from one
    /// this client made by comparing against both the player's last report
    /// and the room: a command updates both before it is even sent, so it
    /// never crosses the thresholds here.
    pub fn poll(&mut self, snapshot: &PlayerSnapshot, now: Instant, epoch: f64) -> Step {
        let mut step = Step::default();
        if !self.player_attached {
            return step;
        }

        let (paused, position) = self.reported(snapshot, now);
        let mut pause_change =
            self.player_paused_now() != paused && self.global_paused_now() != paused;
        let seeked = (self.player_position_at(now) - position).abs() > SEEK_THRESHOLD
            && (self.global_position_at(now) - position).abs() > SEEK_THRESHOLD;

        self.player_position = position;
        self.player_paused = paused;

        let length = self
            .file
            .as_ref()
            .map(|file| file.report.duration)
            .unwrap_or(0.0);
        let at_the_end = paused
            && length > END_OF_FILE_MINIMUM_LENGTH
            && (position - length).abs() < END_OF_FILE_THRESHOLD;

        // mpv pausing itself on the last frame is still a pause the room
        // should follow, but it says nothing about whether anyone is ready.
        if pause_change && !at_the_end && self.server_readiness {
            pause_change = self.toggle_ready(paused, &mut step);
        }

        // Upstream only timestamps the player model once the room's state is
        // known, which leaves it extrapolating from a stale stamp across a
        // reconnect. Stamping every poll keeps the model honest; nothing
        // reads it before then except the first state's catch-up.
        self.last_player_update = Some(now);

        if self.last_global_update.is_some() && (pause_change || seeked) {
            if seeked {
                self.position_before_last_seek = self.global_position_at(now);
            }
            let local = (self.player_position_at(now), self.player_paused_now());
            self.send_state(Some(local), seeked, None, true, epoch, &mut step);
        }

        // A file that ran out by playing now sits on its last frame, where mpv
        // ignores every unpause. Its end has reached the room — just now, or
        // already if the room was paused — so from here on it is parked, as
        // if a seek had sent it there. Otherwise a shorter copy would pause
        // the room again every time somebody pressed play.
        if at_the_end && snapshot.eof_reached && self.last_global_update.is_some() {
            self.parked_at_the_end = true;
        }

        step
    }

    /// What the player is taken to be doing.
    ///
    /// The player's own reports are not believed, and the room's state stands
    /// in for them as upstream's mpv wrapper does, whenever they cannot be
    /// about the user:
    ///
    /// - with nothing open;
    /// - for a moment after a file opens, which reports positions from before
    ///   the seek that brings it to the room;
    /// - while the player shows some other file than the one this client
    ///   opened. mpv swaps files without closing the old one first, so the
    ///   new file is on screen, at its start, before its load is reported —
    ///   upstream stops believing the player the moment the swap begins;
    /// - while it is parked on the last frame, by a seek past the end or by
    ///   having run out.
    fn reported(&mut self, snapshot: &PlayerSnapshot, now: Instant) -> (bool, f64) {
        let room = (self.global_paused_now(), self.global_position_at(now));
        let Some(file) = self.file.as_ref() else {
            return room;
        };

        let shows_this_file = snapshot
            .file
            .as_ref()
            .is_some_and(|shown| shown.path == file.path);
        let settled = now.saturating_duration_since(file.loaded_at) > self.config.load_settle;
        let Some(position) = snapshot
            .position(now)
            .filter(|_| shows_this_file && settled)
        else {
            return room;
        };

        if self.parked_at_the_end {
            let length = file.report.duration;
            if snapshot.eof_reached || position >= length - PAST_THE_END_MARGIN {
                return room;
            }
            // Back inside the file, and not by a seek of ours: the user
            // moved it.
            self.parked_at_the_end = false;
        }

        (snapshot.paused, position)
    }

    // ---------------------------------------------------------- server state

    /// One `State` from the server, upstream's `handleState`. Always answered
    /// with exactly one `State`.
    fn state(&mut self, state: IncomingState, now: Instant, epoch: f64, step: &mut Step) {
        if let Some(ignore) = state.ignoring_on_the_fly {
            if let Some(server) = ignore.server {
                self.server_ignoring = server;
                self.client_ignoring = 0;
            } else if ignore.client == Some(self.client_ignoring) {
                self.client_ignoring = 0;
            }
        }

        let latency_calculation = state.latency_calculation();
        let mut message_age = 0.0;
        if let Some(ping) = state.ping {
            if ping.client_latency_calculation.is_some() {
                self.ping.receive(
                    ping.client_latency_calculation,
                    ping.server_rtt.unwrap_or(0.0),
                    epoch,
                );
            }
            message_age = self.ping.forward_delay();
        }

        if let Some(playstate) = state.playstate {
            if let (Some(position), Some(paused)) = (playstate.position, playstate.paused) {
                // While a change of ours is unacknowledged, everything the
                // server says describes the room before it.
                if self.client_ignoring == 0 {
                    self.update_global_state(
                        position,
                        paused,
                        playstate.do_seek == Some(true),
                        playstate.set_by,
                        message_age,
                        now,
                        epoch,
                        step,
                    );
                }
            }
        }

        let local = self.local_state(now);
        self.send_state(local, false, latency_calculation, false, epoch, step);
    }

    #[allow(clippy::too_many_arguments)]
    fn update_global_state(
        &mut self,
        mut position: f64,
        paused: bool,
        do_seek: bool,
        set_by: Option<String>,
        message_age: f64,
        now: Instant,
        epoch: f64,
        step: &mut Step,
    ) {
        // A playing room has moved on by however long the message took.
        if !paused {
            position += message_age;
        }

        // With no player there is nothing to apply it to, and upstream keeps
        // nothing either.
        if self.player_attached {
            self.change_player_state(position, paused, do_seek, set_by, now, epoch, step);
        }
    }

    /// Upstream's `_changePlayerStateAccordingToGlobalState`, branch for
    /// branch and in the same order.
    #[allow(clippy::too_many_arguments)]
    fn change_player_state(
        &mut self,
        position: f64,
        paused: bool,
        do_seek: bool,
        set_by: Option<String>,
        now: Instant,
        epoch: f64,
        step: &mut Step,
    ) {
        let initialising = self.last_global_update.is_none();
        let pause_changed =
            paused != self.global_paused_now() || paused != self.player_paused_now();
        // Positive means this client is ahead of the room.
        let diff = self.player_position_at(now) - position;

        self.global_paused = paused;
        self.global_position = position;
        self.last_global_update = Some(now);

        if initialising {
            self.init_player_state(position, paused, now, epoch, step);
            return;
        }

        if do_seek {
            self.server_seeked(position, &set_by, now, epoch, step);
        }

        let tuning = self.config.tuning;
        if diff > tuning.rewind_threshold && !do_seek && tuning.rewind_on_desync {
            self.rewind(position, &set_by, now, epoch, step);
        }

        // Upstream gates this on "cannot control the room", which is never
        // true without managed rooms, or on not reporting its own position.
        if tuning.fastforward_on_desync && tuning.dont_slow_down_with_me {
            if diff < -FASTFORWARD_BEHIND_THRESHOLD && !do_seek {
                match self.behind_first_detected {
                    None => self.behind_first_detected = Some(now),
                    Some(since) => {
                        // `since` can be in the future right after a fast
                        // forward, which reads as not behind for long yet.
                        let behind_for = now.saturating_duration_since(since).as_secs_f64();
                        if behind_for > tuning.fastforward_threshold - FASTFORWARD_BEHIND_THRESHOLD
                            && diff < -tuning.fastforward_threshold
                        {
                            self.fastforward(position, &set_by, now, epoch, step);
                            self.behind_first_detected = Some(now + FASTFORWARD_RESET_THRESHOLD);
                        }
                    }
                }
            } else {
                self.behind_first_detected = None;
            }
        }

        // Skipped while paused and on a seek, so a slowdown outlives a pause.
        if !do_seek && !paused && tuning.slow_on_desync {
            self.slow_down(diff, &set_by, step);
        }

        if !paused && pause_changed {
            self.server_unpaused(&set_by, now, step);
        } else if paused && pause_changed {
            self.server_paused(&set_by, now, epoch, step);
        }
    }

    /// The first state applied on this connection brings the player to the
    /// room outright, rather than steering it there.
    ///
    /// Upstream has this branch but never reaches it. Here it runs on every
    /// new connection and every new player, so it skips a seek the player
    /// does not need — a reconnect in the middle of a film should not jump —
    /// and announces nothing, because nobody did anything.
    fn init_player_state(
        &mut self,
        position: f64,
        paused: bool,
        now: Instant,
        epoch: f64,
        step: &mut Step,
    ) {
        if self.file.is_some() {
            let close_enough = self.last_player_update.is_some()
                && (self.player_position_at(now) - position).abs() <= SEEK_THRESHOLD;
            if !close_enough {
                self.set_position(position, now, epoch, step);
            }
        } else {
            // With nothing open the player has no position of its own: it is
            // wherever the room is, which is what every poll will say.
            self.player_position = position;
            self.last_player_update = Some(now);
        }

        self.set_paused(paused, now, step);
    }

    fn is_me(&self, set_by: &Option<String>) -> bool {
        self.me.is_some() && *set_by == self.me
    }

    fn server_seeked(
        &mut self,
        position: f64,
        set_by: &Option<String>,
        now: Instant,
        epoch: f64,
        step: &mut Step,
    ) {
        if !self.is_me(set_by) {
            self.position_before_last_seek = self.player_position_at(now);
            self.set_position(position, now, epoch, step);
        }

        // Shown for this client's own seeks too, from where it was when it
        // seeked: it confirms the room went along.
        step.notices.push(Notice::Seeked {
            user: set_by.clone(),
            from: self.position_before_last_seek,
            to: position,
        });
    }

    fn rewind(
        &mut self,
        position: f64,
        set_by: &Option<String>,
        now: Instant,
        epoch: f64,
        step: &mut Step,
    ) {
        if self.is_me(set_by) {
            return;
        }

        self.set_position(position, now, epoch, step);
        step.notices.push(Notice::Rewound {
            user: set_by.clone(),
        });
    }

    fn fastforward(
        &mut self,
        position: f64,
        set_by: &Option<String>,
        now: Instant,
        epoch: f64,
        step: &mut Step,
    ) {
        if self.is_me(set_by) {
            return;
        }

        self.set_position(position + FASTFORWARD_EXTRA_TIME, now, epoch, step);
        step.notices.push(Notice::FastForwarded {
            user: set_by.clone(),
        });
    }

    fn slow_down(&mut self, diff: f64, set_by: &Option<String>, step: &mut Step) {
        if self.config.tuning.slowdown_threshold < diff && !self.speed_changed {
            if !self.is_me(set_by) {
                step.player.push(PlayerCommand::SetSpeed(SLOWDOWN_RATE));
                self.speed_changed = true;
            }
        } else if self.speed_changed && diff < SLOWDOWN_RESET_THRESHOLD {
            step.player.push(PlayerCommand::SetSpeed(1.0));
            self.speed_changed = false;
        }
    }

    fn server_unpaused(&mut self, set_by: &Option<String>, now: Instant, step: &mut Step) {
        self.set_paused(false, now, step);
        step.notices.push(Notice::Unpaused {
            user: set_by.clone(),
        });
    }

    fn server_paused(
        &mut self,
        set_by: &Option<String>,
        now: Instant,
        epoch: f64,
        step: &mut Step,
    ) {
        // Pausing also lines everyone up on the frame the room paused at.
        if !self.is_me(set_by) {
            let position = self.global_position_at(now);
            self.set_position(position, now, epoch, step);
        }

        self.set_paused(true, now, step);
        step.notices.push(Notice::Paused {
            user: set_by.clone(),
            at: self.global_position_at(now),
        });
    }

    /// Upstream's `client.setPosition`: a seek, only with a file open. The
    /// player model moves with it, which is what keeps the next poll from
    /// mistaking the seek for the user's.
    ///
    /// The model takes the position asked for even when the file ends before
    /// it. That is where the room is, and what replies report, so a short file
    /// never becomes the slowest watcher the server follows.
    fn set_position(&mut self, position: f64, now: Instant, epoch: f64, step: &mut Step) {
        let Some(length) = self.file.as_ref().map(|file| file.report.duration) else {
            return;
        };

        let mut position = position;
        if position < 0.0 {
            position = 0.0;
            let local = (self.player_position_at(now), self.player_paused_now());
            self.send_state(Some(local), true, None, true, epoch, step);
        }

        let was_parked = self.parked_at_the_end;
        self.parked_at_the_end = length > 0.0 && position >= length - PAST_THE_END_MARGIN;

        step.player.push(PlayerCommand::Seek(position));
        self.player_position = position;
        self.last_player_update = Some(now);

        // mpv ignored any unpause while it sat on the last frame, so coming
        // back into the file is when the pause state can be put right.
        if was_parked && !self.parked_at_the_end {
            step.player
                .push(PlayerCommand::SetPaused(self.player_paused_now()));
        }
    }

    /// A pause or unpause, with the player model moved along at once. Sent
    /// even with nothing open, as upstream sends it, so an idle player is
    /// already in the room's state when a file arrives.
    fn set_paused(&mut self, paused: bool, now: Instant, step: &mut Step) {
        self.player_position = self.player_position_at(now);
        self.player_paused = paused;
        self.last_player_update = Some(now);
        step.player.push(PlayerCommand::SetPaused(paused));
    }

    // ---------------------------------------------------------- reporting

    /// What a reply reports: upstream's `getLocalState`. A reply never
    /// carries a seek or a state change, because it is computed from this
    /// client's own model.
    fn local_state(&self, now: Instant) -> Option<(f64, bool)> {
        self.last_global_update?;

        let position = if self.config.tuning.dont_slow_down_with_me {
            self.global_position_at(now)
        } else {
            self.player_position_at(now)
        };
        Some((position, self.player_paused_now()))
    }

    /// Upstream's `sendState`.
    ///
    /// The playstate is left out while a change of ours is unacknowledged —
    /// unless the server is waiting on an acknowledgement of its own, which
    /// must carry the state it is acknowledging. A change bumps the client
    /// counter after that decision, and the server counter is echoed once.
    fn send_state(
        &mut self,
        playstate: Option<(f64, bool)>,
        do_seek: bool,
        latency_calculation: Option<f64>,
        state_change: bool,
        epoch: f64,
        step: &mut Step,
    ) {
        if !self.logged {
            return;
        }

        let reportable = self.client_ignoring == 0 || self.server_ignoring != 0;
        let playstate =
            playstate
                .filter(|_| reportable)
                .map(|(position, paused)| ClientPlaystate {
                    position,
                    paused,
                    do_seek,
                });

        if state_change {
            self.client_ignoring += 1;
        }
        let server = (self.server_ignoring != 0).then_some(self.server_ignoring);
        self.server_ignoring = 0;
        let client = (self.client_ignoring != 0).then_some(self.client_ignoring);

        step.send.push(ClientMessage::State(ClientState {
            playstate,
            latency_calculation: latency_calculation.filter(|stamp| *stamp != 0.0),
            client_latency_calculation: epoch,
            client_rtt: self.ping.rtt().max(0.0),
            ignoring_on_the_fly: (server, client),
        }));
    }

    // ---------------------------------------------------------- the models

    fn global_position_at(&self, now: Instant) -> f64 {
        match self.last_global_update {
            None => 0.0,
            Some(at) if !self.global_paused => self.global_position + seconds_since(at, now),
            Some(_) => self.global_position,
        }
    }

    /// Paused until the room has said otherwise. Upstream starts from a
    /// falsy `0.0` here, which reads as playing.
    fn global_paused_now(&self) -> bool {
        self.last_global_update.is_none() || self.global_paused
    }

    fn player_position_at(&self, now: Instant) -> f64 {
        match self.last_player_update {
            None if self.last_global_update.is_some() => self.global_position_at(now),
            None => 0.0,
            Some(at) if !self.player_paused => self.player_position + seconds_since(at, now),
            Some(_) => self.player_position,
        }
    }

    fn player_paused_now(&self) -> bool {
        match self.last_player_update {
            None if self.last_global_update.is_some() => self.global_paused,
            None => true,
            Some(_) => self.player_paused,
        }
    }
}

fn seconds_since(then: Instant, now: Instant) -> f64 {
    now.saturating_duration_since(then).as_secs_f64()
}

#[cfg(test)]
mod tests;
