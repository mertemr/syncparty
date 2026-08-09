# Contributing

Thanks for taking a look. Issues and pull requests are both welcome.

## Fork and pull request workflow

You do not need to be added as a collaborator to contribute — fork the repo
and open a pull request against `main`:

```bash
gh repo fork Tahckn/syncparty --clone
cd syncparty
git checkout -b my-change
# ... make your change ...
git push -u origin my-change
gh pr create --fill
```

(No `gh`? Fork from the GitHub UI, `git clone` your fork, and open the PR
from there — same result.)

A few things that trip people up the first time:

- **CI does not run automatically on a first-time contributor's PR.** GitHub
  holds fork-originated workflow runs for approval until a maintainer clicks
  "Approve and run" on the Actions tab. This is a GitHub security default, not
  something specific to this repo, and it is expected — ping the PR if it has
  been a while.
- **A PR cannot merge until CI is green.** `main` requires the Backend
  (Windows), Backend (macOS) and Frontend checks to pass before the merge
  button unlocks.
- Keep the branch scoped to one change. Unrelated formatting or refactors in
  the same PR make it harder to review.

## Getting set up

You need [Rust](https://rustup.rs), Node 20+, pnpm, and the
[Tauri prerequisites](https://tauri.app/start/prerequisites/) for your
platform (MSVC Build Tools on Windows, Xcode command line tools on macOS).

```bash
pnpm install
pnpm tauri dev
```

## Before you open a pull request

```bash
cd src-tauri
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

```bash
pnpm build   # runs tsc, so this is the frontend type check too
```

CI runs all of the above on Windows and macOS.

## Cutting a release

Keep the version bump in the same pull request as the release changes. Update
the version in `package.json`, `src-tauri/Cargo.toml`, and
`src-tauri/tauri.conf.json` before that pull request's final CI run. After it is
green and merged, create and push the matching tag:

```bash
git switch main
git pull --ff-only
git tag v0.4.0
git push origin v0.4.0
```

The protected branch has already passed the required Windows, macOS, and
frontend checks, so merging does not run the same CI suite again. The tag starts
one release workflow: it first verifies all version declarations, builds the
three native packages in parallel, and publishes the draft only after every
package is ready. Failed release jobs can be re-run from GitHub Actions; do not
create a second tag for the same version.

The same tag independently starts `docker.yml`, which publishes the container
image to `ghcr.io/tahckn/syncparty`. The two share nothing, so one failing does
not hold up the other — but re-run a tag that produced installers and no image,
since `docker-compose.yml` points at `:latest`.

## How the code is arranged

The rule that matters: **`core` must not depend on Tauri.**

```
src-tauri/src/
  main.rs           the windowed app
  bin/syncpartyd.rs the headless host — same core, no window
  ipc/              Tauri commands and the event bridge — thin, delegates to core
  core/             everything else, testable with plain `cargo test`
```

If you find yourself importing `tauri::` inside `core`, the logic belongs
somewhere else, or the dependency needs to go behind a trait. `EventBus` and
`ProgressSink` in `core/events.rs` exist for exactly that reason.

Two feature sets come out of the same crate:

```bash
cargo clippy --all-targets -- -D warnings                        # desktop
cargo clippy --no-default-features --features headless \
      --all-targets -- -D warnings                               # container
```

The headless set drops Tauri, the webview and the OS keychain, so anything
added to `core` has to compile without all three. Run the second command
before opening a PR that touches `core`; CI does the same.

Concretely:

- **A new external program syncparty depends on** — implement `Dependency`
  in `core/deps/`, register it in `DependencyManager::standard`. Give it a
  working `manual_url()`; a user must never hit a dead end.
- **A new backend capability** — put the logic in `core/`, add a one-line
  handler in `ipc/commands.rs`, register it in the `generate_handler!` list.
- **A new thing the UI needs to be told about** — add a variant to `AppEvent`.

## Generated types

`src/shared/types/*.ts` is generated from the Rust types by `ts-rs` whenever
`cargo test` runs. Do not edit those files; change the Rust type and re-run
the tests. CI fails if they drift.

## Working on the protocol

`core/syncplay/protocol.rs` mirrors Syncplay's wire format. A mismatch there
fails quietly — the monitor connects and simply shows nothing — so please
check any change against the
[Syncplay source](https://github.com/Syncplay/syncplay) rather than inferring
the shape, and add a test with a real captured message.

Two properties the tests guard, both worth keeping:

- The monitor never sends `playstate`. If it did, it could pause, unpause or
  seek everybody's film.
- The server password and salt never appear in `argv`.

## Adding strings

Add the key to `en` in `src/shared/i18n/messages.ts` first. `Messages` is
derived from it, so TypeScript will then tell you the Turkish translation is
missing.
