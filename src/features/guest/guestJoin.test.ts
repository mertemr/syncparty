import { describe, expect, it, vi } from "vitest";

import type { Invite } from "@/shared/types/Invite";
import type { WatchState } from "@/shared/types/WatchState";

import { createGuestJoins, guestJoinInFlight, type GuestJoinBackend } from "./guestJoin";

const TONIGHT: Invite = { endpoint: "abc", password: "pw", room: "movie-night" };
const OTHER: Invite = { endpoint: "def", password: "pw2", room: "late-show" };

const WATCHING: WatchState = {
  role: "guest",
  room: "movie-night",
  username: "mert",
  connection: { state: "connected" },
  player: { state: "idle" },
  ready: false,
};

/** A promise the test settles by hand, to look at the state mid-call. */
function deferred<T = void>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

function backend(overrides: Partial<GuestJoinBackend> = {}) {
  return {
    watchState: vi.fn(async () => null as WatchState | null),
    resumeLastSession: vi.fn(async () => null),
    joinParty: vi.fn(async (_invite: Invite) => {}),
    ...overrides,
  };
}

/** Lets awaited microtasks inside the store run. */
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("resumeOnce", () => {
  it("rejoins the last party when nothing is being watched", async () => {
    const api = backend();
    const joins = createGuestJoins(api);

    await joins.resumeOnce();

    expect(api.resumeLastSession).toHaveBeenCalledTimes(1);
    expect(joins.getState().resume).toBe("settled");
  });

  it("does nothing the second time — a remount must not join again", async () => {
    const api = backend();
    const joins = createGuestJoins(api);

    await joins.resumeOnce();
    await joins.resumeOnce();

    expect(api.watchState).toHaveBeenCalledTimes(1);
    expect(api.resumeLastSession).toHaveBeenCalledTimes(1);
  });

  it("does nothing a second time even while the first is still running", async () => {
    const pending = deferred<null>();
    const api = backend({ resumeLastSession: vi.fn(() => pending.promise) });
    const joins = createGuestJoins(api);

    const first = joins.resumeOnce();
    await flush();
    await joins.resumeOnce();
    expect(api.resumeLastSession).toHaveBeenCalledTimes(1);

    pending.resolve(null);
    await first;
  });

  it("does not resume while this machine is already watching", async () => {
    const api = backend({ watchState: vi.fn(async () => WATCHING) });
    const joins = createGuestJoins(api);

    await joins.resumeOnce();

    expect(api.resumeLastSession).not.toHaveBeenCalled();
    expect(joins.getState().resume).toBe("settled");
  });

  it("skips the resume when an invite is already waiting", async () => {
    const api = backend();
    const joins = createGuestJoins(api);

    joins.offer(TONIGHT, true);
    await joins.resumeOnce();

    expect(api.watchState).not.toHaveBeenCalled();
    expect(api.resumeLastSession).not.toHaveBeenCalled();
    expect(joins.getState().resume).toBe("settled");
  });

  it("settles on a failure, and hands the failure to the caller that ran it", async () => {
    const api = backend({
      resumeLastSession: vi.fn(async () => {
        throw { kind: "sync_rejected", message: "wrong password" };
      }),
    });
    const joins = createGuestJoins(api);

    await expect(joins.resumeOnce()).rejects.toMatchObject({ kind: "sync_rejected" });
    expect(joins.getState().resume).toBe("settled");
    expect(guestJoinInFlight(joins.getState())).toBe(false);

    // And is not retried by the next mount.
    await joins.resumeOnce();
    expect(api.resumeLastSession).toHaveBeenCalledTimes(1);
  });

  it("counts as in flight while it runs, so stepping back has to ask", async () => {
    const pending = deferred<null>();
    const api = backend({ resumeLastSession: vi.fn(() => pending.promise) });
    const joins = createGuestJoins(api);

    expect(guestJoinInFlight(joins.getState())).toBe(false);
    const attempt = joins.resumeOnce();
    expect(joins.getState().resume).toBe("running");
    expect(guestJoinInFlight(joins.getState())).toBe(true);

    pending.resolve(null);
    await attempt;
    expect(guestJoinInFlight(joins.getState())).toBe(false);
  });
});

describe("join", () => {
  it("is in flight from the click until the backend answers, and survives a remount", async () => {
    const pending = deferred();
    const api = backend({ joinParty: vi.fn(() => pending.promise) });
    const joins = createGuestJoins(api);

    joins.offer(TONIGHT, false);
    const join = joins.join();

    // What a freshly mounted screen reads: the invite and that it is joining.
    expect(joins.getState().joining).toBe(TONIGHT);
    expect(joins.getState().invite).toBe(TONIGHT);
    expect(guestJoinInFlight(joins.getState())).toBe(true);

    pending.resolve();
    await join;
    expect(guestJoinInFlight(joins.getState())).toBe(false);
  });

  it("joins once however many times Join is pressed", async () => {
    const pending = deferred();
    const api = backend({ joinParty: vi.fn(() => pending.promise) });
    const joins = createGuestJoins(api);

    joins.offer(TONIGHT, false);
    const first = joins.join();
    await joins.join();
    await joins.join();
    expect(api.joinParty).toHaveBeenCalledTimes(1);

    pending.resolve();
    await first;
  });

  it("puts the invite away once in", async () => {
    const joins = createGuestJoins(backend());

    joins.offer(TONIGHT, true);
    await joins.join();

    expect(joins.getState()).toMatchObject({ invite: null, fromLink: false, joining: null });
  });

  it("keeps the invite up when the party says no, to try again", async () => {
    const api = backend({
      joinParty: vi.fn(async () => {
        throw { kind: "sync_rejected", message: "wrong password" };
      }),
    });
    const joins = createGuestJoins(api);

    joins.offer(TONIGHT, false);
    await expect(joins.join()).rejects.toMatchObject({ kind: "sync_rejected" });

    expect(joins.getState().invite).toBe(TONIGHT);
    expect(joins.getState().joining).toBeNull();
  });

  it("keeps an invite that arrived by link while another was joining", async () => {
    const pending = deferred();
    const api = backend({ joinParty: vi.fn(() => pending.promise) });
    const joins = createGuestJoins(api);

    joins.offer(TONIGHT, false);
    const join = joins.join();
    joins.offer(OTHER, true);

    pending.resolve();
    await join;
    expect(joins.getState()).toMatchObject({ invite: OTHER, fromLink: true, joining: null });
  });

  it("cannot put aside the invite being joined", () => {
    const api = backend({ joinParty: vi.fn(() => new Promise<void>(() => {})) });
    const joins = createGuestJoins(api);

    joins.offer(TONIGHT, false);
    void joins.join();
    joins.dismiss();

    expect(joins.getState().invite).toBe(TONIGHT);
  });

  it("tells subscribers about every change, with a new state each time", async () => {
    const joins = createGuestJoins(backend());
    const seen: unknown[] = [];
    const stop = joins.subscribe(() => seen.push(joins.getState()));

    joins.offer(TONIGHT, false);
    await joins.join();
    stop();
    joins.offer(OTHER, false);

    // offer, joining, put away, joining cleared — and nothing after `stop`.
    expect(seen).toHaveLength(4);
    expect(new Set(seen).size).toBe(4);
  });
});
