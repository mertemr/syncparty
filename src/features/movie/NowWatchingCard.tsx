import { useEffect, useState } from "react";

import { useAppState } from "@/app/AppState";
import { useTranslate } from "@/shared/i18n";
import { errorMessage, ipc } from "@/shared/ipc";
import { Button, Card, EmptyState, Input, cx } from "@/shared/ui";
import type { MovieSummary } from "@/shared/types/MovieSummary";
import type { PartyLogEntry } from "@/shared/types/PartyLogEntry";

import { titleFromFileName } from "./fileTitle";

const POSTER_PLACEHOLDER =
  "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='200' height='300'%3E%3Crect width='200' height='300' fill='%23161a24'/%3E%3C/svg%3E";

/**
 * What the room is watching tonight, recorded against the running party.
 *
 * A vote settles on a movie, but plenty of nights never hold one — someone
 * just puts something on. This is where that gets written down, which is
 * also what makes the party log worth keeping: a night with a date and a
 * roster and no title is barely a record at all.
 *
 * The search box starts on a guess made from whatever file the room already
 * has open, because by the time anyone reaches for this the answer is
 * usually already on everyone's screen.
 */
export function NowWatchingCard() {
  const t = useTranslate();
  const { room, session, reportFailure } = useAppState();

  const [current, setCurrent] = useState<PartyLogEntry | null>(null);
  const [picking, setPicking] = useState(false);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<MovieSummary[] | null>(null);
  const [searching, setSearching] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);

  const hosting = session.phase === "hosting";

  // The open night is the one the party log has not closed yet.
  async function refresh() {
    try {
      const log = await ipc.getPartyLog();
      setCurrent(log.find((entry) => entry.endedAt === null) ?? null);
    } catch (error) {
      reportFailure(error);
    }
  }

  useEffect(() => {
    if (hosting) void refresh();
    else setCurrent(null);
    // `refresh` is stable enough for this — it only reads `ipc`.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [hosting]);

  /** The first file anyone in the room has open, as a search term. */
  const openFile = room?.rooms.flatMap((r) => r.watchers).find((w) => w.file)?.file?.name ?? null;
  const guess = openFile ? titleFromFileName(openFile) : "";

  async function search(term: string) {
    const trimmed = term.trim();
    if (!trimmed) return;

    setSearching(true);
    setProblem(null);
    try {
      setResults(await ipc.searchMovies(trimmed, 1));
    } catch (error) {
      setProblem(errorMessage(error));
      setResults([]);
    } finally {
      setSearching(false);
    }
  }

  async function choose(movie: MovieSummary | null) {
    try {
      await ipc.setNowWatching(
        movie && {
          tmdbId: movie.tmdbId,
          title: movie.title,
          poster: movie.poster,
          releaseDate: movie.releaseDate,
          overview: movie.overview || null,
          genres: [],
          rating: movie.rating,
        },
      );
      setPicking(false);
      setResults(null);
      setQuery("");
      await refresh();
    } catch (error) {
      reportFailure(error);
    }
  }

  if (!hosting) return null;

  return (
    <Card title={t("nowWatching.title")}>
      <div className="space-y-3">
        {current?.movieTitle && !picking ? (
          <div className="flex items-start gap-3">
            <img
              src={current.moviePoster ?? POSTER_PLACEHOLDER}
              alt=""
              className="h-18 w-12 shrink-0 rounded-sm object-cover"
            />
            <div className="min-w-0 flex-1">
              <p className="text-sm font-semibold text-ink" title={current.movieTitle}>
                {current.movieTitle}
              </p>
              <div className="mt-2 flex gap-2">
                <Button
                  variant="ghost"
                  className="min-h-0 px-2 py-1 text-xs"
                  onClick={() => {
                    setPicking(true);
                    setQuery(guess);
                    void search(guess);
                  }}
                >
                  {t("nowWatching.change")}
                </Button>
                <Button
                  variant="ghost"
                  className="min-h-0 px-2 py-1 text-xs"
                  onClick={() => void choose(null)}
                >
                  {t("nowWatching.clear")}
                </Button>
              </div>
            </div>
          </div>
        ) : !picking ? (
          <>
            <EmptyState title={t("nowWatching.empty")} />
            <Button
              variant="secondary"
              className="w-full"
              onClick={() => {
                setPicking(true);
                setQuery(guess);
                void search(guess);
              }}
            >
              {t("nowWatching.set")}
            </Button>
          </>
        ) : null}

        {picking && (
          <div className="space-y-2">
            {guess && (
              <p className="font-mono text-[10px] tracking-[0.14em] text-ink-faint uppercase">
                {t("nowWatching.fromFile")}
              </p>
            )}

            <Input
              type="search"
              autoFocus
              value={query}
              placeholder={t("movie.search.placeholder")}
              aria-label={t("nowWatching.set")}
              className="py-1.5 text-xs"
              onChange={(event) => setQuery(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") void search(query);
                if (event.key === "Escape") setPicking(false);
              }}
            />

            {searching && <p className="text-xs text-ink-faint">{t("common.loading")}</p>}

            {problem && <p className="text-xs text-bad">{problem}</p>}

            {results?.length === 0 && !searching && (
              <p className="text-xs text-ink-faint">{t("nowWatching.noResults")}</p>
            )}

            {results && results.length > 0 && (
              <ul className="max-h-64 space-y-1 overflow-y-auto pr-1">
                {results.slice(0, 12).map((movie) => (
                  <li key={movie.tmdbId.toString()}>
                    <button
                      type="button"
                      onClick={() => void choose(movie)}
                      className={cx(
                        "flex w-full items-center gap-2.5 rounded-[var(--radius-control)] p-1 text-left",
                        "transition-colors hover:bg-surface-raised/70",
                      )}
                    >
                      <img
                        src={movie.poster ?? POSTER_PLACEHOLDER}
                        alt=""
                        className="h-10 w-7 shrink-0 rounded-sm object-cover"
                      />
                      <span className="min-w-0 flex-1 truncate text-xs text-ink">
                        {movie.title}
                      </span>
                      <span className="shrink-0 font-mono text-[10px] text-ink-faint">
                        {movie.releaseDate?.slice(0, 4) ?? "—"}
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            )}

            <Button
              variant="ghost"
              className="min-h-0 w-full px-2 py-1 text-xs"
              onClick={() => setPicking(false)}
            >
              {t("common.cancel")}
            </Button>
          </div>
        )}
      </div>
    </Card>
  );
}
