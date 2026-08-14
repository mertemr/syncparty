import type { InputHTMLAttributes, ReactNode } from "react";

import { cx } from "./cx";

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
