/**
 * A small set of mutually exclusive options, shown side by side.
 *
 * `label` is optional because this also sits inline in a row that already
 * names what is being chosen; pass `ariaLabel` there so the group is still
 * announced.
 */
export function Choice({
  label,
  ariaLabel,
  value,
  options,
  onChange,
  disabled,
}: {
  label?: string;
  ariaLabel?: string;
  value: string;
  options: Array<{ value: string; label: string }>;
  onChange: (value: string) => void;
  disabled?: boolean;
}) {
  return (
    <div className="space-y-1.5">
      {label && <span className="text-sm font-medium text-ink">{label}</span>}
      <div
        role="group"
        aria-label={ariaLabel ?? label}
        className="flex gap-1 rounded-xl border border-line/80 bg-canvas/70 p-1.5"
      >
        {options.map((option) => (
          <button
            key={option.value}
            type="button"
            disabled={disabled}
            aria-pressed={option.value === value}
            onClick={() => onChange(option.value)}
            className={
              option.value === value
                ? "flex-1 rounded-lg bg-accent px-3 py-2 text-sm font-semibold text-accent-ink shadow-sm disabled:opacity-50"
                : "flex-1 rounded-lg px-3 py-2 text-sm text-ink-muted transition-colors hover:bg-surface-raised/50 hover:text-ink disabled:opacity-50"
            }
          >
            {option.label}
          </button>
        ))}
      </div>
    </div>
  );
}
