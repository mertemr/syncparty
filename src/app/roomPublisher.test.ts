import { describe, expect, it } from "vitest";

import type { RoomSnapshot } from "@/shared/types/RoomSnapshot";
import type { SessionState } from "@/shared/types/SessionState";
import type { WatchState } from "@/shared/types/WatchState";

import { roomAfterWatchChange } from "./roomPublisher";

const ROOM: RoomSnapshot = {
  connected: true,
  rooms: [
    {
      name: "movie-night",
      watchers: [],
      everyoneOnTheSameFile: true,
      fileCompatibility: "exact",
    },
  ],
};

function watch(role: WatchState["role"]): WatchState {
  return {
    role,
    room: "movie-night",
    username: "mert",
    connection: { state: "connected" },
    player: { state: "idle" },
    ready: false,
  };
}

function hosting(monitorAttached: boolean): SessionState {
  return {
    phase: "hosting",
    invite: { endpoint: "abc", password: "pw", room: "movie-night" },
    inviteCode: "SP2.abc",
    deepLink: "syncparty://join/abc",
    serverAddress: "127.0.0.1:8999",
    server: { state: "running", port: 8999 },
    monitorAttached,
  };
}

const IDLE: SessionState = { phase: "idle" };

describe("roomAfterWatchChange", () => {
  it("keeps the room while a watch is still going, whoever publishes it", () => {
    expect(roomAfterWatchChange(ROOM, watch("guest"), IDLE)).toBe(ROOM);
    expect(roomAfterWatchChange(ROOM, watch("host"), hosting(false))).toBe(ROOM);
    expect(roomAfterWatchChange(ROOM, watch("host"), hosting(true))).toBe(ROOM);
  });

  it("drops a guest's room once it leaves: its own client was the only source", () => {
    expect(roomAfterWatchChange(ROOM, null, IDLE)).toBeNull();
  });

  it("keeps the host's room when it stops watching with the monitor on", () => {
    expect(roomAfterWatchChange(ROOM, null, hosting(true))).toBe(ROOM);
  });

  it("drops the host's room when it stops watching with the monitor off", () => {
    expect(roomAfterWatchChange(ROOM, null, hosting(false))).toBeNull();
  });

  it("does nothing to a room that was never there", () => {
    expect(roomAfterWatchChange(null, null, IDLE)).toBeNull();
    expect(roomAfterWatchChange(null, watch("guest"), IDLE)).toBeNull();
  });
});
