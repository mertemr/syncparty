/**
 * Pure helpers for the movie-night schedule picker.
 *
 * No timezone library: the browser's own `Date` already knows the local
 * offset, and `core` never parses this string at all — it is stored and
 * broadcast as an opaque label (see `MovieVoteSnapshot.schedule`). Resolving
 * "tonight" or a custom date/time to a real instant is entirely this
 * module's job, once, before it ever reaches the backend.
 */

/** Today at 21:00 local time, as an ISO instant. */
export function tonightIso(now = new Date()): string {
  const at = new Date(now);
  at.setHours(21, 0, 0, 0);
  return at.toISOString();
}

/** Tomorrow at 21:00 local time, as an ISO instant. */
export function tomorrowIso(now = new Date()): string {
  const at = new Date(now);
  at.setDate(at.getDate() + 1);
  at.setHours(21, 0, 0, 0);
  return at.toISOString();
}

/**
 * Combines a `YYYY-MM-DD` date with an optional `HH:MM` time into an ISO
 * instant in local time — or just the bare date when no time was given,
 * since "a day, no particular hour" is a real answer the spec asks for.
 */
export function customSchedule(date: string, time: string): string | null {
  if (!date) return null;
  if (!time) return date;

  const at = new Date(`${date}T${time}`);
  return Number.isNaN(at.getTime()) ? date : at.toISOString();
}

/**
 * Renders a schedule string for display, in the viewer's own locale and
 * timezone — the wire format is just an ISO instant or a bare date, and
 * every viewer resolves it to their own wall-clock time independently.
 */
export function formatSchedule(schedule: string, locale: string): string {
  // A bare date (no time component) formats as a date only, not midnight.
  if (!schedule.includes("T")) {
    const at = new Date(`${schedule}T00:00:00`);
    if (Number.isNaN(at.getTime())) return schedule;
    return at.toLocaleDateString(locale, { month: "short", day: "numeric" });
  }

  const at = new Date(schedule);
  if (Number.isNaN(at.getTime())) return schedule;

  return at.toLocaleString(locale, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}
