import { describe, expect, it } from "vitest";

import { watchedIdOrder } from "./watchlist";

const night = (tmdbId: bigint, watchedAt: bigint) => ({ tmdbId, watchedAt });

describe("watchedIdOrder", () => {
  it("puts real movie nights newest first", () => {
    const order = watchedIdOrder([night(1n, 100n), night(2n, 300n), night(3n, 200n)], []);
    expect(order).toEqual(["2", "3", "1"]);
  });

  it("appends manual marks after the history", () => {
    const order = watchedIdOrder([night(1n, 100n)], ["7", "8"]);
    expect(order).toEqual(["1", "7", "8"]);
  });

  it("keeps a movie that is both watched and marked in its history slot", () => {
    const order = watchedIdOrder([night(1n, 100n), night(2n, 300n)], ["1", "9"]);
    expect(order).toEqual(["2", "1", "9"]);
  });

  it("holds up past the 53-bit range JS numbers stop at", () => {
    const older = 1_700_000_000_000_000_001n;
    const newer = 1_700_000_000_000_000_002n;
    expect(watchedIdOrder([night(1n, older), night(2n, newer)], [])).toEqual(["2", "1"]);
  });

  it("survives an empty history and no marks", () => {
    expect(watchedIdOrder([], [])).toEqual([]);
  });
});
