import type { Invite } from "@/shared/types/Invite";
import type { WatchState } from "@/shared/types/WatchState";

/**
 * A guest's way into a party, held outside any component.
 *
 * Joining takes a while: the backend dials the host, then waits for the
 * sync server to let this machine in, and only then is there a watch state
 * to show. Until that moment the join exists nowhere but here. Kept in a
 * component it was lost every time the guest screen remounted (opening
 * Settings does that), and nothing outside the screen knew it was running,
 * so stepping back mid-join left a party opening in the background with no
 * screen showing it.
 *
 * Kept free of React and of IPC so the rules can be tested as they are.
 */
export interface GuestJoinState {
  /**
   * The once-per-launch attempt to rejoin the last party. "untried" until
   * the guest screen first mounts, which that screen treats the same as
   * "running" so a cold start shows "rejoining" straight away instead of
   * flashing a paste box first.
   */
  resume: "untried" | "running" | "settled";
  /** The invite on screen, waiting for Join or being joined. */
  invite: Invite | null;
  /** Whether that invite arrived through a `syncparty://` link. */
  fromLink: boolean;
  /** The invite being joined right now, if any. */
  joining: Invite | null;
}

/** The three commands this needs, passed in so tests can stand in for them. */
export interface GuestJoinBackend {
  watchState: () => Promise<WatchState | null>;
  resumeLastSession: () => Promise<unknown>;
  joinParty: (invite: Invite) => Promise<void>;
}

export interface GuestJoins {
  getState: () => GuestJoinState;
  subscribe: (listener: () => void) => () => void;
  /** Puts an invite on screen, from a paste or a link. */
  offer: (invite: Invite, fromLink: boolean) => void;
  /** Puts the invite on screen aside. Not while it is being joined. */
  dismiss: () => void;
  /**
   * Rejoins the last party, at most once per launch, and only when nothing
   * is being watched already and no invite is waiting. Rejects with the
   * failure for the one call that ran it; every other call does nothing.
   */
  resumeOnce: () => Promise<void>;
  /** Joins the invite on screen. Does nothing while a join is running. */
  join: () => Promise<void>;
}

/**
 * Whether a join or a resume is under way, so stepping away would leave a
 * party opening with nobody looking.
 */
export function guestJoinInFlight(state: GuestJoinState): boolean {
  return state.resume === "running" || state.joining !== null;
}

export function createGuestJoins(backend: GuestJoinBackend): GuestJoins {
  let state: GuestJoinState = {
    resume: "untried",
    invite: null,
    fromLink: false,
    joining: null,
  };
  const listeners = new Set<() => void>();

  // A new object on every change, never a mutation: `useSyncExternalStore`
  // tells a change apart by identity.
  function update(patch: Partial<GuestJoinState>) {
    state = { ...state, ...patch };
    for (const listener of listeners) listener();
  }

  return {
    getState: () => state,

    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },

    offer(invite, fromLink) {
      update({ invite, fromLink });
    },

    dismiss() {
      if (state.joining) return;
      update({ invite: null, fromLink: false });
    },

    async resumeOnce() {
      if (state.resume !== "untried") return;

      // An invite already waiting — from a link, usually — is tonight's
      // party; rejoining last night's on top of it would be the wrong one.
      if (state.invite) {
        update({ resume: "settled" });
        return;
      }

      update({ resume: "running" });
      try {
        // Asked of the backend rather than read from app state: on a cold
        // start the hydrated watch state may not have landed yet, and
        // joining a party this machine is already in is exactly what the
        // once-per-launch rule is for.
        if (await backend.watchState()) return;
        await backend.resumeLastSession();
      } finally {
        update({ resume: "settled" });
      }
    },

    async join() {
      const invite = state.invite;
      if (!invite || state.joining) return;

      update({ joining: invite });
      try {
        await backend.joinParty(invite);
        // In: from here the backend's watch state is the whole story. Only
        // the invite that was joined is put away — one that arrived by link
        // while this one was joining is still to be decided on.
        if (state.invite === invite) update({ invite: null, fromLink: false });
      } finally {
        // A wrong password lands in the caller's `catch` as
        // `sync_rejected`, before any watch was shown; the invite stays up
        // to try again or put aside.
        update({ joining: null });
      }
    },
  };
}
