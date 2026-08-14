import type { ReactNode } from "react";

import { cx } from "./cx";

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
