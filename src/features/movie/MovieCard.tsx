import { useTranslate } from "@/shared/i18n";
import { cx } from "@/shared/ui";
import type { MovieSummary } from "@/shared/types/MovieSummary";

import { genreAccent } from "./genreColor";

const POSTER_PLACEHOLDER =
  "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='200' height='300'%3E%3Crect width='200' height='300' fill='%23161a24'/%3E%3C/svg%3E";

function year(releaseDate: string | null): string | null {
  return releaseDate ? releaseDate.slice(0, 4) : null;
}

/** A drop shadow standing in for a background: these sit directly on the
 * poster, whose brightness varies, so a plain glyph needs something to stay
 * legible without going back to a badge behind it. */
const INDICATOR_SHADOW = "drop-shadow-[0_1px_3px_rgb(0_0_0_/_0.85)]";

function EyeIcon() {
  return (
    <svg viewBox="0 0 24 24" className="size-4 fill-none stroke-current" strokeWidth="2" aria-hidden>
      <path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7-10-7-10-7Z" strokeLinejoin="round" />
      <circle cx="12" cy="12" r="3" />
    </svg>
  );
}

/** A poster card for the browse grid — the same shape whether it opens the
 * detail view or doubles as a candidate the host can add/remove.
 *
 * Favourite and watched are plain coloured glyphs (top-left) rather than
 * badges — a card already showing title, year, rating and genres has no
 * room for another pill, and a heart or an eye reads on its own without one.
 * The add/remove control (top-right) stays a solid button: it is an action,
 * not a status. */
export function MovieCard({
  movie,
  genreNames,
  watched,
  watchedLocked,
  favorite,
  selected,
  addDisabled,
  onOpen,
  onToggle,
  onToggleFavorite,
  onToggleWatched,
}: {
  movie: MovieSummary;
  genreNames: string[];
  watched: boolean;
  /** True when `watched` came from real session history — nothing to toggle,
   * a movie night actually happened. */
  watchedLocked: boolean;
  favorite: boolean;
  selected: boolean;
  addDisabled: boolean;
  onOpen: () => void;
  onToggle?: () => void;
  onToggleFavorite?: () => void;
  onToggleWatched?: () => void;
}) {
  const t = useTranslate();

  return (
    <div
      className={cx(
        "group relative flex flex-col overflow-hidden rounded-panel border bg-surface/85 transition-colors",
        selected ? "border-accent/70" : "border-line hover:border-ink-faint",
      )}
    >
      <div className="relative">
        <button
          type="button"
          onClick={onOpen}
          className="block w-full text-left focus-visible:outline-2 focus-visible:outline-accent"
        >
          <img
            src={movie.poster ?? POSTER_PLACEHOLDER}
            alt=""
            loading="lazy"
            className="aspect-[2/3] w-full object-cover"
          />
        </button>

        <div className="absolute top-2 left-2 flex items-center gap-2">
          {onToggleFavorite && (
            <button
              type="button"
              aria-label={t(favorite ? "movie.card.unfavorite" : "movie.card.favorite")}
              aria-pressed={favorite}
              onClick={(event) => {
                event.stopPropagation();
                onToggleFavorite();
              }}
              className={cx(
                INDICATOR_SHADOW,
                "text-lg leading-none transition-opacity",
                favorite
                  ? "text-accent opacity-100"
                  : "text-ink opacity-0 hover:text-accent focus-visible:opacity-100 group-hover:opacity-100",
              )}
            >
              {favorite ? "♥" : "♡"}
            </button>
          )}

          {onToggleWatched && (
            <button
              type="button"
              aria-label={t(
                watched ? "movie.card.unmarkWatched" : "movie.card.markWatched",
              )}
              aria-pressed={watched}
              disabled={watchedLocked}
              onClick={(event) => {
                event.stopPropagation();
                onToggleWatched();
              }}
              className={cx(
                INDICATOR_SHADOW,
                "transition-opacity disabled:cursor-default",
                watched
                  ? "text-good opacity-100"
                  : "text-ink opacity-0 hover:text-good focus-visible:opacity-100 group-hover:opacity-100",
              )}
            >
              <EyeIcon />
            </button>
          )}
        </div>

        {onToggle && (
          <button
            type="button"
            aria-label={t(selected ? "movie.card.remove" : "movie.card.add")}
            aria-pressed={selected}
            disabled={!selected && addDisabled}
            onClick={(event) => {
              event.stopPropagation();
              onToggle();
            }}
            className={cx(
              "absolute top-2 right-2 flex size-7 items-center justify-center rounded-full text-sm font-bold backdrop-blur-sm transition-opacity disabled:cursor-not-allowed disabled:opacity-40",
              selected
                ? "bg-accent text-accent-ink opacity-100"
                : "bg-canvas/75 text-ink-muted opacity-0 hover:text-accent focus-visible:opacity-100 group-hover:opacity-100",
            )}
          >
            {selected ? "✓" : "+"}
          </button>
        )}
      </div>

      <div className="flex flex-1 flex-col gap-1.5 p-3">
        <button type="button" onClick={onOpen} className="text-left">
          <p className="line-clamp-2 text-sm font-semibold text-ink">{movie.title}</p>
        </button>

        <div className="flex items-center gap-2 font-mono text-[11px] text-ink-faint">
          {year(movie.releaseDate) && <span>{year(movie.releaseDate)}</span>}
          <span aria-hidden>·</span>
          <span>★ {movie.rating.toFixed(1)}</span>
        </div>

        {genreNames.length > 0 && (
          <div className="flex flex-wrap gap-1">
            {genreNames.slice(0, 2).map((name) => (
              <span
                key={name}
                style={{ color: genreAccent(name) }}
                className="rounded-full border border-current/30 px-1.5 py-0.5 font-mono text-[9px] tracking-wide uppercase"
              >
                {name}
              </span>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
