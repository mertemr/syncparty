import type { RoomSnapshot } from "@/shared/types/RoomSnapshot";

const MONITOR_NAME = "syncparty-panel";

export function getLobbyState(snapshot: RoomSnapshot | null) {
  const people =
    snapshot?.rooms
      .flatMap((room) => room.watchers)
      .filter((watcher) => watcher.name !== MONITOR_NAME) ?? [];
  const readyCount = people.filter((person) => person.isReady).length;
  const fileCount = people.filter((person) => person.file != null).length;
  const filesCompatible =
    snapshot?.rooms.every(
      (room) =>
        room.fileCompatibility === "exact" ||
        room.fileCompatibility === "durationMatch",
    ) ?? false;

  return {
    people,
    readyCount,
    fileCount,
    filesCompatible,
    everyoneReady:
      people.length > 0 &&
      readyCount === people.length &&
      fileCount === people.length &&
      filesCompatible,
  };
}

export type LobbyState = ReturnType<typeof getLobbyState>;

/**
 * Which of three things the lobby badge says.
 *
 * The empty case is separate because "getting ready" is a claim about people
 * who are not there yet: it reads as a stalled step rather than an empty room.
 */
export type LobbyBadge = "empty" | "waiting" | "ready";

export function getLobbyBadge(state: LobbyState): LobbyBadge {
  if (state.people.length === 0) return "empty";
  return state.everyoneReady ? "ready" : "waiting";
}
