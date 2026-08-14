import { describe, expect, it } from "vitest";

import type { FileCompatibility } from "@/shared/types/FileCompatibility";
import type { RoomSnapshot } from "@/shared/types/RoomSnapshot";

import { getLobbyBadge, getLobbyState } from "./lobbyState";

function snapshot(
  compatibility: FileCompatibility,
  people: Array<{ name: string; ready: boolean; hasFile: boolean }>,
): RoomSnapshot {
  return {
    connected: true,
    rooms: [
      {
        name: "MovieNight",
        everyoneOnTheSameFile: compatibility !== "mismatch",
        fileCompatibility: compatibility,
        watchers: people.map((person) => ({
          name: person.name,
          isReady: person.ready,
          isController: false,
          file: person.hasFile
            ? { name: `${person.name}.mkv`, durationSeconds: 7_200 }
            : null,
        })),
      },
    ],
  };
}

describe("getLobbyState", () => {
  it("starts only when every person is ready with compatible files", () => {
    const state = getLobbyState(
      snapshot("durationMatch", [
        { name: "ayse", ready: true, hasFile: true },
        { name: "taha", ready: true, hasFile: true },
      ]),
    );

    expect(state.everyoneReady).toBe(true);
    expect(state.readyCount).toBe(2);
    expect(state.filesCompatible).toBe(true);
  });

  it("waits when a person has no file or is not ready", () => {
    const state = getLobbyState(
      snapshot("waiting", [
        { name: "ayse", ready: true, hasFile: true },
        { name: "taha", ready: false, hasFile: false },
      ]),
    );

    expect(state.everyoneReady).toBe(false);
    expect(state.readyCount).toBe(1);
    expect(state.fileCount).toBe(1);
  });

  it("does not count the read-only monitor as a guest", () => {
    const state = getLobbyState(
      snapshot("exact", [
        { name: "ayse", ready: true, hasFile: true },
        { name: "syncparty-panel", ready: false, hasFile: false },
      ]),
    );

    expect(state.people.map((person) => person.name)).toEqual(["ayse"]);
    expect(state.everyoneReady).toBe(true);
  });

  it("blocks a confirmed file mismatch", () => {
    const state = getLobbyState(
      snapshot("mismatch", [
        { name: "ayse", ready: true, hasFile: true },
        { name: "taha", ready: true, hasFile: true },
      ]),
    );

    expect(state.everyoneReady).toBe(false);
    expect(state.filesCompatible).toBe(false);
  });
});

describe("getLobbyBadge", () => {
  it("says nobody has joined rather than claiming progress", () => {
    const state = getLobbyState(snapshot("waiting", []));

    expect(getLobbyBadge(state)).toBe("empty");
  });

  it("says people are getting ready once somebody is there", () => {
    const state = getLobbyState(
      snapshot("waiting", [{ name: "ayse", ready: false, hasFile: false }]),
    );

    expect(getLobbyBadge(state)).toBe("waiting");
  });

  it("says ready when everyone is", () => {
    const state = getLobbyState(
      snapshot("exact", [{ name: "ayse", ready: true, hasFile: true }]),
    );

    expect(getLobbyBadge(state)).toBe("ready");
  });

  it("treats a room holding only the monitor as empty", () => {
    const state = getLobbyState(
      snapshot("waiting", [
        { name: "syncparty-panel", ready: false, hasFile: false },
      ]),
    );

    expect(getLobbyBadge(state)).toBe("empty");
  });
});
