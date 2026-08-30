/**
 * Manual "mark as watched" flags, kept in `localStorage` — same shape as
 * `favorites.ts`, and for the same reason: this is a personal tag ("I saw
 * this, however I saw it"), not the real watched history TMDB vote outcomes
 * already produce on the backend. That real history still wins wherever the
 * two disagree; this only adds a mark, it can't remove one that came from
 * an actual movie night.
 */
const STORAGE_KEY = "syncparty.movie.watched-manual";

function readAll(): Set<string> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    return new Set(raw ? (JSON.parse(raw) as string[]) : []);
  } catch {
    return new Set();
  }
}

function writeAll(ids: Set<string>) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify([...ids]));
  } catch {
    // Quota exceeded or storage unavailable — the toggle just doesn't stick.
  }
}

const listeners = new Set<() => void>();

/** See the identical pattern in `favorites.ts` — lets the picker grid and
 * anything else reading this react to a toggle happening elsewhere. */
export function subscribeWatchedManual(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function isWatchedManual(tmdbId: bigint): boolean {
  return readAll().has(tmdbId.toString());
}

/** Toggles the manual mark for `tmdbId`. Returns the new state. */
export function toggleWatchedManual(tmdbId: bigint): boolean {
  const ids = readAll();
  const key = tmdbId.toString();
  const nowWatched = !ids.has(key);

  if (nowWatched) ids.add(key);
  else ids.delete(key);

  writeAll(ids);
  for (const listener of listeners) listener();
  return nowWatched;
}
