import type { ReactNode } from "react";

import { cx } from "./cx";

export type BadgeTone =
  | "neutral"
  | "good"
  | "warn"
  | "bad"
  | "accent"
  | "chroma";

const BADGE_TONES: Record<BadgeTone, string> = {
  neutral: "bg-surface-raised text-ink-muted",
  good: "bg-good/15 text-good",
  warn: "bg-warn/15 text-warn",
  bad: "bg-bad/15 text-bad",
  accent: "bg-accent/15 text-accent",
  chroma: "bg-chroma/15 text-chroma",
};

export function Badge({
  tone = "neutral",
  children,
}: {
  tone?: BadgeTone;
  children: ReactNode;
}) {
  return (
    <span
      className={cx(
        "inline-flex items-center gap-1.5 rounded-[var(--radius-control)] border border-current/15 px-2 py-1",
        "font-mono text-[10px] tracking-[0.12em] whitespace-nowrap uppercase",
        BADGE_TONES[tone],
      )}
    >
      {children}
    </span>
  );
}

/** A small filled circle, for status that reads faster than a word. */
export function Dot({ tone }: { tone: BadgeTone }) {
  const colours: Record<BadgeTone, string> = {
    neutral: "bg-ink-faint",
    good: "bg-good",
    warn: "bg-warn",
    bad: "bg-bad",
    accent: "bg-accent",
    chroma: "bg-chroma",
  };

  return <span className={cx("size-2 rounded-full", colours[tone])} />;
}
