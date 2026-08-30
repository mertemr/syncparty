//! "What should we watch" — a host-authoritative vote among 2-10 candidates.
//!
//! Mirrors [`crate::core::session::PartySession`]'s shape: one
//! `Mutex<Option<MovieVoteSnapshot>>`, mutated by a handful of async methods
//! that each publish an [`AppEvent`] afterwards. The one addition is the
//! wire: every mutation is also pushed down the tunnel's control channel — to
//! every guest when this machine is hosting, or to the host alone when it is
//! a guest acting on its own vote or participation.
//!
//! State transitions themselves live in the private [`logic`] module as pure
//! functions on a bare [`MovieVoteSnapshot`], with no `Mutex`, no session and
//! no event bus in sight — that is what keeps the interesting rules (2-10
//! candidates, no duplicates, host-only cancellation, tie handling) testable
//! without spinning up anything.

use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use ts_rs::TS;

use crate::core::config::{generate_token, ConfigStore};
use crate::core::error::{Result, SyncPartyError};
use crate::core::events::{AppEvent, EventBus};
use crate::core::movie::{MovieStore, SessionHistoryEntry, WatchedMovie};
use crate::core::net::ControlChannel;
use crate::core::notify::{self, DiscordNotifier};
use crate::core::session::PartySession;

pub const MIN_CANDIDATES: usize = 2;
pub const MAX_CANDIDATES: usize = 10;

/// Where a vote is in its lifecycle.
///
/// ```text
/// Draft (candidates editable) -> Open (locked, votes accepted) -> Completed
///                                                               -> Cancelled
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum VotePhase {
    Draft,
    Open,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum ParticipationStatus {
    Going,
    Maybe,
    NotGoing,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MovieCandidate {
    pub tmdb_id: i64,
    pub title: String,
    pub poster: Option<String>,
    pub release_date: Option<String>,
    pub overview: Option<String>,
    pub genres: Vec<String>,
    pub rating: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct VoteParticipant {
    /// Whoever this is, keyed by their endpoint id as a string — or the
    /// literal `"host"` for the host's own entry, which has no guest
    /// connection to key on.
    pub peer: String,
    pub display_name: String,
    pub participation: Option<ParticipationStatus>,
    pub selected_movie: Option<i64>,
    pub responded_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct CandidateTally {
    pub tmdb_id: i64,
    pub votes: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct VoteResult {
    pub tally: Vec<CandidateTally>,
    /// `Some` once a single winner is settled — either nobody tied for the
    /// top, or the host broke a tie. `None` while a tie is waiting on the
    /// host to resolve it.
    pub winner: Option<i64>,
    /// The tmdb ids tied for first. Non-empty only while `winner` is `None`.
    pub tied: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MovieVoteSnapshot {
    pub id: String,
    pub phase: VotePhase,
    pub created_at: i64,
    /// ISO 8601 date or date-time, already resolved to the host's timezone by
    /// the frontend — `core` treats it as an opaque label and never parses
    /// it. `None` means no movie night time was set.
    pub schedule: Option<String>,
    pub candidates: Vec<MovieCandidate>,
    pub participants: Vec<VoteParticipant>,
    /// Set once the vote closes.
    pub result: Option<VoteResult>,
}

fn host_only() -> SyncPartyError {
    SyncPartyError::Other("only the host can manage the movie vote".to_owned())
}

fn no_active_vote() -> SyncPartyError {
    SyncPartyError::Other("no movie vote is in progress".to_owned())
}

/// Pure state-machine rules, free of any I/O — every rule from the spec that
/// is worth testing in isolation lives here.
mod logic {
    use super::{
        no_active_vote, CandidateTally, MovieCandidate, MovieVoteSnapshot, ParticipationStatus,
        VoteParticipant, VotePhase, VoteResult, MAX_CANDIDATES, MIN_CANDIDATES,
    };
    use crate::core::error::{Result, SyncPartyError};

    pub fn draft(id: String, created_at: i64, schedule: Option<String>) -> MovieVoteSnapshot {
        MovieVoteSnapshot {
            id,
            phase: VotePhase::Draft,
            created_at,
            schedule,
            candidates: Vec::new(),
            participants: Vec::new(),
            result: None,
        }
    }

    fn require_draft(snapshot: &MovieVoteSnapshot) -> Result<()> {
        if snapshot.phase != VotePhase::Draft {
            return Err(SyncPartyError::Other(
                "the candidate list is locked once a vote is open".to_owned(),
            ));
        }
        Ok(())
    }

    pub fn add_candidate(
        snapshot: &mut MovieVoteSnapshot,
        candidate: MovieCandidate,
    ) -> Result<()> {
        require_draft(snapshot)?;

        if snapshot.candidates.len() >= MAX_CANDIDATES {
            return Err(SyncPartyError::Other(format!(
                "a vote can hold at most {MAX_CANDIDATES} movies"
            )));
        }
        if snapshot
            .candidates
            .iter()
            .any(|existing| existing.tmdb_id == candidate.tmdb_id)
        {
            return Err(SyncPartyError::Other(
                "that movie is already a candidate".to_owned(),
            ));
        }

        snapshot.candidates.push(candidate);
        Ok(())
    }

    pub fn remove_candidate(snapshot: &mut MovieVoteSnapshot, tmdb_id: i64) -> Result<()> {
        require_draft(snapshot)?;
        snapshot.candidates.retain(|c| c.tmdb_id != tmdb_id);
        Ok(())
    }

    pub fn open(snapshot: &mut MovieVoteSnapshot) -> Result<()> {
        require_draft(snapshot)?;

        if snapshot.candidates.len() < MIN_CANDIDATES {
            return Err(SyncPartyError::Other(format!(
                "at least {MIN_CANDIDATES} movies are needed to start a vote"
            )));
        }

        snapshot.phase = VotePhase::Open;
        Ok(())
    }

    fn upsert_participant<'a>(
        participants: &'a mut Vec<VoteParticipant>,
        peer: &str,
    ) -> &'a mut VoteParticipant {
        if let Some(index) = participants.iter().position(|p| p.peer == peer) {
            return &mut participants[index];
        }

        participants.push(VoteParticipant {
            peer: peer.to_owned(),
            display_name: peer.to_owned(),
            participation: None,
            selected_movie: None,
            responded_at: None,
        });
        participants.last_mut().expect("just pushed")
    }

    fn require_open(snapshot: &MovieVoteSnapshot) -> Result<()> {
        if snapshot.phase != VotePhase::Open {
            return Err(SyncPartyError::Other("the vote is not open".to_owned()));
        }
        Ok(())
    }

    pub fn cast_vote(
        snapshot: &mut MovieVoteSnapshot,
        peer: &str,
        tmdb_id: i64,
        now: i64,
    ) -> Result<()> {
        require_open(snapshot)?;

        if !snapshot.candidates.iter().any(|c| c.tmdb_id == tmdb_id) {
            return Err(SyncPartyError::Other(
                "that movie is not a candidate".to_owned(),
            ));
        }

        let participant = upsert_participant(&mut snapshot.participants, peer);
        participant.selected_movie = Some(tmdb_id);
        participant.responded_at = Some(now);
        Ok(())
    }

    pub fn set_participation(
        snapshot: &mut MovieVoteSnapshot,
        peer: &str,
        status: Option<ParticipationStatus>,
        now: i64,
    ) -> Result<()> {
        require_open(snapshot)?;

        let participant = upsert_participant(&mut snapshot.participants, peer);
        participant.participation = status;
        participant.responded_at = Some(now);
        Ok(())
    }

    fn tally(candidates: &[MovieCandidate], participants: &[VoteParticipant]) -> VoteResult {
        let mut counts: Vec<CandidateTally> = candidates
            .iter()
            .map(|candidate| CandidateTally {
                tmdb_id: candidate.tmdb_id,
                votes: 0,
            })
            .collect();

        for participant in participants {
            let Some(movie_id) = participant.selected_movie else {
                continue;
            };
            if let Some(entry) = counts.iter_mut().find(|c| c.tmdb_id == movie_id) {
                entry.votes += 1;
            }
        }

        let top = counts.iter().map(|c| c.votes).max().unwrap_or(0);
        let leaders: Vec<i64> = counts
            .iter()
            .filter(|c| top > 0 && c.votes == top)
            .map(|c| c.tmdb_id)
            .collect();

        let (winner, tied) = match leaders.as_slice() {
            [] => (None, Vec::new()),
            [only] => (Some(*only), Vec::new()),
            many => (None, many.to_vec()),
        };

        VoteResult {
            tally: counts,
            winner,
            tied,
        }
    }

    pub fn close(snapshot: &mut MovieVoteSnapshot) -> Result<()> {
        require_open(snapshot)?;
        snapshot.phase = VotePhase::Completed;
        snapshot.result = Some(tally(&snapshot.candidates, &snapshot.participants));
        Ok(())
    }

    /// A no-op past `Open` rather than an error: cancelling a vote that just
    /// completed, or twice in a row, is not something the host needs to be
    /// scolded for.
    pub fn cancel(snapshot: &mut MovieVoteSnapshot) {
        if matches!(snapshot.phase, VotePhase::Draft | VotePhase::Open) {
            snapshot.phase = VotePhase::Cancelled;
        }
    }

    pub fn resolve_tie(snapshot: &mut MovieVoteSnapshot, tmdb_id: i64) -> Result<()> {
        let Some(result) = snapshot.result.as_mut() else {
            return Err(no_active_vote());
        };
        if result.winner.is_some() {
            return Err(SyncPartyError::Other(
                "the tie has already been resolved".to_owned(),
            ));
        }
        if !result.tied.contains(&tmdb_id) {
            return Err(SyncPartyError::Other(
                "that movie was not one of the tied candidates".to_owned(),
            ));
        }

        result.winner = Some(tmdb_id);
        result.tied.clear();
        Ok(())
    }
}

/// What travels over the tunnel's control channel. Private: the frontend
/// never sees this, only [`MovieVoteSnapshot`] via commands and events.
#[derive(Debug, Clone, Serialize, Deserialize)]
enum Wire {
    /// Host -> guest, sent after every change and again whenever a guest's
    /// control channel reconnects, so a full snapshot is always the recovery
    /// path rather than a delta that could be missed.
    Snapshot(Option<MovieVoteSnapshot>),
    /// Guest -> host: something a guest did. The guest's identity is not
    /// carried here — the host already knows it from which connection the
    /// message arrived on.
    Action(Action),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum Action {
    CastVote { tmdb_id: i64 },
    SetParticipation { status: Option<ParticipationStatus> },
}

fn encode(wire: &Wire) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(wire)?)
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

pub struct MovieVote {
    bus: Arc<dyn EventBus>,
    state: Mutex<Option<MovieVoteSnapshot>>,
    /// Set once at startup by [`Self::attach_session`], after both this and
    /// the [`PartySession`] it needs to reach the network exist — see
    /// `AppState::build`. Left unset in tests that only exercise the domain
    /// logic; [`Self::session`] treats that as "nothing to broadcast to" and
    /// carries on rather than requiring a live session.
    session: OnceLock<Arc<PartySession>>,
    /// Set once at startup by [`Self::attach_store`]. Left unset in tests
    /// that only exercise the domain logic; history/watched-movie writes are
    /// simply skipped when there is nowhere to put them.
    store: OnceLock<Arc<MovieStore>>,
    /// Set once at startup by [`Self::attach_notify`]. Left unset in tests;
    /// Discord announcements are simply skipped when there is nowhere to
    /// send them, the same as everything else attached post-construction.
    notify: OnceLock<(Arc<DiscordNotifier>, Arc<ConfigStore>)>,
}

impl MovieVote {
    pub fn new(bus: Arc<dyn EventBus>) -> Self {
        Self {
            bus,
            state: Mutex::new(None),
            session: OnceLock::new(),
            store: OnceLock::new(),
            notify: OnceLock::new(),
        }
    }

    pub fn attach_store(&self, store: Arc<MovieStore>) {
        let _ = self.store.set(store);
    }

    pub fn attach_notify(&self, discord: Arc<DiscordNotifier>, settings: Arc<ConfigStore>) {
        let _ = self.notify.set((discord, settings));
    }

    /// Announces `content` on Discord if a webhook is configured and
    /// enabled. Best-effort, like every other Discord send in this codebase
    /// (see `PartySession::start_hosting_inner`/`stop_hosting`) — a failed
    /// or unconfigured webhook must never affect the vote itself.
    async fn announce(&self, content: String) {
        let Some((discord, settings)) = self.notify.get() else {
            return;
        };
        if !settings.get().discord_enabled {
            return;
        }
        let _ = discord.send(&content).await;
    }

    /// Records a finished (completed or cancelled) vote to session history.
    async fn persist_history(&self, snapshot: &MovieVoteSnapshot) {
        let Some(store) = self.store.get() else {
            return;
        };

        let entry = SessionHistoryEntry {
            id: snapshot.id.clone(),
            started_at: snapshot.created_at,
            ended_at: Some(now_millis()),
            snapshot: snapshot.clone(),
        };

        if let Err(error) = store.save_session_history(&entry) {
            tracing::warn!(%error, "could not save movie night history");
        }
    }

    /// Records that `tmdb_id` was the movie a vote settled on, so the browse
    /// grid can mark it watched later.
    async fn persist_watched(&self, snapshot: &MovieVoteSnapshot, tmdb_id: i64) {
        let Some(store) = self.store.get() else {
            return;
        };

        let title = snapshot
            .candidates
            .iter()
            .find(|candidate| candidate.tmdb_id == tmdb_id)
            .map(|candidate| candidate.title.clone())
            .unwrap_or_default();

        let participants = snapshot
            .participants
            .iter()
            .filter(|participant| participant.selected_movie == Some(tmdb_id))
            .map(|participant| participant.peer.clone())
            .collect();

        let watched = WatchedMovie {
            tmdb_id,
            title,
            session_id: snapshot.id.clone(),
            watched_at: now_millis(),
            participants,
        };

        if let Err(error) = store.record_watched_movie(&watched) {
            tracing::warn!(%error, "could not record a watched movie");
        }
    }

    pub fn attach_session(&self, session: Arc<PartySession>) {
        let _ = self.session.set(session);
    }

    fn session(&self) -> Option<&Arc<PartySession>> {
        self.session.get()
    }

    pub async fn snapshot(&self) -> Option<MovieVoteSnapshot> {
        self.state.lock().await.clone()
    }

    async fn broadcast(&self, snapshot: Option<MovieVoteSnapshot>) {
        self.bus.publish(AppEvent::MovieVoteChanged {
            snapshot: snapshot.clone(),
        });

        let Some(session) = self.session() else {
            return;
        };

        match encode(&Wire::Snapshot(snapshot)) {
            Ok(bytes) => session.broadcast_control(bytes).await,
            Err(error) => tracing::warn!(%error, "could not encode a movie vote snapshot"),
        }
    }

    /// Applies a pure mutation to the current vote and broadcasts the result.
    /// Fails if there is no active vote — every mutator except [`Self::start`]
    /// needs one.
    async fn mutate<F>(&self, apply: F) -> Result<MovieVoteSnapshot>
    where
        F: FnOnce(&mut MovieVoteSnapshot) -> Result<()>,
    {
        let mut guard = self.state.lock().await;
        let Some(snapshot) = guard.as_mut() else {
            return Err(no_active_vote());
        };

        apply(snapshot)?;
        let result = snapshot.clone();
        drop(guard);

        self.broadcast(Some(result.clone())).await;
        Ok(result)
    }

    /// Starts a new draft, replacing whatever vote existed before — there is
    /// only ever one active at a time, and starting one after the last
    /// completed or was cancelled is exactly how the next movie night begins.
    pub async fn start(
        &self,
        is_host: bool,
        schedule: Option<String>,
    ) -> Result<MovieVoteSnapshot> {
        if !is_host {
            return Err(host_only());
        }

        let id = generate_token(12)?;
        let snapshot = logic::draft(id, now_millis(), schedule);
        *self.state.lock().await = Some(snapshot.clone());
        self.broadcast(Some(snapshot.clone())).await;
        Ok(snapshot)
    }

    pub async fn add_candidate(
        &self,
        is_host: bool,
        candidate: MovieCandidate,
    ) -> Result<MovieVoteSnapshot> {
        if !is_host {
            return Err(host_only());
        }
        self.mutate(|snapshot| logic::add_candidate(snapshot, candidate))
            .await
    }

    pub async fn remove_candidate(&self, is_host: bool, tmdb_id: i64) -> Result<MovieVoteSnapshot> {
        if !is_host {
            return Err(host_only());
        }
        self.mutate(|snapshot| logic::remove_candidate(snapshot, tmdb_id))
            .await
    }

    pub async fn open(&self, is_host: bool) -> Result<MovieVoteSnapshot> {
        if !is_host {
            return Err(host_only());
        }
        let snapshot = self.mutate(logic::open).await?;
        self.announce_card(&snapshot).await;
        Ok(snapshot)
    }

    pub async fn close(&self, is_host: bool) -> Result<MovieVoteSnapshot> {
        if !is_host {
            return Err(host_only());
        }

        let snapshot = self.mutate(logic::close).await?;
        self.persist_history(&snapshot).await;

        self.announce_with_language(notify::movie_vote_completed)
            .await;

        if let Some(winner) = snapshot.result.as_ref().and_then(|result| result.winner) {
            self.persist_watched(&snapshot, winner).await;
            self.announce_winner(&snapshot, winner).await;
        }
        Ok(snapshot)
    }

    pub async fn resolve_tie(&self, is_host: bool, tmdb_id: i64) -> Result<MovieVoteSnapshot> {
        if !is_host {
            return Err(host_only());
        }

        let snapshot = self
            .mutate(|snapshot| logic::resolve_tie(snapshot, tmdb_id))
            .await?;
        self.persist_history(&snapshot).await;
        self.persist_watched(&snapshot, tmdb_id).await;
        self.announce_winner(&snapshot, tmdb_id).await;
        Ok(snapshot)
    }

    /// A no-op if there is nothing to cancel; see [`logic::cancel`].
    pub async fn cancel(&self, is_host: bool) -> Result<()> {
        if !is_host {
            return Err(host_only());
        }

        let mut guard = self.state.lock().await;
        let Some(snapshot) = guard.as_mut() else {
            return Ok(());
        };

        let was_active = matches!(snapshot.phase, VotePhase::Draft | VotePhase::Open);
        logic::cancel(snapshot);
        let result = snapshot.clone();
        drop(guard);

        self.broadcast(Some(result.clone())).await;
        self.persist_history(&result).await;

        if was_active {
            self.announce_with_language(notify::movie_vote_cancelled)
                .await;
        }
        Ok(())
    }

    /// Announces the winning title, looked up from the snapshot's own
    /// candidate list — the wire never carries titles separately from a
    /// `MovieCandidate`, so this is the only place one is available.
    async fn announce_winner(&self, snapshot: &MovieVoteSnapshot, tmdb_id: i64) {
        let Some(candidate) = snapshot.candidates.iter().find(|c| c.tmdb_id == tmdb_id) else {
            return;
        };
        let candidate = candidate.clone();
        self.announce_payload(|language| {
            notify::movie_selected_card(&Self::poster_card(&candidate), language)
        })
        .await;
    }

    /// The opened ballot, as a Discord card listing every candidate.
    async fn announce_card(&self, snapshot: &MovieVoteSnapshot) {
        let candidates = snapshot.candidates.clone();
        self.announce_payload(|language| {
            let cards: Vec<_> = candidates.iter().map(Self::poster_card).collect();
            notify::movie_vote_started_card(&cards, language)
        })
        .await;
    }

    async fn announce_payload(&self, build: impl FnOnce(&str) -> serde_json::Value) {
        let Some((discord, settings)) = self.notify.get() else {
            return;
        };
        let settings = settings.get();
        if !settings.discord_enabled {
            return;
        }
        let _ = discord.send_payload(&build(&settings.language)).await;
    }

    /// A candidate as the notifier wants it. Borrowing rather than copying
    /// the strings — the card is built and posted in one go.
    fn poster_card(candidate: &MovieCandidate) -> notify::PosterCard<'_> {
        notify::PosterCard {
            title: &candidate.title,
            poster: candidate.poster.as_deref(),
            release_date: candidate.release_date.as_deref(),
            rating: candidate.rating,
        }
    }

    async fn announce_with_language(&self, build: impl FnOnce(&str) -> String) {
        let Some((_, settings)) = self.notify.get() else {
            return;
        };
        let language = settings.get().language;
        self.announce(build(&language)).await;
    }

    /// Casts `peer`'s vote. On the host this applies and rebroadcasts
    /// directly; on a guest it forwards the action to the host instead, and
    /// the guest's own view only updates once the host's snapshot comes back.
    pub async fn cast_vote(&self, is_host: bool, peer: &str, tmdb_id: i64) -> Result<()> {
        if is_host {
            self.mutate(|snapshot| logic::cast_vote(snapshot, peer, tmdb_id, now_millis()))
                .await?;
            return Ok(());
        }

        self.send_action(Action::CastVote { tmdb_id }).await
    }

    pub async fn set_participation(
        &self,
        is_host: bool,
        peer: &str,
        status: Option<ParticipationStatus>,
    ) -> Result<()> {
        if is_host {
            self.mutate(|snapshot| logic::set_participation(snapshot, peer, status, now_millis()))
                .await?;
            return Ok(());
        }

        self.send_action(Action::SetParticipation { status }).await
    }

    async fn send_action(&self, action: Action) -> Result<()> {
        let bytes = encode(&Wire::Action(action))?;
        match self.session() {
            Some(session) => session.send_control(bytes).await,
            None => Err(SyncPartyError::NotInParty),
        }
    }
}

impl ControlChannel for MovieVote {
    /// Runs on both sides of a party, doing whichever half applies: a host's
    /// connection only ever delivers an `Action` (from a guest), and a
    /// guest's connection only ever delivers a `Snapshot` (from the host) —
    /// each side only ever sends the message meant for the other.
    fn on_message(self: Arc<Self>, peer: iroh::EndpointId, bytes: Vec<u8>) {
        tokio::spawn(async move {
            let Ok(wire) = serde_json::from_slice::<Wire>(&bytes) else {
                tracing::debug!("dropped an unreadable movie vote message");
                return;
            };

            match wire {
                Wire::Action(Action::CastVote { tmdb_id }) => {
                    let peer = peer.to_string();
                    if let Err(error) = self
                        .mutate(|snapshot| logic::cast_vote(snapshot, &peer, tmdb_id, now_millis()))
                        .await
                    {
                        tracing::debug!(%error, "a guest's vote could not be applied");
                    }
                }
                Wire::Action(Action::SetParticipation { status }) => {
                    let peer = peer.to_string();
                    if let Err(error) = self
                        .mutate(|snapshot| {
                            logic::set_participation(snapshot, &peer, status, now_millis())
                        })
                        .await
                    {
                        tracing::debug!(%error, "a guest's participation update could not be applied");
                    }
                }
                Wire::Snapshot(snapshot) => {
                    *self.state.lock().await = snapshot.clone();
                    self.bus.publish(AppEvent::MovieVoteChanged { snapshot });
                }
            }
        });
    }

    /// A freshly connected guest's control channel has nothing on it yet —
    /// push the current vote down immediately rather than waiting for the
    /// guest to ask, so a reconnect mid-vote hydrates without a round trip.
    fn on_connected(self: Arc<Self>, peer: iroh::EndpointId) {
        tokio::spawn(async move {
            let Some(session) = self.session() else {
                return;
            };

            let snapshot = self.state.lock().await.clone();
            match encode(&Wire::Snapshot(snapshot)) {
                Ok(bytes) => session.send_control_to(peer, bytes).await,
                Err(error) => tracing::warn!(%error, "could not encode a movie vote snapshot"),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::logic;
    use super::*;

    fn candidate(tmdb_id: i64) -> MovieCandidate {
        MovieCandidate {
            tmdb_id,
            title: format!("Movie {tmdb_id}"),
            poster: None,
            release_date: None,
            overview: None,
            genres: Vec::new(),
            rating: 0.0,
        }
    }

    fn draft_with(candidates: usize) -> MovieVoteSnapshot {
        let mut snapshot = logic::draft("vote-1".to_owned(), 0, None);
        for id in 0..candidates as i64 {
            logic::add_candidate(&mut snapshot, candidate(id)).expect("add");
        }
        snapshot
    }

    #[test]
    fn a_vote_needs_at_least_two_candidates_to_open() {
        let mut snapshot = draft_with(1);
        assert!(logic::open(&mut snapshot).is_err());
        assert_eq!(snapshot.phase, VotePhase::Draft);
    }

    #[test]
    fn a_vote_cannot_hold_more_than_ten_candidates() {
        let mut snapshot = draft_with(MAX_CANDIDATES);
        let error = logic::add_candidate(&mut snapshot, candidate(999)).unwrap_err();
        assert!(error.to_string().contains("10"));
        assert_eq!(snapshot.candidates.len(), MAX_CANDIDATES);
    }

    #[test]
    fn the_same_movie_cannot_be_added_twice() {
        let mut snapshot = draft_with(2);
        assert!(logic::add_candidate(&mut snapshot, candidate(0)).is_err());
        assert_eq!(snapshot.candidates.len(), 2);
    }

    #[test]
    fn the_candidate_list_locks_once_the_vote_is_open() {
        let mut snapshot = draft_with(2);
        logic::open(&mut snapshot).expect("open");

        assert!(logic::add_candidate(&mut snapshot, candidate(5)).is_err());
        assert!(logic::remove_candidate(&mut snapshot, 0).is_err());
    }

    #[test]
    fn votes_are_only_accepted_while_open() {
        let mut snapshot = draft_with(2);
        assert!(logic::cast_vote(&mut snapshot, "guest-1", 0, 0).is_err());

        logic::open(&mut snapshot).expect("open");
        assert!(logic::cast_vote(&mut snapshot, "guest-1", 0, 0).is_ok());

        logic::close(&mut snapshot).expect("close");
        assert!(logic::cast_vote(&mut snapshot, "guest-2", 0, 0).is_err());
    }

    #[test]
    fn a_vote_for_a_movie_that_is_not_a_candidate_is_rejected() {
        let mut snapshot = draft_with(2);
        logic::open(&mut snapshot).expect("open");

        assert!(logic::cast_vote(&mut snapshot, "guest-1", 999, 0).is_err());
    }

    #[test]
    fn recasting_a_vote_replaces_the_previous_one() {
        let mut snapshot = draft_with(2);
        logic::open(&mut snapshot).expect("open");

        logic::cast_vote(&mut snapshot, "guest-1", 0, 0).expect("first vote");
        logic::cast_vote(&mut snapshot, "guest-1", 1, 1).expect("changed vote");
        logic::close(&mut snapshot).expect("close");

        let result = snapshot.result.expect("result");
        assert_eq!(result.winner, Some(1));
    }

    #[test]
    fn the_candidate_with_the_most_votes_wins() {
        let mut snapshot = draft_with(3);
        logic::open(&mut snapshot).expect("open");

        logic::cast_vote(&mut snapshot, "a", 0, 0).expect("vote a");
        logic::cast_vote(&mut snapshot, "b", 0, 0).expect("vote b");
        logic::cast_vote(&mut snapshot, "c", 1, 0).expect("vote c");
        logic::close(&mut snapshot).expect("close");

        let result = snapshot.result.expect("result");
        assert_eq!(result.winner, Some(0));
        assert!(result.tied.is_empty());
    }

    #[test]
    fn a_tie_leaves_the_winner_unresolved_until_the_host_breaks_it() {
        let mut snapshot = draft_with(2);
        logic::open(&mut snapshot).expect("open");

        logic::cast_vote(&mut snapshot, "a", 0, 0).expect("vote a");
        logic::cast_vote(&mut snapshot, "b", 1, 0).expect("vote b");
        logic::close(&mut snapshot).expect("close");

        {
            let result = snapshot.result.as_ref().expect("result");
            assert_eq!(result.winner, None);
            assert_eq!(result.tied.len(), 2);
        }

        logic::resolve_tie(&mut snapshot, 1).expect("resolve");
        let result = snapshot.result.expect("result");
        assert_eq!(result.winner, Some(1));
        assert!(result.tied.is_empty());
    }

    #[test]
    fn resolving_a_tie_in_favour_of_a_movie_that_was_not_tied_is_rejected() {
        let mut snapshot = draft_with(3);
        logic::open(&mut snapshot).expect("open");

        logic::cast_vote(&mut snapshot, "a", 0, 0).expect("vote a");
        logic::cast_vote(&mut snapshot, "b", 1, 0).expect("vote b");
        logic::close(&mut snapshot).expect("close");

        assert!(logic::resolve_tie(&mut snapshot, 2).is_err());
    }

    #[test]
    fn a_vote_nobody_voted_in_has_no_winner_and_no_tie() {
        let mut snapshot = draft_with(2);
        logic::open(&mut snapshot).expect("open");
        logic::close(&mut snapshot).expect("close");

        let result = snapshot.result.expect("result");
        assert_eq!(result.winner, None);
        assert!(result.tied.is_empty());
    }

    #[test]
    fn cancelling_a_completed_vote_does_nothing() {
        let mut snapshot = draft_with(2);
        logic::open(&mut snapshot).expect("open");
        logic::close(&mut snapshot).expect("close");

        logic::cancel(&mut snapshot);
        assert_eq!(snapshot.phase, VotePhase::Completed);
    }

    #[tokio::test]
    async fn only_the_host_can_start_a_vote() {
        let movie_vote = MovieVote::new(Arc::new(crate::core::events::NullEventBus));
        assert!(movie_vote.start(false, None).await.is_err());
        assert!(movie_vote.snapshot().await.is_none());
    }

    #[tokio::test]
    async fn only_the_host_can_cancel_a_vote() {
        let movie_vote = MovieVote::new(Arc::new(crate::core::events::NullEventBus));
        movie_vote.start(true, None).await.expect("start");

        assert!(movie_vote.cancel(false).await.is_err());
        let snapshot = movie_vote.snapshot().await.expect("snapshot");
        assert_eq!(snapshot.phase, VotePhase::Draft);
    }

    #[tokio::test]
    async fn starting_a_vote_publishes_an_event() {
        let bus = Arc::new(crate::core::events::test_support::RecordingEventBus::default());
        let movie_vote = MovieVote::new(Arc::clone(&bus) as Arc<dyn EventBus>);

        movie_vote.start(true, None).await.expect("start");

        assert!(bus
            .events()
            .iter()
            .any(|event| matches!(event, AppEvent::MovieVoteChanged { snapshot: Some(_) })));
    }

    #[tokio::test]
    async fn a_guest_casting_a_vote_with_no_session_attached_is_an_error_not_a_panic() {
        let movie_vote = MovieVote::new(Arc::new(crate::core::events::NullEventBus));
        assert!(movie_vote.cast_vote(false, "guest-1", 0).await.is_err());
    }
}
