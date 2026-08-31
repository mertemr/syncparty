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
  syncplay mpv
```

**Fedora**

```bash
sudo dnf install @development-tools webkit2gtk4.1-devel gtk3-devel \
  librsvg2-devel libayatana-appindicator-gtk3-devel openssl-devel \
  libxdo-devel patchelf syncplay mpv
```

**Arch**

```bash
sudo pacman -S base-devel webkit2gtk-4.1 gtk3 librsvg \
  libayatana-appindicator xdotool syncplay pyside6 shiboken6 mpv
```

`pyside6` and `shiboken6` are separate because Arch ships them as *optional*
dependencies of `syncplay` — without them `syncplay` installs a client with no
window.

Substitute `vlc` for `mpv` anywhere above if you prefer it; syncparty accepts
either and prefers mpv when both are present.

`syncplay` is in those lists as the *client* only. The server a party runs on
is built into the syncparty binary, so hosting needs no Python, no Twisted, and
no `syncplay` server process.

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

One backend test is ignored by default because it needs a Syncplay client on
PATH: it starts the built-in server, points a real `syncplay` at it, and
asserts the client shows up in the room. It is the only test that checks our
idea of the protocol against something we did not write, so run it if you have
the client installed:

```bash
cd src-tauri && cargo test --test syncplay_compatibility -- --ignored
```

The Rust tests regenerate `src/shared/types/*.ts` through ts-rs. If `git
status` shows changes there after a run, commit them — they are part of the
source, not build output.
