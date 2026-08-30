/**
 * Favourite movies, kept in `localStorage` rather than the backend.
 *
 * Unlike watched history (real: it comes from a vote everyone was actually
 * in), a favourite is one person's private bookmark on this one machine —
 * there is no session to attach it to and nobody else needs to see it, so a
 * synced/persisted backend record would be solving a problem nobody has.
 *
 * `MovieSummary` carries `bigint` fields, which `JSON.stringify` throws on,
 * so everything is stringified going in and parsed back coming out.
 */
import type { MovieSummary } from "@/shared/types/MovieSummary";

const STORAGE_KEY = "syncparty.movie.favorites";

interface StoredMovie extends Omit<MovieSummary, "tmdbId" | "voteCount"> {
  tmdbId: string;
  voteCount: string;
  /** When this was favourited. Numeric object keys iterate in ascending
   * order regardless of insertion order (a JS quirk for integer-like
   * strings), so "most recent first" needs its own field to sort by. */
  addedAt: number;
}

function readAll(): Record<string, StoredMovie> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    return raw ? (JSON.parse(raw) as Record<string, StoredMovie>) : {};
  } catch {
    // Private browsing, disabled storage, corrupt JSON — favourites just
    // behave as empty rather than breaking the screen that reads them.
    return {};
  }
}

function writeAll(movies: Record<string, StoredMovie>) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(movies));
  } catch {
    // Quota exceeded or storage unavailable — the toggle that triggered
    // this simply doesn't stick this session.
  }
}

function toStored(movie: MovieSummary, addedAt: number): StoredMovie {
  return {
    ...movie,
    tmdbId: movie.tmdbId.toString(),
    voteCount: movie.voteCount.toString(),
    addedAt,
  };
}

function fromStored(stored: StoredMovie): MovieSummary {
  return { ...stored, tmdbId: BigInt(stored.tmdbId), voteCount: BigInt(stored.voteCount) };
}

export function isFavorite(tmdbId: bigint): boolean {
  return tmdbId.toString() in readAll();
}

/**
 * The picker grid and the sidebar's Favourites tab can both be on screen at
 * once, each with its own idea of the current list — this is how one
 * toggling a favourite tells the other to re-read `localStorage` rather
 * than going stale until something unrelated happens to re-render it.
 */
const listeners = new Set<() => void>();

export function subscribeFavorites(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Adds or removes `movie` from favourites. Returns the new state. */
export function toggleFavorite(movie: MovieSummary): boolean {
  const all = readAll();
  const key = movie.tmdbId.toString();
  let nowFavorite: boolean;

  if (key in all) {
    delete all[key];
    nowFavorite = false;
  } else {
    all[key] = toStored(movie, Date.now());
    nowFavorite = true;
  }

  writeAll(all);
  for (const listener of listeners) listener();
  return nowFavorite;
}

/** Every favourite, most recently added first. */
export function listFavorites(): MovieSummary[] {
  return Object.values(readAll())
    .sort((a, b) => b.addedAt - a.addedAt)
    .map(fromStored);
}
