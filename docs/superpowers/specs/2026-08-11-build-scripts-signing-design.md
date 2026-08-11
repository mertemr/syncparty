# Local build script and Windows code signing

Date: 2026-08-11

## Problem

Handing a build to a tester currently means remembering a sequence of flags.
`pnpm tauri build` fails outright with `createUpdaterArtifacts: true` unless a
signing key is in the environment, so the working command is longer than anyone
will retype correctly. Three separate version declarations must agree before a
tag is pushed, and today nothing checks that locally — `release.yml` catches it
only after the tag exists, when the fix is a second commit and a moved tag.

The release pipeline itself is not the problem. `.github/workflows/release.yml`
already validates versions, builds Windows and both macOS targets in parallel,
and publishes the draft only after every platform succeeds. What is missing sits
on either side of it: a repeatable local build, and a signed installer.

## Scope

In scope:

- `scripts/build-local.mjs`, exposed as `pnpm build:local`
- Preflight checks that fail before a long compile rather than during it
- Opt-in Windows code signing wired through the same script
- Documented self-signed certificate creation, and the migration path off it

Out of scope, deliberately:

- A version bump tool. The three-file edit is rare and already documented.
- Stamping or hashing the output for distribution.
- macOS notarization.
- Touching `release.yml`. See "CI integration" below for why.

## Design

### The script

`scripts/build-local.mjs` runs on Node with no new dependencies, so it works on
the macOS side of the contributor matrix as well as Windows.

**Preflight.** Every check runs before any of them reports, so a misconfigured
machine surfaces its problems in one pass instead of one per attempt:

- `cargo`, `rustc`, `node`, and `pnpm` resolve on `PATH`
- on Windows, the MSVC linker is present (located via `vswhere`) — without it
  the build fails at link time, half an hour in
- `package.json`, `src-tauri/Cargo.toml`, and `src-tauri/tauri.conf.json`
  declare the same version, mirroring the `validate` job in `release.yml`
- no stray `Cargo.toml` sits at the repository root — a `cargo init` run in the
  wrong directory produces one, along with a `src/main.rs` that lands in the
  frontend source tree

**Build.** `pnpm tauri build --bundles nsis` on Windows, `--bundles dmg` on
macOS, with `scripts/tauri.local.conf.json` layered on top via `--config`. That
override file exists for one reason: `createUpdaterArtifacts: false`. A local
build has no updater to feed and no key to sign one with.

**Output.** The bundle path and its size, on one line.

**Flags.** `--sign`, `--target <triple>`, `--bundles <list>`.

### Signing

Tauri invokes `signtool` itself. It needs the certificate's thumbprint, not the
certificate, so no signing step is written — three configuration fields are
injected at build time:

| Field | Source |
| --- | --- |
| `bundle.windows.certificateThumbprint` | `SYNCPARTY_CERT_THUMBPRINT` |
| `bundle.windows.digestAlgorithm` | `sha256` |
| `bundle.windows.timestampUrl` | `http://timestamp.digicert.com` |

`--sign` reads the environment variable and passes all three through `--config`.
Nothing machine-specific is committed: not the thumbprint, not the `.pfx`.

Signing is **opt-in**. The default build must succeed on a machine with no
certificate installed, because that is the common case and because a failure
there would be indistinguishable from a real build break.

The timestamp URL is not optional detail. A signature without one becomes
invalid when the certificate expires; a timestamped signature stays valid for
everything built before expiry.

Certificate creation stays in documentation rather than in a script, because it
is a one-time Windows-only operation and folding PowerShell into a
cross-platform Node script to run it once would not pay for itself:

```powershell
New-SelfSignedCertificate -Type CodeSigningCert `
  -Subject "CN=Taha Ceken" `
  -CertStoreLocation Cert:\CurrentUser\My `
  -NotAfter (Get-Date).AddYears(3)
```

### What self-signing does and does not buy

It does not remove the SmartScreen warning. Windows does not trust a certificate
you issued to yourself. A tester still sees "unknown publisher" unless they
import the `.cer` into their own Trusted Root store — which means trusting
everything that certificate ever signs on their machine. That is a reasonable
ask of two or three people you know, and an unreasonable one of anybody else.

What it does buy: a stable publisher identity, a signing path that is exercised
and debugged before it matters, and a migration that costs a credential swap
rather than a redesign. The three configuration fields above are the same ones
Azure Trusted Signing uses.

### Options, for when self-signed stops being enough

| Option | Cost | SmartScreen | Approval |
| --- | --- | --- | --- |
| Self-signed | free | still warns | none |
| Azure Trusted Signing | ~$10/month | clean | identity verification; individuals need a 3-year verifiable history |
| OV certificate | ~$200-400/year | warns until reputation accrues | organisation validation |
| EV certificate | ~$400+/year | clean immediately | organisation validation, hardware token |

Azure Trusted Signing is the recommended target: EV-level trust without the
token, and the only per-month rather than per-year option.

### CI integration

Deferred, by decision rather than by omission.

The shape it would take: a step in the Windows job that, when
`WINDOWS_CERT_PFX_BASE64` is a non-empty secret, decodes the `.pfx`, imports it
into the runner's certificate store, and writes the thumbprint to `GITHUB_ENV`
for the `tauri-action` arguments to pick up. When the secret is absent the step
skips itself and the release is produced exactly as it is today.

It is not being written now because putting a self-signed `.pfx` into repository
secrets buys nothing — the resulting installer still warns — while adding a
credential to rotate. The work is roughly fifteen minutes once a real
certificate exists.

Two keys will then be in play, and they are unrelated:

- `TAURI_SIGNING_PRIVATE_KEY` signs the **update package**, so the running
  application will accept an update. Already configured.
- `WINDOWS_CERT_PFX_BASE64` signs the **installer**, so Windows can name the
  publisher. Not yet configured.

Losing either one leaves the other working.

## Documentation changes

`CONTRIBUTING.md` gains a short "Local test build" section immediately before
"Cutting a release": the command, what it produces, and that it is unsigned.

## Testing

The script's preflight logic is unit-testable under the existing `vitest` setup:
version comparison and stray-root-manifest detection are pure functions over
file contents. The build invocation itself is not unit-tested — it shells out to
`tauri`, and a test that mocks that boundary would assert only that the script
constructs the arguments it was written to construct.

The end-to-end check is manual and already performed once by hand: the command
produces `syncparty_0.3.2_x64-setup.exe`, 7.1 MB, which installs and runs.
