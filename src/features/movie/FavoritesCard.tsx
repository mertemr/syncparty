import { useEffect, useMemo, useState } from "react";

import { useAppState } from "@/app/AppState";
import { useTranslate } from "@/shared/i18n";
import { ipc } from "@/shared/ipc";
import { Card, cx, EmptyState, Input } from "@/shared/ui";
import type { Genre } from "@/shared/types/Genre";
import type { MovieCandidate } from "@/shared/types/MovieCandidate";
import type { MovieSummary } from "@/shared/types/MovieSummary";
import type { WatchedMovie } from "@/shared/types/WatchedMovie";

import { listFavorites, subscribeUserMovies, toggleFavorite } from "./userMovies";
import { MovieDetailModal } from "./MovieDetailModal";

const POSTER_PLACEHOLDER =
  "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='200' height='300'%3E%3Crect width='200' height='300' fill='%23161a24'/%3E%3C/svg%3E";

type Section = "favorites" | "watched";

function toCandidate(movie: MovieSummary, genreNames: (ids: number[]) => string[]): MovieCandidate {
  return {
    tmdbId: movie.tmdbId,
    title: movie.title,
    poster: movie.poster,
    releaseDate: movie.releaseDate,
    overview: movie.overview || null,
    genres: genreNames(movie.genreIds),
    rating: movie.rating,
  };
}

/**
 * A small sidebar card, Favourites and Watched under one roof — the two
 * things worth remembering about a movie that have nothing to do with any
 * single vote. Favourites are a personal bookmark (see `favorites.ts`);
 * watched history is real, backed by past sessions.
 *
 * Only ever shown on the host's side: favouriting a movie is only useful as
 * a shortcut into a candidate list, and only the host builds one.
 */
export function FavoritesCard() {
  const t = useTranslate();
  const { movieVote, reportFailure } = useAppState();

  const [section, setSection] = useState<Section>("favorites");
  const [favorites, setFavorites] = useState<MovieSummary[]>(listFavorites());
  const [watched, setWatched] = useState<WatchedMovie[]>([]);
  const [genres, setGenres] = useState<Genre[]>([]);
  const [openDetail, setOpenDetail] = useState<bigint | null>(null);
  const [filter, setFilter] = useState("");

  useEffect(() => {
    void ipc.getWatchedMovies().then(setWatched).catch(() => setWatched([]));
    void ipc.getGenres().then(setGenres).catch(() => setGenres([]));
  }, []);

  // Re-reads favourites whenever the picker grid (or this card's own heart
  // button) changes them — see `userMovies.ts`.
  useEffect(() => subscribeUserMovies(() => setFavorites(listFavorites())), []);

  const genreNames = useMemo(() => {
    const map = new Map<number, string>();
    for (const genre of genres) map.set(genre.id, genre.name);
    return (ids: number[]) => ids.map((id) => map.get(id)).filter((name): name is string => Boolean(name));
  }, [genres]);

  // Filtering in the rail rather than fetching: both lists are already in
  // memory and short enough that a substring match is the whole feature.
  const needle = filter.trim().toLocaleLowerCase();
  const shownFavorites = needle
    ? favorites.filter((movie) => movie.title.toLocaleLowerCase().includes(needle))
    : favorites;
  const shownWatched = needle
    ? watched.filter((movie) => movie.title.toLocaleLowerCase().includes(needle))
    : watched;

  const draft = movieVote?.phase === "draft" ? movieVote : null;
  const selectedIds = useMemo(
    () => new Set(draft?.candidates.map((candidate) => candidate.tmdbId.toString()) ?? []),
    [draft],
  );
  const full = (draft?.candidates.length ?? 0) >= 10;

  async function addOrRemove(movie: MovieSummary) {
    if (!draft) return;
    try {
      if (selectedIds.has(movie.tmdbId.toString())) {
        await ipc.removeMovieCandidate(movie.tmdbId);
      } else {
        await ipc.addMovieCandidate(toCandidate(movie, genreNames));
      }
    } catch (error) {
      reportFailure(error);
    }
  }

  return (
    <Card title={t("movie.library")}>
      <div className="space-y-3">
        <div
          role="group"
          aria-label={t("movie.library")}
          className="flex gap-1 rounded-[var(--radius-control)] border border-line/80 bg-canvas/70 p-1.5"
        >
          {(["favorites", "watched"] as const).map((value) => (
            <button
              key={value}
              type="button"
              aria-pressed={section === value}
              onClick={() => setSection(value)}
              className={
                section === value
                  ? "flex-1 rounded-[var(--radius-control)] bg-accent px-2 py-1.5 text-xs font-semibold text-accent-ink"
                  : "flex-1 rounded-[var(--radius-control)] px-2 py-1.5 text-xs text-ink-muted hover:bg-surface-raised/50 hover:text-ink"
              }
            >
              {t(value === "favorites" ? "movie.favorites.tab" : "movie.watched.tab")}
            </button>
          ))}
        </div>

        {/* Only once the list is long enough to need it — a search box over
            three favourites is a control looking for a job. */}
        {(section === "favorites" ? favorites.length : watched.length) > 6 && (
          <Input
            type="search"
            value={filter}
            placeholder={t("movie.search.placeholder")}
            aria-label={t("movie.search.placeholder")}
            onChange={(event) => setFilter(event.target.value)}
            className="py-1.5 text-xs"
          />
        )}

        {section === "favorites" &&
          (shownFavorites.length === 0 ? (
            <EmptyState title={t("movie.favorites.empty")} />
          ) : (
            // The rail is a fixed-height column beside a page that scrolls;
            // a hundred favourites in it pushed everything below off the
            // screen, so the list keeps its own bounds and scrolls inside.
            <ul className="max-h-72 space-y-2 overflow-y-auto pr-1">
              {shownFavorites.map((movie) => {
                const key = movie.tmdbId.toString();
                const selected = selectedIds.has(key);
                return (
                  <li key={key} className="flex items-center gap-2.5">
                    <button
                      type="button"
                      onClick={() => setOpenDetail(movie.tmdbId)}
                      className="flex min-w-0 flex-1 items-center gap-2.5 text-left"
                    >
                      <img
                        src={movie.poster ?? POSTER_PLACEHOLDER}
                        alt=""
                        className="h-12 w-8 shrink-0 rounded-sm object-cover"
                      />
                      <span className="min-w-0 truncate text-xs text-ink">{movie.title}</span>
                    </button>

                    <button
                      type="button"
                      aria-label={t("movie.card.unfavorite")}
                      onClick={() => toggleFavorite(movie)}
                      className="shrink-0 text-sm text-bad/80 hover:text-bad"
                    >
                      ♥
                    </button>

                    {draft && (
                      <button
                        type="button"
                        aria-label={t(selected ? "movie.card.remove" : "movie.card.add")}
                        aria-pressed={selected}
                        disabled={!selected && full}
                        onClick={() => void addOrRemove(movie)}
                        className={cx(
                          // A bordered circle around a glyph sat a pixel off
                          // its own centre at this size and read as a stray
                          // mark next to the row. Unselected is now a plain
                          // hit area; the ring only appears once there is a
                          // state worth drawing.
                          "flex size-7 shrink-0 items-center justify-center rounded-full text-sm leading-none transition-colors",
                          selected
                            ? "bg-accent font-bold text-accent-ink"
                            : "text-ink-faint hover:bg-surface-raised/70 hover:text-accent disabled:cursor-not-allowed disabled:opacity-40",
                        )}
                      >
                        {selected ? "✓" : "+"}
                      </button>
                    )}
                  </li>
                );
              })}
            </ul>
          ))}

        {section === "watched" &&
          (shownWatched.length === 0 ? (
            <EmptyState title={t("movie.watched.empty")} />
          ) : (
            <ul className="max-h-72 space-y-2 overflow-y-auto pr-1">
              {shownWatched.map((movie) => (
                <li key={`${movie.tmdbId}-${movie.sessionId}`} className="text-xs">
                  <p className="truncate text-ink">{movie.title}</p>
                  <p className="text-ink-faint">
                    {new Date(Number(movie.watchedAt)).toLocaleDateString()} ·{" "}
                    {movie.participants.length} {t("movieHistory.participantsSuffix")}
                  </p>
                </li>
              ))}
            </ul>
          ))}
      </div>

      {openDetail !== null && (
        <MovieDetailModal tmdbId={openDetail} onClose={() => setOpenDetail(null)} />
      )}
    </Card>
  );
}
