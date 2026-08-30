import { describe, expect, it } from "vitest";

import { tallyVotes } from "./tally";
import type { MovieCandidate } from "@/shared/types/MovieCandidate";
import type { VoteParticipant } from "@/shared/types/VoteParticipant";

function candidate(tmdbId: bigint): MovieCandidate {
  return {
    tmdbId,
    title: `Movie ${tmdbId}`,
    poster: null,
    releaseDate: null,
    overview: null,
    genres: [],
    rating: 0,
  };
}

function participant(peer: string, selectedMovie: bigint | null): VoteParticipant {
  return {
    peer,
    displayName: peer,
    participation: null,
    selectedMovie,
    respondedAt: null,
  };
}

describe("tallyVotes", () => {
  it("counts each candidate's votes independently", () => {
    const candidates = [candidate(1n), candidate(2n)];
    const participants = [
      participant("a", 1n),
      participant("b", 1n),
      participant("c", 2n),
    ];

    const counts = tallyVotes(candidates, participants);

    expect(counts).toEqual([
      { tmdbId: 1n, votes: 2 },
      { tmdbId: 2n, votes: 1 },
    ]);
  });

  it("gives a fresh candidate zero votes rather than omitting it", () => {
    const counts = tallyVotes([candidate(1n)], []);
    expect(counts).toEqual([{ tmdbId: 1n, votes: 0 }]);
  });

  it("ignores a participant who has not voted", () => {
    const counts = tallyVotes([candidate(1n)], [participant("a", null)]);
    expect(counts[0].votes).toBe(0);
  });
});
