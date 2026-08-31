import { useTranslate } from "@/shared/i18n";
import { Badge, Button, cx } from "@/shared/ui";
import type { MovieCandidate } from "@/shared/types/MovieCandidate";

import type { CandidateVoteCount } from "./tally";

const POSTER_PLACEHOLDER =
  "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='200' height='300'%3E%3Crect width='200' height='300' fill='%23161a24'/%3E%3C/svg%3E";

/**
 * Candidates with live vote counts — the one view shared by the host's open
 * vote screen, a guest's ballot, and the closed result. Which buttons show up
 * (vote vs. resolve-a-tie vs. none) depends only on which callbacks are
 * passed, so one component covers all three moments in the spec's flow.
 */
export function VoteCandidateList({
  candidates,
  counts,
  myVote,
  onVote,
  winner,
  tied,
  onResolveTie,
  disabled,
}: {
  candidates: MovieCandidate[];
  counts: CandidateVoteCount[];
  myVote?: bigint | null;
  onVote?: (tmdbId: bigint) => void;
  winner?: bigint | null;
  tied?: bigint[];
  onResolveTie?: (tmdbId: bigint) => void;
  disabled?: boolean;
}) {
  const t = useTranslate();
  const totalVotes = counts.reduce((sum, count) => sum + count.votes, 0);

  return (
    // Container-query breakpoint: this list lives in a centre column whose
    // width has nothing to do with the viewport's — see `MoviePicker`.
    <div className="@container grid grid-cols-2 gap-3 @sm:grid-cols-3">
      {candidates.map((candidate) => {
        const votes = counts.find((count) => count.tmdbId === candidate.tmdbId)?.votes ?? 0;
        const isWinner = winner != null && winner === candidate.tmdbId;
        const isTied = tied?.includes(candidate.tmdbId) ?? false;
        const isMine = myVote != null && myVote === candidate.tmdbId;
        const share = totalVotes > 0 ? Math.round((votes / totalVotes) * 100) : 0;

        return (
          <div
            key={candidate.tmdbId.toString()}
            className={cx(
              "flex flex-col overflow-hidden rounded-panel border bg-surface/85",
              isWinner ? "border-good/70" : "border-line",
            )}
          >
            <img
              src={candidate.poster ?? POSTER_PLACEHOLDER}
              alt=""
              className="aspect-[2/3] w-full object-cover"
            />
            <div className="space-y-1.5 p-2.5">
              <p className="line-clamp-2 text-xs font-semibold text-ink">{candidate.title}</p>

              {isWinner && <Badge tone="good">{t("movieVote.winner")}</Badge>}

              <div className="font-mono text-[10px] text-ink-faint">
                {votes} {t("movieVote.votesSuffix")}
                {totalVotes > 0 && ` · ${share}%`}
              </div>

              {onVote && (
                <Button
                  variant={isMine ? "primary" : "secondary"}
                  className="w-full"
                  disabled={disabled}
                  onClick={() => onVote(candidate.tmdbId)}
                >
                  {isMine ? `✓ ${t("movieVote.yourVote")}` : t("movieVote.vote")}
                </Button>
              )}

              {onResolveTie && isTied && (
                <Button
                  variant="primary"
                  className="w-full"
                  onClick={() => onResolveTie(candidate.tmdbId)}
                >
                  {t("movieVote.resolve")}
                </Button>
              )}
            </div>
          </div>
        );
      })}
    </div>
  );
}
