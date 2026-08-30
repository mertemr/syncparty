//! The Tauri boundary.
//!
//! Two jobs, both thin: expose `core` operations as commands, and carry
//! [`AppEvent`]s out to the window. Any logic that shows up here belongs in
//! `core` instead — that is the line that keeps `core` testable.

pub mod commands;
pub mod movie_commands;

use std::sync::Arc;

use tauri::{AppHandle, Emitter};

use crate::core::config::{ConfigStore, SecretStore};
use crate::core::deps::DependencyManager;
use crate::core::error::Result;
use crate::core::events::{AppEvent, EventBus};
use crate::core::movie::{MovieStore, TmdbClient};
use crate::core::movie_vote::MovieVote;
use crate::core::net::ControlChannel;
use crate::core::notify::DiscordNotifier;
use crate::core::paths::AppPaths;
use crate::core::session::PartySession;
use crate::core::syncplay::UvManagedServer;

/// The single Tauri event name everything travels on.
///
/// One channel rather than one per variant: the payload is already a tagged
/// union, so the frontend narrows on `kind` and gets exhaustiveness checking
/// from TypeScript for free.
pub const EVENT_CHANNEL: &str = "syncparty://event";

/// Publishes [`AppEvent`]s to every window.
pub struct TauriEventBus {
    app: AppHandle,
}

impl TauriEventBus {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl EventBus for TauriEventBus {
    fn publish(&self, event: AppEvent) {
        // A failed emit means the window is gone, which is not worth
        // propagating up through business logic.
        if let Err(error) = self.app.emit(EVENT_CHANNEL, &event) {
            tracing::debug!(%error, "could not deliver an event to the UI");
        }
    }
}

/// Everything the commands need, assembled once at startup.
pub struct AppState {
    pub settings: Arc<ConfigStore>,
    pub secrets: Arc<SecretStore>,
    pub dependencies: DependencyManager,
    pub session: Arc<PartySession>,
    pub movie_vote: Arc<MovieVote>,
    pub tmdb: Arc<TmdbClient>,
    pub movie_store: Arc<MovieStore>,
    pub discord: Arc<DiscordNotifier>,
    pub bus: Arc<dyn EventBus>,
}

impl AppState {
    /// Wires the object graph. The only place that decides which concrete
    /// implementation backs each `core` trait.
    pub fn build(app: &AppHandle) -> Result<Self> {
        let paths = AppPaths::resolve()?;
        let bus: Arc<dyn EventBus> = Arc::new(TauriEventBus::new(app.clone()));

        let settings = Arc::new(ConfigStore::load(paths.clone())?);
        let secrets = Arc::new(SecretStore::new());
        let discord = Arc::new(DiscordNotifier::new(Arc::clone(&secrets)));

        let server = Arc::new(UvManagedServer::new(paths.clone(), Arc::clone(&bus)));

        // `movie_vote` is built first so it can be handed to `PartySession`
        // as its control channel; `attach_session` afterwards closes the
        // loop so `movie_vote` can in turn reach the tunnel to broadcast.
        // Neither needs the other at construction, so there is no cycle.
        let movie_vote = Arc::new(MovieVote::new(Arc::clone(&bus)));

        let session = Arc::new(PartySession::new(
            Arc::clone(&settings),
            Arc::clone(&secrets),
            server,
            Arc::clone(&discord),
            Arc::clone(&bus),
            Arc::clone(&movie_vote) as Arc<dyn ControlChannel>,
        ));
        movie_vote.attach_session(Arc::clone(&session));

        let tmdb = Arc::new(TmdbClient::new(Arc::clone(&secrets)));
        let movie_store = Arc::new(MovieStore::open(&paths.movies_database())?);
        movie_vote.attach_store(Arc::clone(&movie_store));
        session.attach_store(Arc::clone(&movie_store));
        movie_vote.attach_notify(Arc::clone(&discord), Arc::clone(&settings));

        Ok(Self {
            dependencies: DependencyManager::standard(paths, Arc::clone(&settings)),
            settings,
            secrets,
            session,
            movie_vote,
            tmdb,
            movie_store,
            discord,
            bus,
        })
    }
}
