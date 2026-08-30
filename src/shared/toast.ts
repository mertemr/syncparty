/**
 * A tiny global toast queue — module-level rather than a React context,
 * because the thing feeding it (`AppState`'s event handler) already sits
 * outside the component that would render the toasts, and a context would
 * just be a longer way to write the same subscription.
 */
export interface Toast {
  id: number;
  message: string;
}

const VISIBLE_MS = 4000;
const MAX_VISIBLE = 4;

let toasts: Toast[] = [];
let nextId = 0;
const listeners = new Set<(toasts: Toast[]) => void>();

function emit() {
  for (const listener of listeners) listener(toasts);
}

/** Shows `message` briefly. Identical back-to-back messages are dropped
 * rather than shown twice — the backend can publish the same state more
 * than once (a reconnect hydrate, say), and that must not read as two
 * separate events. */
export function pushToast(message: string) {
  if (toasts[toasts.length - 1]?.message === message) return;

  const id = nextId++;
  toasts = [...toasts, { id, message }].slice(-MAX_VISIBLE);
  emit();

  setTimeout(() => {
    toasts = toasts.filter((toast) => toast.id !== id);
    emit();
  }, VISIBLE_MS);
}

export function subscribeToasts(listener: (toasts: Toast[]) => void): () => void {
  listeners.add(listener);
  listener(toasts);
  return () => listeners.delete(listener);
}
