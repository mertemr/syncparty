import { useEffect, useState } from "react";

import { useAppState } from "@/app/AppState";
import { useTranslate } from "@/shared/i18n";
import { ipc } from "@/shared/ipc";
import { Badge, Card, EmptyState } from "@/shared/ui";
import type { PartyLogEntry } from "@/shared/types/PartyLogEntry";

const POSTER_PLACEHOLDER =
  "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='200' height='300'%3E%3Crect width='200' height='300' fill='%23161a24'/%3E%3C/svg%3E";

/** `1h 47m`, or `12m` under the hour. Not a clock reading — a duration read
 * at a glance, which is the only thing anyone wants from it here. */
function duration(startedAt: bigint, endedAt: bigint): string {
  const minutes = Math.max(0, Math.round(Number(endedAt - startedAt) / 60_000));
  const hours = Math.floor(minutes / 60);
  return hours > 0 ? `${hours}h ${minutes % 60}m` : `${minutes}m`;
}

function clock(at: bigint, locale: string): string {
  return new Date(Number(at)).toLocaleTimeString(locale, {
    hour: "2-digit",
    minute: "2-digit",
  });
}

/**
 * Every night the app has hosted: when it ran, what was on, and who was in
 * the room.
 *
 * Not the same list as the vote history — a party can run without a ballot,
 * and a vote's clock starts when the host began drafting rather than when
 * anyone sat down. This is the evening itself.
 */
export function PartyLogCard() {
  const t = useTranslate();
  const { settings, session } = useAppState();
  const [entries, setEntries] = useState<PartyLogEntry[]>([]);

  const locale = settings?.language === "tr" ? "tr-TR" : "en-US";

  // Re-read whenever hosting starts or stops: those are the two moments a
  // row is opened and closed, and nothing else changes this list.
  useEffect(() => {
    void ipc
      .getPartyLog()
      .then(setEntries)
      .catch(() => setEntries([]));
  }, [session.phase]);

  return (
    <Card title={t("partyLog.title")}>
      {entries.length === 0 ? (
        <EmptyState title={t("partyLog.empty")} />
      ) : (
        <ul className="max-h-80 space-y-3 overflow-y-auto pr-1">
          {entries.map((entry) => {
            const running = entry.endedAt === null;

            return (
              <li key={entry.id} className="flex gap-2.5">
                <img
                  src={entry.moviePoster ?? POSTER_PLACEHOLDER}
                  alt=""
                  className="h-15 w-10 shrink-0 rounded-sm object-cover"
                />

                <div className="min-w-0 flex-1">
                  <p
                    className="truncate text-xs font-medium text-ink"
                    title={entry.movieTitle ?? undefined}
                  >
                    {entry.movieTitle ?? t("partyLog.noMovie")}
                  </p>

                  <p className="mt-0.5 font-mono text-[10px] text-ink-faint">
                    {new Date(Number(entry.startedAt)).toLocaleDateString(locale)}
                    {" · "}
                    {clock(entry.startedAt, locale)}
                    {entry.endedAt !== null && ` – ${clock(entry.endedAt, locale)}`}
                  </p>

                  {running ? (
                    <div className="mt-1">
                      <Badge tone="good">{t("partyLog.running")}</Badge>
                    </div>
                  ) : (
                    <p className="mt-0.5 font-mono text-[10px] text-ink-faint">
                      {duration(entry.startedAt, entry.endedAt!)}
                    </p>
                  )}

                  {entry.participants.length > 0 && (
                    <p
                      className="mt-1 truncate text-[11px] text-ink-muted"
                      title={entry.participants.join(", ")}
                    >
                      {entry.participants.join(", ")}
                    </p>
                  )}
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </Card>
  );
}
