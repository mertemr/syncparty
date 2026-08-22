# syncparty

Synchronised movie nights, peer to peer. One app for the person hosting and the
people joining.

syncparty runs a private [Syncplay](https://syncplay.pl) server on your own
machine and hands your friends a single link. They click it and land in the
room — no account to create, no network to join, no router to configure, no
address to copy, nothing exposed to the public internet.

> **Nothing is streamed.** Everyone plays their own local copy of the file;
> syncparty only keeps playback in sync.

## What it does

**For the host**

- Starts the Syncplay server, opens the connection and generates the invite in
  one action
- Live room panel showing real nicknames, the file each person has open and
  who is ready — pushed by the server, not polled
- Warns when two people opened *different* files, which is the usual reason a
  movie night desyncs
- Optional Discord announcement with the join link

**For the guest**

- Paste an invite code, or just click the `syncparty://` link
- Checks for Syncplay and mpv, and installs whatever is missing
- One button to join

## Install

Grab the latest build from [Releases](https://github.com/Tahckn/syncparty/releases).

**Windows** — the `.exe` setup file (or the `.msi`).
**macOS** — `aarch64.dmg` for Apple silicon, `x64.dmg` for Intel.
**Debian / Ubuntu** — `sudo apt install ./syncparty_*.deb`.
**Arch** — `syncparty-bin` from the AUR.
**Anything else** — [`docs/building.md`](docs/building.md).

The installers are unsigned, so Windows SmartScreen and macOS Gatekeeper will
both warn on first run. Updates are still verified: they are cryptographically
signed and checked against a key built into the app before anything is
installed, so this warning is a one-time thing rather than something you see on
every update.

Everything else — the Syncplay client, mpv, and the Python the server runs on —
is handled for you, though by different means depending on the platform.
Windows and macOS detect what is missing on first launch and install it through
winget or Homebrew. The Linux packages declare the same things as dependencies,
so your package manager has already put them in place before the app opens.

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
│  ├─ Syncplay server      │                  │  └─ 127.0.0.1:auto       │
│  │    on 127.0.0.1 ◄─────┤                  │        ▲                 │
│  ├─ tunnel ══════════════╪═══ QUIC ═════════╪════════┘                 │
│  └─ room monitor         │  (direct, or via │        │                 │
│                          │   a relay)       │  Syncplay client ─► mpv  │
└──────────────────────────┘                  └──────────────────────────┘
```

Every machine has an ed25519 key pair, and the public half is its address —
that is what an invite carries. There is no IP, no port and no host name in it,
because [iroh](https://iroh.computer) resolves the key to whatever route works
at the time: a direct connection when hole punching succeeds, and a relay that
only ever forwards ciphertext when it does not. Nothing to sign into, nothing
to configure, and no static address anywhere.

The Syncplay server binds to `127.0.0.1` and stays there. Guests do not dial
it; they connect to the host's endpoint, and syncparty forwards each stream to
loopback on the other side. Their own Syncplay client is likewise pointed at a
loopback port on their machine. Neither program knows it is not talking to a
server on the same computer.

**Nothing is streamed.** Everyone plays their own local copy of the file. What
crosses the connection is Syncplay's control traffic — a few hundred bytes a
second — so a relayed party costs about as much bandwidth as a chat window.

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

- **syncparty is on the path now, and that is a real cost.** Previously the two
  Syncplay processes talked to each other over Tailscale and syncparty could be
  closed mid-film without consequence. Now it carries the connection, so
  closing it ends the party for that person. The tunnels are owned by the
  session and live exactly as long as it does; guests leave a party explicitly
  rather than by quitting.

- **The relay cannot read anything.** Connections are QUIC with the endpoint
  keys as the identity, encrypted end to end. A relay forwards packets by
  endpoint id and never holds a key that could open them. Whether a given party
  is direct or relayed changes its latency, not its privacy.
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
  the process table. They are stored in Windows Credential Manager, the macOS
  Keychain, or the Secret Service on Linux. Linux is the one platform where
  that store may be absent — Secret Service is a daemon, and a minimal window
  manager may run none — so the packages list a provider as a recommendation
  and the app falls back to a `0600` file under `~/.local/share/syncparty/`
  rather than refusing to host. `Settings → Diagnostics` reports which store is
  in use.
- **The salt is generated once and kept.** Syncplay derives room operator
  passwords from it; a new salt on every start would silently invalidate them.
- **The invite is the only thing guarding a party.** There is no network
  membership to check any more, so anyone holding the code can join. That was
  already most of the story — the code carried the server password — but the
  tailnet used to be a second door. Treat a code like a door key and start a
  new party if one leaks.
- **Updates download in the background but never install themselves.**
  syncparty checks on startup and, if there is a new version, downloads it
  silently — there is no reason to make that wait for anyone. Installing
  replaces the running binary and restarts the app, though, which would take
  the Syncplay server down mid-film, so that step is always an explicit
  button, and it stays hidden for as long as a party is running. On Linux it
  does not download at all: the package manager owns the install there, so the
  app reports the new version and leaves it alone.
- **The Linux packages carry their own Syncplay server.** No distribution
  ships one syncparty can use. It binds the server to loopback with
  `--ipv4-only` and `--interface-ipv4`, both added in Syncplay 1.7.1, and the
  newest package in any Ubuntu LTS is 1.7.0 — without them the server would
  listen on every interface. The pinned source is verified against a SHA-256
  recorded in the code and installed to `/usr/lib/syncparty/`; only Twisted
  comes from the distribution.

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
package lists, and the `scripts/fetch-syncplay.mjs` step a packaged build needs.

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
    syncplay/            protocol, server process, room monitor, launcher
    invite/              invite codes and deep links
    session/             the host/guest state machine
    update.rs            whether this build may update itself
  packaging/             the .deb maintainer scripts
scripts/                 version bump, Syncplay vendoring, AUR rendering
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
