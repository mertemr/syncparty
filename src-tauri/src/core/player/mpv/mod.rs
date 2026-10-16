//! mpv, driven over its JSON IPC.
//!
//! Contract only: the implementation lands with the mpv work package. Every
//! method here returns an error until then, so nothing can mistake the stub
//! for a player that works.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::core::config::ConfigStore;
use crate::core::error::{Result, SyncPartyError};
use crate::core::player::{
    find_mpv, LaunchMode, Player, PlayerEvents, PlayerLauncher, PlayerSnapshot, MPV_KEY,
};

/// Starts mpv processes, each with its own private IPC endpoint.
#[derive(Debug, Clone)]
pub struct MpvLauncher {
    executable: PathBuf,
    mode: LaunchMode,
}

impl MpvLauncher {
    pub fn new(executable: PathBuf, mode: LaunchMode) -> Self {
        Self { executable, mode }
    }

    /// Resolves mpv the way the dependency check does, honouring a path the
    /// user chose, so a player reported as ready is one this can launch.
    pub fn discover(settings: &ConfigStore, mode: LaunchMode) -> Result<Self> {
        let manual = settings.executable_override(MPV_KEY);

        find_mpv(manual.as_deref())
            .map(|executable| Self::new(executable, mode))
            .ok_or_else(|| SyncPartyError::DependencyMissing("mpv".to_owned()))
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn mode(&self) -> LaunchMode {
        self.mode
    }

    /// Starts mpv and returns the concrete player, for callers — the
    /// end-to-end test — that need to reach its IPC endpoint themselves.
    pub async fn spawn(&self) -> Result<(Arc<MpvPlayer>, PlayerEvents)> {
        Err(not_yet())
    }
}

#[async_trait]
impl PlayerLauncher for MpvLauncher {
    async fn launch(&self) -> Result<(Arc<dyn Player>, PlayerEvents)> {
        let (player, events) = self.spawn().await?;
        Ok((player as Arc<dyn Player>, events))
    }
}

/// One running mpv.
#[derive(Debug)]
pub struct MpvPlayer {
    endpoint: String,
}

impl MpvPlayer {
    /// The socket path or pipe name this player listens on. Anything that
    /// connects to it can drive mpv the way a user at the keyboard would,
    /// which is exactly what the end-to-end test needs and why it is private
    /// to this user everywhere else.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

#[async_trait]
impl Player for MpvPlayer {
    fn snapshot(&self) -> PlayerSnapshot {
        PlayerSnapshot::default()
    }

    async fn set_paused(&self, _paused: bool) -> Result<()> {
        Err(not_yet())
    }

    async fn seek(&self, _position: f64) -> Result<()> {
        Err(not_yet())
    }

    async fn set_speed(&self, _speed: f64) -> Result<()> {
        Err(not_yet())
    }

    async fn load(&self, _path: &Path) -> Result<()> {
        Err(not_yet())
    }

    async fn show_text(&self, _text: &str, _duration: Duration) -> Result<()> {
        Err(not_yet())
    }

    async fn quit(&self) -> Result<()> {
        Err(not_yet())
    }
}

fn not_yet() -> SyncPartyError {
    SyncPartyError::PlayerFailed("the mpv driver is not implemented yet".to_owned())
}
