import { useState } from "react";

import { useCopy } from "@/shared/hooks/useCopy";
import { useTranslate } from "@/shared/i18n";
import { errorMessage, ipc } from "@/shared/ipc";
import { Badge, Button, Card, Dot } from "@/shared/ui";
import type { DiagnosticsReport } from "@/shared/types/DiagnosticsReport";
import type { TransportReport } from "@/shared/types/TransportReport";

import { safeToShare } from "./diagnosticsReport";

export function DiagnosticsPanel() {
  const t = useTranslate();
  const { copy, copiedKey } = useCopy();
  const [report, setReport] = useState<DiagnosticsReport | null>(null);
  const [running, setRunning] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);

  async function run() {
    setRunning(true);
    setProblem(null);
    try {
      setReport(await ipc.runDiagnostics());
    } catch (error) {
      setProblem(errorMessage(error));
    } finally {
      setRunning(false);
    }
  }

  const installed =
    report?.dependencies.items.filter((item) => item.status.state === "installed")
      .length ?? 0;
  const dependencyTotal = report?.dependencies.items.length ?? 0;
  const dependenciesReady = dependencyTotal > 0 && installed === dependencyTotal;


  return (
    <Card
      title={t("settings.diagnostics.title")}
      action={
        report && (
          <Badge tone={dependenciesReady ? "good" : "warn"}>
            {dependenciesReady
              ? t("settings.diagnostics.healthy")
              : t("settings.diagnostics.attention")}
          </Badge>
        )
      }
    >
      <p className="text-sm leading-relaxed text-ink-muted">
        {t("settings.diagnostics.hint")}
      </p>

      {report && (
        <div className="mt-4 divide-y divide-line/60 rounded-xl border border-line/70 bg-canvas/35 px-4">
          <HealthRow
            label={t("settings.diagnostics.endpoint")}
            good={report.endpoint != null}
            neutral={report.endpoint == null}
            detail={
              report.endpoint
                ? `${report.endpoint.slice(0, 8)}…`
                : t("settings.diagnostics.endpointUnset")
            }
          />
          <HealthRow
            label={t("settings.diagnostics.dependencies")}
            good={dependenciesReady}
            detail={`${installed}/${dependencyTotal} ${t("settings.diagnostics.ready")}`}
          />
          <HealthRow
            label={t("settings.diagnostics.session")}
            good={report.session.phase === "hosting"}
            neutral={report.session.phase === "idle"}
            detail={t(`settings.diagnostics.session.${report.session.phase}`)}
          />

          {report.transport ? (
            <TransportRows transport={report.transport} />
          ) : (
            <HealthRow
              label={t("settings.diagnostics.transport")}
              good={false}
              detail={t("settings.diagnostics.transport.failed")}
            />
          )}
        </div>
      )}

      {report?.transportError && (
        <p className="mt-2 text-xs break-words text-ink-faint">
          {report.transportError}
        </p>
      )}

      <div className="mt-4 flex flex-wrap gap-2">
        <Button variant="primary" disabled={running} onClick={() => void run()}>
          {running
            ? t("settings.diagnostics.running")
            : report
              ? t("settings.diagnostics.runAgain")
              : t("settings.diagnostics.run")}
        </Button>
        {report && (
          <Button
            onClick={() =>
              void copy(
                "diagnostics",
                JSON.stringify(safeToShare(report), null, 2),
              ).catch((error) => setProblem(errorMessage(error)))
            }
          >
            {copiedKey === "diagnostics"
              ? t("common.copied")
              : t("settings.diagnostics.copy")}
          </Button>
        )}
      </div>

      {problem && <p className="mt-3 text-sm text-bad">{problem}</p>}
    </Card>
  );
}

/**
 * The three things that decide whether two machines can reach each other.
 *
 * Read top to bottom they answer one question: is there a way out (relay), is
 * this network a hard one (carrier NAT), and what did the connection actually
 * turn out to be. The last row is the only real proof, and it is empty until
 * somebody is connected — which the row says rather than leaving blank.
 */
function TransportRows({ transport }: { transport: TransportReport }) {
  const t = useTranslate();

  const connected = transport.relays.filter((relay) => relay.connected).length;
  const direct = transport.peers.filter((peer) => peer.kind === "direct").length;
  const relayed = transport.peers.filter((peer) => peer.kind === "relayed").length;

  return (
    <>
      <HealthRow
        label={t("settings.diagnostics.relay")}
        good={connected > 0}
        detail={
          transport.relays.length === 0
            ? t("settings.diagnostics.relay.none")
            : `${connected}/${transport.relays.length} ${t("settings.diagnostics.relay.connected")}`
        }
      />

      <HealthRow
        label={t("settings.diagnostics.nat")}
        // Carrier NAT is not a fault — it is the case syncparty exists to
        // survive — so it reads as something to be aware of, not as broken.
        good={transport.behindCarrierNat === false}
        neutral={transport.behindCarrierNat == null}
        detail={
          transport.behindCarrierNat == null
            ? t("settings.diagnostics.nat.unknown")
            : transport.behindCarrierNat
              ? t("settings.diagnostics.nat.carrier")
              : t("settings.diagnostics.nat.routable")
        }
      />

      <HealthRow
        label={t("settings.diagnostics.paths")}
        good={direct > 0}
        neutral={transport.peers.length === 0}
        detail={
          transport.peers.length === 0
            ? t("settings.diagnostics.paths.none")
            : [
                direct > 0 && `${direct} ${t("settings.diagnostics.path.direct")}`,
                relayed > 0 && `${relayed} ${t("settings.diagnostics.path.relayed")}`,
              ]
                .filter(Boolean)
                .join(" · ") || t("settings.diagnostics.path.unknown")
        }
      />

      {transport.peers.map((peer) => (
        <div key={peer.peer} className="flex items-center gap-3 py-2 pl-5">
          <span className="flex-1 truncate font-mono text-xs text-ink-faint">
            {peer.peer.slice(0, 8)}…
          </span>
          <span className="text-xs text-ink-muted">
            {t(`settings.diagnostics.path.${peer.kind}`)}
            {peer.rttMs != null && ` · ${Number(peer.rttMs)} ms`}
          </span>
        </div>
      ))}
    </>
  );
}

function HealthRow({
  label,
  detail,
  good,
  neutral = false,
}: {
  label: string;
  detail: string;
  good: boolean;
  neutral?: boolean;
}) {
  const tone = neutral ? "neutral" : good ? "good" : "warn";
  return (
    <div className="flex items-center gap-3 py-3">
      <Dot tone={tone} />
      <span className="flex-1 text-sm font-medium text-ink">{label}</span>
      <span className="text-xs text-ink-muted">{detail}</span>
    </div>
  );
}
