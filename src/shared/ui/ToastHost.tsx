import { useEffect, useState } from "react";

import { subscribeToasts, type Toast } from "../toast";

/** Renders whatever `pushToast` has queued, stacked bottom-right. Mounted
 * once at the app shell — nothing else needs to know it exists. */
export function ToastHost() {
  const [toasts, setToasts] = useState<Toast[]>([]);

  useEffect(() => subscribeToasts(setToasts), []);

  if (toasts.length === 0) return null;

  return (
    <div className="pointer-events-none fixed right-4 bottom-4 z-50 flex flex-col gap-2">
      {toasts.map((toast) => (
        <div
          key={toast.id}
          role="status"
          className="pointer-events-auto rounded-panel border border-line bg-surface/95 px-4 py-2.5 text-sm text-ink shadow-lg backdrop-blur-md"
        >
          {toast.message}
        </div>
      ))}
    </div>
  );
}
