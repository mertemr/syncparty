import { useTranslate } from "@/shared/i18n";
import { Badge, Card, Dot } from "@/shared/ui";
import type { RoomSnapshot } from "@/shared/types/RoomSnapshot";

import { getLobbyBadge, getLobbyState } from "./lobbyState";

export function LobbyPanel({
  snapshot,
}: {
  snapshot: RoomSnapshot | null;
}) {
  const t = useTranslate();

  const state = getLobbyState(snapshot);
  const { people, readyCount, fileCount, filesCompatible, everyoneReady } =
    state;
  const badge = getLobbyBadge(state);

  const badgeLabel = {
    empty: t("host.lobby.noGuests"),
    waiting: t("host.lobby.waiting"),
    ready: t("host.lobby.ready"),
  }[badge];
  const tone = badge === "ready" ? "good" : "neutral";

  return (
    <Card
      title={t("host.lobby.title")}
      action={
        <Badge tone={tone}>
          <Dot tone={tone} />
          {badgeLabel}
        </Badge>
      }
      className="border-accent/20"
    >
      <div>
        <p className="text-sm font-semibold text-ink">
          {t("host.lobby.readyCount")}: {readyCount}/{people.length}
        </p>
        <p className="mt-1 text-xs leading-relaxed text-ink-muted">
          {people.length === 0
            ? t("host.lobby.empty")
            : fileCount < people.length
              ? t("host.lobby.filesWaiting")
              : !filesCompatible
                ? t("host.lobby.filesMismatch")
                : everyoneReady
                  ? t("host.lobby.everyoneReady")
                  : t("host.lobby.peopleWaiting")}
        </p>
      </div>
    </Card>
  );
}
