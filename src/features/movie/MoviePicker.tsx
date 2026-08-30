import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { useTranslate } from "@/shared/i18n";
import { errorMessage, ipc } from "@/shared/ipc";
import { Button, cx, EmptyState, Input, Rewind } from "@/shared/ui";
import type { Genre } from "@/shared/types/Genre";
import type { MovieCandidate } from "@/shared/types/MovieCandidate";
import type { MovieSummary } from "@/shared/types/MovieSummary";

import { isFavorite, subscribeFavorites, toggleFavorite } from "./favorites";
import { MovieCard } from "./MovieCard";
import { MovieDetailModal } from "./MovieDetailModal";
import { isWatchedManual, subscribeWatchedManual, toggleWatchedManual } from "./watchlist";

type Tab = "popular" | "nowPlaying" | "upcoming" | "topRated" | "search";

const FETCHERS: Record<Exclude<Tab, "search">, (page: number) => Promise<MovieSummary[]>> = {
  popular: (page) => ipc.getPopularMovies(page),
  nowPlaying: (page) => ipc.getNowPlayingMovies(page),
  upcoming: (page) => ipc.getUpcomingMovies(page),
  topRated: (page) => ipc.getTopRatedMovies(page),
};

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
  // Forces a re-render whenever a favourite changes, in this picker or in
  // the sidebar's Favourites tab — `isFavorite` reads `localStorage` fresh
  // each render, but nothing above depends on its value, so nothing would
  // notice a change on its own without this.
  const [, forceFavoritesRerender] = useState(0);
  useEffect(() => subscribeFavorites(() => forceFavoritesRerender((version) => version + 1)), []);
  // Same trick, same reason, for manual watched marks.
  const [, forceWatchedRerender] = useState(0);
  useEffect(() => subscribeWatchedManual(() => forceWatchedRerender((version) => version + 1)), []);
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

  // A new tab or search term starts a fresh list at page 1 — the fetch
  // effect below tells a first page apart from a "load more" page by
  // whether `page` is 1, so this has to land before that effect runs.
  useEffect(() => {
    setPage(1);
    setHasMore(true);
  }, [tab, searchTerm]);

  useEffect(() => {
    if (tab === "search" && !searchTerm.trim()) {
      setResults([]);
      setHasMore(false);
      return;
    }

    let cancelled = false;
    setLoading(true);
    setProblem(null);

    const request = tab === "search" ? ipc.searchMovies(searchTerm, page) : FETCHERS[tab](page);

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
  }, [tab, searchTerm, page]);

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
      { rootMargin: "400px" },
    );
    observerRef.current.observe(node);
  }, []);

  const selectedIds = useMemo(() => new Set(candidates.map((c) => c.tmdbId.toString())), [candidates]);

  const tabs: Array<{ value: Tab; label: string }> = [
    { value: "popular", label: t("movie.tab.popular") },
    { value: "nowPlaying", label: t("movie.tab.nowPlaying") },
    { value: "upcoming", label: t("movie.tab.upcoming") },
    { value: "topRated", label: t("movie.tab.topRated") },
    { value: "search", label: t("movie.tab.search") },
  ];

  return (
    <div className="@container space-y-4">
      {/* A scrollable strip rather than `Choice`'s equal-width row: this
          panel sits in a narrow centre column now, and five tabs squeezed
          to fit would either wrap mid-word or clip — scrolling sideways is
          the one option that stays readable at any column width. */}
      <div
        role="group"
        aria-label={t("movieVote.candidates")}
        className="flex gap-1 overflow-x-auto rounded-[var(--radius-control)] border border-line/80 bg-canvas/70 p-1.5"
      >
        {tabs.map((option) => (
          <button
            key={option.value}
            type="button"
            aria-pressed={option.value === tab}
            onClick={() => setTab(option.value)}
            className={cx(
              "shrink-0 rounded-[var(--radius-control)] px-3 py-2 text-sm whitespace-nowrap transition-colors",
              option.value === tab
                ? "bg-accent font-semibold text-accent-ink shadow-sm"
                : "text-ink-muted hover:bg-surface-raised/50 hover:text-ink",
            )}
          >
            {option.label}
          </button>
        ))}
      </div>

      {tab === "search" && (
        <div className="flex gap-2">
          <Input
            value={query}
            placeholder={t("movie.search.placeholder")}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && query.trim()) setSearchTerm(query.trim());
            }}
          />
          <Button
            variant="primary"
            disabled={!query.trim()}
            onClick={() => setSearchTerm(query.trim())}
          >
            {t("movie.tab.search")}
          </Button>
        </div>
      )}

      {loading && page === 1 && (
        <div className="py-8">
          <Rewind label={t("common.loading")} />
        </div>
      )}

      {!loading && problem && results.length === 0 && (
        <EmptyState title={t("movie.loadError")} detail={problem} />
      )}

      {!(loading && page === 1) && !problem && results.length === 0 && (
        <EmptyState title={t("movie.empty")} />
      )}

      {results.length > 0 && (
        <>
          {/* Container-query breakpoints, not viewport ones: this grid lives
              in a centre column whose actual width has nothing to do with
              the window's — a viewport `md:` would cram four posters into a
              column a third that wide. */}
          <div className="grid grid-cols-2 gap-3 @sm:grid-cols-3 @xl:grid-cols-4">
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
                  favorite={isFavorite(movie.tmdbId)}
                  selected={selected}
                  addDisabled={full}
                  onOpen={() => setOpenDetail(movie.tmdbId)}
                  onToggle={() =>
                    selected ? onRemove(movie.tmdbId) : onAdd(toCandidate(movie, genreNames))
                  }
                  onToggleFavorite={() => toggleFavorite(movie)}
                  onToggleWatched={() => toggleWatchedManual(movie.tmdbId)}
                />
              );
            })}
          </div>

          {/* The scroll trigger. Present even after the last page (until
              `hasMore` says otherwise) so there is always something for the
              observer to watch; a small spinner doubles as the "there might
              be more" affordance while a page is in flight. */}
          <div ref={sentinelRef} className="flex justify-center py-4">
            {loading && page > 1 && <Rewind label={t("common.loading")} />}
          </div>
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
