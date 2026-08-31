import type { MessageKey } from "@/shared/i18n";
import type { MovieVoteSnapshot } from "@/shared/types/MovieVoteSnapshot";

/**
 * Turns a movie-vote transition into one toast-worthy sentence, or nothing.
 *
 * Pure and phase-based rather than a deep diff: the only transitions worth
 * announcing are the vote's own lifecycle (opened, cancelled, closed, a
 * winner settled) — per-participant activity (someone voted, someone set
 * their status) is deliberately left out, both to avoid spamming the corner
 * of the screen every time anyone touches anything and because a peer id is
 * not a name anyone would recognise (see `VoteParticipant.displayName`).
 */
export function movieVoteToastMessage(
  previous: MovieVoteSnapshot | null,
  next: MovieVoteSnapshot | null,
  t: (key: MessageKey) => string,
): string | null {
  if (!next) return null;
  const previousPhase = previous?.phase;

  if (previousPhase !== "open" && next.phase === "open") {
    return t("movieVote.open");
  }

  if (previousPhase !== "cancelled" && next.phase === "cancelled") {
    return t("movieVote.cancelled");
  }

  if (previousPhase !== "completed" && next.phase === "completed") {
    const winnerId = next.result?.winner;
    if (winnerId == null) return t("movieVote.completed");

    const title = next.candidates.find((candidate) => candidate.tmdbId === winnerId)?.title;
    return title ? `${t("movieVote.winner")}: ${title}` : t("movieVote.completed");
  }

  // A tie that was resolved after the vote already read "completed" — the
  // phase itself doesn't change, only `result.winner` does, so that has to
  // be checked on its own.
  if (
    previousPhase === "completed" &&
    next.phase === "completed" &&
    previous?.result?.winner == null &&
    next.result?.winner != null
  ) {
    const title = next.candidates.find((candidate) => candidate.tmdbId === next.result?.winner)?.title;
    return title ? `${t("movieVote.winner")}: ${title}` : null;
  }

  return null;
}
