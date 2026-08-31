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
    // No backdrop blur and no drop shadow: blur behind an opaque app
    // background costs compositing for an effect nobody can see.
    <section
      className={cx(
        "overflow-hidden rounded-panel border border-line bg-surface/85",
        className,
      )}
    >
      {(title || action) && (
        <header className="flex items-center justify-between gap-3 border-b border-line bg-surface-raised/30 px-5 py-3">
          <h2 className="min-w-0 truncate font-mono text-[11px] tracking-[0.18em] text-ink-muted uppercase">
            {title}
          </h2>
          {action && <div className="shrink-0">{action}</div>}
        </header>
      )}
      <div className="p-5">{children}</div>
    </section>
  );
}
