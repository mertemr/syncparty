import { describe, expect, it } from "vitest";

import type { WatcherView } from "@/shared/types/WatcherView";

import { formatDuration, getChannelStatus } from "./channels";

function watcher(overrides: Partial<WatcherView> = {}): WatcherView {
  return {
    name: "ada",
    file: { name: "movie.mkv", durationSeconds: 7200 },
    isReady: false,
    isController: false,
    ...overrides,
  };
}

describe("getChannelStatus", () => {
  it("is noFile when nothing is open, whatever the ready flag says", () => {
    expect(getChannelStatus(watcher({ file: null, isReady: true }), true)).toBe(
      "noFile",
    );
  });

  // The mismatch belongs to the room, so it outranks this person's own state:
  // someone "ready" on the wrong file is the exact failure being warned about.
  it("is trackingError when the room's files do not match", () => {
    expect(getChannelStatus(watcher({ isReady: true }), false)).toBe(
      "trackingError",
    );
  });

  it("is ready when the file matches and the person is ready", () => {
    expect(getChannelStatus(watcher({ isReady: true }), true)).toBe("ready");
  });

  it("is waiting when the file matches but the person is not ready", () => {
    expect(getChannelStatus(watcher({ isReady: false }), true)).toBe("waiting");
  });
});

describe("formatDuration", () => {
  it("drops the hour field for anything under an hour", () => {
    expect(formatDuration(95)).toBe("1:35");
  });

  it("keeps the hour field and pads the rest above an hour", () => {
    expect(formatDuration(9660)).toBe("2:41:00");
  });
});
