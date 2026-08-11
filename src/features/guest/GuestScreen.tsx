import { useEffect, useRef, useState } from "react";

import { useAppState } from "@/app/AppState";
import { useTranslate } from "@/shared/i18n";
import { errorMessage, ipc } from "@/shared/ipc";
import { Badge, Button, Card, Field, Input, PageHeader } from "@/shared/ui";
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
    setText("");
    setFromLink(false);
  }

  return (
    <div className="mx-auto max-w-2xl space-y-6 px-8 py-10">
      <PageHeader
        title={t("guest.title")}
        action={fromLink ? <Badge tone="accent">{t("guest.received")}</Badge> : null}
      />

      {invite ? (
        <Card title={t("guest.invite.title")}>
          <div className="space-y-4">
            <div>
              <p className="text-lg font-semibold text-ink">{invite.room}</p>
              <p className="selectable font-mono text-xs text-ink-faint">
                {invite.endpoint}
              </p>
            </div>

            {joined ? (
              <p className="rounded-lg border border-good/40 bg-good/10 p-3 text-sm text-good">
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
              <Input
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
