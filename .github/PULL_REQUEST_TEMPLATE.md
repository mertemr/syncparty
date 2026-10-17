## What this does

<!-- One or two sentences. Link an issue if there is one. -->

## Why

<!-- What problem this solves, or what prompted it. -->

## Checked

- [ ] `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test`
      pass in `src-tauri`
- [ ] `pnpm build` passes (this also runs `tsc`)
- [ ] If a Rust type used by the frontend changed, `src/shared/types` was
      regenerated (`cargo test` does this) and committed, and the file of any
      removed type deleted by hand
- [ ] If this touches `core/syncplay/protocol.rs`, it was checked against the
      [Syncplay source](https://github.com/Syncplay/syncplay), not inferred —
      both ends are ours now, so nothing else will notice a wrong field
- [ ] If this touches the sync client, the mpv driver or the protocol,
      `cargo test --test native_watch_e2e -- --ignored` and
      `cargo test --lib player::mpv -- --ignored` pass with at least one test
      each (need mpv on PATH)
- [ ] If this changes how mpv (or anything else) is started, the party
      password still never reaches its command line or environment, and the
      mpv IPC endpoint is still private to the user
