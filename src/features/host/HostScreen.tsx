import { useEffect, useState } from "react";

import { useAppState } from "@/app/AppState";
import { useTranslate, type MessageKey } from "@/shared/i18n";
import { ipc } from "@/shared/ipc";
import { Badge, Button, Card, Counter, EmptyState, Rewind, cx } from "@/shared/ui";
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
  // When the party went live, for the counter. Taken from the transition the
  // frontend already sees rather than asking the backend for a start time.
  const [startedAt, setStartedAt] = useState<number | null>(null);

  const starting = session.phase === "starting";
  const hosting = session.phase === "hosting";

  useEffect(() => {
    setStartedAt(hosting ? Date.now() : null);
  }, [hosting]);

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

  const statusCard = (
    <Card>
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <div className="flex items-center gap-2.5">
            <span
              aria-hidden
              className={cx(
                "size-2.5 rounded-full",
                hosting
                  ? "bg-bad phosphor"
                  : starting
                    ? "bg-warn"
                    : "bg-ink-faint",
              )}
            />
            <span className="font-mono text-[11px] tracking-[0.22em] text-ink-muted uppercase">
              {hosting ? "REC" : starting ? t("host.starting") : "STANDBY"}
            </span>
            {hosting && startedAt !== null && <Counter since={startedAt} />}
            {hosting && <Badge tone="good">{t("host.live")}</Badge>}
          </div>

          <h1 className="mt-2 font-display text-xl font-extrabold tracking-tight text-ink [font-stretch:110%]">
            {t("host.title")}
          </h1>

          {starting ? (
            <div className="mt-3 max-w-xs">
              <Rewind label={t(STEP_LABELS[session.step])} />
            </div>
          ) : (
            <p className="mt-1 text-sm text-ink-muted">
              {hosting
                ? `${session.invite.endpoint.slice(0, 8)}…`
                : t("host.idle.hint")}
            </p>
          )}
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
  );

  const logsCard = (
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
          <EmptyState title={t("host.logs.empty")} />
        ) : (
          <pre className="selectable max-h-64 overflow-auto rounded-[var(--radius-control)] bg-canvas p-3 font-mono text-xs leading-relaxed text-ink-muted">
            {serverLog.join("\n")}
          </pre>
        ))}
    </Card>
  );

  // Before hosting starts there is nothing to fill a second column with —
  // the log is secondary, so it sits quietly under the hero card instead of
  // beside it, where a two-column grid gave it equal billing with the one
  // button that matters.
  if (!hosting) {
    return (
      <div className="mx-auto max-w-2xl space-y-5 px-8 py-8">
        {statusCard}
        {logsCard}
      </div>
    );
  }

  return (
    // `md` rather than `lg`: the default window is 940px wide, so an `lg`
    // breakpoint would mean the two-column layout never appeared in the app
    // it was designed for. The 720px minimum still stacks.
    <div className="mx-auto grid max-w-5xl gap-5 px-8 py-8 md:grid-cols-[1.15fr_1fr]">
      {/* `min-w-0`: without it the invite link/code — unbreakable base64,
          rendered `truncate` — sets this column's grid auto-minimum to its
          full unwrapped width, so the track ignores its `1.15fr` share and
          the other column gets squeezed down to whatever is left over. */}
      <div className="min-w-0 space-y-5">
        {statusCard}

        {joinState === "opened" && (
          <p className="rounded-panel border border-good/40 bg-good/10 px-4 py-3 text-sm text-good">
            {t("host.joined")}
          </p>
        )}
        <InviteCard hosting={session} />
      </div>

      <div className="min-w-0 space-y-5">
        {session.monitorAttached && <LobbyPanel snapshot={room} />}
        <RoomPanel snapshot={room} monitorAttached={session.monitorAttached} />
        {logsCard}
      </div>
    </div>
  );
}
