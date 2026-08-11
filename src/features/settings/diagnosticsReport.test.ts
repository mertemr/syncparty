import { describe, expect, it } from "vitest";

import type { DiagnosticsReport } from "@/shared/types/DiagnosticsReport";

import { safeToShare } from "./diagnosticsReport";

describe("safeToShare", () => {
  it("keeps useful health data without exposing addresses or local paths", () => {
    const report: DiagnosticsReport = {
      appVersion: "0.2.1",
      operatingSystem: "windows",
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
    };

    const shared = JSON.stringify(safeToShare(report));

    expect(shared).toContain('"version":"0.40"');
    expect(shared).toContain('"hasEndpoint":true');
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
      dependencies: { mode: "guest", items: [] },
      endpoint: null,
      session: { phase: "failed", message: "secret detail" },
    } satisfies DiagnosticsReport;

    const shared = safeToShare(report);

    expect(shared.hasEndpoint).toBe(false);
    expect(shared.session).toEqual({ phase: "failed" });
    expect(JSON.stringify(shared)).not.toContain("private");
    expect(JSON.stringify(shared)).not.toContain("secret detail");
  });
});
