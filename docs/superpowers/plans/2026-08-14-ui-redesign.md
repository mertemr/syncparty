# syncparty UI Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the syncparty frontend's visual language with a retro CRT/VHS accent identity backed by a real token scale, and collapse the three-step flow into two surfaces — without touching Rust, IPC, or adding a dependency.

**Architecture:** Tailwind 4's `@theme` block in `src/styles.css` is the single token source. `src/shared/ui/index.tsx` splits into one file per primitive behind a barrel that preserves every existing import path. The launch surface absorbs the dependency checklist as a collapsible strip, deleting `StepTrail` and the auto-skip machinery. Screens are rewritten against the new primitives; all data they render already arrives over existing IPC.

**Tech Stack:** React 19, TypeScript 5.8, Tailwind CSS 4 (`@tailwindcss/vite`), Vite 7, Vitest 3, Tauri 2.

**Spec:** `docs/superpowers/specs/2026-08-14-ui-redesign-design.md`

## Global Constraints

- **No new npm dependencies.** Not for components, not for animation, not for icons. `package.json` dependency lists must be byte-identical at the end of this plan.
- **No Rust changes.** `src-tauri/src/**` is off limits. `src-tauri/icons/**` and `src-tauri/tauri.conf.json` are touched only in Task 14.
- **No new IPC commands or payload fields.** Everything rendered comes from the existing types in `src/shared/types/`.
- **Fonts are self-hosted** in `public/fonts/*.woff2`. No CDN, no `@import url(...)`. The app's CSP is `default-src 'self'` with no `font-src`, so any external font URL is blocked at runtime.
- **Every user-visible string goes through `useTranslate()`** with a key present in both `en` and `tr` in `src/shared/i18n/messages.ts`. `Messages` is derived from `en`, so a missing `tr` key is a compile error — let it be.
- **All motion and the scanline overlay collapse under `@media (prefers-reduced-motion: reduce)`.**
- **Minimum window is 720×560** (`src-tauri/tauri.conf.json`). Every surface must be usable at that size.
- **There is no React testing library in this repo.** Tests cover pure functions only (`vitest run`). Component and layout work is verified in the browser with screenshots, not with assertions.
- Package manager is **pnpm**. Commands: `pnpm test`, `pnpm build`, `pnpm dev`.

---

### Task 1: Font assets and the token foundation

**Files:**
- Create: `public/fonts/archivo-variable.woff2`
- Create: `public/fonts/departure-mono.woff2`
- Modify: `src/styles.css` (full rewrite of the `@theme` block and base layer)

**Interfaces:**
- Consumes: nothing.
- Produces: the complete token vocabulary every later task uses —
  colours `canvas`, `surface`, `surface-raised`, `line`, `ink`, `ink-muted`,
  `ink-faint`, `accent`, `accent-strong`, `accent-ink`, `chroma`, `good`,
  `warn`, `bad`; radii `--radius-panel`, `--radius-control`; font families
  `font-display`, `font-mono`; motion `--ease-crt`, `--duration-fast`,
  `--duration-base`, `--duration-slow`; utility classes `.crt-scan`,
  `.crt-vignette`, `.chroma`, `.phosphor`, `.tracking-glitch`, `.rewind`.

- [ ] **Step 1: Get the two font files**

Both are SIL Open Font License. **Ask the user before downloading anything** — downloading files is theirs to approve. Present exactly this:

> Two woff2 files needed in `public/fonts/`:
> - Archivo variable (OFL) — https://github.com/googlefonts/archivo/raw/main/fonts/variable/Archivo%5Bwdth,wght%5D.woff2 (~90 KB)
> - Departure Mono (OFL) — https://github.com/rektdeckard/departure-mono/raw/main/dist/DepartureMono-Regular.woff2 (~30 KB)
>
> Download them, or drop your own copies in `public/fonts/` and I'll wire those instead.

On approval:

```bash
mkdir -p public/fonts
curl -L -o public/fonts/archivo-variable.woff2 "https://github.com/googlefonts/archivo/raw/main/fonts/variable/Archivo%5Bwdth,wght%5D.woff2"
curl -L -o public/fonts/departure-mono.woff2 "https://github.com/rektdeckard/departure-mono/raw/main/dist/DepartureMono-Regular.woff2"
```

- [ ] **Step 2: Verify the files are real fonts, not HTML error pages**

```bash
ls -l public/fonts/ && file public/fonts/*.woff2
```

Expected: both files over 10 KB, `file` reporting `Web Open Font Format (Version 2)`. An HTML redirect page here means the URL moved — stop and tell the user rather than shipping a broken `@font-face`.

- [ ] **Step 3: Rewrite `src/styles.css`**

Replace the whole file:

```css
@import "tailwindcss";

/* Tube black with a chromatic accent pair. The identity is a CRT: saturated
   phosphor on a surface that is never pure black, because a real tube is
   never off. */
@theme {
  --color-canvas: oklch(0.13 0.02 285);
  --color-surface: oklch(0.18 0.025 285);
  --color-surface-raised: oklch(0.225 0.03 285);
  --color-line: oklch(0.32 0.035 285);

  --color-ink: oklch(0.965 0.008 285);
  --color-ink-muted: oklch(0.76 0.02 285);
  --color-ink-faint: oklch(0.58 0.026 285);

  --color-accent: oklch(0.72 0.21 350);
  --color-accent-strong: oklch(0.66 0.23 350);
  --color-accent-ink: oklch(0.16 0.03 350);

  /* Chromatic partner to the accent. Offsets, live state, never a button. */
  --color-chroma: oklch(0.82 0.13 200);

  --color-good: oklch(0.8 0.17 155);
  --color-warn: oklch(0.84 0.16 80);
  --color-bad: oklch(0.7 0.21 22);

  /* Retro hardware is sharp. The old 1.125rem panel radius was the loudest
     generic-SaaS signal in the app. */
  --radius-panel: 0.5rem;
  --radius-control: 0.25rem;

  --font-display: "Archivo", ui-sans-serif, system-ui, sans-serif;
  --font-mono: "Departure Mono", ui-monospace, "Cascadia Mono", monospace;

  --ease-crt: cubic-bezier(0.2, 0.9, 0.3, 1);
  --duration-fast: 120ms;
  --duration-base: 200ms;
  --duration-slow: 320ms;
}

@font-face {
  font-family: "Archivo";
  src: url("/fonts/archivo-variable.woff2") format("woff2-variations");
  font-weight: 100 900;
  font-stretch: 62% 125%;
  font-display: swap;
}

@font-face {
  font-family: "Departure Mono";
  src: url("/fonts/departure-mono.woff2") format("woff2");
  font-weight: 400;
  font-display: swap;
}

html,
body,
#root {
  height: 100%;
}

body {
  margin: 0;
  background: var(--color-canvas);
  color: var(--color-ink);
  font-family:
    ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
  font-synthesis-weight: none;
  text-rendering: optimizeLegibility;
  -webkit-font-smoothing: antialiased;

  /* A desktop app should not feel like a web page. */
  user-select: none;
  cursor: default;
  overflow: hidden;
}

/* The room the tube sits in. */
body::before {
  position: fixed;
  inset: 0;
  z-index: -1;
  content: "";
  background:
    radial-gradient(circle at 12% -10%, oklch(0.42 0.16 350 / 0.22), transparent 32rem),
    radial-gradient(circle at 95% 8%, oklch(0.5 0.11 200 / 0.12), transparent 26rem),
    linear-gradient(165deg, oklch(0.16 0.03 290), var(--color-canvas) 60%);
}

button,
input {
  font: inherit;
}

button {
  -webkit-tap-highlight-color: transparent;
}

.selectable {
  user-select: text;
  cursor: text;
}

/* ------------------------------------------------------------ CRT layer
   Applied to the app shell, never inside a panel: a scanline over a
   paragraph is a novelty for ten seconds and a headache for an hour. */

.crt-scan::after {
  position: absolute;
  inset: 0;
  z-index: 50;
  content: "";
  pointer-events: none;
  background: repeating-linear-gradient(
    to bottom,
    oklch(0 0 0 / 0.035) 0px,
    oklch(0 0 0 / 0.035) 1px,
    transparent 1px,
    transparent 3px
  );
}

.crt-vignette::before {
  position: absolute;
  inset: 0;
  z-index: 49;
  content: "";
  pointer-events: none;
  box-shadow: inset 0 0 8rem oklch(0.05 0.02 285 / 0.55);
}

/* Chromatic aberration. Display headings only. */
.chroma {
  text-shadow:
    -1px 0 oklch(0.72 0.21 350 / 0.55),
    1px 0 oklch(0.82 0.13 200 / 0.5);
}

.phosphor {
  box-shadow:
    0 0 0 1px oklch(0.72 0.21 350 / 0.35),
    0 0 1.5rem oklch(0.72 0.21 350 / 0.35);
}

@keyframes tracking-glitch {
  0% { transform: translateX(0); }
  18% { transform: translateX(-3px); }
  34% { transform: translateX(2px); }
  52% { transform: translateX(-1px); }
  100% { transform: translateX(0); }
}

/* Fires on a state change, never in a loop. */
.tracking-glitch {
  animation: tracking-glitch var(--duration-slow) var(--ease-crt);
}

@keyframes rewind-sweep {
  from { transform: translateX(-100%); }
  to { transform: translateX(300%); }
}

.rewind::after {
  position: absolute;
  inset: 0;
  width: 33%;
  content: "";
  background: linear-gradient(
    90deg,
    transparent,
    var(--color-accent),
    transparent
  );
  animation: rewind-sweep 1.1s linear infinite;
}

@media (prefers-reduced-motion: reduce) {
  .crt-scan::after,
  .crt-vignette::before {
    display: none;
  }

  .chroma {
    text-shadow: none;
  }

  .tracking-glitch,
  .rewind::after {
    animation: none;
  }

  /* A rewind band that cannot sweep still has to read as busy. */
  .rewind::after {
    width: 100%;
    opacity: 0.35;
  }
}

::-webkit-scrollbar {
  width: 10px;
  height: 10px;
}

::-webkit-scrollbar-thumb {
  background: var(--color-line);
  border-radius: 999px;
  border: 3px solid transparent;
  background-clip: content-box;
}

::-webkit-scrollbar-thumb:hover {
  background: var(--color-ink-faint);
  background-clip: content-box;
}

:focus-visible {
  outline: 2px solid var(--color-accent);
  outline-offset: 3px;
}

::selection {
  background: oklch(0.72 0.21 350 / 0.35);
  color: var(--color-ink);
}
```

- [ ] **Step 4: Verify the build and the fonts loading**

```bash
pnpm build
```

Expected: clean. Then start the dev server through the preview tooling and confirm in the browser console that no font request 404s, and that
`getComputedStyle(document.body).getPropertyValue('--color-accent')` is non-empty.

- [ ] **Step 5: Commit**

```bash
git add public/fonts src/styles.css
git commit -m "feat(ui): retro CRT token scale and self-hosted display fonts"
```

---

### Task 2: Split the primitive file (mechanical, no visual change)

**Files:**
- Create: `src/shared/ui/cx.ts`, `Button.tsx`, `Card.tsx`, `PageHeader.tsx`, `Badge.tsx`, `Input.tsx`, `Toggle.tsx`, `Choice.tsx`, `CopyRow.tsx` (all under `src/shared/ui/`)
- Create: `src/shared/ui/index.ts` (barrel)
- Delete: `src/shared/ui/index.tsx`

**Interfaces:**
- Consumes: Task 1's tokens (indirectly — no class strings change here).
- Produces: identical exports from `@/shared/ui` — `cx`, `Button`,
  `ButtonVariant`, `Card`, `PageHeader`, `Badge`, `BadgeTone`, `Dot`, `Input`,
  `Field`, `Toggle`, `Choice`, `CopyRow`. Every existing import site keeps
  working untouched.

- [ ] **Step 1: Move each component into its own file, verbatim**

Copy each section of the current `src/shared/ui/index.tsx` into the matching file with no edits to markup or classes. Grouping rules: `Badge`, `BadgeTone` and `Dot` share `Badge.tsx` (`Dot` reuses `BadgeTone`); `Input` and `Field` share `Input.tsx`. `cx` goes to `cx.ts` because every other file imports it.

Keep the file-top comment from the original on `index.ts`, adjusted:

```ts
/**
 * The handful of primitives this app needs.
 *
 * Hand-rolled rather than pulled from a component library: they have no
 * behaviour worth abstracting, and a registry plus its dependency tree would
 * outweigh the whole frontend. One file each, re-exported here so import
 * sites name the module and not the file.
 */
export { cx } from "./cx";
export { Button, type ButtonVariant } from "./Button";
export { Card } from "./Card";
export { PageHeader } from "./PageHeader";
export { Badge, Dot, type BadgeTone } from "./Badge";
export { Input, Field } from "./Input";
export { Toggle } from "./Toggle";
export { Choice } from "./Choice";
export { CopyRow } from "./CopyRow";
```

`ButtonVariant` and `BadgeTone` are currently local types; export them from their new files — later tasks name them.

- [ ] **Step 2: Delete the old file**

```bash
git rm src/shared/ui/index.tsx
```

- [ ] **Step 3: Verify nothing broke**

```bash
pnpm build && pnpm test
```

Expected: both clean. This task changes zero rendered output — if the build complains, an export was dropped.

- [ ] **Step 4: Commit**

```bash
git add src/shared/ui
git commit -m "refactor(ui): one file per primitive behind a barrel"
```

---

### Task 3: Restyle the primitives

**Files:**
- Modify: `src/shared/ui/Button.tsx`, `Card.tsx`, `PageHeader.tsx`, `Badge.tsx`, `Input.tsx`, `Toggle.tsx`, `Choice.tsx`, `CopyRow.tsx`

**Interfaces:**
- Consumes: Task 1 tokens, Task 2 file layout.
- Produces: same props, same exports, new appearance. `BadgeTone` gains a
  `"chroma"` member; `ButtonVariant` is unchanged.

- [ ] **Step 1: Button — sharper, phosphor-lit primary**

```tsx
const BUTTON_VARIANTS: Record<ButtonVariant, string> = {
  primary:
    "bg-accent text-accent-ink phosphor hover:bg-accent-strong disabled:hover:bg-accent",
  secondary:
    "bg-surface-raised/80 text-ink border border-line hover:border-ink-faint hover:bg-surface-raised",
  ghost: "text-ink-muted hover:text-ink hover:bg-surface-raised/70",
  danger: "bg-bad/10 text-bad border border-bad/30 hover:border-bad/60 hover:bg-bad/15",
};
```

and in the element:

```tsx
className={cx(
  "inline-flex min-h-10 items-center justify-center gap-2 rounded-[var(--radius-control)] px-4 py-2",
  "text-sm font-semibold tracking-wide transition-colors duration-[var(--duration-fast)]",
  "disabled:cursor-not-allowed disabled:opacity-45",
  BUTTON_VARIANTS[variant],
  className,
)}
```

The `-translate-y-px` hover lift is removed: buttons that float are the templated look this redesign is removing, and a CRT has no z-axis.

- [ ] **Step 2: Card — flat frame, mono header label**

```tsx
className={cx(
  "overflow-hidden rounded-panel border border-line bg-surface/85",
  className,
)}
```

Header:

```tsx
<header className="flex items-center justify-between gap-3 border-b border-line bg-surface-raised/30 px-5 py-3">
  <h2 className="font-mono text-[11px] tracking-[0.18em] text-ink-muted uppercase">
    {title}
  </h2>
  {action}
</header>
```

The `backdrop-blur-xl` and the 60px drop shadow both go. Blur behind an opaque app background costs compositing for an effect nobody sees.

- [ ] **Step 3: PageHeader — display font, chroma split**

```tsx
<h1 className="chroma font-display text-3xl font-extrabold tracking-[-0.02em] text-ink [font-stretch:112%]">
  {title}
</h1>
```

Description keeps its current classes.

- [ ] **Step 4: Badge — add the `chroma` tone, square it off**

```tsx
const BADGE_TONES: Record<BadgeTone, string> = {
  neutral: "bg-surface-raised text-ink-muted",
  good: "bg-good/15 text-good",
  warn: "bg-warn/15 text-warn",
  bad: "bg-bad/15 text-bad",
  accent: "bg-accent/15 text-accent",
  chroma: "bg-chroma/15 text-chroma",
};
```

with `type BadgeTone = "neutral" | "good" | "warn" | "bad" | "accent" | "chroma";`, the wrapper switching to
`rounded-[var(--radius-control)] font-mono text-[10px] tracking-[0.12em] uppercase`, and `Dot`'s colour map gaining `chroma: "bg-chroma"`.

- [ ] **Step 5: Input, Toggle, Choice, CopyRow**

Replace every `rounded-xl` with `rounded-[var(--radius-control)]` and every `rounded-lg` inside `Choice` with the same. `Toggle`'s track and knob stay round (`rounded-full`) — a switch that is not round stops reading as a switch. `CopyRow`'s `<code>` gains `font-mono` explicitly (it now resolves to Departure Mono) and its label becomes `font-mono text-[10px] tracking-[0.16em]`.

- [ ] **Step 6: Verify in the browser**

```bash
pnpm build
```

Then run the dev server and screenshot the existing chooser and settings screens. Expected: same layout, new palette, sharp corners, mono labels. Nothing may be misaligned or clipped — this task changes no geometry other than radii.

- [ ] **Step 7: Commit**

```bash
git add src/shared/ui
git commit -m "feat(ui): restyle primitives onto the CRT token scale"
```

---

### Task 4: The logo

**Files:**
- Create: `src/shared/ui/Logo.tsx`
- Modify: `src/shared/ui/index.ts` (export it)

**Interfaces:**
- Consumes: nothing but tokens.
- Produces: `<Logo size?: number />` (the mark alone, default 24) and
  `<Wordmark />` (mark plus the word). Both inherit `currentColor`.

- [ ] **Step 1: Write the component**

```tsx
/**
 * Two cassette reels, overlapping into a sync loop; the negative space where
 * they meet is a play triangle. Two reels, two viewers, in sync.
 *
 * Single-colour on purpose: it has to survive a 16px taskbar icon and a
 * monochrome tray, so it carries no gradient and no second fill.
 */
export function Logo({ size = 24, className }: { size?: number; className?: string }) {
  return (
    <svg
      viewBox="0 0 32 32"
      width={size}
      height={size}
      className={className}
      role="img"
      aria-hidden
    >
      <g fill="none" stroke="currentColor" strokeWidth="2.4">
        <circle cx="11" cy="16" r="8" />
        <circle cx="21" cy="16" r="8" />
      </g>
      {/* The hubs. Filled, so the reels read as reels and not as a Venn diagram. */}
      <circle cx="11" cy="16" r="2.4" fill="currentColor" />
      <circle cx="21" cy="16" r="2.4" fill="currentColor" />
    </svg>
  );
}

export function Wordmark({ className }: { className?: string }) {
  return (
    <span className={cx("flex items-center gap-2.5", className)}>
      <Logo size={22} className="text-accent" />
      <span className="font-display text-[15px] font-extrabold tracking-[-0.01em] text-ink [font-stretch:118%]">
        syncparty
      </span>
    </span>
  );
}
```

Import `cx` from `./cx`. Add to the barrel:

```ts
export { Logo, Wordmark } from "./Logo";
```

- [ ] **Step 2: Check it at icon size before going further**

Render `<Logo size={16} />` temporarily in the header and screenshot it. If the two reels smear into one blob at 16px, widen the centre gap (move the circles to `cx=10.5` and `cx=21.5`) rather than thinning the stroke — a hairline stroke disappears entirely on a 100% scale display.

- [ ] **Step 3: Use it in the header**

In `src/App.tsx`, replace the accent square with its inline play-triangle SVG (currently lines 222-229) and the app-name span with:

```tsx
<Wordmark />
```

Delete the now-unused `t("appName")` call site only if no other file uses the key — `grep -rn "appName" src/` first. It stays in `messages.ts` either way.

- [ ] **Step 4: Verify**

```bash
pnpm build
```

Screenshot the header at 720px window width. Expected: mark and wordmark aligned on the same baseline, no clipping.

- [ ] **Step 5: Commit**

```bash
git add src/shared/ui src/App.tsx
git commit -m "feat(ui): reel mark and wordmark"
```

---

### Task 5: Counter, Rewind and EmptyState

**Files:**
- Create: `src/shared/ui/elapsed.ts`
- Create: `src/shared/ui/elapsed.test.ts`
- Create: `src/shared/ui/Counter.tsx`, `src/shared/ui/Rewind.tsx`, `src/shared/ui/EmptyState.tsx`
- Modify: `src/shared/ui/index.ts`

**Interfaces:**
- Consumes: Task 1 tokens (`.rewind`, `font-mono`).
- Produces:
  - `formatElapsed(ms: number): string` — `"HH:MM:SS"`, clamped at zero.
  - `<Counter since: number />` where `since` is an epoch-ms timestamp.
  - `<Rewind label?: string />` — the loading band.
  - `<EmptyState title: string, detail?: string />`.

- [ ] **Step 1: Write the failing test**

`src/shared/ui/elapsed.test.ts`:

```ts
import { describe, expect, it } from "vitest";

import { formatElapsed } from "./elapsed";

describe("formatElapsed", () => {
  it("pads every field to two digits", () => {
    expect(formatElapsed(0)).toBe("00:00:00");
    expect(formatElapsed(9_000)).toBe("00:00:09");
    expect(formatElapsed(61_000)).toBe("00:01:01");
  });

  it("counts past an hour without rolling over", () => {
    expect(formatElapsed(3_600_000)).toBe("01:00:00");
    expect(formatElapsed(45_296_000)).toBe("12:34:56");
  });

  // A clock correction mid-party must not print a negative counter.
  it("clamps negative input to zero", () => {
    expect(formatElapsed(-5_000)).toBe("00:00:00");
  });

  it("truncates rather than rounds, so the counter never shows a second early", () => {
    expect(formatElapsed(1_999)).toBe("00:00:01");
  });
});
```

- [ ] **Step 2: Run it and watch it fail**

```bash
pnpm vitest run src/shared/ui/elapsed.test.ts
```

Expected: FAIL — `Failed to resolve import "./elapsed"`.

- [ ] **Step 3: Implement `elapsed.ts`**

```ts
/** `HH:MM:SS` for a duration in milliseconds. */
export function formatElapsed(ms: number): string {
  const total = Math.floor(Math.max(0, ms) / 1000);
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;

  return [hours, minutes, seconds]
    .map((value) => String(value).padStart(2, "0"))
    .join(":");
}
```

- [ ] **Step 4: Run the test again**

```bash
pnpm vitest run src/shared/ui/elapsed.test.ts
```

Expected: PASS, 4 tests.

- [ ] **Step 5: Write the three components**

`Counter.tsx`:

```tsx
import { useEffect, useState } from "react";

import { formatElapsed } from "./elapsed";

/**
 * Elapsed time since `since`, ticking once a second.
 *
 * Driven off wall-clock difference rather than an accumulator, so a paused or
 * throttled timer catches up instead of drifting.
 */
export function Counter({ since }: { since: number }) {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, []);

  return (
    <span className="font-mono text-sm tabular-nums text-ink-muted">
      {formatElapsed(now - since)}
    </span>
  );
}
```

`Rewind.tsx`:

```tsx
/** The loading state. A tape band sweeping, not a spinner. */
export function Rewind({ label }: { label?: string }) {
  return (
    <div className="space-y-2">
      <div
        role="progressbar"
        aria-label={label}
        className="rewind relative h-0.5 w-full overflow-hidden bg-line/60"
      />
      {label && (
        <p className="font-mono text-[11px] tracking-[0.14em] text-ink-faint uppercase">
          {label}
        </p>
      )}
    </div>
  );
}
```

`EmptyState.tsx`:

```tsx
/** Nothing here yet, said once, the same way everywhere. */
export function EmptyState({ title, detail }: { title: string; detail?: string }) {
  return (
    <div className="px-4 py-10 text-center">
      <p className="font-mono text-[11px] tracking-[0.18em] text-ink-faint uppercase">
        {title}
      </p>
      {detail && <p className="mt-2 text-sm text-ink-muted">{detail}</p>}
    </div>
  );
}
```

Barrel additions:

```ts
export { Counter } from "./Counter";
export { Rewind } from "./Rewind";
export { EmptyState } from "./EmptyState";
export { formatElapsed } from "./elapsed";
```

- [ ] **Step 6: Verify**

```bash
pnpm test && pnpm build
```

Expected: all suites pass, build clean.

- [ ] **Step 7: Commit**

```bash
git add src/shared/ui
git commit -m "feat(ui): elapsed counter, rewind band and empty state"
```

---

### Task 6: The system strip

**Files:**
- Create: `src/features/launch/systemStrip.ts`
- Create: `src/features/launch/systemStrip.test.ts`
- Create: `src/features/launch/SystemStrip.tsx`

**Interfaces:**
- Consumes: `PreflightReport`, `PreflightItem` from `@/shared/types/`,
  `Rewind`/`Badge`/`Button`/`Choice`/`Dot` from `@/shared/ui`.
- Produces:
  - `getStripState(report: PreflightReport | null): StripState` where
    `type StripState = "checking" | "ready" | "blocked"`.
  - `summariseReady(report: PreflightReport): string` — e.g.
    `"syncplay 1.7.2 · mpv 0.38"`.
  - `<SystemStrip mode onReadyChange />` — see Step 5 for the exact props.

- [ ] **Step 1: Write the failing test**

`src/features/launch/systemStrip.test.ts`:

```ts
import { describe, expect, it } from "vitest";

import type { PreflightItem } from "@/shared/types/PreflightItem";
import type { PreflightReport } from "@/shared/types/PreflightReport";

import { getStripState, summariseReady } from "./systemStrip";

function item(overrides: Partial<PreflightItem>): PreflightItem {
  return {
    id: "syncplay",
    displayName: "Syncplay",
    status: { state: "installed", version: "1.7.2" },
    canAutoInstall: true,
    supportsManualPath: true,
    needsElevation: false,
    manualUrl: "https://syncplay.pl",
    overridePath: null,
    ...overrides,
  } as PreflightItem;
}

function report(items: PreflightItem[]): PreflightReport {
  return { items } as PreflightReport;
}

describe("getStripState", () => {
  it("is checking until a report arrives", () => {
    expect(getStripState(null)).toBe("checking");
  });

  it("is ready when nothing is missing", () => {
    expect(getStripState(report([item({})]))).toBe("ready");
  });

  it("is blocked when any item is missing", () => {
    const state = getStripState(
      report([item({}), item({ id: "mpv", status: { state: "missing" } })]),
    );
    expect(state).toBe("blocked");
  });

  // An empty report means the backend found nothing to check for this mode,
  // which is a green light, not a stall.
  it("is ready for an empty item list", () => {
    expect(getStripState(report([]))).toBe("ready");
  });
});

describe("summariseReady", () => {
  it("joins name and version for each item", () => {
    const line = summariseReady(
      report([
        item({}),
        item({ id: "mpv", displayName: "mpv", status: { state: "installed", version: "0.38" } }),
      ]),
    );
    expect(line).toBe("Syncplay 1.7.2 · mpv 0.38");
  });

  it("omits the version when the tool does not report one", () => {
    const line = summariseReady(
      report([item({ status: { state: "installed", version: null } })]),
    );
    expect(line).toBe("Syncplay");
  });
});
```

Before writing the implementation, open `src/shared/types/PreflightItem.ts` and `PreflightReport.ts` and correct the fixture above to the real field names and unions. The fixtures must not need a cast beyond the `as` already shown — if they do, the fixture is wrong, not the type.

- [ ] **Step 2: Run it and watch it fail**

```bash
pnpm vitest run src/features/launch/systemStrip.test.ts
```

Expected: FAIL — cannot resolve `./systemStrip`.

- [ ] **Step 3: Implement `systemStrip.ts`**

```ts
import type { PreflightReport } from "@/shared/types/PreflightReport";

/** What the strip under the launch slots is saying right now. */
export type StripState = "checking" | "ready" | "blocked";

export function getStripState(report: PreflightReport | null): StripState {
  if (report === null) return "checking";

  return report.items.every((item) => item.status.state !== "missing")
    ? "ready"
    : "blocked";
}

/** The one line shown when there is nothing to do: what is installed. */
export function summariseReady(report: PreflightReport): string {
  return report.items
    .map((item) =>
      item.status.state === "installed" && item.status.version
        ? `${item.displayName} ${item.status.version}`
        : item.displayName,
    )
    .join(" · ");
}
```

- [ ] **Step 4: Run the test again**

```bash
pnpm vitest run src/features/launch/systemStrip.test.ts
```

Expected: PASS, 6 tests.

- [ ] **Step 5: Write `SystemStrip.tsx`**

This owns everything `Preflight.tsx` did except the continue gate: the checking effect, `install`, `locate`, `applyPath`, `locateErrors`, `playerChoice`, and the `DependencyRow` markup. Move that code across rather than rewriting it — it is correct, and its comments explain decisions worth keeping.

Props:

```tsx
export function SystemStrip({
  mode,
  onStateChange,
}: {
  mode: AppMode;
  onStateChange: (state: StripState) => void;
}) 
```

Structure:

- `useEffect` calls `onStateChange(getStripState(report))` whenever `report` changes, so the launch screen can disable its slots without owning the check.
- `checking`: `<Rewind label={t("system.checking")} />`.
- `ready`: a single collapsed row — `<Dot tone="good" />` plus
  `<p className="font-mono text-[11px] tracking-[0.14em] text-ink-faint uppercase">{t("system.ready")} — {summariseReady(report)}</p>` — and a ghost `t("system.recheck")` button aligned right.
- `blocked`: the same header row with `<Dot tone="warn" />` and
  `t("system.blocked")`, followed by the `DependencyRow` list, unchanged in behaviour.

Wrapper: `<div className="border-t border-line bg-surface/60 px-6 py-3">`.

- [ ] **Step 6: Verify**

```bash
pnpm test && pnpm build
```

Expected: green. The component is not mounted anywhere yet — that is Task 7.

- [ ] **Step 7: Commit**

```bash
git add src/features/launch
git commit -m "feat(launch): dependency checklist as a collapsible system strip"
```

---

### Task 7: The launch surface

**Files:**
- Create: `src/features/launch/LaunchScreen.tsx`
- Delete: `src/features/onboarding/ModeChooser.tsx`
- Delete: `src/features/onboarding/Preflight.tsx`
- Delete: `src/features/onboarding/autoContinue.ts`, `autoContinue.test.ts`

**Interfaces:**
- Consumes: `SystemStrip`, `getStripState`'s `StripState`, `Logo`, `Badge`,
  `EmptyState` (not used here but exported), `useTranslate`, `AppMode`.
- Produces: `<LaunchScreen onChoose: (mode: AppMode) => void />`. The screen
  itself owns which mode the strip checks against — see Step 2.

- [ ] **Step 1: Write the screen**

```tsx
export function LaunchScreen({ onChoose }: { onChoose: (mode: AppMode) => void }) {
  const t = useTranslate();
  // The strip has to check *something*; host is the stricter of the two, so
  // checking against it means a guest is never blocked by a host-only tool.
  const [strip, setStrip] = useState<StripState>("checking");

  const blocked = strip === "blocked";

  return (
    <div className="flex min-h-full flex-col">
      <div className="mx-auto flex w-full max-w-3xl flex-1 flex-col justify-center px-8 py-10">
        <div className="mb-8 flex items-center gap-3 font-mono text-[11px] tracking-[0.24em] text-accent uppercase">
          <span className="h-px w-8 bg-accent/60" />
          {t("onboarding.eyebrow")}
        </div>

        <h1 className="chroma font-display text-4xl font-extrabold tracking-[-0.03em] text-ink [font-stretch:115%]">
          {t("onboarding.title")}
        </h1>
        <p className="mt-3 max-w-xl text-sm leading-relaxed text-ink-muted">
          {t("onboarding.subtitle")}
        </p>

        <div className="mt-9 grid gap-4 sm:grid-cols-2">
          <Slot
            kind="host"
            title={t("onboarding.host.title")}
            detail={t("onboarding.host.detail")}
            disabled={blocked}
            onClick={() => onChoose("host")}
          />
          <Slot
            kind="guest"
            title={t("onboarding.guest.title")}
            detail={t("onboarding.guest.detail")}
            disabled={blocked}
            onClick={() => onChoose("guest")}
          />
        </div>
      </div>

      <SystemStrip mode="host" onStateChange={setStrip} />
    </div>
  );
}
```

`Slot` is a local component — a cassette bay rather than a card:

```tsx
function Slot({
  kind,
  title,
  detail,
  disabled,
  onClick,
}: {
  kind: "host" | "guest";
  title: string;
  detail: string;
  disabled: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      className="group relative overflow-hidden rounded-panel border border-line bg-surface/80 p-6 text-left transition-colors duration-[var(--duration-base)] hover:border-accent/70 hover:bg-surface-raised/80 disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:border-line disabled:hover:bg-surface/80"
    >
      {/* The bay's slot mouth. Nothing but a line, until you hover it. */}
      <span
        aria-hidden
        className="absolute inset-x-6 top-0 h-0.5 bg-line transition-colors group-hover:bg-accent group-disabled:bg-line"
      />
      <span
        aria-hidden
        className="font-mono text-[10px] tracking-[0.22em] text-ink-faint uppercase"
      >
        {kind === "host" ? "REC" : "PLAY"}
      </span>
      <h2 className="mt-4 font-display text-lg font-extrabold tracking-tight text-ink [font-stretch:110%]">
        {title}
      </h2>
      <p className="mt-2 text-sm leading-relaxed text-ink-muted">{detail}</p>
    </button>
  );
}
```

`REC` and `PLAY` are deliberately untranslated: they are the labels printed on the hardware this borrows from, and they are the same on a Turkish tape deck.

- [ ] **Step 2: Delete the replaced screens**

```bash
git rm src/features/onboarding/ModeChooser.tsx src/features/onboarding/Preflight.tsx src/features/onboarding/autoContinue.ts src/features/onboarding/autoContinue.test.ts
```

`src/features/onboarding/` should now be empty; remove the directory if git leaves it behind. `App.tsx` will not compile after this — Task 8 fixes it, and the two tasks land in one working tree.

- [ ] **Step 3: Verify after Task 8**

This task's build check is deferred: the app is mid-surgery until the shell is rewritten. Do not commit a broken tree — carry straight on to Task 8 and commit both together at Task 8 Step 5.

---

### Task 8: Rewrite the shell

**Files:**
- Modify: `src/App.tsx`
- Delete: `src/app/StepTrail.tsx`

**Interfaces:**
- Consumes: `LaunchScreen`, `HostScreen`, `GuestScreen`, `SettingsScreen`,
  `Wordmark`.
- Produces: a shell with two surfaces and two pieces of local state.

- [ ] **Step 1: Delete `StepTrail`**

```bash
git rm src/app/StepTrail.tsx
```

- [ ] **Step 2: Cut the dead state out of `Shell`**

Remove `setupConfirmed`, `rechoosingMode`, `autoSkipSpent` and the `Step`
import. Keep `showSettings` and `confirmingLeave`. The new derivation:

```tsx
const mode = settings?.mode ?? null;
const partyRunning = session.phase === "starting" || session.phase === "hosting";
const canGoBack = settings !== null && !showSettings && mode !== null;
```

`stepBack` becomes a single move — back always means "return to the launch surface", which is now expressible as clearing the mode:

```tsx
function stepBack() {
  setConfirmingLeave(false);
  void patchSettings({ mode: null }).catch(reportFailure);
}
```

Check `src/shared/types/SettingsPatch.ts` and `AppSettings.ts` first. If `mode` is not nullable in the patch type, do **not** change the Rust side: keep a local `const [atLaunch, setAtLaunch] = useState(false)` instead, set it in `stepBack`, clear it in `chooseMode`, and derive `const mode = atLaunch ? null : (settings?.mode ?? null)`. That is one state field rather than three, and it stays inside the frontend.

`goBack` and `stopAndGoBack` keep their current bodies — the hosting guard is the one confirmation in the app and it does not change.

- [ ] **Step 3: Rewrite the render**

```tsx
<main className="min-h-0 flex-1 overflow-y-auto scroll-smooth">
  {!settings ? (
    <Rewind label={t("common.loading")} />
  ) : showSettings ? (
    <SettingsScreen />
  ) : mode === null ? (
    <LaunchScreen onChoose={chooseMode} />
  ) : mode === "host" ? (
    <HostScreen />
  ) : (
    <GuestScreen />
  )}
</main>
```

The `<StepTrail />` line goes. Wrap `Rewind` in `<div className="mx-auto max-w-sm px-8 py-16">` so the band is not full-bleed on a wide window.

Give the outer shell the CRT layer:

```tsx
<div className="crt-scan crt-vignette relative flex h-full flex-col">
```

- [ ] **Step 4: Restyle the three banners**

`FailureBanner`, `UpdateBanner` and `LeaveHostingPrompt` keep their logic exactly. Change only their heading line to
`font-mono text-[11px] tracking-[0.16em] uppercase` and their wrapper radius to none (they are full-bleed strips). Add `tracking-glitch` to the `FailureBanner` wrapper — an error is exactly the moment a tape would skip.

- [ ] **Step 5: Verify and commit both tasks**

```bash
pnpm test && pnpm build
```

Expected: green, and the removed `autoContinue` suite is gone from the run.

Then in the browser: launch surface renders, the system strip shows either
`SYSTEM READY` or the dependency rows, choosing HOST navigates to the host
screen, the header back button returns to launch. Screenshot the launch
surface in both strip states — force the blocked state by pointing a
dependency at a bogus path through the settings screen if all tools are
installed locally.

```bash
git add -A src/App.tsx src/app src/features
git commit -m "feat(launch): fold setup into the launch surface, drop the step trail"
```

---

### Task 9: Channel rows

**Files:**
- Create: `src/features/party/channels.ts`
- Create: `src/features/party/channels.test.ts`
- Create: `src/features/party/ChannelRow.tsx`

**Interfaces:**
- Consumes: `RoomSnapshot`, `WatcherView`, `getLobbyState` from
  `@/features/host/lobbyState`.
- Produces:
  - `type ChannelStatus = "ready" | "waiting" | "noFile" | "trackingError"`
  - `getChannelStatus(watcher: WatcherView, filesCompatible: boolean): ChannelStatus`
  - `<ChannelRow watcher filesCompatible />`

- [ ] **Step 1: Write the failing test**

`src/features/party/channels.test.ts`:

```ts
import { describe, expect, it } from "vitest";

import type { WatcherView } from "@/shared/types/WatcherView";

import { getChannelStatus } from "./channels";

function watcher(overrides: Partial<WatcherView>): WatcherView {
  return {
    name: "ada",
    isReady: false,
    file: { name: "movie.mkv", duration: 7200 },
    ...overrides,
  } as WatcherView;
}

describe("getChannelStatus", () => {
  it("is noFile when nothing is open, whatever the ready flag says", () => {
    expect(getChannelStatus(watcher({ file: null, isReady: true }), true)).toBe("noFile");
  });

  // The mismatch belongs to the room, so it outranks this person's own state:
  // someone "ready" on the wrong file is the exact failure being warned about.
  it("is trackingError when the room's files do not match", () => {
    expect(getChannelStatus(watcher({ isReady: true }), false)).toBe("trackingError");
  });

  it("is ready when the file matches and the person is ready", () => {
    expect(getChannelStatus(watcher({ isReady: true }), true)).toBe("ready");
  });

  it("is waiting when the file matches but the person is not ready", () => {
    expect(getChannelStatus(watcher({ isReady: false }), true)).toBe("waiting");
  });
});
```

Open `src/shared/types/WatcherView.ts` and correct the fixture to the real shape before implementing.

- [ ] **Step 2: Run it and watch it fail**

```bash
pnpm vitest run src/features/party/channels.test.ts
```

Expected: FAIL — cannot resolve `./channels`.

- [ ] **Step 3: Implement `channels.ts`**

```ts
import type { WatcherView } from "@/shared/types/WatcherView";

/** What one row in the channel list is saying. */
export type ChannelStatus = "ready" | "waiting" | "noFile" | "trackingError";

export function getChannelStatus(
  watcher: WatcherView,
  filesCompatible: boolean,
): ChannelStatus {
  if (watcher.file == null) return "noFile";
  if (!filesCompatible) return "trackingError";

  return watcher.isReady ? "ready" : "waiting";
}
```

- [ ] **Step 4: Run the test again**

```bash
pnpm vitest run src/features/party/channels.test.ts
```

Expected: PASS, 4 tests.

- [ ] **Step 5: Write `ChannelRow.tsx`**

```tsx
const TONES: Record<ChannelStatus, BadgeTone> = {
  ready: "good",
  waiting: "warn",
  noFile: "neutral",
  trackingError: "bad",
};

const LABELS: Record<ChannelStatus, MessageKey> = {
  ready: "party.channel.ready",
  waiting: "party.channel.waiting",
  noFile: "party.channel.noFile",
  trackingError: "party.channel.trackingError",
};

export function ChannelRow({
  watcher,
  filesCompatible,
}: {
  watcher: WatcherView;
  filesCompatible: boolean;
}) {
  const t = useTranslate();
  const status = getChannelStatus(watcher, filesCompatible);

  return (
    <li className="flex items-center gap-3 border-b border-line/50 py-2.5 last:border-b-0">
      <Dot tone={TONES[status]} />
      <div className="min-w-0 flex-1">
        <p className="truncate text-sm font-medium text-ink">{watcher.name}</p>
        <p className="truncate font-mono text-[11px] text-ink-faint">
          {watcher.file?.name ?? "—"}
        </p>
      </div>
      <Badge tone={TONES[status]}>{t(LABELS[status])}</Badge>
    </li>
  );
}
```

- [ ] **Step 6: Verify**

```bash
pnpm test && pnpm build
```

- [ ] **Step 7: Commit**

```bash
git add src/features/party
git commit -m "feat(party): channel row with tracking-error state"
```

---

### Task 10: The host party surface

**Files:**
- Modify: `src/features/host/HostScreen.tsx`
- Modify: `src/features/host/InviteCard.tsx`
- Modify: `src/features/host/RoomPanel.tsx`
- Modify: `src/features/host/LobbyPanel.tsx`

**Interfaces:**
- Consumes: `ChannelRow`, `Counter`, `Rewind`, `EmptyState`, `getLobbyState`,
  `getLobbyBadge`.
- Produces: nothing new — this task changes layout and appearance only. The
  `lobbyState` module and its test are untouched.

- [ ] **Step 1: Restructure `HostScreen` into two columns**

```tsx
<div className="mx-auto grid max-w-5xl gap-5 px-8 py-8 lg:grid-cols-[1.15fr_1fr]">
  <div className="space-y-5">{/* deck + invite */}</div>
  <div className="space-y-5">{/* channels + log */}</div>
</div>
```

Below `lg` the columns stack, which is what a 720px window gets. Verify at that width — the invite code must not overflow its row.

- [ ] **Step 2: Turn the status card into the deck**

Replace the current header card body with:

```tsx
<div className="flex items-center justify-between gap-4">
  <div className="min-w-0">
    <div className="flex items-center gap-2.5">
      <span
        aria-hidden
        className={cx(
          "size-2.5 rounded-full",
          hosting ? "bg-bad phosphor" : starting ? "bg-warn" : "bg-ink-faint",
        )}
      />
      <span className="font-mono text-[11px] tracking-[0.22em] text-ink-muted uppercase">
        {hosting ? "REC" : starting ? t("host.starting") : "STANDBY"}
      </span>
      {hosting && <Counter since={startedAt} />}
    </div>
    <h1 className="mt-2 font-display text-xl font-extrabold tracking-tight text-ink [font-stretch:110%]">
      {t("host.title")}
    </h1>
  </div>
  {/* the existing button cluster, unchanged */}
</div>
```

`startedAt` is new local state in `HostScreen`: `const [startedAt, setStartedAt] = useState<number | null>(null)`, set to `Date.now()` in a `useEffect` that fires when `session.phase` becomes `"hosting"` and reset to `null` otherwise. Render the `Counter` only when `startedAt !== null`. No IPC change — the frontend already learns the transition.

While `starting`, replace the sub-line with `<Rewind label={t(STEP_LABELS[session.step])} />`. The four startup steps are exactly the kind of progress a sweeping band communicates better than text alone.

- [ ] **Step 3: Make `InviteCard` a cassette label**

Keep every behaviour. The invite value becomes:

```tsx
<code className="selectable block truncate border border-dashed border-line bg-canvas/70 px-3 py-2.5 font-mono text-xs tracking-wide text-ink">
```

Add a `tracking-glitch` class to the wrapper keyed on the copied state so the label visibly "stamps" when copied — `className={cx("...", copied && "tracking-glitch")}` with a `key={String(copied)}` on the element, otherwise React reuses the node and the animation does not re-fire.

- [ ] **Step 4: Rebuild `RoomPanel` on `ChannelRow`**

The three early returns (monitor off, disconnected, empty) all become
`<EmptyState title={...} detail={...} />` inside the existing `Card`. The
watcher list becomes:

```tsx
<ul>
  {room.watchers.map((watcher) => (
    <ChannelRow
      key={watcher.name}
      watcher={watcher}
      filesCompatible={
        room.fileCompatibility === "exact" || room.fileCompatibility === "durationMatch"
      }
    />
  ))}
</ul>
```

Keep the `durationMatch` notice block — it says something `ChannelRow` does not.

- [ ] **Step 5: Restyle `LobbyPanel`**

No structural change. Its badge switches to the `chroma` tone when everyone is ready, so "ready to start" reads as a different kind of event from a green status dot.

- [ ] **Step 6: Verify**

```bash
pnpm test && pnpm build
```

Then in the browser at both 940×720 and 720×560: start hosting, screenshot the deck with the counter running, the invite label, and the channel list. Confirm the counter advances and the REC dot glows.

- [ ] **Step 7: Commit**

```bash
git add src/features/host
git commit -m "feat(host): tape-deck party surface with channel list and counter"
```

---

### Task 11: The guest party surface

**Files:**
- Modify: `src/features/guest/GuestScreen.tsx`

**Interfaces:**
- Consumes: `Rewind`, `EmptyState`, `Counter`, the primitives.
- Produces: nothing new.

- [ ] **Step 1: Match the host's deck header**

Above the existing card, add the same status line so the two surfaces read as one app:

```tsx
<div className="flex items-center gap-2.5">
  <span
    aria-hidden
    className={cx("size-2.5 rounded-full", joined ? "bg-good phosphor" : "bg-ink-faint")}
  />
  <span className="font-mono text-[11px] tracking-[0.22em] text-ink-muted uppercase">
    {joined ? "PLAY" : "STANDBY"}
  </span>
  {joined && joinedAt !== null && <Counter since={joinedAt} />}
</div>
```

`joinedAt` is local state set to `Date.now()` when `join()` succeeds and when a resumed session is restored, cleared in `reset()`.

- [ ] **Step 2: Restyle the invite card**

The room name becomes `font-display text-lg font-extrabold [font-stretch:110%]`; the endpoint keeps `font-mono` and now renders in Departure Mono. The joined confirmation strip loses its rounded corners (`rounded-[var(--radius-control)]`).

- [ ] **Step 3: Keep the paste form, sharpen it**

No behaviour change. The decode error keeps `text-bad`. Add
`className="font-mono"` to the `Input` — an invite code is a machine string and reads better in the mono face, and it makes a mistyped character visible.

- [ ] **Step 4: Verify**

```bash
pnpm build
```

In the browser: paste a malformed code and screenshot the error state, then screenshot the empty paste state at 720×560.

- [ ] **Step 5: Commit**

```bash
git add src/features/guest
git commit -m "feat(guest): deck header and mono invite entry"
```

---

### Task 12: Settings and diagnostics

**Files:**
- Modify: `src/features/settings/SettingsScreen.tsx`
- Modify: `src/features/settings/DiagnosticsPanel.tsx`

**Interfaces:**
- Consumes: the restyled primitives.
- Produces: nothing new. `diagnosticsReport.ts` and its test are untouched.

- [ ] **Step 1: Remove the auto-skip control**

Find the `skipSetupWhenReady` toggle and delete it along with its handler. The setting no longer has a gate to skip. Leave the field in `AppSettings` and the Rust side alone — a dead settings field is a smaller problem than a backend change in a UI plan.

Add a `ponytail:` comment where the field is still typed:

```ts
// ponytail: skipSetupWhenReady is now unused by the UI — remove the backend
// field the next time settings change for another reason.
```

- [ ] **Step 2: Restyle both panels**

Section headings become `font-mono text-[11px] tracking-[0.18em] uppercase` (they are `Card` titles already, so this comes free from Task 3 — verify rather than re-apply). The diagnostics output block becomes `font-mono text-xs` on `bg-canvas` with `rounded-[var(--radius-control)]`.

- [ ] **Step 3: Verify**

```bash
pnpm test && pnpm build
```

Expected: `diagnosticsReport.test.ts` still passes untouched.

In the browser: open settings, screenshot, confirm no orphaned label sits where the removed toggle was.

- [ ] **Step 4: Commit**

```bash
git add src/features/settings
git commit -m "feat(settings): restyle and drop the obsolete auto-skip toggle"
```

---

### Task 13: Message keys

**Files:**
- Modify: `src/shared/i18n/messages.ts`

**Interfaces:**
- Consumes: every key referenced by Tasks 6-12.
- Produces: an `en`/`tr` pair with no unused and no missing keys.

- [ ] **Step 1: Remove the dead keys**

Delete from both locales: `nav.steps`, `nav.step.mode`, `nav.step.setup`,
`nav.step.party`, `preflight.skipWhenReady`, `preflight.continue`,
`preflight.allReady`, `preflight.title`, `preflight.subtitle`.

Verify each is genuinely unreferenced before deleting:

```bash
grep -rn "nav.step\|preflight.skipWhenReady\|preflight.continue\|preflight.allReady\|preflight.title\|preflight.subtitle" src/
```

Expected: no hits outside `messages.ts`.

- [ ] **Step 2: Add the new keys**

English:

```ts
"system.checking": "Checking your setup",
"system.ready": "System ready",
"system.blocked": "Missing something",
"system.recheck": "Check again",

"party.channel.ready": "Ready",
"party.channel.waiting": "Waiting",
"party.channel.noFile": "No file",
"party.channel.trackingError": "Tracking error",
```

Turkish:

```ts
"system.checking": "Kurulumun kontrol ediliyor",
"system.ready": "Sistem hazır",
"system.blocked": "Eksik var",
"system.recheck": "Yeniden kontrol et",

"party.channel.ready": "Hazır",
"party.channel.waiting": "Bekliyor",
"party.channel.noFile": "Dosya yok",
"party.channel.trackingError": "İzleme hatası",
```

Keep the remaining `preflight.*` keys — the dependency rows moved into
`SystemStrip` but still use them.

- [ ] **Step 3: Verify**

```bash
pnpm build
```

Expected: clean. `Messages` is derived from `en`, so a key present in `en` and missing from `tr` fails the type check here — that is the whole safety net for this task.

Then switch the app to Turkish in settings and screenshot the launch surface and the host party surface. Look for clipped labels: Turkish strings run longer, and the mono uppercase treatment does not wrap gracefully.

- [ ] **Step 4: Commit**

```bash
git add src/shared/i18n
git commit -m "feat(i18n): keys for the system strip and channel list"
```

---

### Task 14: App icons and the final pass

**Files:**
- Create: `assets/logo-source.png` (1024×1024)
- Modify: `src-tauri/icons/*` (regenerated)
- Modify: `README.md` (screenshot, if one is present)

**Interfaces:**
- Consumes: the `Logo` mark from Task 4.
- Produces: the platform icon set.

- [ ] **Step 1: Produce the 1024px source**

Render the mark on a solid `oklch(0.13 0.02 285)` square with the reels in
`oklch(0.72 0.21 350)`, at 1024×1024 with roughly 12% padding. Write the SVG to a scratch file and convert it, or export it from the running app — whichever the environment supports. The source must be a flat PNG with no transparency-dependent detail: Windows renders it on unpredictable backgrounds.

- [ ] **Step 2: Regenerate the icon set**

```bash
pnpm tauri icon assets/logo-source.png
```

This rewrites every file listed under `bundle.icon` in `tauri.conf.json` plus the Square*Logo variants. `tauri.conf.json` itself needs no edit — the paths are unchanged.

- [ ] **Step 3: Verify the icons**

```bash
git status --short src-tauri/icons
```

Expected: all existing icon files modified, none deleted, no new unexpected files. Open `src-tauri/icons/32x32.png` and confirm the two reels are still distinguishable at that size. If they merge, widen the gap in the source and regenerate — do not thin the stroke.

- [ ] **Step 4: Full verification pass**

```bash
pnpm test && pnpm build
```

Then, in the browser, capture the full set and hand it to the user:

1. Launch surface, system strip ready
2. Launch surface, system strip blocked
3. Host party, hosting, counter running
4. Guest party, joined
5. Guest party, paste state
6. Settings
7. Any surface at 720×560
8. Any surface with `prefers-reduced-motion: reduce` forced — confirm the scanline overlay and the rewind sweep are both gone

Confirm `git diff --stat main -- package.json` shows no dependency change, and `git diff --stat main -- src-tauri/src` is empty.

- [ ] **Step 5: Commit**

```bash
git add assets src-tauri/icons README.md
git commit -m "feat(brand): reel mark as the application icon"
```

---

## Self-Review Notes

Checked against the spec:

- Palette, typography, radius, CRT layer, token groups — Task 1.
- Deck metaphor (REC/PLAY, cassette label, channel list, rewind, counter) —
  Tasks 5, 7, 9, 10, 11.
- Three steps to two surfaces, `StepTrail` and `autoContinue` deleted,
  `App.tsx` state reduced, `skipSetupWhenReady` removed from the UI —
  Tasks 7, 8, 12.
- Component layer split plus the seven new primitives — Tasks 2, 4, 5, 6, 9.
  (`Deck` and `SystemStrip` are named in the spec as primitives; `Deck` ships
  as the host/guest header markup in Tasks 10 and 11 rather than a shared
  component, because the two decks share five lines of markup and no
  behaviour. Extracting it would be an abstraction with one and a half
  implementations.)
- Logo mark, wordmark, icon set — Tasks 4 and 14.
- Verification — every task ends with `pnpm build`; Task 14 collects the
  screenshots.

Two things an executor must not silently work around:

- If the font URLs in Task 1 have moved, stop and ask. Do not substitute a
  CDN `@import` — the CSP blocks it and the failure is silent at build time.
- If `SettingsPatch` cannot express `mode: null` (Task 8 Step 2), take the
  local-state fallback. Do not edit Rust to make the tidier version work.
