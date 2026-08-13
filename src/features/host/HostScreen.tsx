import { useState } from "react";

import { useAppState } from "@/app/AppState";
import { useTranslate, type MessageKey } from "@/shared/i18n";
import { ipc } from "@/shared/ipc";
import { Badge, Button, Card, Dot } from "@/shared/ui";
import type { StartupStep } from "@/shared/types/StartupStep";

import { InviteCard } from "./InviteCard";
import { LobbyPanel } from "./LobbyPanel";
import { RoomPanel } from "./RoomPanel";

const STEP_LABELS: Record<StartupStep, MessageKey> = {
  joiningNetwork: "host.step.joiningNetwork",
  openingTunnel: "host.step.openingTunnel",
  startingServer: "host.step.startingServer",
  attachingMonitor: "host.step.attachingMonitor",
};

export function HostScreen() {
  const t = useTranslate();
  const { session, room, serverLog, reportFailure } = useAppState();

  const [busy, setBusy] = useState(false);
  const [logOpen, setLogOpen] = useState(false);
  const [joinState, setJoinState] = useState<"idle" | "opening" | "opened">(
    "idle",
  );

  const starting = session.phase === "starting";
  const hosting = session.phase === "hosting";

  async function run(action: () => Promise<unknown>) {
    setBusy(true);
    try {
      await action();
    } catch (error) {
      reportFailure(error);
    } finally {
      setBusy(false);
    }
  }

  /**
   * The host watches too, so they need their own client. It connects on the
   * bound address rather than the invite's — the backend handles that.
   */
  async function join() {
    setJoinState("opening");
    try {
      await ipc.joinHostedParty();
      setJoinState("opened");
    } catch (error) {
      setJoinState("idle");
      reportFailure(error);
    }
  }

  return (
    <div className="mx-auto max-w-4xl space-y-5 px-8 py-8">
      <Card>
        <div className="flex items-center justify-between gap-4">
          <div className="min-w-0">
            <div className="flex items-center gap-2">
              <Dot tone={hosting ? "good" : starting ? "warn" : "neutral"} />
              <h1 className="text-lg font-bold tracking-tight text-ink">
                {t("host.title")}
              </h1>
              {hosting && <Badge tone="good">{t("host.live")}</Badge>}
            </div>
            <p className="mt-1 text-sm text-ink-muted">
              {starting
                ? t(STEP_LABELS[session.step])
                : hosting
                  ? `${session.invite.endpoint.slice(0, 8)}…`
                  : t("host.idle.hint")}
            </p>
          </div>

          {hosting ? (
            <div className="flex shrink-0 items-center gap-2">
              <Button
                variant="primary"
                disabled={joinState === "opening"}
                onClick={() => void join()}
              >
                {joinState === "opening" ? t("host.joining") : t("host.join")}
              </Button>
              <Button
                variant="danger"
                disabled={busy}
                onClick={() => {
                  setJoinState("idle");
                  void run(ipc.stopHosting);
                }}
              >
                {t("host.stop")}
              </Button>
            </div>
          ) : (
            <Button
              variant="primary"
              disabled={busy || starting}
              onClick={() => void run(ipc.startHosting)}
            >
              {busy || starting ? t("host.starting") : t("host.start")}
            </Button>
          )}
        </div>
      </Card>

      {hosting && (
        <>
          {joinState === "opened" && (
            <p className="rounded-panel border border-good/40 bg-good/10 px-4 py-3 text-sm text-good">
              {t("host.joined")}
            </p>
          )}
          <InviteCard hosting={session} />
          {session.monitorAttached && <LobbyPanel snapshot={room} />}
          <RoomPanel snapshot={room} monitorAttached={session.monitorAttached} />
        </>
      )}

      <Card
        title={t("host.logs.title")}
        action={
          <Button variant="ghost" onClick={() => setLogOpen((open) => !open)}>
            {logOpen ? t("host.logs.hide") : t("host.logs.show")}
          </Button>
        }
      >
        {/* Collapsed shows nothing rather than a line count: the count is not
            information anyone acts on, and the header already says the log is
            there. */}
        {logOpen &&
          (serverLog.length === 0 ? (
            <p className="text-sm text-ink-faint">{t("host.logs.empty")}</p>
          ) : (
            <pre className="selectable max-h-64 overflow-auto rounded-lg bg-canvas p-3 font-mono text-xs leading-relaxed text-ink-muted">
              {serverLog.join("\n")}
            </pre>
          ))}
      </Card>
    </div>
  );
}
