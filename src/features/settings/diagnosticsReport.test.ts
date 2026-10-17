import { describe, expect, it } from "vitest";

import type { DiagnosticsReport } from "@/shared/types/DiagnosticsReport";

import { safeToShare } from "./diagnosticsReport";

describe("safeToShare", () => {
  it("keeps useful health data without exposing addresses or local paths", () => {
    const report: DiagnosticsReport = {
      appVersion: "0.2.1",
      operatingSystem: "windows",
      secretStorage: "keychain",
      dependencies: {
        mode: "host",
        items: [
          {
            id: "mpv",
            displayName: "mpv",
            status: {
              state: "installed",
              version: "0.40",
              path: "C:\\Users\\Taha\\private\\mpv.exe",
            },
            canAutoInstall: true,
            needsElevation: false,
            manualUrl: "https://example.com",
            supportsManualPath: true,
            overridePath: "C:\\Users\\Taha\\private",
          },
        ],
      },
      endpoint: "k7yvcfvw3wm2gqbkbmzsvwmnl6yxu4h3fbtjgnfhpcowcprivate",
      session: { phase: "idle" },
      watch: null,
      transport: {
        endpointId: "k7yvcfvw3wm2gqbkbmzsvwmnl6yxu4h3fbtjgnfhpcowcprivate",
        addresses: ["192.168.1.42:11204", "203.0.113.7:11204"],
        behindCarrierNat: false,
        relays: [
          { url: "https://euw1-1.relay.iroh.link", connected: true, lastError: null },
        ],
        peers: [
          {
            peer: "92cdf7276ea13fb0b593a1a6bbe29ddf2fab708e332ac2f8f2e1private",
            kind: "direct",
            remote: "Ip(203.0.113.9:50311)",
            rttMs: 24n,
          },
        ],
      },
      transportError: null,
    };

    const shared = JSON.stringify(safeToShare(report));

    expect(shared).toContain('"version":"0.40"');
    expect(shared).toContain('"hasEndpoint":true');
    // The shape of the connection is the point of sharing a report at all.
    expect(shared).toContain('"kind":"direct"');
    expect(shared).toContain('"rttMs":24');
    expect(shared).toContain('"behindCarrierNat":false');
    // ...but not this machine's addresses, nor the other end's.
    expect(shared).not.toContain("192.168.1.42");
    expect(shared).not.toContain("203.0.113.7");
    expect(shared).not.toContain("203.0.113.9");
    expect(shared).not.toContain("92cdf7276ea13fb0b593a1a6bbe29ddf");
    // The endpoint id names this machine, and an invite carrying it may still
    // be live, so the report says whether there is one and never which.
    expect(shared).not.toContain("k7yvcfvw3wm2gqbkbmzsvwmnl6yxu4h3fbtjgnfhpcow");
    expect(shared).not.toContain("Taha");
    expect(shared).not.toContain("example.com");
  });

  it("redacts diagnostic error details", () => {
    const report = {
      appVersion: "0.2.1",
      operatingSystem: "windows",
      secretStorage: "file",
      dependencies: { mode: "guest", items: [] },
      endpoint: null,
      session: { phase: "failed", message: "secret detail" },
      watch: null,
      transport: null,
      transportError: "could not reach relay-private.example: timed out",
    } satisfies DiagnosticsReport;

    const shared = safeToShare(report);

    expect(shared.hasEndpoint).toBe(false);
    expect(shared.session).toEqual({ phase: "failed" });
    // That the measurement failed is worth sharing; the message can name a
    // relay host or a local interface, so it is reduced to a flag.
    expect(shared.transportFailed).toBe(true);
    expect(JSON.stringify(shared)).not.toContain("private");
    expect(JSON.stringify(shared)).not.toContain("secret detail");
  });

  it("says whether a file is open without saying which, or who is watching it", () => {
    const report = {
      appVersion: "0.9.0",
      operatingSystem: "linux",
      secretStorage: "keychain",
      dependencies: { mode: "guest", items: [] },
      endpoint: null,
      session: { phase: "idle" },
      watch: {
        role: "guest",
        room: "taha-birthday-private",
        username: "mert-private",
        connection: { state: "connected" },
        player: {
          state: "loaded",
          file: { name: "Home.Video.2024.private.mkv", durationSeconds: 3600 },
          paused: true,
        },
        ready: true,
      },
      transport: null,
      transportError: null,
    } satisfies DiagnosticsReport;

    const shared = safeToShare(report);

    expect(shared.watch).toEqual({
      role: "guest",
      connection: "connected",
      player: "loaded",
      hasFile: true,
      ready: true,
    });
    expect(JSON.stringify(shared)).not.toContain("private");
  });

  // A rejection or a player failure can quote anything — a path, a room
  // name — so only the state survives, never its message.
  it("keeps watch failures to their state", () => {
    const report = {
      appVersion: "0.9.0",
      operatingSystem: "linux",
      secretStorage: "keychain",
      dependencies: { mode: "guest", items: [] },
      endpoint: null,
      session: { phase: "idle" },
      watch: {
        role: "host",
        room: "movie-night",
        username: null,
        connection: { state: "rejected", message: "room private-room is closed" },
        player: { state: "failed", message: "/home/private/mpv crashed" },
        ready: false,
      },
      transport: null,
      transportError: null,
    } satisfies DiagnosticsReport;

    const shared = safeToShare(report);

    expect(shared.watch).toMatchObject({
      connection: "rejected",
      player: "failed",
      hasFile: false,
    });
    expect(JSON.stringify(shared)).not.toContain("private");
  });

  it("reports nothing being watched as no watch at all", () => {
    const report = {
      appVersion: "0.9.0",
      operatingSystem: "linux",
      secretStorage: "keychain",
      dependencies: { mode: "host", items: [] },
      endpoint: null,
      session: { phase: "idle" },
      watch: null,
      transport: null,
      transportError: null,
    } satisfies DiagnosticsReport;

    expect(safeToShare(report).watch).toBeNull();
  });
});
