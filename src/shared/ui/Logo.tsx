import { cx } from "./cx";

/**
 * Two cassette reels, overlapping into a sync loop; the negative space where
 * they meet is a play triangle. Two reels, two viewers, in sync.
 *
 * Single-colour on purpose: it has to survive a 16px taskbar icon and a
 * monochrome tray, so it carries no gradient and no second fill.
 */
export function Logo({
  size = 24,
  className,
}: {
  size?: number;
  className?: string;
}) {
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
        <circle cx="10.5" cy="16" r="8" />
        <circle cx="21.5" cy="16" r="8" />
      </g>
      {/* The hubs. Filled, so the reels read as reels rather than as a Venn
          diagram of two empty circles. */}
      <circle cx="10.5" cy="16" r="2.4" fill="currentColor" />
      <circle cx="21.5" cy="16" r="2.4" fill="currentColor" />
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
