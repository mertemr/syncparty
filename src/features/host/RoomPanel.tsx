import { useTranslate } from "@/shared/i18n";
import { Card, EmptyState } from "@/shared/ui";
import type { RoomSnapshot } from "@/shared/types/RoomSnapshot";

import { RoomCards } from "@/features/party/RoomCards";

/**
 * Who is in the room, what they have open, and whether they are ready.
 *
 * Driven entirely by pushed snapshots — the panel never asks. They come from
 * the monitor while it is attached, and otherwise from the host's own client
 * while the host is watching; `unavailable` is for when neither is there to
 * publish anything.
 */
export function RoomPanel({
  snapshot,
  unavailable,
}: {
  snapshot: RoomSnapshot | null;
  unavailable: boolean;
}) {
  const t = useTranslate();

  if (unavailable) {
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

  return <RoomCards snapshot={snapshot} />;
}
