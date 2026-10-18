import type { RoomSnapshot } from "@/shared/types/RoomSnapshot";
import type { SessionState } from "@/shared/types/SessionState";
import type { WatchState } from "@/shared/types/WatchState";

/**
 * Whether the host's room monitor is publishing the room. While it is, it is
 * the only writer; the host's own client stays quiet so the two do not take
 * turns overwriting each other.
 */
export function monitorPublishes(session: SessionState): boolean {
  return session.phase === "hosting" && session.monitorAttached;
}

/**
 * The room snapshot to keep once the watch has changed.
 *
 * A guest's room comes from its own client, and so does a host's while the
 * monitor is off. Once that client is gone nothing will update the room
 * again, so a stale snapshot is worse than none: it would go on listing
 * people who may have left. With the monitor on, the room never depended on
 * this machine's client, so it stays.
 */
export function roomAfterWatchChange(
  room: RoomSnapshot | null,
  next: WatchState | null,
  session: SessionState,
): RoomSnapshot | null {
  if (next !== null) return room;
  return monitorPublishes(session) ? room : null;
}
