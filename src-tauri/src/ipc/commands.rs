//! The command surface the frontend calls.
//!
//! Each one is a one-liner over `core`. If a handler grows a branch, that
//! branch belongs in `core` where it can be tested.

use serde::Deserialize;
use tauri::State;
use ts_rs::TS;

use crate::core::config::{AppMode, AppSettings};
use crate::core::deps::{DependencyId, PreflightReport};
use crate::core::diagnostics::{self, DiagnosticsReport};
use crate::core::error::{Result, SyncPartyError};
use crate::core::invite::Invite;
use crate::core::notify;
use crate::core::session::{HostingInfo, SessionState};
use crate::ipc::AppState;

/// A partial settings update. Absent fields are left alone, so the UI can send
/// just the toggle the user touched.
///
/// `#[ts(optional)]` matters here: without it every field generates as
/// `T | null`, which would force the frontend to spell out all seven on every
/// call and defeat the point of a patch.
#[derive(Debug, Default, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase", default)]
pub struct SettingsPatch {
    #[ts(optional)]
    pub mode: Option<AppMode>,
    #[ts(optional)]
    pub port: Option<u16>,
    #[ts(optional)]
    pub room: Option<String>,
    #[ts(optional)]
    pub nickname: Option<String>,
    #[ts(optional)]
    pub language: Option<String>,
    #[ts(optional)]
    pub monitor_enabled: Option<bool>,
    #[ts(optional)]
    pub discord_enabled: Option<bool>,
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> AppSettings {
    state.settings.get()
}

#[tauri::command]
pub fn update_settings(state: State<'_, AppState>, patch: SettingsPatch) -> Result<AppSettings> {
    state.settings.update(|settings| {
        if let Some(mode) = patch.mode {
            settings.mode = Some(mode);
        }
        if let Some(port) = patch.port {
            settings.port = port;
        }
        if let Some(room) = patch.room {
            settings.room = room;
        }
        if let Some(nickname) = patch.nickname {
            settings.nickname = nickname;
        }
        if let Some(language) = patch.language {
            settings.language = language;
        }
        if let Some(enabled) = patch.monitor_enabled {
            settings.monitor_enabled = enabled;
        }
        if let Some(enabled) = patch.discord_enabled {
            settings.discord_enabled = enabled;
        }
    })
}

#[tauri::command]
pub async fn run_preflight(state: State<'_, AppState>, mode: AppMode) -> Result<PreflightReport> {
    Ok(state.dependencies.preflight(mode).await)
}

#[tauri::command]
pub async fn run_diagnostics(state: State<'_, AppState>) -> Result<DiagnosticsReport> {
    let mode = state.settings.get().mode.unwrap_or(AppMode::Host);
    Ok(diagnostics::collect(&state.dependencies, &state.session, &state.secrets, mode).await)
}

/// Installs one dependency. Progress arrives as events while this runs.
#[tauri::command]
pub async fn install_dependency(state: State<'_, AppState>, id: DependencyId) -> Result<()> {
    state.dependencies.install(id, state.bus.as_ref()).await
}

/// Points a dependency at a program the user chose, for portable builds that
/// automatic detection cannot see. `path` may be the executable or the folder
/// holding it; passing `null` clears the choice.
#[tauri::command]
pub async fn set_dependency_path(
    state: State<'_, AppState>,
    id: DependencyId,
    path: Option<String>,
) -> Result<()> {
    state.dependencies.set_manual_path(id, path).await
}

#[tauri::command]
pub async fn start_hosting(state: State<'_, AppState>) -> Result<HostingInfo> {
    state.session.start_hosting().await
}

#[tauri::command]
pub async fn stop_hosting(state: State<'_, AppState>) -> Result<()> {
    state.session.stop_hosting().await
}

#[tauri::command]
pub async fn session_state(state: State<'_, AppState>) -> Result<SessionState> {
    Ok(state.session.state().await)
}

/// Parses whatever the guest pasted — a bare code, a link, or a whole message.
#[tauri::command]
pub fn decode_invite(text: String) -> Result<Invite> {
    Invite::decode(&text)
}

#[tauri::command]
pub async fn join_party(state: State<'_, AppState>, invite: Invite) -> Result<()> {
    state.session.join(&invite).await
}

/// Closes the tunnel a guest is connected through.
///
/// Needed now that syncparty carries the connection rather than standing
/// beside it: without this the only way to leave a party would be to quit the
/// app, and the tunnel would stay open for as long as the window did.
#[tauri::command]
pub async fn leave_party(state: State<'_, AppState>) -> Result<()> {
    state.session.leave().await
}

/// Opens the host's own Syncplay client on the party they are running.
///
/// Separate from `join_party` because the host has to connect on the address
/// the server is bound to, not the one handed out to guests.
#[tauri::command]
pub async fn join_hosted_party(state: State<'_, AppState>) -> Result<()> {
    state.session.join_as_host().await
}

#[tauri::command]
pub async fn resume_last_session(state: State<'_, AppState>) -> Result<Option<Invite>> {
    state.session.resume_last_session().await
}

#[tauri::command]
pub fn clear_last_session(state: State<'_, AppState>) -> Result<()> {
    state.session.clear_last_session()
}

#[tauri::command]
pub fn discord_status(state: State<'_, AppState>) -> bool {
    state.discord.is_configured()
}

#[tauri::command]
pub fn set_discord_webhook(state: State<'_, AppState>, url: String) -> Result<()> {
    state.discord.set_webhook(&url)
}

#[tauri::command]
pub fn clear_discord_webhook(state: State<'_, AppState>) -> Result<()> {
    state.discord.clear_webhook()
}

/// Posts a test message so the user can confirm the webhook lands in the right
/// channel before relying on it mid-party.
#[tauri::command]
pub async fn test_discord_webhook(state: State<'_, AppState>) -> Result<()> {
    let language = state.settings.get().language;

    if state.discord.send(&notify::webhook_test(&language)).await? {
        Ok(())
    } else {
        Err(SyncPartyError::Config(
            "no Discord webhook has been set".to_owned(),
        ))
    }
}
