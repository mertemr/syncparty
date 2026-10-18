# Building syncparty from source

For anyone on a distribution the packages do not cover — NixOS, Gentoo, Void,
openSUSE, Alpine, or a machine where you would rather compile than install a
package. On Debian, Ubuntu, or Arch, prefer the `.deb` or `syncparty-bin` from
the AUR: they declare their dependencies, so the app opens with everything it
needs already in place.

## What you need

| | Version |
|---|---|
| Rust | stable, 1.91 or newer |
| Node.js | 22 or newer |
| pnpm | 10 (`corepack enable pnpm`) |

Plus the WebKitGTK stack Tauri links against, and mpv — the one program
syncparty drives at runtime.

**Debian / Ubuntu**

```bash
sudo apt install build-essential curl file git pkg-config \
  libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev \
  libayatana-appindicator3-dev libssl-dev libxdo-dev patchelf mpv
```

**Fedora**

```bash
sudo dnf install @development-tools webkit2gtk4.1-devel gtk3-devel \
  librsvg2-devel libayatana-appindicator-gtk3-devel openssl-devel \
  libxdo-devel patchelf mpv
```

**Arch**

```bash
sudo pacman -S base-devel webkit2gtk-4.1 gtk3 librsvg \
  libayatana-appindicator xdotool mpv
```

mpv is the only supported player. Earlier versions also accepted VLC, through
the Syncplay client; syncparty now drives the player itself over mpv's JSON
IPC, which VLC does not have.

Neither half of a party needs Syncplay installed. The server and the client
are both built into the syncparty binary — they speak the Syncplay protocol,
but there is no Python, no Twisted, no Qt and no `syncplay` package anywhere
in these lists.

### Optional: a keyring

syncparty keeps the server password and salt in the OS keyring through the
Secret Service API. If no provider is running — common on a minimal window
manager — it falls back to a `0600` file in `~/.local/share/syncparty/`. That
works, and the app will not complain, but the keyring is the better home:

```bash
# whichever suits your desktop
sudo apt install gnome-keyring      # or kwalletmanager, or keepassxc
```

`Settings → Diagnostics` reports which store is in use as `secretStorage`.

## Build

```bash
git clone https://github.com/mertemr/syncparty.git
cd syncparty
pnpm install
pnpm tauri build
```

That is the whole build. Earlier versions needed a `fetch-syncplay.mjs` step
first, which vendored a pinned Syncplay server into the package because no
distribution shipped a new enough one. The server is now written in Rust and
compiled into the binary, so there is nothing to vendor and nothing to pin.

Artifacts land in `src-tauri/target/release/bundle/`. `pnpm tauri build
--bundles deb` restricts it to a `.deb`; the valid Linux targets are `deb`,
`rpm`, and `appimage`.

## Running it without installing

```bash
pnpm tauri dev
```

Deep links (`syncparty://join/...`) will not work this way. They need a
`.desktop` file registered with `MimeType=x-scheme-handler/syncparty`, which
only exists once the app is installed from a package. Paste the invite code
into the join field instead — it accepts the raw code as well as the link.

## Checks

```bash
pnpm test                    # frontend
cd src-tauri && cargo test   # backend, including the TypeScript bindings
```

A few backend tests are ignored by default because they need mpv on PATH.
The end-to-end one starts the built-in server, connects the built-in client to
it, lets that client drive a headless mpv, and asserts that playback state
really crosses the server. It makes its own test clip with `mpv --o`, so mpv is
all it needs. It is the only test that runs the whole path, from one player's
state to the server and back into a player, so run it if you have mpv
installed — together with the mpv driver's own ignored tests and the check
that recognises a real mpv. The end-to-end test runs its films one at a time,
because two parties starting players side by side on a busy machine test the
machine rather than syncparty:

```bash
cd src-tauri
cargo test --test native_watch_e2e -- --ignored --test-threads=1
cargo test --lib player::mpv -- --ignored
cargo test --lib deps::mpv -- --ignored
```

They need no display — mpv runs with `--vo=null --ao=null` — so they work over
SSH and in CI, where the `Backend (native client + mpv)` job runs all three.

The Rust tests regenerate `src/shared/types/*.ts` through ts-rs. If `git
status` shows changes there after a run, commit them — new files included —
because they are part of the source, not build output. ts-rs never deletes a
file, so the `.ts` of a type that no longer exists has to be removed by hand.
