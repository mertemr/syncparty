//! Starting the Syncplay desktop client already pointed at a party.
//!
//! This is the whole point of the guest half: instead of reading an address,
//! a password and a room name out of a chat message and retyping all three
//! into a dialog, the guest clicks once.

use std::net::SocketAddr;
use std::path::PathBuf;

use crate::core::config::ConfigStore;
use crate::core::error::{Result, SyncPartyError};
use crate::core::invite::Invite;
use crate::core::process;

#[cfg(windows)]
const CLIENT_FALLBACKS: &[&str] = &[
    r"C:\Program Files (x86)\Syncplay\Syncplay.exe",
    r"C:\Program Files\Syncplay\Syncplay.exe",
];

#[cfg(target_os = "macos")]
const CLIENT_FALLBACKS: &[&str] = &["/Applications/Syncplay.app/Contents/MacOS/Syncplay"];

#[cfg(not(any(windows, target_os = "macos")))]
const CLIENT_FALLBACKS: &[&str] = &["/usr/bin/syncplay", "/usr/local/bin/syncplay"];

#[cfg(windows)]
const MPV_FALLBACKS: &[&str] = &[
    r"C:\Program Files\mpv\mpv.exe",
    r"C:\Program Files\mpv.net\mpvnet.exe",
];

#[cfg(windows)]
const VLC_FALLBACKS: &[&str] = &[
    r"C:\Program Files\VideoLAN\VLC\vlc.exe",
    r"C:\Program Files (x86)\VideoLAN\VLC\vlc.exe",
];

#[cfg(target_os = "macos")]
const VLC_FALLBACKS: &[&str] = &["/Applications/VLC.app/Contents/MacOS/VLC"];

#[cfg(not(any(windows, target_os = "macos")))]
const VLC_FALLBACKS: &[&str] = &["/usr/bin/vlc", "/usr/local/bin/vlc"];

#[cfg(target_os = "macos")]
const MPV_FALLBACKS: &[&str] = &[
    "/Applications/mpv.app/Contents/MacOS/mpv",
    "/opt/homebrew/bin/mpv",
    "/usr/local/bin/mpv",
];

#[cfg(not(any(windows, target_os = "macos")))]
const MPV_FALLBACKS: &[&str] = &["/usr/bin/mpv", "/usr/local/bin/mpv"];

/// Settings keys under which a manually chosen path is stored.
pub const SYNCPLAY_CLIENT_KEY: &str = "syncplayClient";
pub const MPV_KEY: &str = "mpv";

/// Locates the Syncplay client executable.
///
/// A path the user set by hand wins over everything else — they told us where
/// it is, so second-guessing them would be strange.
pub fn find_client(manual: Option<&str>) -> Option<PathBuf> {
    manual
        .and_then(|raw| process::resolve_manual(raw, "syncplay"))
        .or_else(|| process::locate("syncplay", CLIENT_FALLBACKS))
}

/// Locates a player Syncplay can drive, preferring mpv when both are present.
pub fn find_player(manual: Option<&str>) -> Option<PathBuf> {
    manual
        .and_then(|raw| process::resolve_manual(raw, "mpv"))
        .or_else(|| manual.and_then(|raw| process::resolve_manual(raw, "vlc")))
        .or_else(|| process::locate("mpv", MPV_FALLBACKS))
        .or_else(|| process::locate("vlc", VLC_FALLBACKS))
}

pub struct ClientLauncher {
    client: PathBuf,
    player: Option<PathBuf>,
}

impl ClientLauncher {
    /// Resolves both programs, honouring whatever the user pointed at.
    ///
    /// Reads the same overrides the preflight check does, so a dependency
    /// reported as ready is one this can actually launch.
    pub fn discover(settings: &ConfigStore) -> Result<Self> {
        let client_override = settings.executable_override(SYNCPLAY_CLIENT_KEY);
        let player_override = settings.executable_override(MPV_KEY);

        Ok(Self {
            client: find_client(client_override.as_deref())
                .ok_or_else(|| SyncPartyError::DependencyMissing("Syncplay".to_owned()))?,
            player: find_player(player_override.as_deref()),
        })
    }

    /// Launches the client against `address`, using the room and password from
    /// `invite`.
    ///
    /// The address is passed in rather than read off the invite because it is
    /// never the host's: a guest connects to the near end of its own tunnel on
    /// loopback, and a host to its own Syncplay server, also on loopback. The
    /// invite names an endpoint, which Syncplay would not know what to do with.
    ///
    /// The client is detached deliberately — it outlives syncparty's window,
    /// so closing that mid-film does not close the film. The tunnel behind it
    /// does not, which is why the session holds on to it.
    pub async fn join(&self, invite: &Invite, address: SocketAddr, nickname: &str) -> Result<()> {
        let mut command = process::spawnable(&self.client);
        command
            .args(self.arguments(invite, address, nickname))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        command
            .spawn()
            .map_err(|error| SyncPartyError::CommandFailed {
                command: self.client.to_string_lossy().into_owned(),
                status: "could not start".to_owned(),
                stderr: error.to_string(),
            })?;

        Ok(())
    }

    /// Builds the client's argument list.
    ///
    /// `--host` carries the port too: the client splits on the last colon, so
    /// there is no separate `--port` flag. `--no-store` keeps a one-off party
    /// from overwriting whatever the guest normally connects to.
    fn arguments(&self, invite: &Invite, address: SocketAddr, nickname: &str) -> Vec<String> {
        let mut arguments = vec![
            "--host".to_owned(),
            address.to_string(),
            "--name".to_owned(),
            nickname.to_owned(),
            "--room".to_owned(),
            invite.room.clone(),
            "--password".to_owned(),
            invite.password.clone(),
            "--no-store".to_owned(),
        ];

        if let Some(player) = &self.player {
            arguments.push("--player-path".to_owned());
            arguments.push(player.to_string_lossy().into_owned());
        }

        arguments
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    /// The near end of a guest's tunnel: always loopback, always a port the OS
    /// picked, which is what Syncplay is actually pointed at now.
    fn local_address() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 51_234)
    }

    fn sample_invite() -> Invite {
        Invite {
            endpoint: "ki6ht7dnc4rsvbxfmhtkmqvjs6f5hbjbnvpfsxfzjxxfemn7zbaq".to_owned(),
            password: "swordfish".to_owned(),
            room: "MovieNight".to_owned(),
        }
    }

    fn launcher_without_player() -> ClientLauncher {
        ClientLauncher {
            client: PathBuf::from("/tmp/Syncplay"),
            player: None,
        }
    }

    fn arguments_for(launcher: &ClientLauncher) -> Vec<String> {
        launcher.arguments(&sample_invite(), local_address(), "ahmet")
    }

    #[test]
    fn folds_the_port_into_the_host_argument() {
        let arguments = arguments_for(&launcher_without_player());

        let host_index = arguments
            .iter()
            .position(|a| a == "--host")
            .expect("--host");
        assert_eq!(arguments[host_index + 1], "127.0.0.1:51234");
        assert!(
            !arguments.iter().any(|a| a == "--port"),
            "the client has no --port flag"
        );
    }

    #[test]
    fn points_syncplay_at_the_tunnel_rather_than_at_the_invite() {
        // The invite names an iroh endpoint, which Syncplay cannot dial. What
        // it must be given is the local address the tunnel is listening on,
        // and nothing about the endpoint id may leak into the arguments.
        let arguments = arguments_for(&launcher_without_player());
        let invite = sample_invite();

        assert!(
            !arguments.iter().any(|a| a.contains(&invite.endpoint)),
            "the endpoint id must not reach the Syncplay command line"
        );

        let host_index = arguments
            .iter()
            .position(|a| a == "--host")
            .expect("--host");
        assert!(arguments[host_index + 1].starts_with("127.0.0.1:"));
    }

    #[test]
    fn carries_the_room_and_password_from_the_invite() {
        let arguments = arguments_for(&launcher_without_player());
        let invite = sample_invite();

        let room = arguments
            .iter()
            .position(|a| a == "--room")
            .expect("--room");
        assert_eq!(arguments[room + 1], invite.room);

        let password = arguments
            .iter()
            .position(|a| a == "--password")
            .expect("--password");
        assert_eq!(arguments[password + 1], invite.password);
    }

    #[test]
    fn always_passes_no_store_so_a_party_does_not_overwrite_saved_settings() {
        assert!(arguments_for(&launcher_without_player()).contains(&"--no-store".to_owned()));
    }

    #[test]
    fn omits_the_player_path_when_mpv_is_not_installed() {
        assert!(!arguments_for(&launcher_without_player())
            .iter()
            .any(|a| a == "--player-path"));
    }

    #[test]
    fn passes_the_player_path_when_mpv_is_present() {
        let launcher = ClientLauncher {
            client: PathBuf::from("/tmp/Syncplay"),
            player: Some(PathBuf::from("/usr/local/bin/mpv")),
        };

        let arguments = arguments_for(&launcher);
        let index = arguments
            .iter()
            .position(|a| a == "--player-path")
            .expect("--player-path");
        assert_eq!(arguments[index + 1], "/usr/local/bin/mpv");
    }

    #[test]
    fn finds_vlc_in_a_manually_selected_folder() {
        let directory =
            std::env::temp_dir().join(format!("syncparty-vlc-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("directory");
        let vlc = directory.join(if cfg!(windows) { "vlc.exe" } else { "vlc" });
        std::fs::write(&vlc, b"").expect("vlc");

        assert_eq!(
            find_player(directory.to_str()),
            Some(vlc),
            "a VLC folder should satisfy the player requirement"
        );
    }
}
