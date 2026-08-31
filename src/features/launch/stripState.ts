import type { PreflightReport } from "@/shared/types/PreflightReport";

/** What the strip under the launch slots is saying right now. */
export type StripState = "checking" | "ready" | "blocked";

export function getStripState(report: PreflightReport | null): StripState {
  if (report === null) return "checking";

  // Installed, rather than "not missing": a dependency can also be present
  // and unable to run, and that blocks a party the same way an absent one
  // does. This mirrors `PreflightReport::is_satisfied` on the Rust side,
  // which asks the same question of the same states.
  return report.items.every((item) => item.status.state === "installed")
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
