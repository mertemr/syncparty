//! The one-way channel from `core` to the UI.
//!
//! `core` must not depend on Tauri — that is what keeps it testable without a
//! webview. So it publishes [`AppEvent`]s through the [`EventBus`] trait, and
//! the Tauri bridge in [`crate::ipc`] is the only place that knows how an
//! event actually reaches a window.

use serde::Serialize;
use ts_rs::TS;

use crate::core::deps::{DependencyId, PreflightReport};
use crate::core::movie_vote::MovieVoteSnapshot;
use crate::core::session::SessionState;
use crate::core::syncplay::RoomSnapshot;

/// Everything the frontend can be told about. Tagged so TypeScript gets a
/// discriminated union it can exhaustively match on.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AppEvent {
    /// A preflight run finished; carries the full report, not a delta.
    PreflightCompleted { report: PreflightReport },

    /// Progress while installing one dependency.
    InstallProgress {
        dependency: DependencyId,
        stage: String,
        /// Absent when the underlying installer reports no percentage.
        percent: Option<u8>,
        detail: Option<String>,
    },

    /// The session state machine advanced.
    SessionChanged { state: SessionState },

    /// Live room contents, pushed by the monitor rather than polled.
    RoomUpdated { snapshot: RoomSnapshot },

    /// A line the Syncplay server wrote to stdout or stderr.
    ServerLog { line: String, is_error: bool },

    /// The app was opened through a `syncparty://` link. The UI switches to
    /// the guest screen with these details already filled in.
    InviteReceived { invite: crate::core::invite::Invite },

    /// Something failed outside a command call, so there was no `Result` to
    /// return it on. The field is `errorKind` rather than `kind` because
    /// `kind` is already the union's discriminant.
    Failed { error_kind: String, message: String },

    /// The movie vote changed — created, opened, a candidate/participant/vote
    /// changed, or it closed. Always the whole snapshot, same as
    /// `RoomUpdated`. `None` once there is no active vote (cancelled, or
    /// never started).
    MovieVoteChanged { snapshot: Option<MovieVoteSnapshot> },
}

/// Publishes events to whoever is listening. Implemented by the Tauri bridge
/// in production and by a recording double in tests.
pub trait EventBus: Send + Sync + 'static {
    fn publish(&self, event: AppEvent);
}

/// Drops every event. Useful for tests and for code paths that run before a
/// window exists.
pub struct NullEventBus;

impl EventBus for NullEventBus {
    fn publish(&self, _event: AppEvent) {}
}

/// Receives progress from a long-running install.
///
/// Narrower than [`EventBus`] on purpose: an installer should be able to
/// report progress without being handed the ability to emit arbitrary
/// application events.
pub trait ProgressSink: Send + Sync {
    fn report(&self, stage: &str, percent: Option<u8>, detail: Option<String>);
}

/// Discards progress, for callers that only care whether the install
/// succeeded.
pub struct NullProgressSink;

impl ProgressSink for NullProgressSink {
    fn report(&self, _stage: &str, _percent: Option<u8>, _detail: Option<String>) {}
}

/// Forwards installer progress onto the event bus as [`AppEvent::InstallProgress`].
pub struct DependencyProgress<'bus> {
    bus: &'bus dyn EventBus,
    dependency: DependencyId,
}

impl<'bus> DependencyProgress<'bus> {
    pub fn new(bus: &'bus dyn EventBus, dependency: DependencyId) -> Self {
        Self { bus, dependency }
    }
}

impl ProgressSink for DependencyProgress<'_> {
    fn report(&self, stage: &str, percent: Option<u8>, detail: Option<String>) {
        self.bus.publish(AppEvent::InstallProgress {
            dependency: self.dependency,
            stage: stage.to_owned(),
            percent,
            detail,
        });
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Mutex;

    use super::{AppEvent, EventBus};

    /// Captures published events so tests can assert on them.
    #[derive(Default)]
    pub struct RecordingEventBus {
        events: Mutex<Vec<AppEvent>>,
    }

    impl RecordingEventBus {
        pub fn events(&self) -> Vec<AppEvent> {
            self.events.lock().expect("event mutex poisoned").clone()
        }
    }

    impl EventBus for RecordingEventBus {
        fn publish(&self, event: AppEvent) {
            self.events
                .lock()
                .expect("event mutex poisoned")
                .push(event);
        }
    }
}
