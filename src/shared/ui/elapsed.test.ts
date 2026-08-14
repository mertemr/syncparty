import { describe, expect, it } from "vitest";

import { formatElapsed } from "./elapsed";

describe("formatElapsed", () => {
  it("pads every field to two digits", () => {
    expect(formatElapsed(0)).toBe("00:00:00");
    expect(formatElapsed(9_000)).toBe("00:00:09");
    expect(formatElapsed(61_000)).toBe("00:01:01");
  });

  it("counts past an hour without rolling over", () => {
    expect(formatElapsed(3_600_000)).toBe("01:00:00");
    expect(formatElapsed(45_296_000)).toBe("12:34:56");
  });

  // A clock correction mid-party must not print a negative counter.
  it("clamps negative input to zero", () => {
    expect(formatElapsed(-5_000)).toBe("00:00:00");
  });

  it("truncates rather than rounds, so the counter never shows a second early", () => {
    expect(formatElapsed(1_999)).toBe("00:00:01");
  });
});
