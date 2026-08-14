import type { PreflightReport } from "@/shared/types/PreflightReport";

/** What the strip under the launch slots is saying right now. */
export type StripState = "checking" | "ready" | "blocked";

export function getStripState(report: PreflightReport | null): StripState {
  if (report === null) return "checking";

  return report.items.every((item) => item.status.state !== "missing")
    ? "ready"
    : "blocked";
}

/** The one line shown when there is nothing to do: what is installed. */
export function summariseReady(report: PreflightReport): string {
  return report.items
    .map((item) =>
      item.status.state === "installed" && item.status.version
        ? `${item.displayName} ${item.status.version}`
        : item.displayName,
    )
    .join(" · ");
}
