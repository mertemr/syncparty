# syncparty

**Grab the movie, send one link, watch together in sync.**

syncparty runs a private watch-party server on your own machine and hands
your friends a single link. They click it and land in the room, with mpv open
and following along — no account to create, no network to join, no router to
configure, no address to copy, nothing exposed to the public internet. Under
the hood it speaks the [Syncplay](https://syncplay.pl) protocol, but nobody
needs Syncplay installed.

> **Nothing is streamed.** Everyone plays their own local copy of the file;
> syncparty only keeps playback in sync.

## What it does

**For the host**

- Starts the sync server, opens the connection and generates the invite in
  one action
- Opens your own mpv in the room too, kept in step exactly like everyone
  else's
- Live room panel showing real nicknames, the file each person has open and
  who is ready — pushed by the server, not polled
- Warns when two people opened *different* files, which is the usual reason a
  movie night desyncs
- Optional Discord announcement with the join link

**For the guest**

- Paste an invite code, or just click the `syncparty://` link
- Checks for mpv, and on Windows and macOS installs it if it is missing
- One button to join — syncparty opens mpv and keeps it in step with the room
- See who else is in the room, the file each person has open and who is ready

## Install

| Platform | Download |
| --- | --- |
| 🪟 Windows | [`.exe` setup](https://github.com/mertemr/syncparty/releases/latest) · [`.msi`](https://github.com/mertemr/syncparty/releases/latest) |
| 🍎 macOS (Apple silicon) | [`.dmg`](https://github.com/mertemr/syncparty/releases/latest) |
| 🍎 macOS (Intel) | [`.dmg`](https://github.com/mertemr/syncparty/releases/latest) |
| 🐧 Debian / Ubuntu | [`.deb`](https://github.com/mertemr/syncparty/releases/latest) |
| 🐧 Fedora / openSUSE | [`.rpm`](https://github.com/mertemr/syncparty/releases/latest) |
| 🐧 Arch | [`syncparty-bin`](https://aur.archlinux.org/packages/syncparty-bin) |
| 🐧 Any other desktop Linux | [`.AppImage`](https://github.com/mertemr/syncparty/releases/latest) |

Every link lands on the same page — the
[latest release](https://github.com/mertemr/syncparty/releases/latest), grab the
file for your machine. That's the only manual install: from v0.2.0 on,
syncparty checks for updates on startup and offers to install new ones in
place.

On Debian or Ubuntu that is `sudo apt install ./syncparty_*.deb`; on Fedora or
openSUSE, `sudo dnf install ./syncparty-*.rpm` or `sudo zypper install
./syncparty-*.rpm`; on Arch, `syncparty-bin` from the AUR. The AppImage needs
no install step — mark it executable and run it. Any other distribution
builds from source — [`docs/building.md`](docs/building.md).

The installers are unsigned, so Windows SmartScreen and macOS Gatekeeper will
both warn on first run. Updates are still verified: they are cryptographically
signed and checked against a key built into the app before anything is
installed, so this warning is a one-time thing rather than something you see on
every update.

The one other thing syncparty needs is mpv. The server and the client that
keeps everyone's mpv in step are both built in, so the player is all that is
left, and how it gets there depends on the platform. Windows and macOS detect
a missing mpv on first launch and install it through winget or Homebrew.
Homebrew only has a prebuilt mpv for Apple silicon on recent macOS releases —
Sequoia and newer at the time of writing — so on an Intel Mac, or on an older
macOS, it compiles mpv instead, which takes a while. The `.deb` declares it as
a hard dependency, so apt has already put it in place before the app opens;
`syncparty-bin` does the same on Arch. The `.rpm` only recommends it — an RPM
can land on a system whose repositories carry no mpv, and a hard dependency
would make the package refuse to install there — and the AppImage carries no
package-manager dependencies at all. Both fall back to the first-launch check,
which on Linux reports a missing mpv and links to mpv.io rather than
installing it, since there is no one package manager to ask.

Updates work the same way. On Windows and macOS syncparty downloads them in the
background and offers to restart. On Linux it tells you a new version exists
and stops there: every Linux build is owned by a package manager, and
installing behind apt's or pacman's back is how a package database ends up
holding a version it did not put there.

## How it works

```
Host machine                                    Guest machine
┌──────────────────────────┐                  ┌──────────────────────────┐
│ syncparty                │                  │ syncparty                │
│  ├─ sync server          │                  │  └─ 127.0.0.1:auto       │
│  │    on 127.0.0.1 ◄─────┤                  │        ▲                 │
│  ├─ tunnel ══════════════╪═══ QUIC ═════════╪════════┘                 │
│  ├─ room panel           │  (direct, or via │        │                 │
│  └─ sync client          │   a relay)       │     sync client          │
│        │ JSON IPC        │                  │        │ JSON IPC        │
└────────┼─────────────────┘                  └────────┼─────────────────┘
         ▼                                             ▼
        mpv                                           mpv
```

Every machine has an ed25519 key pair, and the public half is its address —
that is what an invite carries. There is no IP, no port and no host name in it,
because [iroh](https://iroh.computer) resolves the key to whatever route works
at the time: a direct connection when hole punching succeeds, and a relay that
only ever forwards ciphertext when it does not. Nothing to sign into, nothing
to configure, and no static address anywhere.

The sync server binds to `127.0.0.1` and stays there. Guests do not dial it;
they connect to the host's endpoint, and syncparty forwards each stream to
loopback on the other side. A guest's built-in client connects to a loopback
port on its own machine — the near end of that tunnel — while the host's
client and the room panel connect to the server directly, so none of them
knows it is not talking to something on the same computer.

Each client drives its mpv over mpv's JSON IPC: a local socket (a named pipe
on Windows) that only the user running syncparty can open. It watches mpv for
pauses and seeks, tells the server about them, and applies everyone else's.
The server holds the room's state and sends it to every client once a second,
the way Syncplay's server does. A pause or a seek from anyone reaches
everyone, and each client corrects its own drift the way Syncplay's client
does: a player running a little ahead is slowed down until the others catch
up, and one that is seconds out jumps back into place. It is Syncplay's
protocol message for message, so an older syncparty, which still joins
through the real Syncplay client, can join a party hosted on a new one.

**Nothing is streamed.** Everyone plays their own local copy of the file. What
crosses the connection is Syncplay-protocol control traffic — a few hundred
bytes a second — so a relayed party costs about as much bandwidth as a chat
window.

## Design notes

A few decisions worth knowing about:

- **An invite is a key, not an address.** The old Tailscale-era codes had to
  carry every address that might reach the host — a masqueraded share address,
  its tailnet IP, its MagicDNS name — because which one worked depended on
  which tailnet the guest was on, something the host could not know. An
  endpoint id has none of that problem: it means the same thing from every
  network, and it survives the host's connection changing. It also survives a
  restart, because the key is generated once and kept, so a code sent last week
  still works tonight.

- **syncparty is on the path, and it is the player's only link to the
  party.** Previously the two Syncplay processes talked to each other over
  Tailscale and syncparty could be closed mid-film without consequence. Then
  it carried the connection, so closing it cut that person off. Now it is the
  client as well, and mpv on its own knows nothing about the party — so
  leaving a party or closing syncparty also closes the mpv it was driving,
  rather than leaving behind a player that looks in sync and is not. The
  tunnels and the player are owned by the session and live exactly as long as
  it does; guests leave a party explicitly rather than by quitting.

- **The relay cannot read anything.** Connections are QUIC with the endpoint
  keys as the identity, encrypted end to end. A relay forwards packets by
  endpoint id and never holds a key that could open them. Whether a given party
  is direct or relayed changes its latency, not its privacy.
- **Portable builds can be pointed at by hand.** Detection covers installers
  and `PATH`, which misses an mpv zip extracted to some folder. Rather than
  guessing at where people keep those, the setup screen has a "Locate…"
  button — give it the program or the folder holding it. A location that
  does not turn out to be a working mpv is rejected rather than saved.
- **The room panel is a participant, not an observer.** It is a read-only
  client of syncparty's own that joins the room over loopback like anyone
  else, because the Syncplay protocol has no observer role and the panel wants
  exactly what a participant sees: nicknames, the file each person has open,
  who is ready. It never sends playback state, so it cannot pause or seek
  anyone's film. It appears in everyone's user list as `syncparty-panel`, and
  can be switched off in Settings. Your own sync client sees the same room, so
  while you are watching too it fills the panel instead; only with neither of
  them connected is there no room detail to show. A guest's list of who is in
  the room comes from their own client the same way.
- **Secrets are never handed to another program.** The server password and
  salt are stored in Windows Credential Manager, the macOS Keychain, or the
  Secret Service on Linux. The server, the client and the room panel all run
  inside syncparty, so nothing it starts is given either one — not on a
  command line, not in an environment variable. mpv is started with its IPC
  endpoint and is told nothing about the party, and that endpoint — a socket
  in a directory only you can open, or on Windows a pipe only you can connect
  to — is private to you, because whoever can reach it can drive the player.
  Linux is the one platform where the secret store may be absent — Secret
  Service is a daemon, and a minimal window manager may run none — so the
  packages list a provider as a recommendation and the app falls back to a
  `0600` file under `~/.local/share/syncparty/` rather than refusing to host.
  `Settings → Diagnostics` reports which store is in use.
- **The salt is generated once and kept.** The server derives room operator
  passwords from it, the same way Syncplay's does; a new salt on every start
  would silently invalidate them.
- **The invite is the only thing guarding a party.** There is no network
  membership to check any more, so anyone holding the code can join. That was
  already most of the story — the code carried the server password — but the
  tailnet used to be a second door. Treat a code like a door key — one that
  does not change. The endpoint key and the password are generated once and
  kept, which is what lets last week's code work tonight, so starting a new
  party does not retire a code that leaked. Removing syncparty's
  `endpoint-key` and `server-password` entries from the system keychain (or
  from the fallback secrets file) does, and the next party hands out a new
  one. A `syncparty://` link is the same code, so it
  carries the password too, and on Windows and Linux the system hands a
  clicked link to syncparty as a command-line argument — briefly if the app is
  already open, and for as long as it runs if the link is what started it.
  That is syncparty's own command line, and on Linux any local user can read
  it by default. Pasting the code into the app keeps the password off it.
- **Updates download in the background but never install themselves.**
  syncparty checks on startup and, if there is a new version, downloads it
  silently — there is no reason to make that wait for anyone. Installing
  replaces the running binary and restarts the app, though, which would take
  the sync server — and the client keeping your mpv in step — down mid-film,
  so that step is always an explicit button, and it stays hidden for as long
  as you are in a party. On Linux it does not download at all: the package
  manager owns the install there, so the app reports the new version and
  leaves it alone.
- **Both ends of the protocol are ours.** syncparty speaks the Syncplay
  protocol from Rust on both sides: the host runs the server in-process, and
  every participant, the host included, runs a client in-process that drives
  mpv. Nothing about a party depends on a Python program being shipped,
  installed, found, or kept at a particular version — which used to be the
  usual reason a guest could not join: a distribution Syncplay that could not
  start (Ubuntu 24.04's 1.7.0 on Python 3.12), or a client installed without
  its Qt bindings. syncparty also used to vendor a pinned Syncplay server into
  the Linux packages, because no distribution shipped one new enough to bind
  to loopback only; now there is nothing to vendor, and the server cannot
  listen anywhere but `127.0.0.1` because that is the only thing the code
  does. The wire format is still Syncplay's, pinned by tests built from
  messages captured from the upstream source, and the suite's end-to-end test
  starts our server and our client with a real, headless mpv and asserts that
  playback state really crosses the server.
- **mpv is the only player.** Syncplay drove VLC through a Lua interface
  script that had to be copied into VLC's interface directory and kept at a
  matching version. mpv has a JSON IPC built in on every platform, with
  nothing to install alongside it, and it was already the player syncparty
  preferred and installed by default. One player supported well beats two
  supported through different mechanisms.

## Building from source

Requires [Rust](https://rustup.rs), Node 22+, pnpm, and the
[Tauri prerequisites](https://tauri.app/start/prerequisites/) for your platform.

```bash
pnpm install
pnpm tauri dev
```

Run the backend tests:

```bash
cd src-tauri && cargo test
```

TypeScript types under `src/shared/types` are generated from the Rust types by
`ts-rs` when the tests run — do not edit them by hand.

[`docs/building.md`](docs/building.md) covers Linux in full: the per-distribution
package lists, and the end-to-end test that runs the built-in server and
client against a real mpv.

## Layout

```
src/                     React frontend
  features/              onboarding · host · guest · settings
  shared/                ipc wrappers, generated types, UI primitives
src-tauri/
  src/ipc/               Tauri commands and the event bridge
  src/core/              all logic, no Tauri dependency
    deps/                dependency detection and installation
    net/                 the peer-to-peer endpoint and the loopback tunnel
    player/              mpv, driven over its JSON IPC
    syncplay/            the protocol, the native server, the room monitor
      client/            the sync client that keeps mpv in step with the room
    watch/               one watch session: a sync client and its player
    invite/              invite codes and deep links
    session/             the host/guest state machine
    update.rs            whether this build may update itself
  packaging/             .desktop template and post-install script (.deb, .rpm)
scripts/                 version bump, AUR rendering
```

`core` never imports from `ipc`, which is what lets the whole of it run under
`cargo test` without a webview.

## Contributing

Issues and pull requests are welcome — you do not need to be a collaborator,
forking and opening a PR against `main` is enough. See
[CONTRIBUTING.md](CONTRIBUTING.md) for the fork/branch/PR workflow, what CI
checks before a PR can merge, and how the code is arranged.

## Licence

MIT — see [LICENSE](LICENSE).

syncparty is not affiliated with Syncplay, mpv or iroh.
