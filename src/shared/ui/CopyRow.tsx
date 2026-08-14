import { Button } from "./Button";

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
        <code className="selectable min-w-0 flex-1 truncate rounded-[var(--radius-control)] border border-line/70 bg-canvas/65 px-3.5 py-2.5 font-mono text-xs text-ink">
          {value}
        </code>
        <Button onClick={onCopy} className="shrink-0">
          {copied ? copiedLabel : copyLabel}
        </Button>
      </div>
    </div>
  );
}
