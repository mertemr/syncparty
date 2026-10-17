import { useEffect, useRef, useState } from "react";

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
import type { Invite } from "@/shared/types/Invite";
import type { RoomSnapshot } from "@/shared/types/RoomSnapshot";

const DOT_TONES: Record<WatchTone, string> = {
  neutral: "bg-ink-faint",
  good: "bg-good",
  warn: "bg-warn",
  bad: "bg-bad",
};

/**
 * Rejoining the last party is for coming back after a restart, so it is
 * tried once per launch. Held outside the component because the screen
 * remounts every time Settings opens and closes, and a remount must not join
 * again — kept as the attempt itself, so a screen mounted while one is still
 * running shows that rather than a paste box.
 */
let resumeAttempt: { settled: boolean; done: Promise<void> } | null = null;

/**
 * The guest half: accept an invite, then watch the party on this machine —
 * syncparty connects to the host and keeps mpv in step itself.
 *
 * What is being watched lives in the backend, not here: this screen only
 * holds an invite that has not been joined yet. An invite arriving by deep
 * link skips the paste step entirely, which is the path this whole feature
 * exists to make possible.
 */
export function GuestScreen() {
  const t = useTranslate();
  const { pendingInvite, clearPendingInvite, reportFailure, watch, room } =
    useAppState();
  // A host watching its own party on this machine is not something this
  // screen started or can leave.
  const watching = watch?.role === "guest" ? watch : null;

  const [text, setText] = useState("");
  const [invite, setInvite] = useState<Invite | null>(null);
  const [fromLink, setFromLink] = useState(false);
  const [parseError, setParseError] = useState<string | null>(null);
  const [joining, setJoining] = useState(false);
  // True before the first attempt has even started, so a cold start goes
  // straight to "rejoining" instead of flashing the paste box first.
  const [resuming, setResuming] = useState(() => !resumeAttempt?.settled);
  // State alone lets a fast second click through before the re-render that
  // disables the button; the ref closes that gap.
  const joinInFlight = useRef(false);
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

    setInvite(pendingInvite);
    setFromLink(true);
    setParseError(null);
    clearPendingInvite();
  }, [pendingInvite, clearPendingInvite]);

  // Deliberately once per mount, with what was true at the first one: whether
  // to resume is decided once per launch, not re-decided as state moves.
  useEffect(() => {
    if (!resumeAttempt) {
      const attempt = { settled: false, done: Promise.resolve() };
      attempt.done = (pendingInvite ? Promise.resolve() : resumeUnlessWatching())
        .catch(reportFailure)
        .finally(() => {
          attempt.settled = true;
        });
      resumeAttempt = attempt;
    }

    let cancelled = false;
    void resumeAttempt.done.finally(() => {
      if (!cancelled) setResuming(false);
    });

    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  async function decode() {
    setParseError(null);
    try {
      setInvite(await ipc.decodeInvite(text));
      setFromLink(false);
    } catch (error) {
      setParseError(errorMessage(error));
    }
  }

  async function join() {
    if (!invite || joinInFlight.current) return;

    joinInFlight.current = true;
    setJoining(true);
    try {
      await ipc.joinParty(invite);
      // In: from here the backend's watch state is the whole story.
      dismissInvite();
    } catch (error) {
      // A wrong password lands here as `sync_rejected`, before any watch
      // card was shown, so the invite stays up to try another.
      reportFailure(error);
    } finally {
      joinInFlight.current = false;
      setJoining(false);
    }
  }

  function dismissInvite() {
    setInvite(null);
    setText("");
    setFromLink(false);
  }

  async function leave() {
    // Leaving is what closes the player and the tunnel. Clearing the saved
    // invite only stops it being reopened on the next launch.
    await ipc.leaveParty();
    await ipc.clearLastSession();
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

            {joining ? (
              <Rewind label={t("guest.joining")} />
            ) : (
              <Button variant="primary" className="w-full" onClick={() => void join()}>
                {t("guest.join")}
              </Button>
            )}

            {/* While already in a party, putting a new invite aside goes back
                to that party rather than to a paste box. Mid-join the party
                being watched is the one being joined, so the words stay. */}
            <Button
              variant="ghost"
              className="w-full"
              disabled={joining}
              onClick={dismissInvite}
            >
              {watching && !joining ? t("common.cancel") : t("guest.clear")}
            </Button>
          </div>
        </Card>
      )}

      {shown && (
        <>
          <WatchCard watch={shown} room={room} onLeave={leave} />
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
 * Asked of the backend rather than read from state: on a cold start the
 * hydrated watch state may not have landed yet, and joining a party this
 * machine is already in is exactly what the once-per-launch rule is for.
 */
async function resumeUnlessWatching() {
  if (await ipc.watchState()) return;
  await ipc.resumeLastSession();
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
