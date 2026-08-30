/**
 * Deterministic genre -> colour, so "Horror" looks the same on every card,
 * every screen, every session — never a random pick per render.
 *
 * Hashes the label itself rather than keeping an id table: TMDB's genre ids
 * are stable but this needs no maintenance as genres are added, and a
 * `MovieCandidate` only carries genre names once selected, not ids.
 */
export function genreAccent(label: string): string {
  let hash = 0;
  for (let index = 0; index < label.length; index += 1) {
    hash = (hash * 31 + label.charCodeAt(index)) >>> 0;
  }
  const hue = hash % 360;
  // Same lightness/chroma as the app's own accent tokens (see styles.css),
  // only the hue varies — so a genre chip never looks like it wandered in
  // from a different design system.
  return `oklch(0.78 0.15 ${hue})`;
}
