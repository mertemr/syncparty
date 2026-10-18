import { describe, expect, it } from "vitest";

import type { MessageKey } from "@/shared/i18n";
import type { RoomSnapshot } from "@/shared/types/RoomSnapshot";
import type { RoomView } from "@/shared/types/RoomView";
import type { WatcherView } from "@/shared/types/WatcherView";
import type { WatchState } from "@/shared/types/WatchState";

import { describeWatch, roomOf, watchToastMessage } from "./watchState";

/** Hands the key back, so assertions read as which message was chosen. */
const t = (key: MessageKey) => key;

const STALKER = { name: "Stalker.1979.1080p.mkv", durationSeconds: 9661 };

function watch(overrides: Partial<WatchState> = {}): WatchState {
  return {
    role: "guest",
    room: "movie-night",
    username: "mert",
    connection: { state: "connected" },
    player: { state: "loaded", file: STALKER, paused: false },
    ready: false,
    ...overrides,
  };
}

function watcher(name: string, overrides: Partial<WatcherView> = {}): WatcherView {
  return { name, file: STALKER, isReady: false, isController: false, ...overrides };
}

function snapshot(rooms: Partial<RoomView>[]): RoomSnapshot {
  return {
    connected: true,
    rooms: rooms.map((room) => ({
      name: "movie-night",
      watchers: [watcher("mert"), watcher("taha")],
      everyoneOnTheSameFile: true,
      fileCompatibility: "exact",
      ...room,
    })),
  };
}

describe("describeWatch", () => {
  it("shows only the tape band while the party has not let this machine in yet", () => {
    const view = describeWatch(
      watch({ connection: { state: "connecting" }, player: { state: "starting" } }),
      null,
      t,
    );

    expect(view.tape).toBe("TRACKING");
    expect(view.loading).toBe("watch.connecting");
    expect(view.body).toEqual({ kind: "none" });
    expect(view.fileAction).toBeNull();
    expect(view.reopen).toBe(false);
    expect(view.ready).toBeNull();
  });

  it("puts the server's own words in a bad banner when the party turns this machine away", () => {
    const view = describeWatch(
      watch({ connection: { state: "rejected", message: "the room password is wrong" } }),
      null,
      t,
    );

    expect(view.tape).toBe("STOP");
    expect(view.tone).toBe("bad");
    expect(view.banner).toEqual({
      tone: "bad",
      title: "watch.rejected",
      detail: "the room password is wrong",
    });
    // A loaded file underneath does not matter: nothing can be synced.
    expect(view.body).toEqual({ kind: "none" });
    expect(view.ready).toBeNull();
  });

  // Retrying the same invite would be turned away the same way, so the way
  // out a guest is offered is a different one.
  it("offers a rejected guest a different invite, and a rejected host its way out", () => {
    const rejected = { state: "rejected", message: "no" } as const;

    expect(describeWatch(watch({ connection: rejected }), null, t).leave).toBe("guest.clear");
    expect(
      describeWatch(watch({ role: "host", connection: rejected }), null, t).leave,
    ).toBe("host.watch.stop");
  });

  it("says which attempt a dropped connection is on, and keeps the player as it was", () => {
    const view = describeWatch(
      watch({ connection: { state: "reconnecting", attempt: 3 }, ready: true }),
      null,
      t,
    );

    expect(view.tape).toBe("TRACKING");
    expect(view.status).toBe("watch.status.reconnecting");
    expect(view.banner).toEqual({
      tone: "warn",
      title: "watch.reconnecting",
      detail: "watch.reconnecting.attempt 3",
    });
    expect(view.body).toMatchObject({ kind: "loaded", name: STALKER.name });
    expect(view.fileAction).toBe("watch.file.change");
  });

  it("shows the tape band while the player is starting", () => {
    const view = describeWatch(watch({ player: { state: "starting" } }), null, t);

    expect(view.loading).toBe("watch.player.starting");
    expect(view.tape).toBe("STANDBY");
    expect(view.fileAction).toBeNull();
  });

  it("asks for a file when the player is open with nothing in it", () => {
    const view = describeWatch(watch({ player: { state: "idle" } }), null, t);

    expect(view.body).toEqual({ kind: "idle" });
    expect(view.fileAction).toBe("watch.file.choose");
    expect(view.status).toBe("watch.status.idle");
    expect(view.ready).toBeNull();
    expect(view.banner).toBeNull();
  });

  it("names the loaded file with its runtime and offers to change it", () => {
    const view = describeWatch(watch(), null, t);

    expect(view.tape).toBe("PLAY");
    expect(view.status).toBe("watch.status.playing");
    expect(view.body).toEqual({
      kind: "loaded",
      name: STALKER.name,
      duration: "2:41:01",
    });
    expect(view.fileAction).toBe("watch.file.change");
  });

  it("reads as paused when the player is paused", () => {
    const view = describeWatch(
      watch({ player: { state: "loaded", file: STALKER, paused: true } }),
      null,
      t,
    );

    expect(view.tape).toBe("PAUSE");
    expect(view.status).toBe("watch.status.paused");
  });

  it("leaves the runtime out when the player could not tell", () => {
    const view = describeWatch(
      watch({
        player: {
          state: "loaded",
          file: { name: "stream.ts", durationSeconds: null },
          paused: true,
        },
      }),
      null,
      t,
    );

    expect(view.body).toEqual({ kind: "loaded", name: "stream.ts", duration: null });
  });

  it("offers to reopen a player the user closed", () => {
    const view = describeWatch(watch({ player: { state: "closed" } }), null, t);

    expect(view.body).toEqual({ kind: "closed" });
    expect(view.reopen).toBe(true);
    expect(view.fileAction).toBeNull();
    expect(view.tone).toBe("neutral");
  });

  it("shows why a player failed and offers to reopen it", () => {
    const view = describeWatch(
      watch({ player: { state: "failed", message: "mpv stopped answering" } }),
      null,
      t,
    );

    expect(view.body).toEqual({ kind: "failed", message: "mpv stopped answering" });
    expect(view.reopen).toBe(true);
    expect(view.tone).toBe("bad");
  });

  describe("the ready switch", () => {
    it("mirrors the backend's readiness once a file is loaded", () => {
      expect(describeWatch(watch({ ready: true }), null, t).ready).toEqual({
        checked: true,
        enabled: true,
      });
      expect(describeWatch(watch({ ready: false }), null, t).ready).toEqual({
        checked: false,
        enabled: true,
      });
    });

    it("waits while the connection is down, since nobody would hear it", () => {
      const view = describeWatch(
        watch({ connection: { state: "reconnecting", attempt: 1 }, ready: true }),
        null,
        t,
      );

      expect(view.ready).toEqual({ checked: true, enabled: false });
    });

    it("is not offered without a file", () => {
      expect(describeWatch(watch({ player: { state: "idle" } }), null, t).ready).toBeNull();
      expect(describeWatch(watch({ player: { state: "closed" } }), null, t).ready).toBeNull();
    });
  });

  describe("file compatibility", () => {
    const compatibility = (room: RoomSnapshot | null) =>
      describeWatch(watch(), room, t).compatibility;

    it("says everyone has the same file when they do", () => {
      expect(compatibility(snapshot([{ fileCompatibility: "exact" }]))).toEqual({
        tone: "good",
        label: "watch.file.same",
      });
    });

    it("says different names of the same length are fine", () => {
      expect(compatibility(snapshot([{ fileCompatibility: "durationMatch" }]))).toEqual({
        tone: "good",
        label: "watch.file.durationMatch",
      });
    });

    it("warns when someone has a different film open", () => {
      expect(compatibility(snapshot([{ fileCompatibility: "mismatch" }]))).toEqual({
        tone: "warn",
        label: "watch.file.mismatch",
      });
    });

    it("waits while someone has not opened anything yet", () => {
      expect(compatibility(snapshot([{ fileCompatibility: "waiting" }]))).toEqual({
        tone: "neutral",
        label: "watch.file.waiting",
      });
    });

    it("says the room is empty rather than that a lone file matches itself", () => {
      expect(
        compatibility(snapshot([{ watchers: [watcher("mert")], fileCompatibility: "exact" }])),
      ).toEqual({ tone: "neutral", label: "watch.file.alone" });
    });

    it("says nothing before the room has been listed", () => {
      expect(compatibility(null)).toBeNull();
      expect(compatibility({ connected: false, rooms: [] })).toBeNull();
    });

    it("is only worked out once a file is loaded", () => {
      const view = describeWatch(
        watch({ player: { state: "idle" } }),
        snapshot([{ fileCompatibility: "mismatch" }]),
        t,
      );
      expect(view.compatibility).toBeNull();
    });
  });
});

describe("roomOf", () => {
  // The server shortens long room names, so the name this machine asked for
  // is not always the one it is listed under.
  it("finds this machine's room by its own entry before its name", () => {
    const rooms = snapshot([
      { name: "movie-night", watchers: [watcher("taha")] },
      { name: "movie-ni", watchers: [watcher("mert"), watcher("ada")] },
    ]);

    expect(roomOf(watch(), rooms)?.name).toBe("movie-ni");
  });

  it("falls back to the room name before the server has named this machine", () => {
    const rooms = snapshot([
      { name: "elsewhere", watchers: [watcher("ada")] },
      { name: "movie-night", watchers: [watcher("taha")] },
    ]);

    expect(roomOf(watch({ username: null }), rooms)?.name).toBe("movie-night");
  });

  it("is nothing when the room is not listed", () => {
    expect(roomOf(watch({ username: null }), snapshot([{ name: "elsewhere" }]))).toBeNull();
  });
});

describe("watchToastMessage", () => {
  const connected = watch();
  const reconnecting = watch({ connection: { state: "reconnecting", attempt: 1 } });

  it("announces a connection that dropped", () => {
    expect(watchToastMessage(connected, reconnecting, t)).toBe("watch.toast.lost");
  });

  it("announces a connection that came back", () => {
    expect(watchToastMessage(reconnecting, connected, t)).toBe("watch.toast.reconnected");
  });

  // Every retry republishes the state with a higher attempt; only the drop
  // itself is news.
  it("stays quiet while the retries count up", () => {
    const later = watch({ connection: { state: "reconnecting", attempt: 2 } });
    expect(watchToastMessage(reconnecting, later, t)).toBeNull();
  });

  it("stays quiet on joining and leaving, which the user just did themselves", () => {
    expect(watchToastMessage(null, connected, t)).toBeNull();
    expect(watchToastMessage(connected, null, t)).toBeNull();
  });
});
