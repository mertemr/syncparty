import { useTranslate } from "@/shared/i18n";
import { cx } from "@/shared/ui";
import type { MovieSummary } from "@/shared/types/MovieSummary";

import { genreAccent } from "./genreColor";

const POSTER_PLACEHOLDER =
  "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='200' height='300'%3E%3Crect width='200' height='300' fill='%23161a24'/%3E%3C/svg%3E";

function year(releaseDate: string | null): string | null {
  return releaseDate ? releaseDate.slice(0, 4) : null;
}

/** Both marks are drawn the same way — one outline, filled in when set —
 * so the control strip reads as one family rather than a heart glyph next
 * to an icon next to a button. `fill` is what carries the state. */
function HeartIcon({ filled }: { filled: boolean }) {
  return (
    <svg
      viewBox="0 0 24 24"
      className={cx("size-3.5 stroke-current", filled ? "fill-current" : "fill-none")}
      strokeWidth="2"
      aria-hidden
    >
      <path d="M12 20s-7-4.5-7-9.5A4.5 4.5 0 0 1 12 7a4.5 4.5 0 0 1 7 3.5c0 5-7 9.5-7 9.5Z" strokeLinejoin="round" />
    </svg>
  );
}

function EyeIcon({ filled }: { filled: boolean }) {
  return (
    <svg
      viewBox="0 0 24 24"
      className={cx("size-3.5 stroke-current", filled ? "fill-current" : "fill-none")}
      strokeWidth="2"
      aria-hidden
    >
      <path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7-10-7-10-7Z" strokeLinejoin="round" />
      <circle cx="12" cy="12" r="3" className="fill-none" />
    </svg>
  );
}

/** The two icon buttons beside Add, and Add itself, share this. */
const CARD_CONTROL =
  "pointer-events-auto flex items-center justify-center rounded-[var(--radius-control)] py-1.5 text-xs font-semibold tracking-wide transition-colors duration-[var(--duration-fast)] disabled:cursor-not-allowed disabled:opacity-45";

/** A poster card for the browse grid — the same shape whether it opens the
 * detail view or doubles as a candidate the host can add/remove.
 *
 * The poster is the whole card: title, year, rating and the add control all
 * sit on a scrim over its lower third rather than in a strip below it. Three
 * to a row in a 260px-flanked centre column leaves each card about 190px
 * wide, and a separate meta block at that width costs a third of the card's
 * height to say what fits over the artwork for free.
 *
 * Add, favourite and watched sit together in one strip at the bottom of the
 * scrim, drawn in one grammar: an outline is off, a fill is on. They were
 * three different shapes in two different corners before — a text heart and
 * an eye glyph floating on the artwork, and a real button below them — which
 * read as three unrelated things rather than three switches on one card.
 */
export function MovieCard({
  movie,
  genreNames,
  watched,
  watchedLocked,
  favorite,
  selected,
  addDisabled,
  showWatchedMark = true,
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
  /** Off in a grid where everything is watched — marking every poster in
   * the list says nothing and just makes the page look broken. */
  showWatchedMark?: boolean;
  onOpen: () => void;
  onToggle?: () => void;
  onToggleFavorite?: () => void;
  onToggleWatched?: () => void;
}) {
  const t = useTranslate();
  const genre = genreNames[0];
  const released = year(movie.releaseDate);

  return (
    <div
      className={cx(
        // `isolate`: the scrim and the indicator row below stack against
        // each other, not against the page. Without it a card's `z-20` scrim
        // outranks the panel's sticky action bar and paints over it.
        "group relative isolate aspect-2/3 overflow-hidden rounded-panel border bg-surface transition-colors",
        selected ? "border-accent shadow-[0_0_0_1px_var(--color-accent)]" : "border-line",
      )}
    >
      <img
        src={movie.poster ?? POSTER_PLACEHOLDER}
        alt=""
        loading="lazy"
        className={cx(
          "absolute inset-0 size-full object-cover",
          "transition-[transform,filter] duration-[var(--duration-slow)] group-hover:scale-[1.04]",
          // Already seen reads at a glance without spending a badge on it.
          // Hovering clears it — a dimmed poster is a hint, not a verdict.
          watched &&
            showWatchedMark &&
            "saturate-50 brightness-75 group-hover:saturate-100 group-hover:brightness-100",
          "motion-reduce:transform-none motion-reduce:transition-none",
        )}
      />

      {/* The card's own hit area, under every control and over the poster.
          Everything above it that should stay clickable opts back in with
          `pointer-events-auto`; the scrim's text does not, so a click on the
          title still opens the detail view. */}
      <button
        type="button"
        onClick={onOpen}
        aria-label={movie.title}
        className="absolute inset-0 z-10 focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent"
      />

      {/* Two stops rather than a plain fade: a linear gradient over a poster
          leaves the title floating in the middle of the image, and a solid
          band cuts the artwork in half. This is opaque under the text and
          gone by the time it reaches the middle of the card. */}
      <div className="pointer-events-none absolute inset-x-0 bottom-0 z-20 bg-linear-to-t from-canvas via-canvas/85 via-40% to-transparent px-2.5 pt-10 pb-2.5">
        <p className="line-clamp-2 text-sm leading-tight font-semibold text-ink">{movie.title}</p>

        <div className="mt-1 flex items-center gap-1.5 overflow-hidden font-mono text-[10px] whitespace-nowrap text-ink-faint">
          {released && <span>{released}</span>}
          <span className="text-accent">★ {movie.rating.toFixed(1)}</span>
          {genre && (
            <span style={{ color: genreAccent(genre) }} className="truncate uppercase">
              {genre}
            </span>
          )}
        </div>

        {/* One strip, three controls, one grammar: outline means off, filled
            means on. The marks used to be loose glyphs floating on the
            poster's top corner, in a different shape and a different size
            from every other control in the app — they sit with Add now
            because they are the same kind of thing, a switch on this card. */}
        <div className="mt-2 flex gap-1.5">
          {onToggle && (
            <button
              type="button"
              disabled={!selected && addDisabled}
              onClick={onToggle}
              className={cx(
                CARD_CONTROL,
                "min-w-0 flex-1 gap-1.5 px-2",
                selected
                  ? "bg-accent text-accent-ink hover:bg-accent-strong"
                  : "border border-line bg-surface-raised/80 text-ink hover:border-accent/60 hover:bg-surface-raised",
              )}
            >
              <span className="truncate">
                {selected ? `✓ ${t("movie.card.added")}` : `+ ${t("movie.card.add")}`}
              </span>
            </button>
          )}

          {onToggleFavorite && (
            <button
              type="button"
              aria-label={t(favorite ? "movie.card.unfavorite" : "movie.card.favorite")}
              aria-pressed={favorite}
              onClick={onToggleFavorite}
              className={cx(
                CARD_CONTROL,
                "w-8 shrink-0",
                favorite
                  ? "bg-accent text-accent-ink hover:bg-accent-strong"
                  : "border border-line bg-surface-raised/80 text-ink-muted hover:border-accent/60 hover:text-accent",
              )}
            >
              <HeartIcon filled={favorite} />
            </button>
          )}

          {onToggleWatched && (
            <button
              type="button"
              aria-label={t(watched ? "movie.card.unmarkWatched" : "movie.card.markWatched")}
              aria-pressed={watched}
              disabled={watchedLocked}
              onClick={onToggleWatched}
              className={cx(
                CARD_CONTROL,
                "w-8 shrink-0",
                watched
                  ? "bg-good text-canvas hover:bg-good/85"
                  : "border border-line bg-surface-raised/80 text-ink-muted hover:border-good/60 hover:text-good",
              )}
            >
              <EyeIcon filled={watched} />
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
