/**
 * How the watched grid orders itself.
 *
 * The marks themselves live in `userMovies.ts` (and in SQLite behind it);
 * this is only the rule for merging them with the backend's real movie-night
 * history, kept apart because it is the one part worth testing on its own.
 */

/**
 * The order the watched grid shows: real movie nights newest first, then
 * manual marks that never had a night of their own.
 *
 * Real history wins on a tie, which is why it is laid down first — a movie
 * both marked by hand and actually watched should sit with the date it was
 * watched on, not at the end of the list.
 */
export function watchedIdOrder(
  history: Array<{ tmdbId: bigint; watchedAt: bigint }>,
  manual: string[],
): string[] {
  const ordered = [...history]
    .sort((a, b) => (a.watchedAt === b.watchedAt ? 0 : a.watchedAt > b.watchedAt ? -1 : 1))
    .map((movie) => movie.tmdbId.toString());

  const seen = new Set(ordered);
  for (const id of manual) {
    if (seen.has(id)) continue;
    seen.add(id);
    ordered.push(id);
  }
  return ordered;
}
