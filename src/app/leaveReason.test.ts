import { describe, expect, it } from "vitest";

import type { GuestJoinState } from "@/features/guest/guestJoin";
import type { Invite } from "@/shared/types/Invite";
import type { SessionState } from "@/shared/types/SessionState";
import type { WatchState } from "@/shared/types/WatchState";

import { leaveReason } from "./leaveReason";

const INVITE: Invite = { endpoint: "abc", password: "pw", room: "movie-night" };

const IDLE: SessionState = { phase: "idle" };
const HOSTING: SessionState = {
  phase: "hosting",
  invite: INVITE,
  inviteCode: "SP2.abc",
  deepLink: "syncparty://join/abc",
  serverAddress: "127.0.0.1:8999",
  server: { state: "running", port: 8999 },
  monitorAttached: true,
};

const SETTLED: GuestJoinState = {
  resume: "settled",
  invite: null,
  fromLink: false,
  joining: null,
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

const base = { mode: "guest" as const, session: IDLE, guestJoin: SETTLED, watch: null };

describe("leaveReason", () => {
  it("lets a guest with nothing going step straight back", () => {
    expect(leaveReason(base)).toBeNull();
  });

  it("asks while a join is still connecting, before any watch state exists", () => {
    const guestJoin = { ...SETTLED, invite: INVITE, joining: INVITE };
    expect(leaveReason({ ...base, guestJoin })).toBe("joining");
  });

  it("asks while the last party is being rejoined", () => {
    const guestJoin = { ...SETTLED, resume: "running" as const };
    expect(leaveReason({ ...base, guestJoin })).toBe("joining");
  });

  it("does not count a resume that has not started as one under way", () => {
    const guestJoin = { ...SETTLED, resume: "untried" as const };
    expect(leaveReason({ ...base, guestJoin })).toBeNull();
  });

  it("asks a guest who is watching", () => {
    expect(leaveReason({ ...base, watch: watch("guest") })).toBe("watching");
  });

  it("asks about a guest party even from the host screen", () => {
    // Settings switched the mode with the party still open.
    expect(leaveReason({ ...base, mode: "host", watch: watch("guest") })).toBe("watching");

    const guestJoin = { ...SETTLED, invite: INVITE, joining: INVITE };
    expect(leaveReason({ ...base, mode: "host", guestJoin })).toBe("joining");
  });

  it("asks a host whose party is running or starting, before anything else", () => {
    expect(leaveReason({ ...base, mode: "host", session: HOSTING })).toBe("hosting");
    expect(
      leaveReason({
        ...base,
        mode: "host",
        session: { phase: "starting", step: "joiningNetwork" },
      }),
    ).toBe("hosting");
  });

  it("lets a host step back once the party is over, its own watch going with it", () => {
    expect(leaveReason({ ...base, mode: "host", watch: watch("host") })).toBeNull();
  });
});
