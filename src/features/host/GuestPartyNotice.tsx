import { useState } from "react";

import { useAppState } from "@/app/AppState";
import { guestJoinInFlight } from "@/features/guest/guestJoin";
import { leaveGuestParty, useGuestJoin } from "@/features/guest/useGuestJoin";
import { useTranslate } from "@/shared/i18n";
import { Button, Card, Rewind } from "@/shared/ui";

/**
 * Someone else's party, still open on this machine as a guest.
 *
 * Reached by switching the mode in Settings while a guest — or while still
 * joining as one. That party's player stays open and in step with it, and
 * its client publishes its room alongside this host's, so the host screen
 * cannot just leave it out: it says so, and offers the one thing to do.
 * Watching here as the host leaves it too, in the backend.
 */
export function GuestPartyNotice() {
  const t = useTranslate();
  const { watch, reportFailure } = useAppState();
  const join = useGuestJoin();
  const [leaving, setLeaving] = useState(false);

  const party = watch?.role === "guest" ? watch : null;
  const inFlight = guestJoinInFlight(join);
  if (!party && !inFlight) return null;

  // Mid-join there is no watch yet, so the room comes from the invite.
  const room = party?.room ?? join.joining?.room ?? null;

  async function leave() {
    setLeaving(true);
    try {
      await leaveGuestParty();
    } catch (error) {
      reportFailure(error);
    } finally {
      setLeaving(false);
    }
  }

  return (
    <Card title={t("host.guestParty.title")} className="border-warn/40">
      <div className="space-y-3">
        {room && (
          <p className="truncate font-display text-lg font-extrabold text-ink [font-stretch:110%]">
            {room}
          </p>
        )}
        <p className="text-sm leading-relaxed text-ink-muted">
          {t("host.guestParty.detail")}
        </p>
        {inFlight && !party && (
          <Rewind
            label={t(join.joining ? "guest.joining" : "guest.resuming")}
          />
        )}
        <Button
          variant="danger"
          className="w-full"
          disabled={leaving}
          onClick={() => void leave()}
        >
          {t("watch.leave")}
        </Button>
      </div>
    </Card>
  );
}
