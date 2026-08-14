import { useEffect, useRef, useState } from "react";

import { useAppState } from "@/app/AppState";
import { useTranslate } from "@/shared/i18n";
import { errorMessage, ipc } from "@/shared/ipc";
import {
  Badge,
  Button,
  Card,
  Counter,
  Field,
  Input,
  PageHeader,
  cx,
} from "@/shared/ui";
import type { Invite } from "@/shared/types/Invite";

/**
 * The guest half: accept an invite, then open Syncplay pointed at it.
 *
 * An invite arriving by deep link skips the paste step entirely, which is the
 * path this whole feature exists to make possible.
 */
export function GuestScreen() {
  const t = useTranslate();
  const { pendingInvite, clearPendingInvite, reportFailure } = useAppState();

  const [text, setText] = useState("");
  const [invite, setInvite] = useState<Invite | null>(null);
  const [fromLink, setFromLink] = useState(false);
  const [parseError, setParseError] = useState<string | null>(null);
  const [joined, setJoined] = useState(false);
  // When this guest connected, for the counter. Same source as the host's:
  // the transition the frontend already sees.
  const [joinedAt, setJoinedAt] = useState<number | null>(null);
  const attemptedResume = useRef(false);

  // A link that arrived while the app was open takes over the screen.
  useEffect(() => {
    if (!pendingInvite) return;
    attemptedResume.current = true;

    setInvite(pendingInvite);
    setFromLink(true);
    setJoined(false);
    setParseError(null);
    clearPendingInvite();
  }, [pendingInvite, clearPendingInvite]);

  useEffect(() => {
    if (pendingInvite || attemptedResume.current) return;
    attemptedResume.current = true;

    let cancelled = false;
    void ipc.resumeLastSession()
      .then((saved) => {
        if (!cancelled && saved) {
          setInvite(saved);
          setJoined(true);
          setJoinedAt(Date.now());
        }
      })
      .catch(reportFailure);

    return () => {
      cancelled = true;
    };
  }, [pendingInvite, reportFailure]);

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
    if (!invite) return;

    try {
      await ipc.joinParty(invite);
      setJoined(true);
      setJoinedAt(Date.now());
    } catch (error) {
      reportFailure(error);
    }
  }

  function reset() {
    // Closing the tunnel is the part that actually leaves the party. Clearing
    // the saved invite only stops it being reopened on the next launch.
    void ipc.leaveParty().catch(reportFailure);
    void ipc.clearLastSession().catch(reportFailure);
    setInvite(null);
    setJoined(false);
    setJoinedAt(null);
    setText("");
    setFromLink(false);
  }

  return (
    <div className="mx-auto max-w-2xl space-y-6 px-8 py-10">
      {/* The same status line the host screen opens with, so the two sides of
          the app read as one deck rather than two products. */}
      <div className="flex items-center gap-2.5">
        <span
          aria-hidden
          className={cx(
            "size-2.5 rounded-full",
            joined ? "bg-good phosphor" : "bg-ink-faint",
          )}
        />
        <span className="font-mono text-[11px] tracking-[0.22em] text-ink-muted uppercase">
          {joined ? "PLAY" : "STANDBY"}
        </span>
        {joined && joinedAt !== null && <Counter since={joinedAt} />}
      </div>

      <PageHeader
        title={t("guest.title")}
        action={fromLink ? <Badge tone="accent">{t("guest.received")}</Badge> : null}
      />

      {invite ? (
        <Card title={t("guest.invite.title")}>
          <div className="space-y-4">
            <div>
              <p className="font-display text-lg font-extrabold text-ink [font-stretch:110%]">
                {invite.room}
              </p>
              <p className="selectable font-mono text-xs text-ink-faint">
                {invite.endpoint}
              </p>
            </div>

            {joined ? (
              <p className="rounded-[var(--radius-control)] border border-good/40 bg-good/10 p-3 text-sm text-good">
                {t("guest.joined")}
              </p>
            ) : (
              <Button variant="primary" className="w-full" onClick={() => void join()}>
                {t("guest.join")}
              </Button>
            )}

            <Button variant="ghost" className="w-full" onClick={reset}>
              {t("guest.clear")}
            </Button>
          </div>
        </Card>
      ) : (
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
