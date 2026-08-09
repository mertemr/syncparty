//! syncparty, without a window.
//!
//! Runs the same [`PartySession`] the host screen drives, reporting through
//! the log instead of a webview, so a movie night does not depend on somebody's
//! laptop staying awake. This is what the container image runs; nothing here is
//! Docker-specific.
//!
//! Configuration is entirely `SYNCPARTY_*` environment variables, because that
//! is what a container gets.

use std::sync::Arc;
use std::time::Duration;

use syncparty_lib::core::config::{
    AppMode, AppSettings, ConfigStore, SecretKey, SecretStore, DEFAULT_PORT,
};
use syncparty_lib::core::error::{Result, SyncPartyError};
use syncparty_lib::core::events::{AppEvent, EventBus};
use syncparty_lib::core::notify::DiscordNotifier;
use syncparty_lib::core::paths::AppPaths;
use syncparty_lib::core::session::{HostingInfo, PartySession};
use syncparty_lib::core::syncplay::UvManagedServer;

/// How long to wait before retrying after Tailscale asks to be signed in.
///
/// Long enough that an hour of waiting does not fill the log, short enough that
/// the party starts within half a minute of the URL being opened.
const LOGIN_RETRY_DELAY: Duration = Duration::from_secs(30);

fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "syncparty=info,syncpartyd=info".into()),
        )
        .init();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!("could not start the async runtime: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };

    match runtime.block_on(run()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(kind = error.kind(), "{error}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let paths = AppPaths::resolve()?;
    tracing::info!("data directory: {}", paths.data_dir().display());

    let settings = Arc::new(ConfigStore::load(paths.clone())?);
    let applied = settings.update(apply_environment)?;
    warn_about_volatile_storage(&paths);

    // A file rather than the keychain: there is no desktop session here.
    let secrets = Arc::new(SecretStore::file(paths.secrets_file()));
    seed_secrets_from_environment(&secrets)?;

    let bus: Arc<dyn EventBus> = Arc::new(LoggingEventBus);
    let server = Arc::new(UvManagedServer::new(paths.clone(), Arc::clone(&bus)));
    let discord = Arc::new(DiscordNotifier::new(Arc::clone(&secrets)));

    let session = Arc::new(PartySession::new(
        Arc::clone(&settings),
        secrets,
        server,
        discord,
        bus,
    ));

    tracing::info!(
        port = applied.port,
        room = %applied.room,
        monitor = applied.monitor_enabled,
        "starting"
    );

    // Without an auth key, start-up can wait for a sign-in indefinitely. Racing
    // the signal is what stops `docker stop` being ignored in that window until
    // the grace period runs out and the container is killed outright.
    let info = tokio::select! {
        started = start_with_login_retries(&session) => started?,
        () = shutdown_signal() => {
            tracing::info!("stopped before the party started");
            return session.stop_hosting().await;
        }
    };

    announce(&info, &paths);

    shutdown_signal().await;

    tracing::info!("shutting down");
    session.stop_hosting().await?;
    Ok(())
}

/// Brings the party up, waiting out an interactive Tailscale sign-in.
///
/// Every other failure is fatal: a container that cannot bind its port or find
/// its Python is misconfigured, and looping would hide the reason behind an
/// endlessly repeating error.
async fn start_with_login_retries(session: &PartySession) -> Result<HostingInfo> {
    let mut announced_url: Option<String> = None;

    loop {
        match session.start_hosting().await {
            Ok(info) => return Ok(info),
            Err(SyncPartyError::TailscaleLoginRequired { auth_url }) => {
                // Logged once per distinct URL, or it would push everything
                // else out of `docker logs`.
                if announced_url.as_deref() != Some(auth_url.as_str()) {
                    tracing::warn!(
                        "Tailscale needs signing in — open {auth_url} and this continues on \
                         its own. Set TS_AUTHKEY to skip this next time."
                    );
                    announced_url = Some(auth_url);
                }
                tokio::time::sleep(LOGIN_RETRY_DELAY).await;
            }
            Err(error) => return Err(error),
        }
    }
}

fn announce(info: &HostingInfo, paths: &AppPaths) {
    tracing::info!(
        "the party is up — invite {} · link {} · server {}",
        info.invite_code,
        info.deep_link,
        info.invite.server_address(),
    );

    let file = paths.invite_file();
    let contents = format!("{}\n{}\n", info.invite_code, info.deep_link);

    // Best effort: a read-only volume is a reason to log and carry on, not to
    // take a working party down.
    if let Err(error) = std::fs::write(&file, contents) {
        tracing::warn!("could not write {}: {error}", file.display());
    }
}

/// Warns when the data directory looks like it will not survive a restart,
/// which regenerates the password and salt and breaks every invite and room
/// operator password already in circulation.
fn warn_about_volatile_storage(paths: &AppPaths) {
    if std::env::var_os(syncparty_lib::core::paths::DATA_DIR_VAR).is_none() {
        tracing::warn!(
            "{} is not set, so state is going to {} — point it somewhere that survives \
             a restart, or the server password and salt are regenerated every time",
            syncparty_lib::core::paths::DATA_DIR_VAR,
            paths.data_dir().display(),
        );
    }
}

/// Copies secrets supplied through the environment into the store, so an
/// operator can pin a password across a rebuild or configure Discord without
/// any secret reaching `argv`.
///
/// The environment stays authoritative whenever it is set at all; otherwise
/// changing one would appear to do nothing.
fn seed_secrets_from_environment(secrets: &SecretStore) -> Result<()> {
    let seeds = [
        ("SYNCPARTY_SERVER_PASSWORD", SecretKey::ServerPassword),
        ("SYNCPARTY_SERVER_SALT", SecretKey::ServerSalt),
        ("SYNCPARTY_DISCORD_WEBHOOK", SecretKey::DiscordWebhook),
    ];

    for (variable, key) in seeds {
        let Some(value) = non_empty_var(variable) else {
            continue;
        };

        if secrets.get(key)?.as_deref() != Some(value.as_str()) {
            tracing::info!("taking {variable} from the environment");
            secrets.set(key, &value)?;
        }
    }

    Ok(())
}

/// Applies the `SYNCPARTY_*` variables over whatever is on disk.
///
/// The environment wins on every start, so a compose file stays the single
/// description of the deployment.
fn apply_environment(settings: &mut AppSettings) {
    settings.mode = Some(AppMode::Host);

    if let Some(port) = non_empty_var("SYNCPARTY_PORT") {
        match port.parse::<u16>() {
            Ok(parsed) if parsed != 0 => settings.port = parsed,
            _ => tracing::warn!(
                "SYNCPARTY_PORT={port} is not a usable port number, keeping {}",
                settings.port
            ),
        }
    }

    if let Some(room) = non_empty_var("SYNCPARTY_ROOM") {
        settings.room = room;
    }

    if let Some(language) = non_empty_var("SYNCPARTY_LANGUAGE") {
        settings.language = language;
    }

    if let Some(enabled) = env_flag("SYNCPARTY_MONITOR") {
        settings.monitor_enabled = enabled;
    }

    // Having a webhook is what turns Discord on, so there is no separate
    // switch to forget.
    settings.discord_enabled = non_empty_var("SYNCPARTY_DISCORD_WEBHOOK").is_some()
        || env_flag("SYNCPARTY_DISCORD").unwrap_or(settings.discord_enabled);

    if settings.port == 0 {
        settings.port = DEFAULT_PORT;
    }
}

fn non_empty_var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Reads a boolean the way a person writing a compose file would spell it.
fn env_flag(name: &str) -> Option<bool> {
    match non_empty_var(name)?.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        other => {
            tracing::warn!("{name}={other} is not a yes/no value, ignoring it");
            None
        }
    }
}

/// Resolves when the supervisor asks the daemon to stop.
///
/// SIGTERM matters as much as Ctrl-C: it is what `docker stop` sends, and
/// missing it means the grace period expires and the Syncplay child is killed
/// without the session ever unwinding.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(stream) => stream,
            Err(error) => {
                tracing::error!("could not listen for SIGTERM: {error}");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };

        tokio::select! {
            _ = terminate.recv() => tracing::info!("SIGTERM received"),
            _ = tokio::signal::ctrl_c() => tracing::info!("interrupted"),
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Turns the events the UI would render into log lines.
struct LoggingEventBus;

impl EventBus for LoggingEventBus {
    fn publish(&self, event: AppEvent) {
        match event {
            AppEvent::ServerLog { line, is_error } => {
                if is_error {
                    tracing::warn!(target: "syncplay", "{line}");
                } else {
                    tracing::info!(target: "syncplay", "{line}");
                }
            }

            AppEvent::SessionChanged { state } => {
                tracing::debug!("session: {state:?}");
            }

            // The one genuinely useful thing the monitor gives a headless
            // host: who turned up, and whether they opened the same file.
            AppEvent::RoomUpdated { snapshot } => {
                for room in &snapshot.rooms {
                    let watchers: Vec<&str> = room
                        .watchers
                        .iter()
                        .map(|watcher| watcher.name.as_str())
                        .collect();

                    tracing::info!(
                        room = %room.name,
                        files = ?room.file_compatibility,
                        "watching: {}",
                        watchers.join(", ")
                    );
                }
            }

            // Quiet: `start_with_login_retries` prints this properly and only
            // when the URL changes.
            AppEvent::TailscaleLoginRequired { auth_url } => {
                tracing::debug!("Tailscale sign-in required: {auth_url}");
            }

            AppEvent::Failed {
                error_kind,
                message,
            } => {
                tracing::error!(kind = %error_kind, "{message}");
            }

            // Preflight, installs and deep links are things the windowed app
            // does on a person's behalf. Nothing here triggers them.
            AppEvent::PreflightCompleted { .. }
            | AppEvent::InstallProgress { .. }
            | AppEvent::InviteReceived { .. } => {}
        }
    }
}
