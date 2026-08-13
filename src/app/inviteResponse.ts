/**
 * What an arriving `syncparty://` link should do to an app already in use.
 *
 * A link normally settles the mode question outright — whoever sent it is
 * hosting, so tonight the user is a guest. A host is the exception: the server
 * and everyone connected to it belong to a session that walking away from
 * would leave running with nothing on screen pointing at it. That is the same
 * reason stepping back out of hosting asks first, so an invite asks too.
 */
import type { SessionState } from "@/shared/types/SessionState";

type Phase = SessionState["phase"];

export type InviteResponse = "ignore" | "switchToGuest" | "askToStopHosting";

/** Whether a party is up, or on its way up. */
export function isPartyRunning(phase: Phase): boolean {
  return phase === "starting" || phase === "hosting";
}

export function inviteResponse({
  invite,
  phase,
}: {
  invite: boolean;
  phase: Phase;
}): InviteResponse {
  if (!invite) return "ignore";
  return isPartyRunning(phase) ? "askToStopHosting" : "switchToGuest";
}
