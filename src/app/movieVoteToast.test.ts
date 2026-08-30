import { describe, expect, it } from "vitest";

import { movieVoteToastMessage } from "./movieVoteToast";
import type { MovieVoteSnapshot } from "@/shared/types/MovieVoteSnapshot";

const t = (key: string) => key;

function snapshot(overrides: Partial<MovieVoteSnapshot>): MovieVoteSnapshot {
  return {
    id: "vote-1",
    phase: "draft",
    createdAt: 0n,
    schedule: null,
    candidates: [],
    participants: [],
    result: null,
    ...overrides,
  };
}

describe("movieVoteToastMessage", () => {
  it("says nothing when there is no vote", () => {
    expect(movieVoteToastMessage(null, null, t)).toBeNull();
  });

  it("announces the vote opening", () => {
    const message = movieVoteToastMessage(
      snapshot({ phase: "draft" }),
      snapshot({ phase: "open" }),
      t,
    );
    expect(message).toBe("movieVote.open");
  });

  it("does not re-announce staying open", () => {
    const open = snapshot({ phase: "open" });
    expect(movieVoteToastMessage(open, open, t)).toBeNull();
  });

  it("announces a cancellation", () => {
    const message = movieVoteToastMessage(
      snapshot({ phase: "open" }),
      snapshot({ phase: "cancelled" }),
      t,
    );
    expect(message).toBe("movieVote.cancelled");
  });

  it("announces the winner by title when the vote closes decisively", () => {
    const message = movieVoteToastMessage(
      snapshot({ phase: "open" }),
      snapshot({
        phase: "completed",
        candidates: [
          { tmdbId: 1n, title: "Interstellar", poster: null, releaseDate: null, overview: null, genres: [], rating: 0 },
        ],
        result: { tally: [], winner: 1n, tied: [] },
      }),
      t,
    );
    expect(message).toBe("movieVote.winner: Interstellar");
  });

  it("announces a plain close when the vote ends tied", () => {
    const message = movieVoteToastMessage(
      snapshot({ phase: "open" }),
      snapshot({ phase: "completed", result: { tally: [], winner: null, tied: [1n, 2n] } }),
      t,
    );
    expect(message).toBe("movieVote.completed");
  });

  it("announces the winner once a tie is resolved after the fact", () => {
    const tied = snapshot({ phase: "completed", result: { tally: [], winner: null, tied: [1n, 2n] } });
    const resolved = snapshot({
      phase: "completed",
      candidates: [
        { tmdbId: 2n, title: "Parasite", poster: null, releaseDate: null, overview: null, genres: [], rating: 0 },
      ],
      result: { tally: [], winner: 2n, tied: [] },
    });

    expect(movieVoteToastMessage(tied, resolved, t)).toBe("movieVote.winner: Parasite");
  });

  it("does not re-announce an already-resolved completion", () => {
    const resolved = snapshot({ phase: "completed", result: { tally: [], winner: 1n, tied: [] } });
    expect(movieVoteToastMessage(resolved, resolved, t)).toBeNull();
  });
});
