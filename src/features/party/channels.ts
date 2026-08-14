import type { WatcherView } from "@/shared/types/WatcherView";

/** What one row in the channel list is saying. */
export type ChannelStatus = "ready" | "waiting" | "noFile" | "trackingError";

export function getChannelStatus(
  watcher: WatcherView,
  filesCompatible: boolean,
): ChannelStatus {
  if (watcher.file == null) return "noFile";
  if (!filesCompatible) return "trackingError";

  return watcher.isReady ? "ready" : "waiting";
}

/**
 * Seconds to `h:mm:ss`, or `m:ss` for anything under an hour.
 *
 * Shown next to the filename because two different releases of the same film
 * rarely share a name but usually share a runtime — the duration is what makes
 * a "files match" claim believable.
 */
export function formatDuration(totalSeconds: number): string {
  const seconds = Math.max(0, Math.floor(totalSeconds));
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const remainder = seconds % 60;

  const padded = (value: number) => String(value).padStart(2, "0");

  return hours > 0
    ? `${hours}:${padded(minutes)}:${padded(remainder)}`
    : `${minutes}:${padded(remainder)}`;
}
