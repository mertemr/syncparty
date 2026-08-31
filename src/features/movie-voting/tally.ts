import type { MovieCandidate } from "@/shared/types/MovieCandidate";
import type { VoteParticipant } from "@/shared/types/VoteParticipant";

export interface CandidateVoteCount {
  tmdbId: bigint;
  votes: number;
}

/**
 * Live vote counts, computed on the frontend from `participants` rather than
 * read from `result` — the backend only fills in `result` once the vote
 * closes, but every participant's current pick is broadcast the whole time
 * the vote is open, which is what the spec's live counts are built from.
 */
export function tallyVotes(
  candidates: MovieCandidate[],
  participants: VoteParticipant[],
): CandidateVoteCount[] {
  return candidates.map((candidate) => ({
    tmdbId: candidate.tmdbId,
    votes: participants.filter((participant) => participant.selectedMovie === candidate.tmdbId)
      .length,
  }));
}
