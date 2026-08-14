import { useEffect, useState } from "react";

import { formatElapsed } from "./elapsed";

/**
 * Elapsed time since `since`, ticking once a second.
 *
 * Driven off the wall-clock difference rather than an accumulator, so a tab
 * that was throttled catches up instead of drifting behind.
 */
export function Counter({ since }: { since: number }) {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, []);

  return (
    <span className="font-mono text-sm text-ink-muted tabular-nums">
      {formatElapsed(now - since)}
    </span>
  );
}
