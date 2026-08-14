/** Nothing here yet, said once, the same way everywhere. */
export function EmptyState({
  title,
  detail,
}: {
  title: string;
  detail?: string;
}) {
  return (
    <div className="px-4 py-10 text-center">
      <p className="font-mono text-[11px] tracking-[0.18em] text-ink-faint uppercase">
        {title}
      </p>
      {detail && <p className="mt-2 text-sm text-ink-muted">{detail}</p>}
    </div>
  );
}
