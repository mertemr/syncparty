import type { DiagnosticsReport } from "@/shared/types/DiagnosticsReport";

/** Removes local paths and this machine's address from copied reports. */
export function safeToShare(report: DiagnosticsReport) {
  return {
    appVersion: report.appVersion,
    operatingSystem: report.operatingSystem,
    mode: report.dependencies.mode,
    dependencies: report.dependencies.items.map((item) => ({
      id: item.id,
      status: item.status.state,
      version: item.status.state === "installed" ? item.status.version : null,
    })),
    // Whether this machine has an endpoint at all is useful; which endpoint
    // it is names the machine, and an invite naming it may still be live.
    hasEndpoint: report.endpoint != null,
    session: { phase: report.session.phase },
  };
}
