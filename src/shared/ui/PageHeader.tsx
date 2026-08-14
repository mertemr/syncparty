import type { ReactNode } from "react";

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
