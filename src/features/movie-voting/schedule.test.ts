import { describe, expect, it } from "vitest";

import { customSchedule, formatSchedule, tomorrowIso, tonightIso } from "./schedule";

describe("tonightIso", () => {
  it("keeps the same calendar day, at 21:00 local time", () => {
    const now = new Date(2026, 7, 30, 14, 0, 0);
    const at = new Date(tonightIso(now));

    expect(at.getDate()).toBe(30);
    expect(at.getHours()).toBe(21);
  });
});

describe("tomorrowIso", () => {
  it("rolls over to the next calendar day", () => {
    const now = new Date(2026, 7, 30, 14, 0, 0);
    const at = new Date(tomorrowIso(now));

    expect(at.getDate()).toBe(31);
    expect(at.getHours()).toBe(21);
  });
});

describe("customSchedule", () => {
  it("returns null when no date was given", () => {
    expect(customSchedule("", "21:30")).toBeNull();
  });

  it("returns a bare date when no time was given", () => {
    expect(customSchedule("2026-08-31", "")).toBe("2026-08-31");
  });

  it("combines a date and time into a local instant", () => {
    const at = new Date(customSchedule("2026-08-31", "21:30")!);
    expect(at.getFullYear()).toBe(2026);
    expect(at.getHours()).toBe(21);
    expect(at.getMinutes()).toBe(30);
  });
});

describe("formatSchedule", () => {
  it("formats a bare date without a time of day", () => {
    expect(formatSchedule("2026-08-31", "en-US")).toBe("Aug 31");
  });

  it("formats an instant with both date and time", () => {
    const iso = new Date(2026, 7, 31, 21, 30).toISOString();
    expect(formatSchedule(iso, "en-US")).toContain("Aug 31");
  });

  it("falls back to the raw string when it cannot be parsed", () => {
    expect(formatSchedule("not-a-date", "en-US")).toBe("not-a-date");
  });
});
