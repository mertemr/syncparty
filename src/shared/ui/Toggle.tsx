import { cx } from "./cx";

export function Toggle({
  checked,
  onChange,
  label,
  hint,
}: {
  checked: boolean;
  onChange: (next: boolean) => void;
  label: string;
  hint?: string;
}) {
  return (
    <div className="flex items-start justify-between gap-4">
      <div className="min-w-0">
        <p className="text-sm font-medium text-ink">{label}</p>
        {hint && <p className="mt-1 text-xs text-ink-faint">{hint}</p>}
      </div>
      <button
        type="button"
        role="switch"
        aria-checked={checked}
        aria-label={label}
        onClick={() => onChange(!checked)}
        className={cx(
          "relative mt-0.5 h-7 w-12 shrink-0 rounded-full transition-colors",
          checked ? "bg-accent" : "bg-surface-raised border border-line",
        )}
      >
        <span
          className={cx(
            "absolute top-1 size-5 rounded-full shadow-sm transition-all",
            checked ? "left-6 bg-accent-ink" : "left-1 bg-ink-faint",
          )}
        />
      </button>
    </div>
  );
}
