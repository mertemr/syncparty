import { useSyncExternalStore } from "react";

import { ipc } from "@/shared/ipc";

import { createGuestJoins, type GuestJoinState } from "./guestJoin";

/**
 * The app's one guest join, shared by the guest screen (which runs it), the
 * shell (which must not step back past it) and the host screen (which shows
 * it if the mode was switched mid-join).
 */
export const guestJoins = createGuestJoins({
  watchState: ipc.watchState,
  resumeLastSession: ipc.resumeLastSession,
  joinParty: ipc.joinParty,
});

export function useGuestJoin(): GuestJoinState {
  return useSyncExternalStore(guestJoins.subscribe, guestJoins.getState);
}

/**
 * Leaves the party a guest is in — or is still joining: the backend runs
 * one lifecycle step at a time, so a leave sent mid-join waits for the join
 * to land and then undoes it. Clearing the saved invite as well means a
 * party left on purpose is not the one reopened on the next launch.
 */
export async function leaveGuestParty() {
  await ipc.leaveParty();
  await ipc.clearLastSession();
}
