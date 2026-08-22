# Building syncparty from source

For anyone on a distribution the packages do not cover — NixOS, Gentoo, Void,
openSUSE, Alpine, or a machine where you would rather compile than install a
package. On Debian, Ubuntu, or Arch, prefer the `.deb` or `syncparty-bin` from
the AUR: they declare their dependencies, so the app opens with everything it
needs already in place.

## What you need

| | Version |
|---|---|
| Rust | stable, 1.77 or newer |
| Node.js | 22 or newer |
| pnpm | 10 (`corepack enable pnpm`) |

Plus the WebKitGTK stack Tauri links against, and the programs syncparty
drives at runtime.

**Debian / Ubuntu**

```bash
sudo apt install build-essential curl file git pkg-config \
  libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev \
  libayatana-appindicator3-dev libssl-dev libxdo-dev patchelf \
  syncplay mpv python3 python3-twisted
```

**Fedora**

```bash
sudo dnf install @development-tools webkit2gtk4.1-devel gtk3-devel \
  librsvg2-devel libayatana-appindicator-gtk3-devel openssl-devel \
  libxdo-devel patchelf syncplay mpv python3 python3-twisted
```

**Arch**

```bash
sudo pacman -S base-devel webkit2gtk-4.1 gtk3 librsvg \
  libayatana-appindicator xdotool syncplay pyside6 shiboken6 \
  mpv python python-twisted
```

`pyside6` and `shiboken6` are separate because Arch ships them as *optional*
dependencies of `syncplay` — without them `syncplay` installs a client with no
window.

Substitute `vlc` for `mpv` anywhere above if you prefer it; syncparty accepts
either and prefers mpv when both are present.

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
git clone https://github.com/Tahckn/syncparty.git
cd syncparty
pnpm install
node scripts/fetch-syncplay.mjs
pnpm tauri build
```

`fetch-syncplay.mjs` vendors the pinned Syncplay server into
`src-tauri/syncplay-source/`, verifying it against the SHA-256 recorded in
`src-tauri/src/core/deps/server_runtime.rs`. It is required for a packaged
build. If you skip it and run `pnpm tauri dev` instead, the app downloads the
same archive into its data directory on first host — the step exists so the
package can carry the server rather than fetch it on a user's machine.

Why the server is vendored at all: no distribution packages a new enough one.
syncparty binds the Syncplay server to loopback with `--ipv4-only` and
`--interface-ipv4`, which arrived in Syncplay 1.7.1, and the newest package in
any Ubuntu LTS is 1.7.0. Arch and Fedora do ship 1.7.6, but one code path
across every distribution beats a version check that behaves differently
depending on where it runs. Only Twisted comes from your distribution.

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

The Rust tests regenerate `src/shared/types/*.ts` through ts-rs. If `git
status` shows changes there after a run, commit them — they are part of the
source, not build output.
