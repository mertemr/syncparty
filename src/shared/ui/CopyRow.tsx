import { Button } from "./Button";
import { cx } from "./cx";

/** A label, a monospace value, and a copy button. */
export function CopyRow({
  label,
  value,
  copyLabel,
  copiedLabel,
  onCopy,
  copied,
}: {
  label: string;
  value: string;
  copyLabel: string;
  copiedLabel: string;
  onCopy: () => void;
  copied: boolean;
}) {
  return (
    <div className="space-y-1.5">
      <p className="font-mono text-[10px] tracking-[0.16em] text-ink-faint uppercase">
        {label}
      </p>
      <div className="flex items-center gap-2">
        {/* Dashed, like the write-on strip down the spine of a cassette. The
            key remounts on copy so the stamp animation fires again rather
            than being reused. */}
        <code
          key={String(copied)}
          className={cx(
            "selectable min-w-0 flex-1 truncate rounded-[var(--radius-control)] border border-dashed border-line bg-canvas/70 px-3.5 py-2.5 font-mono text-xs tracking-wide text-ink",
            copied && "tracking-glitch border-accent/60",
          )}
        >
          {value}
        </code>
        <Button onClick={onCopy} className="shrink-0">
          {copied ? copiedLabel : copyLabel}
        </Button>
      </div>
    </div>
  );
}
