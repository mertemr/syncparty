import { describe, expect, it } from "vitest";

import type { PreflightItem } from "@/shared/types/PreflightItem";
import type { PreflightReport } from "@/shared/types/PreflightReport";

import { getStripState, summariseReady } from "./stripState";

function item(overrides: Partial<PreflightItem> = {}): PreflightItem {
  return {
    id: "mpv",
    displayName: "mpv",
    status: { state: "installed", version: "0.41.0", path: null },
    canAutoInstall: true,
    needsElevation: false,
    manualUrl: "https://mpv.io/installation/",
    supportsManualPath: true,
    overridePath: null,
    ...overrides,
  };
}

function report(items: PreflightItem[]): PreflightReport {
  return { mode: "host", items };
}

describe("getStripState", () => {
  it("is checking until a report arrives", () => {
    expect(getStripState(null)).toBe("checking");
  });

  it("is ready when nothing is missing", () => {
    expect(getStripState(report([item()]))).toBe("ready");
  });

  it("is blocked when any item is missing", () => {
    const state = getStripState(report([item({ status: { state: "missing" } })]));
    expect(state).toBe("blocked");
  });

  // "Not missing" is not the same as "usable". A location that holds some
  // other program — VLC, left over from when either player would do — blocks
  // a party just as completely as an absent mpv, and the strip is where the
  // user finds that out.
  it("is blocked when the chosen program runs but is not mpv", () => {
    const state = getStripState(
      report([
        item({
          status: {
            state: "unusable",
            path: "/usr/bin/vlc",
            reason: "VLC media player 3.0.21 Vetinari",
          },
        }),
      ]),
    );
    expect(state).toBe("blocked");
  });

  // An empty report means the backend found nothing to check for this mode,
  // which is a green light rather than a stall.
  it("is ready for an empty item list", () => {
    expect(getStripState(report([]))).toBe("ready");
  });
});

describe("summariseReady", () => {
  it("names the player with its version", () => {
    expect(summariseReady(report([item()]))).toBe("mpv 0.41.0");
  });

  it("omits the version when the tool does not report one", () => {
    const line = summariseReady(
      report([item({ status: { state: "installed", version: null, path: null } })]),
    );
    expect(line).toBe("mpv");
  });
});
