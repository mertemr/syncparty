import type { ButtonHTMLAttributes } from "react";

import { cx } from "./cx";

export type ButtonVariant = "primary" | "secondary" | "ghost" | "danger";

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
