# syncparty UI redesign — design

Date: 2026-08-14
Status: approved for planning

## Goal

The frontend works but has four problems the owner named together: it looks
templated, the three-step flow confuses people, spacing and typography drift
between screens, and nothing about it feels finished. This redesign gives the
app a real visual identity, a token system the screens actually obey, and one
fewer surface to walk through.

Non-goal: changing what the app does. No Rust, no IPC command changes, no new
npm dependency.

## Constraints

- **No new dependencies.** Tailwind 4's `@theme` is the token layer; the eight
  hand-rolled primitives stay hand-rolled. A component library would ship ~40
  transitive packages to restyle six screens, and shadcn's own look is the
  templated feel this redesign exists to remove.
- **Fonts ship inside the bundle.** The app runs offline and enforces a CSP;
  webfonts live in `public/fonts/` as woff2, never a CDN.
- **The Rust side is untouched.** Every new element renders data the frontend
  already receives: `SessionState`, `RoomSnapshot`, `WatchedFile`, `Invite`,
  `PreflightReport`.
- **Existing tests stay green.** `lobbyState.test.ts` and
  `diagnosticsReport.test.ts` cover logic this redesign does not move.

## Direction: retro CRT / VHS, as an accent layer

The skeleton stays modern and legible. Retro treatment lands only where it is
seen and not read: the logo, display headings, empty states, loading, focus,
transitions. Body text is untouched — a scanline over a paragraph is a
novelty for ten seconds and a headache for an hour.

### Palette

Amber-and-midnight is replaced by tube black with a chromatic accent pair.
Amber survives as the `warn` tone.

| Token | Value | Role |
| --- | --- | --- |
| `--color-canvas` | `oklch(0.13 0.02 285)` | tube black, faint blue-violet cast |
| `--color-surface` | `oklch(0.18 0.025 285)` | panels |
| `--color-surface-raised` | `oklch(0.225 0.03 285)` | nested surfaces, inputs |
| `--color-line` | `oklch(0.32 0.035 285)` | borders |
| `--color-ink` / `-muted` / `-faint` | unchanged lightness ramp, hue retuned to 285 | text |
| `--color-accent` | `oklch(0.72 0.21 350)` | phosphor magenta — primary action |
| `--color-accent-strong` | `oklch(0.66 0.23 350)` | hover |
| `--color-accent-ink` | `oklch(0.16 0.03 350)` | text on accent |
| `--color-chroma` | `oklch(0.82 0.13 200)` | cyan — chromatic offset, live state, never a button |
| `--color-good` / `warn` / `bad` | existing hues, chroma raised ~15% | status |

CRT phosphor is saturated. Muted status colours read as washed-out here, so
they gain chroma rather than losing it.

### Typography

Three roles, all OFL, self-hosted:

- **Display — Archivo (variable, expanded axis).** Wide bold grotesk, the
  cassette-sleeve voice. `h1`/`h2` and eyebrows only.
- **Mono — Departure Mono.** Invite codes, endpoints, versions, logs, the
  session counter. Its bitmap origin supplies the retro terminal feel for
  free.
- **Body — system sans stack (unchanged).** Legibility is not negotiable here.

### Radius

`--radius-panel: 1.125rem` is the single strongest "generic SaaS card" signal
in the current UI. Retro hardware is sharp:

- `--radius-panel: 0.5rem`
- `--radius-control: 0.25rem`
- badges, dots and the toggle stay fully round

### Retro layer (CSS only, no JS)

Utility classes in `styles.css`:

- `.crt-scan` — 2px repeating-linear-gradient, opacity ~0.035. Applied to the
  app background only, never inside a panel.
- `.crt-vignette` — inset radial shadow at the window edge, hinting at tube
  curvature.
- `.chroma` — 1px magenta/cyan text-shadow split on display headings. Never on
  body copy.
- `.phosphor` — glow on live or active elements (hosting badge, primary
  button).
- `.tracking-glitch` — short horizontal jitter keyframe, fired on state
  transitions only (party started, guest joined). Never looping.

All of the above collapse to nothing under
`@media (prefers-reduced-motion: reduce)`, including `.crt-scan`.

### New token groups

Spacing scale, radius scale, shadow scale, motion durations and easings
(`--ease-crt`, 120/200/320ms), z-index layers. These values are currently
scattered as literals across the screens, which is the direct cause of the
inconsistency complaint.

## Interface metaphor: the app is a tape deck

Identity is carried by behaviour, not only colour. Every element below renders
data the frontend already has.

- The main surface is a **deck**. Hosting is the recording side, with a
  pulsing magenta REC dot; guest is the playing side.
- The invite code is a **cassette label** — a narrow mono strip that stamps
  itself when copied.
- The room roster is a **channel list**: one row per person, the file they
  opened, ready state. A person on a different file is marked `TRACKING
  ERROR` — the retro form of the existing desync warning, and far more legible
  than the current one.
- Loading states **rewind**: a band sweeping left to right. No spinners.
- Session duration renders as a mono **counter** (`00:42:17`). It does not
  exist today and is the cheapest thing that keeps the party screen alive.

## Flow: three steps become two surfaces

`StepTrail` is deleted. Two of the three steps were ceremony.

### Surface 1 — Deck (launch)

Two large slots: `HOST` and `JOIN`. The dependency check is no longer its own
screen but a **quiet system strip** beneath them:

- Everything present: one mono line, `SYSTEM READY — syncplay 1.7.2 · mpv 0.38`.
- Something missing: the strip expands into the per-dependency rows (install,
  locate, manual link, mpv/VLC choice) that `Preflight` renders today, and the
  two slots stay disabled until it clears.

The guarantee is identical — the app still refuses to start hosting without
Syncplay — but one screen disappears.

A guest arriving through a `syncparty://` link lands directly on the join
card and never sees the chooser.

### Surface 2 — Party

Host and guest share one layout: deck status and the primary action on the
left, channel list on the right. Today the host renders four stacked cards and
the guest renders one, and they do not read as the same application.

### Consequences

- `src/app/StepTrail.tsx` — deleted.
- `src/features/onboarding/autoContinue.ts` and its test — deleted. Auto-skip
  exists to bypass a gate that no longer exists.
- `App.tsx` loses `setupConfirmed`, `rechoosingMode` and `autoSkipSpent`;
  `showSettings` and `confirmingLeave` remain.
- `skipSetupWhenReady` becomes meaningless. The Rust settings field stays (no
  backend change); the control is removed from the settings screen.
- Message keys `nav.step.*`, `nav.steps`, `preflight.skipWhenReady` are
  removed; the deck, system strip, channel list and counter add new keys in
  both `en` and `tr`.

## Component layer

`src/shared/ui/index.tsx` (317 lines, one file) splits into `src/shared/ui/`
with one file per primitive and a barrel `index.ts` that keeps every existing
import path working.

Existing, restyled: `Button`, `Card`, `PageHeader`, `Badge`, `Dot`, `Input`,
`Field`, `Toggle`, `Choice`, `CopyRow`, `cx`.

New:

- `Logo` — mark and wordmark, `currentColor`.
- `Deck` — the framed status block with its REC/PLAY state.
- `ChannelRow` — one participant in the roster.
- `Rewind` — the loading band that replaces every "…" placeholder.
- `Counter` — mono elapsed time.
- `SystemStrip` — the collapsed/expanded dependency surface.
- `EmptyState` — currently open-coded three different ways.

## Logo

**Mark.** Two cassette reels as overlapping circles forming a sync loop; the
negative space at their intersection is a play triangle. Two reels, two
viewers, in sync. Single-colour safe, legible at 16px.

**Wordmark.** Archivo Expanded Bold, lowercase `syncparty`. A chroma-split
variant is used on the splash only.

**Delivery.** `src/shared/ui/Logo.tsx` for in-app use; a 1024px source PNG
regenerates all of `src-tauri/icons/` through `tauri icon`.

## Verification

- `pnpm test` — the existing suite stays green; deleted tests are the two
  belonging to deleted logic.
- `pnpm build` — `tsc` plus `vite build`, clean.
- The vite dev server is driven in a browser and each surface is captured:
  chooser, system strip expanded, host party, guest party, settings, plus one
  pass at `prefers-reduced-motion` and one at the smallest supported window
  size. Screenshots are the evidence; "it works" is not.
