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
    transport: report.transport && {
      // Which addresses were discovered names this machine and its network.
      // How many there are, and whether any of them was carrier-grade, is the
      // part someone helping with a connection problem actually needs.
      addressCount: report.transport.addresses.length,
      behindCarrierNat: report.transport.behindCarrierNat,
      // Relay URLs are n0's public infrastructure rather than anything of the
      // user's, and which one is home explains a slow party, so they stay.
      relays: report.transport.relays.map((relay) => ({
        url: relay.url,
        connected: relay.connected,
        failed: relay.lastError != null,
      })),
      // A peer id names someone else's machine, so only the shape of each
      // connection survives — which is the whole question anyway.
      //
      // `Number` is not cosmetic: `rttMs` arrives as a bigint, and
      // `JSON.stringify` throws on those rather than skipping them.
      peers: report.transport.peers.map((peer) => ({
        kind: peer.kind,
        rttMs: peer.rttMs == null ? null : Number(peer.rttMs),
      })),
    },
    // The message can name a relay host or a local interface, so only the
    // fact of the failure crosses.
    transportFailed: report.transportError != null,
  };
}
