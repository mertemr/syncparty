import { guestJoinInFlight, type GuestJoinState } from "@/features/guest/guestJoin";
import type { AppMode } from "@/shared/types/AppMode";
import type { SessionState } from "@/shared/types/SessionState";
import type { WatchState } from "@/shared/types/WatchState";

/** What stepping back would break, if anything. */
export type LeaveReason = "hosting" | "joining" | "watching";

/**
 * Whether the Back button has to ask first, and in which words.
 *
 * Hosting takes the server down and everyone connected to it. A guest who
 * is watching would be walking away from a player that nothing keeps in
 * step any more. And a guest join still under way has no watch state yet —
 * the backend publishes one only once the party has answered — but will
 * open the player the moment it does; stepping past it would leave a party
 * running behind a screen that does not show it.
 *
 * The guest checks do not look at the mode: Settings can switch it with a
 * guest party open, and stepping back from the host screen must not lose
 * that party either.
 */
export function leaveReason({
  mode,
  session,
  guestJoin,
  watch,
}: {
  mode: AppMode | null;
  session: SessionState;
  guestJoin: GuestJoinState;
  watch: WatchState | null;
}): LeaveReason | null {
  const partyRunning =
    session.phase === "starting" || session.phase === "hosting";

  if (mode === "host" && partyRunning) return "hosting";
  if (guestJoinInFlight(guestJoin)) return "joining";
  if (watch?.role === "guest") return "watching";
  return null;
}
