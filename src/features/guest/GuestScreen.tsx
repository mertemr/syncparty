import { useEffect, useState } from "react";

import { useAppState } from "@/app/AppState";
import { GuestMoviePanel } from "@/features/movie-voting/GuestMoviePanel";
import { RoomCards } from "@/features/party/RoomCards";
import { WatchCard } from "@/features/watch/WatchCard";
import { describeWatch, type WatchTone } from "@/features/watch/watchState";
import { useTranslate } from "@/shared/i18n";
import { errorMessage, ipc } from "@/shared/ipc";
import {
  Badge,
  Button,
  Card,
  Counter,
  EmptyState,
  Field,
  Input,
  PageHeader,
  Rewind,
  cx,
} from "@/shared/ui";
import type { RoomSnapshot } from "@/shared/types/RoomSnapshot";

import { guestJoins, leaveGuestParty, useGuestJoin } from "./useGuestJoin";

const DOT_TONES: Record<WatchTone, string> = {
  neutral: "bg-ink-faint",
  good: "bg-good",
  warn: "bg-warn",
  bad: "bg-bad",
};

/**
 * The guest half: accept an invite, then watch the party on this machine —
 * syncparty connects to the host and keeps mpv in step itself.
 *
 * What is being watched lives in the backend, not here, and the invite on
 * screen and any join under way live in `guestJoins`: the screen remounts
 * every time Settings opens and closes, and a join outlives that. An invite
 * arriving by deep link skips the paste step entirely, which is the path
 * this whole feature exists to make possible.
 */
export function GuestScreen() {
  const t = useTranslate();
  const { pendingInvite, clearPendingInvite, reportFailure, watch, room } =
    useAppState();
  // A host watching its own party on this machine is not something this
  // screen started or can leave.
  const watching = watch?.role === "guest" ? watch : null;

  const joinState = useGuestJoin();
  const { invite, fromLink } = joinState;
  const joining = joinState.joining !== null;
  // The invite on screen is the one being joined — not one that arrived by
  // link while another was already on its way in.
  const joiningThis = joining && joinState.joining === invite;
  // "untried" counts too, so a cold start goes straight to "rejoining"
  // instead of flashing the paste box before the first effect runs.
  const resuming = joinState.resume !== "settled";

  const [text, setText] = useState("");
  const [parseError, setParseError] = useState<string | null>(null);
  // When this guest started watching, for the counter. Same source as the
  // host's: the transition the frontend already sees.
  const [since, setSince] = useState<number | null>(null);
  const isWatching = watching !== null;

  useEffect(() => {
    setSince(isWatching ? Date.now() : null);
  }, [isWatching]);

  // A link that arrived while the app was open takes over the screen.
  useEffect(() => {
    if (!pendingInvite) return;

    guestJoins.offer(pendingInvite, true);
    setParseError(null);
    clearPendingInvite();
  }, [pendingInvite, clearPendingInvite]);

  // Rejoining the last party is for coming back after a restart, so it is
  // tried once per launch: every mount after the first, Settings closing
  // included, finds it already tried and does nothing. Runs after the effect
  // above, so an invite from a link is already on screen and wins.
  useEffect(() => {
    guestJoins.resumeOnce().catch(reportFailure);
  }, [reportFailure]);

  async function decode() {
    setParseError(null);
    try {
      guestJoins.offer(await ipc.decodeInvite(text), false);
    } catch (error) {
      setParseError(errorMessage(error));
    }
  }

  async function join() {
    try {
      // Guarded in `guestJoins` rather than by the button alone: state lets a
      // fast second click through before the re-render that disables it.
      await guestJoins.join();
      setText("");
    } catch (error) {
      // A wrong password lands here as `sync_rejected`, before any watch
      // card was shown, so the invite stays up to try another.
      reportFailure(error);
    }
  }

  function dismissInvite() {
    guestJoins.dismiss();
    setText("");
  }

  const view = watching ? describeWatch(watching, room, t) : null;
  const busy = joining || resuming;
  const tape = busy ? "TRACKING" : (view?.tape ?? "STANDBY");
  const tone: WatchTone = busy ? "warn" : (view?.tone ?? "neutral");
  // While a different party is being joined the old one is on its way out;
  // showing both would be two parties on one screen.
  const shown = joining ? null : watching;

  return (
    <div className="mx-auto max-w-2xl space-y-5 px-8 py-10">
      {/* The same status line the host screen opens with, so the two sides of
          the app read as one deck rather than two products. */}
      <div className="flex items-center gap-2.5">
        <span
          aria-hidden
          className={cx(
            "size-2.5 rounded-full",
            DOT_TONES[tone],
            tape === "PLAY" && "phosphor",
          )}
        />
        <span className="font-mono text-[11px] tracking-[0.22em] text-ink-muted uppercase">
          {tape}
        </span>
        {shown && since !== null && <Counter since={since} />}
      </div>

      <PageHeader
        title={t("guest.title")}
        action={
          fromLink && invite ? <Badge tone="accent">{t("guest.received")}</Badge> : null
        }
      />

      {invite && (
        <Card title={t("guest.invite.title")}>
          <div className="space-y-4">
            <div>
              <p className="font-display text-lg font-extrabold text-ink [font-stretch:110%]">
                {invite.room}
              </p>
              <p className="selectable font-mono text-xs break-all text-ink-faint">
                {invite.endpoint}
              </p>
            </div>

            {joiningThis ? (
              <Rewind label={t("guest.joining")} />
            ) : (
              <Button
                variant="primary"
                className="w-full"
                disabled={joining}
                onClick={() => void join()}
              >
                {t("guest.join")}
              </Button>
            )}

            {/* While already in a party, putting a new invite aside goes back
                to that party rather than to a paste box. Mid-join the party
                being watched is the one being joined, so the words stay. */}
            <Button
              variant="ghost"
              className="w-full"
              disabled={joiningThis}
              onClick={dismissInvite}
            >
              {watching && !joining ? t("common.cancel") : t("guest.clear")}
            </Button>
          </div>
        </Card>
      )}

      {shown && (
        <>
          <WatchCard watch={shown} room={room} onLeave={leaveGuestParty} />
          {/* A party that turned this machine away will never list its room. */}
          {shown.connection.state !== "rejected" && <GuestRoster snapshot={room} />}
          <GuestMoviePanel />
        </>
      )}

      {!watching && !invite && resuming && (
        <Card>
          <Rewind label={t("guest.resuming")} />
        </Card>
      )}

      {!watching && !invite && !resuming && (
        <Card>
          <div className="space-y-4">
            <Field label={t("guest.paste.label")} hint={t("guest.paste.hint")}>
              {/* Mono: an invite code is a machine string, and a mistyped
                  character has to be visible. */}
              <Input
                className="font-mono"
                value={text}
                autoFocus
                placeholder={t("guest.paste.placeholder")}
                onChange={(event) => setText(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" && text.trim()) void decode();
                }}
              />
            </Field>

            {parseError && <p className="text-sm text-bad">{parseError}</p>}

            <Button
              variant="primary"
              className="w-full"
              disabled={!text.trim()}
              onClick={() => void decode()}
            >
              {t("guest.decode")}
            </Button>
          </div>
        </Card>
      )}
    </div>
  );
}

/**
 * Who else is in the room, from this guest's own client — the same rows the
 * host's room panel shows, so both ends describe the room in the same words.
 */
function GuestRoster({ snapshot }: { snapshot: RoomSnapshot | null }) {
  const t = useTranslate();

  if (!snapshot?.connected) {
    return (
      <Card title={t("host.room.title")}>
        <EmptyState title={t("watch.room.waiting")} />
      </Card>
    );
  }

  if (snapshot.rooms.length === 0) {
    return (
      <Card title={t("host.room.title")}>
        <EmptyState title={t("host.room.empty")} />
      </Card>
    );
  }

  return <RoomCards snapshot={snapshot} />;
}
