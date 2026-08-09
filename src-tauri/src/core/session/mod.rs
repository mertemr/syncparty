//! The orchestrator the UI talks to.
//!
//! Tailscale, the server process, the room monitor and Discord are each
//! useful on their own; this is the piece that knows the order they go in and
//! how to unwind when a step fails partway through.

use std::sync::Arc;

use serde::Serialize;
use tokio::sync::Mutex;
use ts_rs::TS;

use crate::core::config::{ConfigStore, SecretKey, SecretStore};
use crate::core::error::{Result, SyncPartyError};
use crate::core::events::{AppEvent, EventBus};
use crate::core::invite::Invite;
use crate::core::notify::{self, DiscordNotifier};
use crate::core::syncplay::{
    ClientLauncher, MonitorConfig, RoomMonitor, ServerConfig, ServerController, ServerState,
};
use crate::core::tailscale::{AuthFlow, CliTailscaleClient, TailscaleClient};

/// Length of the generated server password. Long enough that guessing is
/// hopeless, short enough to read aloud if the link ever fails.
const PASSWORD_LENGTH: usize = 18;
const SALT_LENGTH: usize = 10;

/// Where the host is in the start-up sequence.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub enum SessionState {
    Idle,
    /// Carries a translation key rather than prose, so the UI picks the
    /// wording and this module stays out of the i18n business.
    Starting {
        step: StartupStep,
    },
    Hosting(Box<HostingInfo>),
    Failed {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub enum StartupStep {
    ConnectingTailscale,
    StartingServer,
    AttachingMonitor,
}

/// Everything the host screen needs once a party is live.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct HostingInfo {
    pub invite: Invite,
    pub invite_code: String,
    pub deep_link: String,
    /// This machine's own tailnet address — the one the server is bound to.
    ///
    /// Not decoration: it is the address the host's own Syncplay has to use.
    /// `invite.host` may be a masqueraded address that only resolves inside
    /// the tailnet this node was shared into, which does not include here.
    pub tailscale_address: String,
    pub server: ServerState,
    pub monitor_attached: bool,
}

pub struct PartySession {
    settings: Arc<ConfigStore>,
    secrets: Arc<SecretStore>,
    server: Arc<dyn ServerController>,
    discord: Arc<DiscordNotifier>,
    bus: Arc<dyn EventBus>,
    state: Mutex<SessionState>,
    monitor: Mutex<Option<RoomMonitor>>,
}

impl PartySession {
    pub fn new(
        settings: Arc<ConfigStore>,
        secrets: Arc<SecretStore>,
        server: Arc<dyn ServerController>,
        discord: Arc<DiscordNotifier>,
        bus: Arc<dyn EventBus>,
    ) -> Self {
        Self {
            settings,
            secrets,
            server,
            discord,
            bus,
            state: Mutex::new(SessionState::Idle),
            monitor: Mutex::new(None),
        }
    }

    pub async fn state(&self) -> SessionState {
        self.state.lock().await.clone()
    }

    async fn transition(&self, state: SessionState) {
        *self.state.lock().await = state.clone();
        self.bus.publish(AppEvent::SessionChanged { state });
    }

    /// Brings a party up end to end.
    ///
    /// On failure the server is stopped again rather than left running in the
    /// background where the next attempt would collide with it.
    pub async fn start_hosting(&self) -> Result<HostingInfo> {
        match self.start_hosting_inner().await {
            Ok(info) => Ok(info),
            Err(error) => {
                let _ = self.stop_hosting().await;
                self.transition(SessionState::Failed {
                    message: error.to_string(),
                })
                .await;
                Err(error)
            }
        }
    }

    async fn start_hosting_inner(&self) -> Result<HostingInfo> {
        let settings = self.settings.get();

        self.transition(SessionState::Starting {
            step: StartupStep::ConnectingTailscale,
        })
        .await;

        let tailscale = CliTailscaleClient::discover()?;
        let address = match tailscale.bring_up().await? {
            AuthFlow::Ready(address) => address,
            AuthFlow::NeedsLogin { auth_url } => {
                // Published once. The UI opens it; nothing here does, which is
                // what stops the browser-tab storm the prototype produced.
                self.bus.publish(AppEvent::TailscaleLoginRequired {
                    auth_url: auth_url.clone(),
                });
                return Err(SyncPartyError::TailscaleLoginRequired { auth_url });
            }
        };

        // Generated once and reused forever. The salt in particular must not
        // change: Syncplay derives room operator passwords from it, and a new
        // salt silently invalidates every one of them.
        let password = self
            .secrets
            .get_or_create(SecretKey::ServerPassword, PASSWORD_LENGTH)?;
        let salt = self
            .secrets
            .get_or_create(SecretKey::ServerSalt, SALT_LENGTH)?;

        self.transition(SessionState::Starting {
            step: StartupStep::StartingServer,
        })
        .await;

        self.server
            .start(&ServerConfig {
                port: settings.port,
                password: password.clone(),
                salt,
                bind_address: address,
            })
            .await?;

        // Guests may reach this node on a masqueraded address or its MagicDNS
        // name rather than the raw IP it binds to.
        let advertised = tailscale
            .shareable_address()
            .await?
            .unwrap_or_else(|| address.to_string());

        // Every other address that reaches this server goes along for the
        // ride, because which one works depends on which tailnet the guest is
        // on — something the host has no way to know. `candidates` drops the
        // duplicates this produces when a host has only one address.
        let mut alternates = vec![address.to_string()];
        if let Some(dns_name) = tailscale.status().await.ok().and_then(|s| s.dns_name) {
            alternates.push(dns_name);
        }

        let invite = Invite {
            host: advertised,
            alternate_hosts: alternates,
            port: settings.port,
            password: password.clone(),
            room: settings.room.clone(),
        };

        let monitor_attached = if settings.monitor_enabled {
            self.transition(SessionState::Starting {
                step: StartupStep::AttachingMonitor,
            })
            .await;

            self.attach_monitor(
                &address.to_string(),
                settings.port,
                &password,
                &settings.room,
            )
            .await;
            true
        } else {
            false
        };

        let info = HostingInfo {
            invite_code: invite.encode(),
            deep_link: invite.deep_link(),
            tailscale_address: address.to_string(),
            server: self.server.state().await,
            monitor_attached,
            invite,
        };

        self.transition(SessionState::Hosting(Box::new(info.clone())))
            .await;

        if settings.discord_enabled {
            // A failed announcement must not take the party down with it.
            if let Err(error) = self
                .discord
                .send(&notify::party_ready(&info.invite, &settings.language))
                .await
            {
                self.bus.publish(AppEvent::Failed {
                    error_kind: error.kind().to_owned(),
                    message: error.to_string(),
                });
            }
        }

        Ok(info)
    }

    /// Attaches the monitor and republishes its snapshots as events.
    ///
    /// Connects over the loopback-facing tailnet address rather than the
    /// advertised one, since the server binds to exactly that.
    async fn attach_monitor(&self, host: &str, port: u16, password: &str, room: &str) {
        let monitor = RoomMonitor::start(MonitorConfig {
            host: host.to_owned(),
            port,
            password: password.to_owned(),
            room: room.to_owned(),
        });

        let mut updates = monitor.subscribe();
        let bus = Arc::clone(&self.bus);

        tokio::spawn(async move {
            // Ends on its own when the monitor is dropped and the sender goes
            // away, so there is nothing extra to cancel.
            while updates.changed().await.is_ok() {
                let snapshot = updates.borrow_and_update().clone();
                bus.publish(AppEvent::RoomUpdated { snapshot });
            }
        });

        *self.monitor.lock().await = Some(monitor);
    }

    /// Stops the party. Tailscale is left alone.
    pub async fn stop_hosting(&self) -> Result<()> {
        let settings = self.settings.get();

        if settings.discord_enabled && matches!(self.state().await, SessionState::Hosting(_)) {
            let _ = self
                .discord
                .send(&notify::party_stopped(&settings.language))
                .await;
        }

        // Dropping the monitor aborts its task and closes the connection.
        self.monitor.lock().await.take();

        self.server.stop().await?;
        self.transition(SessionState::Idle).await;
        Ok(())
    }

    /// Opens the Syncplay client on an invite. Used by the guest half.
    pub async fn join(&self, invite: &Invite) -> Result<()> {
        let nickname = self.settings.get().nickname;
        ClientLauncher::discover(&self.settings)?
            .join(invite, &nickname)
            .await?;
        self.secrets
            .set(SecretKey::LastInvite, &encode_last_invite(invite)?)
    }

    /// Reopens the last successfully launched guest session after an app restart.
    pub async fn resume_last_session(&self) -> Result<Option<Invite>> {
        let Some(raw) = self.secrets.get(SecretKey::LastInvite)? else {
            return Ok(None);
        };

        let invite = match parse_last_invite(&raw) {
            Some(invite) => invite,
            None => {
                self.secrets.delete(SecretKey::LastInvite)?;
                return Ok(None);
            }
        };
        self.join(&invite).await?;
        Ok(Some(invite))
    }

    /// Opens the host's own Syncplay client on the party it is running.
    ///
    /// This deliberately bypasses `join`: a host-local connection is not a
    /// guest session and must not replace the invite resumed on next startup.
    pub async fn join_as_host(&self) -> Result<()> {
        let SessionState::Hosting(info) = self.state().await else {
            return Err(SyncPartyError::ServerNotRunning);
        };

        let nickname = self.settings.get().nickname;
        ClientLauncher::discover(&self.settings)?
            .join(&info.invite.at_host(&info.tailscale_address), &nickname)
            .await
    }

    pub fn clear_last_session(&self) -> Result<()> {
        self.secrets.delete(SecretKey::LastInvite)
    }
}

fn parse_last_invite(raw: &str) -> Option<Invite> {
    serde_json::from_str(raw).ok()
}

fn encode_last_invite(invite: &Invite) -> Result<String> {
    Ok(serde_json::to_string(invite)?)
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use super::*;
    use crate::core::config::AppMode;
    use crate::core::events::test_support::RecordingEventBus;
    use crate::core::paths::AppPaths;

    /// A server that records what it was asked to do without starting Python.
    #[derive(Default)]
    struct FakeServer {
        started: Mutex<Vec<ServerConfig>>,
        stops: Mutex<usize>,
        fail_on_start: bool,
    }

    #[async_trait]
    impl ServerController for FakeServer {
        async fn start(&self, config: &ServerConfig) -> Result<()> {
            if self.fail_on_start {
                return Err(SyncPartyError::ServerStartFailed("nope".to_owned()));
            }
            self.started.lock().await.push(config.clone());
            Ok(())
        }

        async fn stop(&self) -> Result<()> {
            *self.stops.lock().await += 1;
            Ok(())
        }

        async fn state(&self) -> ServerState {
            ServerState::Stopped
        }
    }

    /// Each test gets its own directory: they run in parallel, and a shared
    /// one means whichever test wipes it first breaks the others.
    fn session_with(
        label: &str,
        server: Arc<FakeServer>,
    ) -> (PartySession, Arc<RecordingEventBus>) {
        let dir = std::env::temp_dir().join(format!("syncparty-session-{label}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        let settings = Arc::new(ConfigStore::load(AppPaths::rooted_at(&dir)).expect("settings"));
        settings
            .update(|s| s.mode = Some(AppMode::Host))
            .expect("update");

        // A file backend so these tests leave no credentials behind on
        // whoever's machine ran them.
        let secrets = Arc::new(SecretStore::file(dir.join("secrets.json")));
        let bus = Arc::new(RecordingEventBus::default());

        let session = PartySession::new(
            settings,
            Arc::clone(&secrets),
            server,
            Arc::new(DiscordNotifier::new(secrets)),
            Arc::clone(&bus) as Arc<dyn EventBus>,
        );

        (session, bus)
    }

    #[tokio::test]
    async fn a_new_session_is_idle() {
        let (session, _) = session_with("idle", Arc::new(FakeServer::default()));

        assert!(matches!(session.state().await, SessionState::Idle));
    }

    #[tokio::test]
    async fn stopping_an_idle_session_still_stops_the_server_and_returns_to_idle() {
        let server = Arc::new(FakeServer::default());
        let (session, bus) = session_with("stop", Arc::clone(&server));

        session.stop_hosting().await.expect("stop");

        assert_eq!(*server.stops.lock().await, 1);
        assert!(matches!(session.state().await, SessionState::Idle));
        assert!(bus
            .events()
            .iter()
            .any(|event| matches!(event, AppEvent::SessionChanged { .. })));
    }

    #[tokio::test]
    async fn a_failed_start_leaves_no_server_running() {
        let server = Arc::new(FakeServer {
            fail_on_start: true,
            ..FakeServer::default()
        });
        let (session, _) = session_with("failed-start", Arc::clone(&server));

        // Tailscale is almost certainly absent in CI, so this fails before
        // reaching the server; either way the cleanup path must run.
        let _ = session.start_hosting().await;

        assert!(
            *server.stops.lock().await >= 1,
            "a failed start must stop whatever it started"
        );
        assert!(matches!(
            session.state().await,
            SessionState::Idle | SessionState::Failed { .. }
        ));
    }

    #[test]
    fn ignores_a_corrupt_saved_invite() {
        assert!(parse_last_invite("not an invite").is_none());
    }

    #[test]
    fn saved_invites_round_trip() {
        let invite = Invite {
            host: "movie-box.tail1a2b3.ts.net".to_owned(),
            alternate_hosts: Vec::new(),
            port: 8999,
            password: "swordfish".to_owned(),
            room: "MovieNight".to_owned(),
        };

        assert_eq!(
            parse_last_invite(&encode_last_invite(&invite).expect("encode")),
            Some(invite)
        );
    }
}
