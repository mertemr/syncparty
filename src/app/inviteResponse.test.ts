import { describe, expect, it } from "vitest";

import { inviteResponse, isPartyRunning } from "./inviteResponse";

describe("isPartyRunning", () => {
  it("counts a party that is still starting", () => {
    expect(isPartyRunning("starting")).toBe(true);
    expect(isPartyRunning("hosting")).toBe(true);
  });

  it("does not count a session that never came up", () => {
    expect(isPartyRunning("idle")).toBe(false);
    expect(isPartyRunning("failed")).toBe(false);
  });
});

describe("inviteResponse", () => {
  it("does nothing until a link actually arrives", () => {
    expect(inviteResponse({ invite: false, phase: "idle" })).toBe("ignore");
  });

  it("settles the mode question when no party is running", () => {
    expect(inviteResponse({ invite: true, phase: "idle" })).toBe(
      "switchToGuest",
    );
  });

  /**
   * The bug this exists to prevent: the screen used to flip to the guest side
   * while the Syncplay server kept running behind it, with nothing left on
   * screen pointing at the party or able to stop it.
   */
  it("asks before abandoning a party that is already up", () => {
    expect(inviteResponse({ invite: true, phase: "hosting" })).toBe(
      "askToStopHosting",
    );
  });

  /**
   * The server is coming up in the background, so walking away silently
   * strands the same process a moment later.
   */
  it("asks while the party is still starting", () => {
    expect(inviteResponse({ invite: true, phase: "starting" })).toBe(
      "askToStopHosting",
    );
  });

  /** A party that failed to start has nothing to lose. */
  it("switches straight over after a failed start", () => {
    expect(inviteResponse({ invite: true, phase: "failed" })).toBe(
      "switchToGuest",
    );
  });
});
