import type { ButtonHTMLAttributes } from "react";

import { cx } from "./cx";

export type ButtonVariant = "primary" | "secondary" | "ghost" | "danger";

const BUTTON_VARIANTS: Record<ButtonVariant, string> = {
  primary:
    "bg-accent text-accent-ink phosphor hover:bg-accent-strong disabled:hover:bg-accent",
  secondary:
    "bg-surface-raised/80 text-ink border border-line hover:border-ink-faint hover:bg-surface-raised",
  ghost: "text-ink-muted hover:text-ink hover:bg-surface-raised/70",
  danger: "bg-bad/10 text-bad border border-bad/30 hover:border-bad/60 hover:bg-bad/15",
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
        "inline-flex min-h-10 items-center justify-center gap-2 rounded-[var(--radius-control)] px-4 py-2",
        // No hover lift: buttons that float are the templated look this is
        // getting away from, and a CRT has no z-axis.
        "text-sm font-semibold tracking-wide transition-colors duration-[var(--duration-fast)]",
        "disabled:cursor-not-allowed disabled:opacity-45",
        BUTTON_VARIANTS[variant],
        className,
      )}
    />
  );
}
