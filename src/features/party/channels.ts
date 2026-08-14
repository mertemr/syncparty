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
