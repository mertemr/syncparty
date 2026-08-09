# syncparty

Synchronised movie nights over [Tailscale](https://tailscale.com). One app for
the person hosting and the people joining.

syncparty runs a private [Syncplay](https://syncplay.pl) server bound to your
Tailscale address, then hands your friends a single link. They click it and land
in the room — no address to copy, no password to retype, nothing exposed to the
public internet.

> **Nothing is streamed.** Everyone plays their own local copy of the file;
> syncparty only keeps playback in sync.

## What it does

**For the host**

- Brings Tailscale up, starts the Syncplay server and generates the invite in
  one action
- Live room panel showing real nicknames, the file each person has open and
  who is ready — pushed by the server, not polled
- Warns when two people opened *different* files, which is the usual reason a
  movie night desyncs
- Optional Discord announcement with the join link

**For the guest**

- Paste an invite code, or just click the `syncparty://` link
- Checks for Tailscale, Syncplay and mpv, and installs whatever is missing
- One button to join

## Install

Grab the latest build from [Releases](https://github.com/Tahckn/syncparty/releases).
That's the only manual install — from v0.2.0 on, syncparty checks for updates
on startup and offers to install new ones in place.

The installers themselves are unsigned, so Windows SmartScreen and macOS
Gatekeeper will both warn on first run. Updates are still verified: they are
cryptographically signed and checked against a key built into the app before
anything is installed, so this warning is a one-time thing rather than
something you see on every update.

Everything else — Tailscale, the Syncplay client, mpv, and the Python
environment the server needs — is detected on first launch and installed for
you if it is missing.

## An always-on host

The app hosts from whatever machine is running it, so the party ends when that
laptop sleeps. If you have something that is already on all the time — a NAS, a
VPS, a Raspberry Pi — there is a container that runs the server half by itself:

```bash
curl -O https://raw.githubusercontent.com/Tahckn/syncparty/main/docker-compose.yml
echo "TS_AUTHKEY=tskey-auth-..." > .env
docker compose up -d
```

Make the auth key at
[login.tailscale.com](https://login.tailscale.com/admin/settings/keys) —
reusable, and **not** ephemeral, or the node disappears with every restart.

The invite is printed by `docker compose logs -f` and written to `invite.txt`
on the volume. Paste it into syncparty on any machine exactly as a guest would:
with the server running elsewhere, the host is just another guest of it.

Only the server is containerised. The guest half plays a local file through
mpv, which needs a screen and the film on disk, so it stays on the desktop.

| Variable | Default | |
|---|---|---|
| `TS_AUTHKEY` | — | Without one, a sign-in URL is printed and the daemon waits |
| `TS_HOSTNAME` | `syncparty` | The name this node appears under in your tailnet |
| `SYNCPARTY_ROOM` | `MovieNight` | |
| `SYNCPARTY_PORT` | `8999` | |
| `SYNCPARTY_MONITOR` | `true` | Logs who is in the room, at the cost of a `syncparty-panel` entry in everyone's user list |
| `SYNCPARTY_DISCORD_WEBHOOK` | — | Announce the invite in a channel |

Two things are worth getting right:

- **`/data` has to be a real volume.** It holds this node's Tailscale identity,
  the server password and the salt. Recreating the container without it mints a
  new password and a new salt, which invalidates every invite already shared
  and every room operator password derived from the old salt. The container
  warns at startup if `/data` is not mounted.
- **`--device=/dev/net/tun --cap-add=NET_ADMIN` are required**, and the compose
  file sets both. Userspace networking is not an alternative: the server binds
  to the tailnet address itself, and in userspace mode nothing holds it.

Images are published for amd64 and arm64.

## How it works

```
Host machine                                Guest machine
┌────────────────────────┐                  ┌────────────────────────┐
│ syncparty              │                  │ syncparty              │
│  ├─ Syncplay server ───┼── Tailscale ─────┼─→ Syncplay client      │
│  │   (tailnet IP only) │   (WireGuard)    │      └─ mpv            │
│  └─ room monitor       │                  │                        │
└────────────────────────┘                  └────────────────────────┘
```

The server binds to the machine's Tailscale IPv4 address rather than
`0.0.0.0`, so it is unreachable from the local network and from the internet.
All traffic rides the existing WireGuard tunnel.

## Design notes

A few decisions worth knowing about:

- **An invite carries every address that reaches the server.** No single one
  works for everybody: a node shared into somebody else's tailnet is reached on
  a masqueraded address that means nothing anywhere else — including on the
  host's own machine — while peers inside the host's tailnet need its real
  address. The joining side tries each candidate and uses whichever answers,
  so the host never has to know who is on which tailnet.
- **Portable builds can be pointed at by hand.** Detection covers installers
  and `PATH`, which misses an mpv or Syncplay zip extracted to some folder.
  Rather than guessing at where people keep those, the setup screen has a
  "Locate…" button — give it the program or the folder holding it. A location
  that turns out not to work is rejected rather than saved.
- **The room panel attaches a real Syncplay client.** The server has no admin
  API, so the only way to learn nicknames and open files is to be a
  participant. It appears in everyone's user list as `syncparty-panel`, and can
  be switched off in Settings at the cost of the detail it provides.
- **Secrets never touch the command line.** The server password and salt reach
  Syncplay through `SYNCPLAY_PASSWORD` and `SYNCPLAY_SALT`, so they stay out of
  the process table. They are stored in Windows Credential Manager or the macOS
  Keychain.
- **The salt is generated once and kept.** Syncplay derives room operator
  passwords from it; a new salt on every start would silently invalidate them.
- **Stopping a party does not stop Tailscale.** Your tailnet is used for other
  things.
- **The container is its own tailnet node, not a tenant of the host's.**
  Sharing the machine's network namespace would be simpler, but only works on
  Linux: under Docker Desktop the "host" is a Linux VM whose tailnet address
  means nothing to Windows or macOS. Joining separately costs an auth key and
  works everywhere.
- **The headless host keeps its secrets in a file, not a keychain.** A
  container has no desktop session to unlock one. The file is `0600` on a
  volume, which is weaker — put it on a machine you control, not a shared box.
- **Updates download in the background but never install themselves.**
  syncparty checks on startup and, if there is a new version, downloads it
  silently — there is no reason to make that wait for anyone. Installing
  replaces the running binary and restarts the app, though, which would take
  the Syncplay server down mid-film, so that step is always an explicit
  button, and it stays hidden for as long as a party is running.

## Building from source

Requires [Rust](https://rustup.rs), Node 20+, pnpm, and the
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

## Layout

```
src/                     React frontend
  features/              onboarding · host · guest · settings
  shared/                ipc wrappers, generated types, UI primitives
src-tauri/
  src/main.rs            the windowed app          (--features desktop)
  src/bin/syncpartyd.rs  the headless host         (--features headless)
  src/ipc/               Tauri commands and the event bridge
  src/core/              all logic, no Tauri dependency
    deps/                dependency detection and installation
    tailscale/           tailnet status and addresses
    syncplay/            protocol, server process, room monitor, launcher
    invite/              invite codes and deep links
    session/             the host/guest state machine
```

`core` never imports from `ipc`, which is what lets the whole of it run under
`cargo test` without a webview — and is what `syncpartyd` is built out of. It
drives the same `PartySession` the host screen does, replacing only where
events go and where secrets are kept.

```bash
cargo build --release --no-default-features --features headless --bin syncpartyd
```

## Contributing

Issues and pull requests are welcome — you do not need to be a collaborator,
forking and opening a PR against `main` is enough. See
[CONTRIBUTING.md](CONTRIBUTING.md) for the fork/branch/PR workflow, what CI
checks before a PR can merge, and how the code is arranged.

## Licence

MIT — see [LICENSE](LICENSE).

syncparty is not affiliated with Syncplay or Tailscale.
