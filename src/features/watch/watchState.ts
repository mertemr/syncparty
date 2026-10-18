import type { MessageKey } from "@/shared/i18n";
import type { RoomSnapshot } from "@/shared/types/RoomSnapshot";
import type { RoomView } from "@/shared/types/RoomView";
import type { WatchState } from "@/shared/types/WatchState";

import { formatDuration } from "@/features/party/channels";

type Translate = (key: MessageKey) => string;

/**
 * The word on the status line. Untranslated, like REC and PLAY on the host's
 * side: they are the labels printed on the deck this borrows from.
 * TRACKING is the deck hunting for a signal it has not locked onto yet.
 */
export type TapeWord = "PLAY" | "PAUSE" | "STOP" | "STANDBY" | "TRACKING";

export type WatchTone = "neutral" | "good" | "warn" | "bad";

/** The part of the card that is about the player rather than the party. */
export type PlayerBody =
  | { kind: "none" }
  | { kind: "idle" }
  | { kind: "loaded"; name: string; duration: string | null }
  | { kind: "closed" }
  | { kind: "failed"; message: string };

/** Everything the watch card shows, already decided. */
export interface WatchView {
  tape: TapeWord;
  tone: WatchTone;
  /** A word or two for the badge, and for the diagnostics row. */
  status: string;
  banner: { tone: "warn" | "bad"; title: string; detail: string | null } | null;
  /** Set while something is on its way; the card shows the tape band. */
  loading: string | null;
  body: PlayerBody;
  /** How this machine's file compares with the rest of the room. */
  compatibility: { tone: WatchTone; label: string } | null;
  /** The file button's label, when there is a player to load a file into. */
  fileAction: string | null;
  reopen: boolean;
  ready: { checked: boolean; enabled: boolean } | null;
  leave: string;
}

/**
 * Turns the backend's watch state into what the card shows.
 *
 * The connection decides first: a party that has not let this machine in, or
 * has turned it away, has nothing to say about the player. Once connected the
 * player decides, and a dropped connection keeps the player part as it was —
 * the film is still on this machine's screen while the client retries.
 */
export function describeWatch(
  watch: WatchState,
  snapshot: RoomSnapshot | null,
  t: Translate,
): WatchView {
  const leave = t(
    watch.role === "host"
      ? "host.watch.stop"
      : watch.connection.state === "rejected"
        ? "guest.clear"
        : "watch.leave",
  );
  const quiet = {
    banner: null,
    loading: null,
    body: { kind: "none" },
    compatibility: null,
    fileAction: null,
    reopen: false,
    ready: null,
    leave,
  } as const;

  switch (watch.connection.state) {
    case "connecting":
      return {
        ...quiet,
        tape: "TRACKING",
        tone: "warn",
        status: t("watch.status.connecting"),
        loading: t("watch.connecting"),
      };

    // Not retried by the backend, so nothing here offers to: the way out is
    // a different invite, or for a host, closing the player.
    case "rejected":
      return {
        ...quiet,
        tape: "STOP",
        tone: "bad",
        status: t("watch.status.rejected"),
        banner: {
          tone: "bad",
          title: t("watch.rejected"),
          detail: watch.connection.message,
        },
      };

    case "reconnecting":
      return {
        ...describePlayer(watch, snapshot, false, t),
        tape: "TRACKING",
        tone: "warn",
        status: t("watch.status.reconnecting"),
        banner: {
          tone: "warn",
          title: t("watch.reconnecting"),
          detail: `${t("watch.reconnecting.attempt")} ${watch.connection.attempt}`,
        },
        leave,
      };

    case "connected":
      return { ...describePlayer(watch, snapshot, true, t), banner: null, leave };
  }
}

function describePlayer(
  watch: WatchState,
  snapshot: RoomSnapshot | null,
  connected: boolean,
  t: Translate,
): Omit<WatchView, "banner" | "leave"> {
  const none = {
    loading: null,
    body: { kind: "none" },
    compatibility: null,
    fileAction: null,
    reopen: false,
    ready: null,
  } as const;
  const player = watch.player;

  switch (player.state) {
    case "starting":
      return {
        ...none,
        tape: "STANDBY",
        tone: "neutral",
        status: t("watch.status.starting"),
        loading: t("watch.player.starting"),
      };

    case "idle":
      return {
        ...none,
        tape: "STANDBY",
        tone: "good",
        status: t("watch.status.idle"),
        body: { kind: "idle" },
        fileAction: t("watch.file.choose"),
      };

    case "loaded":
      return {
        ...none,
        tape: player.paused ? "PAUSE" : "PLAY",
        tone: "good",
        status: t(player.paused ? "watch.status.paused" : "watch.status.playing"),
        body: {
          kind: "loaded",
          name: player.file.name,
          duration:
            player.file.durationSeconds == null
              ? null
              : formatDuration(player.file.durationSeconds),
        },
        compatibility: compatibilityFor(watch, snapshot, t),
        fileAction: t("watch.file.change"),
        // Readiness goes to the server; with no connection there is nobody to
        // tell, so the switch waits rather than flipping and meaning nothing.
        ready: { checked: watch.ready, enabled: connected },
      };

    case "closed":
      return {
        ...none,
        tape: "STOP",
        tone: "neutral",
        status: t("watch.status.closed"),
        body: { kind: "closed" },
        reopen: true,
      };

    case "failed":
      return {
        ...none,
        tape: "STOP",
        tone: "bad",
        status: t("watch.status.failed"),
        body: { kind: "failed", message: player.message },
        reopen: true,
      };
  }
}

/**
 * The room this machine is in, out of everything the server listed.
 *
 * Found by this machine's own entry first: the server may shorten a long room
 * name, but it never lists anyone in a room other than the one they are in.
 */
export function roomOf(
  watch: WatchState,
  snapshot: RoomSnapshot | null,
): RoomView | null {
  if (!snapshot?.connected) return null;

  const byOwnEntry = watch.username
    ? snapshot.rooms.find((room) =>
        room.watchers.some((watcher) => watcher.name === watch.username),
      )
    : undefined;

  return byOwnEntry ?? snapshot.rooms.find((room) => room.name === watch.room) ?? null;
}

function compatibilityFor(
  watch: WatchState,
  snapshot: RoomSnapshot | null,
  t: Translate,
): WatchView["compatibility"] {
  const room = roomOf(watch, snapshot);
  if (!room) return null;

  // On its own a file always "matches", which would be a strange thing to
  // congratulate anyone on.
  if (room.watchers.length <= 1) {
    return { tone: "neutral", label: t("watch.file.alone") };
  }

  switch (room.fileCompatibility) {
    case "exact":
      return { tone: "good", label: t("watch.file.same") };
    case "durationMatch":
      return { tone: "good", label: t("watch.file.durationMatch") };
    case "mismatch":
      return { tone: "warn", label: t("watch.file.mismatch") };
    case "waiting":
      return { tone: "neutral", label: t("watch.file.waiting") };
  }
}

/**
 * A watch transition worth a toast, or nothing.
 *
 * Only the connection dropping and coming back: the card already shows every
 * state, but it is not on screen from Settings, and a party that has quietly
 * gone away is the one thing a viewer needs to hear about wherever they are.
 */
export function watchToastMessage(
  previous: WatchState | null,
  next: WatchState | null,
  t: Translate,
): string | null {
  const was = previous?.connection.state;
  const now = next?.connection.state;

  if (was === "connected" && now === "reconnecting") return t("watch.toast.lost");
  if (was === "reconnecting" && now === "connected") return t("watch.toast.reconnected");
  return null;
}
