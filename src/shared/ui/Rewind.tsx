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
