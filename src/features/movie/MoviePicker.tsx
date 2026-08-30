import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { useTranslate } from "@/shared/i18n";
import { errorMessage, ipc } from "@/shared/ipc";
import { cx, EmptyState, Input } from "@/shared/ui";
import type { Genre } from "@/shared/types/Genre";
import type { MovieCandidate } from "@/shared/types/MovieCandidate";
import type { MovieSummary } from "@/shared/types/MovieSummary";

import { MovieCard } from "./MovieCard";
import { MovieDetailModal } from "./MovieDetailModal";
import {
  isFavorite,
  isWatchedManual,
  listWatchedManual,
  subscribeUserMovies,
  toggleFavorite,
  toggleWatchedManual,
} from "./userMovies";
import { watchedIdOrder } from "./watchlist";

type Tab = "popular" | "nowPlaying" | "upcoming" | "topRated" | "watched";

/**
 * Everything already seen, newest first: real movie nights from the backend's
 * session history, then anything manually marked here.
 *
 * Manual marks arrive with their poster already on file. Only a real movie
 * night's history row needs filling out against TMDB — those carry a title
 * and a date and nothing else. One request per unknown movie, which is fine
 * for a list this size and would not be for a browse page; it is also why
 * this list has no second page.
 */
async function watchedSummaries(): Promise<MovieSummary[]> {
  const marked = listWatchedManual();
  const known = new Map(marked.map((movie) => [movie.tmdbId.toString(), movie]));

  const history = await ipc.getWatchedMovies();
  const ordered = watchedIdOrder(
    history,
    marked.map((movie) => movie.tmdbId.toString()),
  );

  // Only the ids with nothing on file cost a request. A movie marked by hand
  // was stored with its poster and rating at the moment it was marked; a
  // movie night's history row carries a title and a date and nothing else,
  // so those still have to be filled out against TMDB.
  let firstFailure: unknown = null;
  const fetched = await Promise.all(
    ordered
      .filter((id) => !known.has(id))
      .map((id) =>
        ipc
          .getMovieDetails(BigInt(id))
          .then((details) => ({
            tmdbId: details.tmdbId,
            title: details.title,
            originalTitle: details.originalTitle,
            poster: details.poster,
            backdrop: details.backdrop,
            releaseDate: details.releaseDate,
            overview: details.overview,
            genreIds: details.genres.map((genre) => genre.id),
            rating: details.rating,
            voteCount: details.voteCount,
            popularity: 0,
          }))
          .catch((error: unknown) => {
            firstFailure ??= error;
            return null;
          }),
      ),
  );

  for (const movie of fetched) {
    if (movie) known.set(movie.tmdbId.toString(), movie);
  }

  // A movie TMDB no longer answers for is dropped rather than rendered as a
  // blank card — the history row it came from is still intact in the sidebar.
  // Losing every one of them is a different thing entirely, though: that is
  // TMDB being unreachable, and it has to say so instead of showing the
  // "nothing watched yet" empty state to someone with a full history.
  const resolved = ordered.map((id) => known.get(id)).filter((movie) => movie !== undefined);
  if (ordered.length > 0 && resolved.length === 0) {
    // Re-throwing what the backend actually said, rather than a summary of
    // it: "nothing came back" is the symptom, and the reason is the only
    // part anyone can act on.
    throw firstFailure ?? new Error("No watched movie could be resolved");
  }

  return resolved;
}

const FETCHERS: Record<Tab, (page: number) => Promise<MovieSummary[]>> = {
  popular: (page) => ipc.getPopularMovies(page),
  nowPlaying: (page) => ipc.getNowPlayingMovies(page),
  upcoming: (page) => ipc.getUpcomingMovies(page),
  topRated: (page) => ipc.getTopRatedMovies(page),
  watched: (page) => (page > 1 ? Promise.resolve([]) : watchedSummaries()),
};

/** Poster-shaped placeholders in the grid's own columns.
 *
 * A spinner was the wrong instrument here: a page arrives in well under a
 * second, so it appeared and vanished as a flash, and the grid jumped by
 * whatever height the spinner had occupied. These take exactly the space the
 * cards are about to take, so nothing moves when the real ones land. */
function PosterSkeletons({ count }: { count: number }) {
  return (
    <>
      {Array.from({ length: count }, (_, index) => (
        <div
          key={index}
          aria-hidden
          className="aspect-2/3 animate-pulse rounded-panel border border-line/60 bg-surface/50 motion-reduce:animate-none"
        />
      ))}
    </>
  );
}

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

/** The host's movie browser, for building a vote's candidate list. Never
 * shown to a guest — candidates are picked before a vote is broadcast. */
export function MoviePicker({
  candidates,
  full,
  onAdd,
  onRemove,
}: {
  candidates: MovieCandidate[];
  full: boolean;
  onAdd: (candidate: MovieCandidate) => void;
  onRemove: (tmdbId: bigint) => void;
}) {
  const t = useTranslate();

  const [tab, setTab] = useState<Tab>("popular");
  const [query, setQuery] = useState("");
  const [searchTerm, setSearchTerm] = useState("");
  const [page, setPage] = useState(1);
  const [hasMore, setHasMore] = useState(true);
  const [results, setResults] = useState<MovieSummary[]>([]);
  const [loading, setLoading] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [genres, setGenres] = useState<Genre[]>([]);
  const [watchedIds, setWatchedIds] = useState<Set<string>>(new Set());
  // Forces a re-render whenever a mark changes, in this grid or in the
  // sidebar's library card — `isFavorite`/`isWatchedManual` read the store's
  // in-memory copy each render, but nothing above depends on its value, so
  // nothing would notice a change on its own without this.
  const [, forceMarksRerender] = useState(0);
  useEffect(() => subscribeUserMovies(() => forceMarksRerender((version) => version + 1)), []);
  const [openDetail, setOpenDetail] = useState<bigint | null>(null);

  useEffect(() => {
    void ipc.getGenres().then(setGenres).catch(() => setGenres([]));
    void ipc
      .getWatchedMovies()
      .then((watched) => setWatchedIds(new Set(watched.map((movie) => movie.tmdbId.toString()))))
      .catch(() => setWatchedIds(new Set()));
  }, []);

  const genreNameMap = useMemo(() => {
    const map = new Map<number, string>();
    for (const genre of genres) map.set(genre.id, genre.name);
    return map;
  }, [genres]);

  const genreNames = (ids: number[]) =>
    ids.map((id) => genreNameMap.get(id)).filter((name): name is string => Boolean(name));

  const searching = searchTerm.trim().length > 0;

  // Typing runs the search on its own — no button, no Enter. Long enough
  // that a word costs one request rather than one per letter, short enough
  // that the grid still feels like it is answering the box.
  useEffect(() => {
    const timer = setTimeout(() => setSearchTerm(query.trim()), 350);
    return () => clearTimeout(timer);
  }, [query]);

  // A new tab or search term starts a fresh list at page 1 — the fetch
  // effect below tells a first page apart from a "load more" page by
  // whether `page` is 1, so this has to land before that effect runs.
  useEffect(() => {
    setPage(1);
    setHasMore(true);
  }, [tab, searchTerm]);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setProblem(null);

    // A live query outranks the tabs rather than living beside them: while
    // there is something in the box the grid answers it, and emptying the
    // box drops straight back to whichever tab is still selected.
    const request = searching ? ipc.searchMovies(searchTerm, page) : FETCHERS[tab](page);

    void request
      .then((movies) => {
        if (cancelled) return;
        // Page 1 replaces (a fresh tab/search); anything after appends —
        // that is the whole of "infinite scroll" here, the rest is just
        // noticing the bottom of the list and asking for the next page.
        setResults((current) => (page === 1 ? movies : [...current, ...movies]));
        setHasMore(movies.length > 0);
      })
      .catch((error) => {
        if (!cancelled) {
          setProblem(errorMessage(error));
          if (page === 1) setResults([]);
          setHasMore(false);
        }
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [tab, searchTerm, searching, page]);

  // Advances the page once the sentinel at the bottom of the grid scrolls
  // into view — `hasMore`/`loading` are read through a ref inside the
  // callback so the observer doesn't need recreating every time they change.
  const stateRef = useRef({ loading, hasMore });
  stateRef.current = { loading, hasMore };

  // A callback ref rather than `useRef` + a mount-only `useEffect`: the
  // sentinel only exists once `results` is non-empty (see below), so a
  // plain ref would still be `null` when an effect with `[]` deps ran right
  // after the first render and would never observe anything. A callback ref
  // fires again every time the node itself mounts or unmounts, which is
  // exactly when the observer needs to (re)attach.
  const observerRef = useRef<IntersectionObserver | null>(null);
  const sentinelRef = useCallback((node: HTMLDivElement | null) => {
    observerRef.current?.disconnect();
    if (!node) return;

    observerRef.current = new IntersectionObserver(
      (entries) => {
        if (!entries[0]?.isIntersecting) return;
        if (stateRef.current.loading || !stateRef.current.hasMore) return;
        setPage((current) => current + 1);
      },
      { rootMargin: "1200px" },
    );
    observerRef.current.observe(node);
  }, []);

  const selectedIds = useMemo(() => new Set(candidates.map((c) => c.tmdbId.toString())), [candidates]);

  const tabs: Array<{ value: Tab; label: string }> = [
    { value: "popular", label: t("movie.tab.popular") },
    { value: "nowPlaying", label: t("movie.tab.nowPlaying") },
    { value: "upcoming", label: t("movie.tab.upcoming") },
    { value: "topRated", label: t("movie.tab.topRated") },
    { value: "watched", label: t("movie.watched.tab") },
  ];

  return (
    <div className="@container space-y-4">
      {/* A scrollable strip rather than `Choice`'s equal-width row: this
          panel sits in a narrow centre column now, and five tabs squeezed
          to fit would either wrap mid-word or clip — scrolling sideways is
          the one option that stays readable at any column width. */}
      {/* Search is the way in, not a fifth destination: a tab meant picking
          "search" before you could type, and losing the list you were on to
          do it. The box is always here, and clearing it hands the grid back
          to the tabs. */}
      <div className="relative">
        <span aria-hidden className="absolute top-1/2 left-3 -translate-y-1/2 text-ink-faint">
          <svg viewBox="0 0 24 24" className="size-4 fill-none stroke-current" strokeWidth="2">
            <circle cx="11" cy="11" r="7" />
            <path d="m20 20-3.5-3.5" strokeLinecap="round" />
          </svg>
        </span>
        <Input
          type="search"
          value={query}
          placeholder={t("movie.search.placeholder")}
          aria-label={t("movie.tab.search")}
          onChange={(event) => setQuery(event.target.value)}
          className="pl-9"
        />
      </div>

      <div
        role="group"
        aria-label={t("movieVote.candidates")}
        className={cx(
          "flex gap-1 overflow-x-auto rounded-[var(--radius-control)] border border-line/80 bg-canvas/70 p-1.5 transition-opacity",
          // The tabs are still the state the grid returns to, so they stay
          // legible — just visibly not what is on screen right now.
          searching && "opacity-45",
        )}
      >
        {tabs.map((option) => (
          <button
            key={option.value}
            type="button"
            aria-pressed={!searching && option.value === tab}
            onClick={() => {
              setQuery("");
              setTab(option.value);
            }}
            className={cx(
              "shrink-0 rounded-[var(--radius-control)] px-3 py-2 text-sm whitespace-nowrap transition-colors",
              !searching && option.value === tab
                ? "bg-accent font-semibold text-accent-ink shadow-sm"
                : "text-ink-muted hover:bg-surface-raised/50 hover:text-ink",
            )}
          >
            {option.label}
          </button>
        ))}
      </div>

      {/* Only ever shown on an empty list. Switching tabs keeps the old
          results on screen until the new ones land — inserting placeholders
          above a grid that is still there shoves everything down a screen
          and then yanks it back, which is the jump this avoids. */}
      {loading && results.length === 0 && (
        <div
          role="status"
          aria-label={t("common.loading")}
          className="grid grid-cols-2 gap-3 @lg:grid-cols-3 @2xl:grid-cols-4 @4xl:grid-cols-5 @5xl:grid-cols-6"
        >
          <PosterSkeletons count={12} />
        </div>
      )}

      {!loading && problem && results.length === 0 && (
        <EmptyState title={t("movie.loadError")} detail={problem} />
      )}

      {!loading && !problem && results.length === 0 && (
        <EmptyState
          title={t(!searching && tab === "watched" ? "movie.watched.empty" : "movie.empty")}
        />
      )}

      {results.length > 0 && (
        <>
          {/* Container-query breakpoints, not viewport ones: this grid lives
              in a centre column whose actual width has nothing to do with
              the window's — a viewport `md:` would cram six posters into a
              column a third that wide.
              Every step is set where the next column still leaves a card
              above ~160px, which is where a two-line title stops fitting
              over the poster and the add button loses its label. A 1080p
              window lands on six; the 940px default sits at two. */}
          <div
            className={cx(
              "grid grid-cols-2 gap-3 transition-opacity @lg:grid-cols-3 @2xl:grid-cols-4 @4xl:grid-cols-5 @5xl:grid-cols-6",
              loading && page === 1 && "opacity-50",
            )}
          >
            {results.map((movie) => {
              const key = movie.tmdbId.toString();
              const selected = selectedIds.has(key);
              const watchedLocked = watchedIds.has(key);
              return (
                <MovieCard
                  key={key}
                  movie={movie}
                  genreNames={genreNames(movie.genreIds)}
                  watched={watchedLocked || isWatchedManual(movie.tmdbId)}
                  watchedLocked={watchedLocked}
                  showWatchedMark={tab !== "watched" || searching}
                  favorite={isFavorite(movie.tmdbId)}
                  selected={selected}
                  addDisabled={full}
                  onOpen={() => setOpenDetail(movie.tmdbId)}
                  onToggle={() =>
                    selected ? onRemove(movie.tmdbId) : onAdd(toCandidate(movie, genreNames))
                  }
                  onToggleFavorite={() => toggleFavorite(movie)}
                  onToggleWatched={() => toggleWatchedManual(movie)}
                />
              );
            })}

            {/* The next page, drawn before it arrives. Placed inside the
                grid so the placeholders finish the current row rather than
                sitting under it, which is what makes the swap invisible. */}
            {loading && page > 1 && <PosterSkeletons count={6} />}
          </div>

          {/* The scroll trigger. Present even after the last page (until
              `hasMore` says otherwise) so there is always something for the
              observer to watch. */}
          <div ref={sentinelRef} className="h-px" />
        </>
      )}

      {openDetail !== null && (
        <MovieDetailModal
          tmdbId={openDetail}
          onClose={() => setOpenDetail(null)}
          action={(() => {
            const key = openDetail.toString();
            const selected = selectedIds.has(key);
            const movie = results.find((item) => item.tmdbId === openDetail);
            return {
              label: selected ? t("movie.card.remove") : t("movie.card.add"),
              disabled: !selected && full,
              onClick: () => {
                if (selected) onRemove(openDetail);
                else if (movie) onAdd(toCandidate(movie, genreNames));
                setOpenDetail(null);
              },
            };
          })()}
        />
      )}
    </div>
  );
}
