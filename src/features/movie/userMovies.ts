/**
 * Favourites and manual "seen it" marks, held in SQLite next to the rest of
 * the app's data.
 *
 * Both lived in the webview's `localStorage` until now, which put a person's
 * own marks in the one place that is neither backed up with the app's data
 * directory nor readable by anything but the window that wrote it. Clearing
 * the webview's storage — something a Tauri update or a reinstall can do on
 * its own — took every favourite with it.
 *
 * The reads here stay synchronous even though the store is not: a poster
 * grid asks "is this one a favourite?" once per card per render, and an
 * `await` in that path would mean every card flickering through an unmarked
 * state on the way to its real one. The whole table is small, personal, and
 * loaded once at startup, so it is kept in memory and written through.
 */
import { ipc } from "@/shared/ipc";
import type { MovieSummary } from "@/shared/types/MovieSummary";

interface Marks {
  movie: MovieSummary;
  favorite: boolean;
  watched: boolean;
  markedAt: number;
}

/** Keyed by `tmdbId.toString()` — a `bigint` is not a usable Map key across
 * separately parsed values, and every caller has the string form anyway. */
const marks = new Map<string, Marks>();

const listeners = new Set<() => void>();

/**
 * Both grids and the sidebar can be on screen at once, each with its own
 * idea of the current list — this is how one toggling a mark tells the
 * others to re-read rather than going stale until an unrelated render.
 */
export function subscribeUserMovies(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function announce() {
  for (const listener of listeners) listener();
}

const LEGACY_FAVORITES_KEY = "syncparty.movie.favorites";
const LEGACY_WATCHED_KEY = "syncparty.movie.watched-manual";

/**
 * Moves anything still in `localStorage` into the store, once.
 *
 * Favourites carry their full summary so they migrate whole. Manual watched
 * marks were only ever a list of ids, so one that is not also a favourite
 * has no summary to move and is dropped — re-marking it is one click, and
 * the alternative is a TMDB lookup per id on first launch.
 */
async function migrateLegacyStorage(): Promise<boolean> {
  let migrated = false;

  try {
    const rawFavorites = localStorage.getItem(LEGACY_FAVORITES_KEY);
    const rawWatched = localStorage.getItem(LEGACY_WATCHED_KEY);
    if (!rawFavorites && !rawWatched) return false;

    const watchedIds = new Set<string>(rawWatched ? (JSON.parse(rawWatched) as string[]) : []);
    const stored = rawFavorites
      ? (JSON.parse(rawFavorites) as Record<string, Record<string, unknown>>)
      : {};

    for (const [key, entry] of Object.entries(stored)) {
      const movie = {
        ...entry,
        tmdbId: BigInt(entry.tmdbId as string),
        voteCount: BigInt(entry.voteCount as string),
      } as unknown as MovieSummary;
      await ipc.setUserMovie(movie, true, watchedIds.has(key));
      watchedIds.delete(key);
      migrated = true;
    }

    localStorage.removeItem(LEGACY_FAVORITES_KEY);
    localStorage.removeItem(LEGACY_WATCHED_KEY);
  } catch {
    // Corrupt or unavailable storage. Nothing to move, and nothing that
    // should stop the app from starting.
  }

  return migrated;
}

let hydrated: Promise<void> | null = null;

/** Loads the store into memory. Safe to call from more than one place —
 * every caller after the first waits on the same load. */
export function hydrateUserMovies(): Promise<void> {
  hydrated ??= (async () => {
    await migrateLegacyStorage();
    const stored = await ipc.listUserMovies();
    marks.clear();
    for (const entry of stored) {
      marks.set(entry.movie.tmdbId.toString(), {
        movie: entry.movie,
        favorite: entry.favorite,
        watched: entry.watched,
        markedAt: Number(entry.markedAt),
      });
    }
    announce();
  })();

  return hydrated;
}

/** Applies a change in memory first, then writes it through. The UI has
 * already moved on by the time the store answers; a failed write is rolled
 * back so the next render tells the truth. */
function update(movie: MovieSummary, favorite: boolean, watched: boolean) {
  const key = movie.tmdbId.toString();
  const previous = marks.get(key);

  if (!favorite && !watched) marks.delete(key);
  else marks.set(key, { movie, favorite, watched, markedAt: Date.now() });
  announce();

  void ipc.setUserMovie(movie, favorite, watched).catch(() => {
    if (previous) marks.set(key, previous);
    else marks.delete(key);
    announce();
  });
}

export function isFavorite(tmdbId: bigint): boolean {
  return marks.get(tmdbId.toString())?.favorite ?? false;
}

export function isWatchedManual(tmdbId: bigint): boolean {
  return marks.get(tmdbId.toString())?.watched ?? false;
}

/** Adds or removes `movie` from favourites. Returns the new state. */
export function toggleFavorite(movie: MovieSummary): boolean {
  const current = marks.get(movie.tmdbId.toString());
  const nowFavorite = !(current?.favorite ?? false);
  update(movie, nowFavorite, current?.watched ?? false);
  return nowFavorite;
}

/** Toggles the manual mark for `movie`. Returns the new state. */
export function toggleWatchedManual(movie: MovieSummary): boolean {
  const current = marks.get(movie.tmdbId.toString());
  const nowWatched = !(current?.watched ?? false);
  update(movie, current?.favorite ?? false, nowWatched);
  return nowWatched;
}

function sortedByMark(predicate: (entry: Marks) => boolean): MovieSummary[] {
  return [...marks.values()]
    .filter(predicate)
    .sort((a, b) => b.markedAt - a.markedAt)
    .map((entry) => entry.movie);
}

/** Every favourite, most recently marked first. */
export function listFavorites(): MovieSummary[] {
  return sortedByMark((entry) => entry.favorite);
}

/** Every manually marked movie, most recently marked first. Unlike the
 * backend's watched history these never had a movie night — they are just
 * "I saw this, however I saw it". */
export function listWatchedManual(): MovieSummary[] {
  return sortedByMark((entry) => entry.watched);
}
