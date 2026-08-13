/**
 * The handful of primitives this app needs.
 *
 * Hand-rolled rather than pulled from a component library: there are six of
 * them, they have no behaviour worth abstracting, and a registry plus its
 * dependency tree would outweigh the whole frontend.
 */
import type { ButtonHTMLAttributes, InputHTMLAttributes, ReactNode } from "react";

export function cx(...values: Array<string | false | null | undefined>) {
  return values.filter(Boolean).join(" ");
}

// ------------------------------------------------------------------ Button

type ButtonVariant = "primary" | "secondary" | "ghost" | "danger";

const BUTTON_VARIANTS: Record<ButtonVariant, string> = {
  primary:
    "bg-accent text-accent-ink shadow-[0_8px_24px_oklch(0.65_0.18_42/0.2)] hover:bg-accent-strong hover:-translate-y-px disabled:hover:translate-y-0 disabled:hover:bg-accent",
  secondary:
    "bg-surface-raised/80 text-ink border border-line/80 shadow-sm hover:border-ink-faint hover:bg-surface-raised",
  ghost: "text-ink-muted hover:text-ink hover:bg-surface-raised/70",
  danger: "bg-bad/10 text-bad border border-bad/25 hover:border-bad/60 hover:bg-bad/15",
};

export function Button({
  variant = "secondary",
  className,
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: ButtonVariant }) {
  return (
    <button
      {...props}
      className={cx(
        "inline-flex min-h-10 items-center justify-center gap-2 rounded-xl px-4 py-2",
        "text-sm font-semibold transition-all duration-200",
        "disabled:cursor-not-allowed disabled:opacity-45",
        BUTTON_VARIANTS[variant],
        className,
      )}
    />
  );
}

// -------------------------------------------------------------------- Card

export function Card({
  title,
  action,
  className,
  children,
}: {
  title?: ReactNode;
  action?: ReactNode;
  className?: string;
  children: ReactNode;
}) {
  return (
    <section
      className={cx(
        "overflow-hidden rounded-panel border border-line/70 bg-surface/78 shadow-[0_20px_60px_oklch(0.08_0.03_275/0.28)] backdrop-blur-xl",
        className,
      )}
    >
      {(title || action) && (
        <header className="flex items-center justify-between gap-3 border-b border-line/60 bg-surface-raised/25 px-5 py-4">
          <h2 className="text-xs font-bold tracking-[0.14em] text-ink-muted uppercase">
            {title}
          </h2>
          {action}
        </header>
      )}
      <div className="p-5">{children}</div>
    </section>
  );
}

// ------------------------------------------------------------- Page heading

export function PageHeader({
  title,
  description,
  action,
}: {
  title: ReactNode;
  description?: ReactNode;
  action?: ReactNode;
}) {
  return (
    <header className="flex items-start justify-between gap-5">
      <div className="min-w-0">
        <h1 className="text-3xl font-bold tracking-[-0.035em] text-ink">
          {title}
        </h1>
        {description && (
          <p className="mt-2 max-w-2xl text-sm leading-relaxed text-ink-muted">
            {description}
          </p>
        )}
      </div>
      {action && <div className="shrink-0 pt-1">{action}</div>}
    </header>
  );
}

// ------------------------------------------------------------------- Badge

type BadgeTone = "neutral" | "good" | "warn" | "bad" | "accent";

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

// ------------------------------------------------------------------- Input

export function Input({
  className,
  ...props
}: InputHTMLAttributes<HTMLInputElement>) {
  return (
    <input
      {...props}
      className={cx(
        "w-full rounded-xl border border-line/80 bg-canvas/70 px-3.5 py-2.5",
        "text-sm text-ink placeholder:text-ink-faint",
        "cursor-text select-text",
        "transition-colors focus:border-accent focus:bg-canvas focus:outline-none",
        className,
      )}
    />
  );
}

export function Field({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: string;
  children: ReactNode;
}) {
  return (
    <label className="block space-y-1.5">
      <span className="text-sm font-medium text-ink">{label}</span>
      {children}
      {hint && <span className="block text-xs text-ink-faint">{hint}</span>}
    </label>
  );
}

// ------------------------------------------------------------------ Toggle

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

// ------------------------------------------------------------ Segmented choice

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

// ------------------------------------------------------------- Copyable row

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
      <p className="text-xs font-medium tracking-wide text-ink-faint uppercase">
        {label}
      </p>
      <div className="flex items-center gap-2">
        <code className="selectable min-w-0 flex-1 truncate rounded-xl border border-line/70 bg-canvas/65 px-3.5 py-2.5 font-mono text-xs text-ink">
          {value}
        </code>
        <Button onClick={onCopy} className="shrink-0">
          {copied ? copiedLabel : copyLabel}
        </Button>
      </div>
    </div>
  );
}
