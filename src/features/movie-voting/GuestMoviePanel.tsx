import { useState } from "react";

import { useAppState } from "@/app/AppState";
import { useTranslate } from "@/shared/i18n";
import { ipc } from "@/shared/ipc";
import { Badge, Card, EmptyState } from "@/shared/ui";

import { ParticipationPicker } from "./ParticipationPicker";
import { formatSchedule } from "./schedule";
import { tallyVotes } from "./tally";
import { VoteCandidateList } from "./VoteCandidateList";

/** The guest half of Movie Night, embedded in `GuestScreen`. A guest never
 * builds the candidate list — that happens on the host's side before a vote
 * is ever broadcast, so `draft` reads identically to "nothing yet" here. */
export function GuestMoviePanel() {
  const t = useTranslate();
  const { movieVote, settings, reportFailure } = useAppState();
  const [busy, setBusy] = useState(false);

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

  const locale = settings?.language === "tr" ? "tr-TR" : "en-US";

  if (!movieVote || movieVote.phase === "draft" || movieVote.phase === "cancelled") {
    return (
      <Card title={t("movieVote.title")}>
        <EmptyState title={t("movieVote.none.title")} detail={t("movieVote.none.guestDetail")} />
      </Card>
    );
  }

  const counts = tallyVotes(movieVote.candidates, movieVote.participants);
  const me = movieVote.participants.find((participant) => participant.peer !== "host") ?? null;
  // A guest's own entry is keyed by whatever endpoint id the host assigned
  // it — this process has no way to know that id in advance, so the guest's
  // row is whichever non-host participant last responded from here. Good
  // enough for a single guest window; a second guest on the same machine is
  // not a scenario syncparty supports.
  const myVote = me?.selectedMovie ?? null;

  if (movieVote.phase === "open") {
    return (
      <Card title={t("movieVote.whatToWatch")} action={<Badge tone="good">{t("movieVote.open")}</Badge>}>
        <div className="space-y-5">
          {movieVote.schedule && (
            <p className="text-sm text-ink-muted">{formatSchedule(movieVote.schedule, locale)}</p>
          )}

          <ParticipationPicker
            value={me?.participation ?? null}
            disabled={busy}
            onChange={(status) => void run(() => ipc.setMovieVoteParticipation(status))}
          />

          <VoteCandidateList
            candidates={movieVote.candidates}
            counts={counts}
            myVote={myVote}
            disabled={busy}
            onVote={(tmdbId) => void run(() => ipc.castMovieVote(tmdbId))}
          />
        </div>
      </Card>
    );
  }

  // completed
  const result = movieVote.result;
  const going = movieVote.participants.filter((participant) => participant.participation === "going");

  return (
    <Card title={t("movieVote.title")} action={<Badge tone="neutral">{t("movieVote.completed")}</Badge>}>
      <div className="space-y-5">
        {result?.tied && result.tied.length > 0 && result.winner == null && (
          <p className="text-sm text-warn">{t("movieVote.tie")}</p>
        )}

        <VoteCandidateList
          candidates={movieVote.candidates}
          counts={counts}
          winner={result?.winner ?? null}
        />

        {result?.winner != null && going.length > 0 && (
          <p className="text-sm text-ink-muted">
            {t("movieVote.everyoneWatching")} · {going.length}
          </p>
        )}
      </div>
    </Card>
  );
}
