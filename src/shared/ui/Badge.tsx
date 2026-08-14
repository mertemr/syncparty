import type { ReactNode } from "react";

import { cx } from "./cx";

export type BadgeTone = "neutral" | "good" | "warn" | "bad" | "accent";

const BADGE_TONES: Record<BadgeTone, string> = {
  neutral: "bg-surface-raised text-ink-muted",
  good: "bg-good/15 text-good",
  warn: "bg-warn/15 text-warn",
  bad: "bg-bad/15 text-bad",
  accent: "bg-accent/15 text-accent",
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
        "inline-flex items-center gap-1.5 rounded-full border border-current/10 px-2.5 py-1",
        "text-[11px] font-bold tracking-wide whitespace-nowrap",
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
  };

  return <span className={cx("size-2 rounded-full", colours[tone])} />;
}
