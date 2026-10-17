import { useTranslate } from "@/shared/i18n";
import { Badge, Card } from "@/shared/ui";
import type { RoomSnapshot } from "@/shared/types/RoomSnapshot";

import { ChannelRow } from "./ChannelRow";

/**
 * One card per room: who is in it, what they have open, and whether the
 * files line up.
 *
 * Only the listing — whoever renders it decides what an empty or unconnected
 * room says, because the host's panel and a guest's roster have different
 * reasons to have nothing yet.
 */
export function RoomCards({ snapshot }: { snapshot: RoomSnapshot }) {
  const t = useTranslate();

  return (
    <div className="space-y-4">
      {snapshot.rooms.map((room) => {
        // Only a known mismatch marks a row. A room still waiting on
        // someone's file says nothing yet about the files already open, and
        // calling those a tracking error would blame the people who were
        // ready first.
        const filesCompatible = room.fileCompatibility !== "mismatch";

        return (
          <Card
            key={room.name}
            title={room.name}
            action={<Badge tone="neutral">{room.watchers.length}</Badge>}
          >
            {/* Says something the per-person rows cannot: that two different
                filenames are still the same runtime. */}
            {room.fileCompatibility === "durationMatch" && (
              <div className="mb-3 rounded-[var(--radius-control)] border border-good/35 bg-good/10 p-3">
                <p className="text-sm font-medium text-good">
                  {t("host.room.durationMatch")}
                </p>
                <p className="mt-0.5 text-xs text-ink-muted">
                  {t("host.room.durationMatchDetail")}
                </p>
              </div>
            )}

            {room.fileCompatibility === "waiting" && (
              <div className="mb-3 rounded-[var(--radius-control)] border border-line bg-surface-raised/35 p-3">
                <p className="text-sm text-ink-muted">
                  {t("host.room.waitingForFiles")}
                </p>
              </div>
            )}

            {room.fileCompatibility === "mismatch" && (
              <div className="mb-3 rounded-[var(--radius-control)] border border-warn/40 bg-warn/10 p-3">
                <p className="text-sm font-medium text-warn">
                  {t("host.room.mismatch")}
                </p>
                <p className="mt-0.5 text-xs text-ink-muted">
                  {t("host.room.mismatchDetail")}
                </p>
              </div>
            )}

            <ul>
              {room.watchers.map((watcher) => (
                <ChannelRow
                  key={watcher.name}
                  watcher={watcher}
                  filesCompatible={filesCompatible}
                />
              ))}
            </ul>
          </Card>
        );
      })}
    </div>
  );
}
