import { useEffect, useState } from "react";

import { useTranslate } from "@/shared/i18n";
import { errorMessage, ipc } from "@/shared/ipc";
import { Badge, Button, EmptyState, Rewind } from "@/shared/ui";
import type { MovieDetails } from "@/shared/types/MovieDetails";

import { genreAccent } from "./genreColor";

/**
 * A movie's full detail, in an overlay. Trailer embeds YouTube directly when
 * there is one; syncparty never streams anything itself, so this is the one
 * place in the app that plays video at all.
 */
export function MovieDetailModal({
  tmdbId,
  onClose,
  action,
}: {
  tmdbId: bigint;
  onClose: () => void;
  /** The add/remove button, owned by whoever opened this — a browse grid
   * during Draft, say. Omitted where the detail is read-only (guests). */
  action?: { label: string; disabled?: boolean; onClick: () => void };
}) {
  const t = useTranslate();
  const [details, setDetails] = useState<MovieDetails | null>(null);
  const [problem, setProblem] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setDetails(null);
    setProblem(null);

    void ipc
      .getMovieDetails(tmdbId)
      .then((result) => {
        if (!cancelled) setDetails(result);
      })
      .catch((error) => {
        if (!cancelled) setProblem(errorMessage(error));
      });

    return () => {
      cancelled = true;
    };
  }, [tmdbId]);

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") onClose();
    }
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  return (
    <div
      role="presentation"
      className="fixed inset-0 z-50 flex items-center justify-center bg-canvas/80 p-4 backdrop-blur-sm"
      onClick={onClose}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-label={details?.title ?? t("movie.detail.close")}
        onClick={(event) => event.stopPropagation()}
        className="max-h-[85vh] w-full max-w-2xl overflow-y-auto rounded-panel border border-line bg-surface"
      >
        {!details && !problem && (
          <div className="p-10">
            <Rewind label={t("common.loading")} />
          </div>
        )}

        {problem && (
          <div className="p-6">
            <EmptyState title={t("movie.loadError")} detail={problem} />
            <Button variant="ghost" className="mt-4 w-full" onClick={onClose}>
              {t("movie.detail.close")}
            </Button>
          </div>
        )}

        {details && (
          <div>
            {details.backdrop && (
              <img
                src={details.backdrop}
                alt=""
                className="aspect-video w-full object-cover"
              />
            )}

            <div className="space-y-4 p-6">
              <div className="flex items-start justify-between gap-4">
                <div className="min-w-0">
                  <h2 className="font-display text-xl font-extrabold text-ink [font-stretch:110%]">
                    {details.title}
                  </h2>
                  {details.originalTitle !== details.title && (
                    <p className="text-xs text-ink-faint">{details.originalTitle}</p>
                  )}
                </div>
                <Button variant="ghost" onClick={onClose} aria-label={t("movie.detail.close")}>
                  ✕
                </Button>
              </div>

              <div className="flex flex-wrap items-center gap-2 font-mono text-[11px] text-ink-faint">
                {details.releaseDate && <span>{details.releaseDate.slice(0, 4)}</span>}
                {details.runtimeMinutes != null && <span>· {details.runtimeMinutes}min</span>}
                <span>· ★ {details.rating.toFixed(1)} ({String(details.voteCount)})</span>
              </div>

              {details.genres.length > 0 && (
                <div className="flex flex-wrap gap-1.5">
                  {details.genres.map((genre) => (
                    <span
                      key={genre.id}
                      style={{ color: genreAccent(genre.name) }}
                      className="rounded-full border border-current/30 px-2 py-0.5 font-mono text-[10px] tracking-wide uppercase"
                    >
                      {genre.name}
                    </span>
                  ))}
                </div>
              )}

              <p className="text-sm text-ink-muted">{details.overview}</p>

              {details.trailer ? (
                <div className="space-y-2">
                  <iframe
                    className="aspect-video w-full rounded-[var(--radius-control)]"
                    src={`https://www.youtube.com/embed/${details.trailer.key}`}
                    title={t("movie.detail.watchTrailer")}
                    allow="accelerometer; autoplay; clipboard-write; encrypted-media; gyroscope; picture-in-picture"
                    allowFullScreen
                  />
                  <a
                    href={`https://www.youtube.com/watch?v=${details.trailer.key}`}
                    target="_blank"
                    rel="noreferrer"
                    className="text-xs text-accent underline-offset-2 hover:underline"
                  >
                    {t("movie.detail.openOnYoutube")}
                  </a>
                </div>
              ) : (
                <p className="text-sm text-ink-faint">{t("movie.detail.noTrailer")}</p>
              )}

              {details.watchProviders.length > 0 && (
                <div>
                  <p className="mb-1.5 text-xs text-ink-faint">{t("movie.detail.availableOn")}</p>
                  <div className="flex flex-wrap items-center gap-2">
                    {details.watchProviders.map((provider) =>
                      details.watchLink ? (
                        <a
                          key={provider.name}
                          href={details.watchLink}
                          target="_blank"
                          rel="noreferrer"
                        >
                          <Badge tone="neutral">{provider.name}</Badge>
                        </a>
                      ) : (
                        <Badge key={provider.name} tone="neutral">
                          {provider.name}
                        </Badge>
                      ),
                    )}
                  </div>
                  {/* JustWatch attribution: TMDB's watch-provider data is
                      sourced from JustWatch, which requires crediting them
                      wherever that data is shown. */}
                  <p className="mt-1.5 text-[10px] text-ink-faint">
                    Streaming data provided by JustWatch.
                  </p>
                </div>
              )}

              {action && (
                <Button
                  variant="primary"
                  className="w-full"
                  disabled={action.disabled}
                  onClick={action.onClick}
                >
                  {action.label}
                </Button>
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
