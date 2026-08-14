import { useTranslate } from "@/shared/i18n";
import { Badge, Card, EmptyState } from "@/shared/ui";
import type { RoomSnapshot } from "@/shared/types/RoomSnapshot";

import { ChannelRow } from "@/features/party/ChannelRow";

/**
 * Who is in the room, what they have open, and whether they are ready.
 *
 * Driven entirely by pushed snapshots — the panel never asks.
 */
export function RoomPanel({
  snapshot,
  monitorAttached,
}: {
  snapshot: RoomSnapshot | null;
  monitorAttached: boolean;
}) {
  const t = useTranslate();

  if (!monitorAttached) {
    return (
      <Card title={t("host.room.title")}>
        <EmptyState title={t("host.room.monitorOff")} />
      </Card>
    );
  }

  if (!snapshot?.connected) {
    return (
      <Card title={t("host.room.title")}>
        <EmptyState title={t("host.room.disconnected")} />
      </Card>
    );
  }

  if (snapshot.rooms.length === 0) {
    return (
      <Card title={t("host.room.title")}>
        <EmptyState title={t("host.room.empty")} />
      </Card>
    );
  }

  return (
    <div className="space-y-4">
      {snapshot.rooms.map((room) => {
        const filesCompatible =
          room.fileCompatibility === "exact" ||
          room.fileCompatibility === "durationMatch";

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
