import { useState } from "react";

import { useAppState } from "@/app/AppState";
import { MoviePicker } from "@/features/movie/MoviePicker";
import { useTranslate } from "@/shared/i18n";
import { ipc } from "@/shared/ipc";
import { Badge, Button, Card, Choice, Field, Input } from "@/shared/ui";
import type { MovieCandidate } from "@/shared/types/MovieCandidate";
import type { VoteParticipant } from "@/shared/types/VoteParticipant";

import { ParticipationPicker } from "./ParticipationPicker";
import { customSchedule, formatSchedule, tomorrowIso, tonightIso } from "./schedule";
import { tallyVotes } from "./tally";
import { VoteCandidateList } from "./VoteCandidateList";

const MAX_CANDIDATES = 10;

/** The host half of Movie Night, embedded in `HostScreen`. Everything here
 * is a thin wrapper over `movieVote`/IPC — the state machine itself lives in
 * `core::movie_vote`, this only ever mirrors it. */
export function HostMoviePanel() {
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

  if (!movieVote || movieVote.phase === "cancelled") {
    return <SetupPanel busy={busy} run={run} />;
  }

  if (movieVote.phase === "draft") {
    return (
      <DraftPanel
        candidates={movieVote.candidates}
        schedule={movieVote.schedule}
        locale={locale}
        busy={busy}
        run={run}
      />
    );
  }

  const counts = tallyVotes(movieVote.candidates, movieVote.participants);
  const me = movieVote.participants.find((participant) => participant.peer === "host");

  if (movieVote.phase === "open") {
    return (
      <Card title={t("movieVote.title")} action={<Badge tone="good">{t("movieVote.open")}</Badge>}>
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
            myVote={me?.selectedMovie ?? null}
            disabled={busy}
            onVote={(tmdbId) => void run(() => ipc.castMovieVote(tmdbId))}
          />

          <ParticipantList participants={movieVote.participants} />

          <div className="flex gap-2">
            <Button
              variant="primary"
              className="flex-1"
              disabled={busy}
              onClick={() => void run(() => ipc.closeMovieVote())}
            >
              {t("movieVote.close")}
            </Button>
            <Button variant="danger" disabled={busy} onClick={() => void run(() => ipc.cancelMovieVote())}>
              {t("movieVote.cancel")}
            </Button>
          </div>
        </div>
      </Card>
    );
  }

  // completed
  const result = movieVote.result;
  return (
    <Card title={t("movieVote.title")} action={<Badge tone="neutral">{t("movieVote.completed")}</Badge>}>
      <div className="space-y-5">
        {result?.tied && result.tied.length > 0 && (
          <p className="text-sm text-warn">
            {t("movieVote.tie")} — {t("movieVote.tie.detail")}
          </p>
        )}

        <VoteCandidateList
          candidates={movieVote.candidates}
          counts={counts}
          winner={result?.winner ?? null}
          tied={result?.tied ?? []}
          onResolveTie={(tmdbId) => void run(() => ipc.resolveMovieVoteTie(tmdbId))}
        />

        <ParticipantList participants={movieVote.participants} />

        <Button
          variant="primary"
          className="w-full"
          disabled={busy}
          onClick={() => void run(() => ipc.startMovieVote(null))}
        >
          {t("movieVote.start")}
        </Button>
      </div>
    </Card>
  );
}

function ParticipantList({ participants }: { participants: VoteParticipant[] }) {
  const t = useTranslate();
  if (participants.length === 0) return null;

  const label = (status: string | null) =>
    status === "going"
      ? t("movieVote.going")
      : status === "maybe"
        ? t("movieVote.maybe")
        : status === "notGoing"
          ? t("movieVote.notGoing")
          : t("movieVote.status.pending");

  return (
    <div>
      <p className="mb-1.5 font-mono text-[11px] tracking-[0.14em] text-ink-faint uppercase">
        {t("movieVote.participants")}
      </p>
      <div className="flex flex-wrap gap-2">
        {participants.map((participant) => (
          <Badge
            key={participant.peer}
            tone={participant.participation === "going" ? "good" : "neutral"}
          >
            {participant.peer === "host" ? "Host" : participant.displayName} ·{" "}
            {label(participant.participation)}
          </Badge>
        ))}
      </div>
    </div>
  );
}

type SchedulePreset = "none" | "tonight" | "tomorrow" | "custom";

/** Before a vote exists: pick when it is (if at all) and create the draft.
 * Schedule is fixed at this point — `core` has no "reschedule" command, the
 * draft's schedule is whatever `start` was called with. */
function SetupPanel({
  busy,
  run,
}: {
  busy: boolean;
  run: (action: () => Promise<unknown>) => Promise<void>;
}) {
  const t = useTranslate();
  const [preset, setPreset] = useState<SchedulePreset>("none");
  const [customDate, setCustomDate] = useState("");
  const [customTime, setCustomTime] = useState("");

  function resolvedSchedule(): string | null {
    if (preset === "none") return null;
    if (preset === "tonight") return tonightIso();
    if (preset === "tomorrow") return tomorrowIso();
    return customSchedule(customDate, customTime);
  }

  return (
    <Card title={t("movieVote.title")}>
      <div className="space-y-4">
        <p className="text-sm text-ink-muted">{t("movieVote.none.hostDetail")}</p>

        <Choice
          label={t("movieVote.schedule")}
          value={preset}
          onChange={(value) => setPreset(value as SchedulePreset)}
          options={[
            { value: "none", label: t("movieVote.schedule.none") },
            { value: "tonight", label: t("movieVote.schedule.tonight") },
            { value: "tomorrow", label: t("movieVote.schedule.tomorrow") },
            { value: "custom", label: t("movieVote.schedule.custom") },
          ]}
        />

        {preset === "custom" && (
          <div className="grid grid-cols-2 gap-3">
            <Field label={t("movieVote.schedule.date")}>
              <Input type="date" value={customDate} onChange={(event) => setCustomDate(event.target.value)} />
            </Field>
            <Field label={t("movieVote.schedule.time")}>
              <Input type="time" value={customTime} onChange={(event) => setCustomTime(event.target.value)} />
            </Field>
          </div>
        )}

        <Button
          variant="primary"
          className="w-full"
          disabled={busy}
          onClick={() => void run(() => ipc.startMovieVote(resolvedSchedule()))}
        >
          {t("movieVote.start")}
        </Button>
      </div>
    </Card>
  );
}

function DraftPanel({
  candidates,
  schedule,
  locale,
  busy,
  run,
}: {
  candidates: MovieCandidate[];
  schedule: string | null;
  locale: string;
  busy: boolean;
  run: (action: () => Promise<unknown>) => Promise<void>;
}) {
  const t = useTranslate();
  const full = candidates.length >= MAX_CANDIDATES;
  const canOpen = candidates.length >= 2;

  return (
    <Card title={t("movieVote.title")}>
      <div className="space-y-5">
        {schedule && <p className="text-sm text-ink-muted">{formatSchedule(schedule, locale)}</p>}

        <div>
          <p className="mb-2 font-mono text-[11px] tracking-[0.14em] text-ink-faint uppercase">
            {t("movieVote.candidates")} ({candidates.length}/{MAX_CANDIDATES})
          </p>
          <p className="mb-3 text-xs text-ink-faint">{t("movieVote.pickMovies")}</p>

          {candidates.length > 0 && (
            <div className="mb-3 flex flex-wrap gap-2">
              {candidates.map((candidate) => (
                <div
                  key={candidate.tmdbId.toString()}
                  className="flex items-center gap-2 rounded-[var(--radius-control)] border border-line bg-surface-raised/60 py-1 pr-1 pl-2"
                >
                  <span className="max-w-32 truncate text-xs text-ink">{candidate.title}</span>
                  <Button
                    variant="ghost"
                    className="min-h-0 px-1.5 py-1"
                    onClick={() => void run(() => ipc.removeMovieCandidate(candidate.tmdbId))}
                  >
                    ✕
                  </Button>
                </div>
              ))}
            </div>
          )}

          <MoviePicker
            candidates={candidates}
            full={full}
            onAdd={(candidate) => void run(() => ipc.addMovieCandidate(candidate))}
            onRemove={(tmdbId) => void run(() => ipc.removeMovieCandidate(tmdbId))}
          />
        </div>

        <div className="flex gap-2">
          <Button
            variant="primary"
            className="flex-1"
            disabled={busy || !canOpen}
            onClick={() => void run(() => ipc.openMovieVote())}
          >
            {t("movieVote.start")}
          </Button>
          <Button variant="danger" disabled={busy} onClick={() => void run(() => ipc.cancelMovieVote())}>
            {t("movieVote.cancel")}
          </Button>
        </div>
        {!canOpen && <p className="text-xs text-ink-faint">{t("movieVote.startHint")}</p>}
      </div>
    </Card>
  );
}
