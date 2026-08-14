import { describe, expect, it } from "vitest";

import type { PreflightItem } from "@/shared/types/PreflightItem";
import type { PreflightReport } from "@/shared/types/PreflightReport";

import { getStripState, summariseReady } from "./stripState";

function item(overrides: Partial<PreflightItem> = {}): PreflightItem {
  return {
    id: "syncplayClient",
    displayName: "Syncplay",
    status: { state: "installed", version: "1.7.2", path: null },
    canAutoInstall: true,
    needsElevation: false,
    manualUrl: "https://syncplay.pl",
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
    const state = getStripState(
      report([item(), item({ id: "mpv", status: { state: "missing" } })]),
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
  it("joins name and version for each item", () => {
    const line = summariseReady(
      report([
        item(),
        item({
          id: "mpv",
          displayName: "mpv",
          status: { state: "installed", version: "0.38", path: null },
        }),
      ]),
    );
    expect(line).toBe("Syncplay 1.7.2 · mpv 0.38");
  });

  it("omits the version when the tool does not report one", () => {
    const line = summariseReady(
      report([item({ status: { state: "installed", version: null, path: null } })]),
    );
    expect(line).toBe("Syncplay");
  });
});
