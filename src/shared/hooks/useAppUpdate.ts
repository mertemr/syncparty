/**
 * Checks for a new release on startup and downloads it in the background.
 *
 * Deliberately does not install and relaunch on its own — doing that while a
 * party is running would kill the host's Syncplay server out from under
 * everyone still watching. Downloading ahead of time is silent and has no
 * downside, so it happens automatically; the actual restart is always an
 * explicit action, and the caller is expected to gate it on
 * `session.phase !== "hosting"` (see `UpdateBanner`).
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

import { insideTauri } from "@/shared/ipc";

export type UpdateState =
  | { status: "idle" }
  | { status: "downloading"; version: string; percent: number | null }
  | { status: "ready"; version: string; body: string | null }
  | { status: "installing" };

export function useAppUpdate() {
  const [state, setState] = useState<UpdateState>({ status: "idle" });
  const pending = useRef<Update | null>(null);

  useEffect(() => {
    let cancelled = false;

    // Outside the Tauri shell the updater plugin throws synchronously, before
    // there is a promise to catch — so this is a guard rather than a `catch`.
    if (!insideTauri) return;

    void (async () => {
      // A failed check (offline, GitHub unreachable, no releases yet) is not
      // worth bothering the user with — it just tries again next launch.
      const update = await check().catch(() => null);
      if (!update || cancelled) return;

      pending.current = update;
      setState({ status: "downloading", version: update.version, percent: null });

      let total = 0;
      let downloaded = 0;

      await update
        .download((event) => {
          if (cancelled) return;

          if (event.event === "Started") {
            total = event.data.contentLength ?? 0;
          } else if (event.event === "Progress") {
            downloaded += event.data.chunkLength;
            setState({
              status: "downloading",
              version: update.version,
              // Some servers do not report a content length; the bar just
              // stays indeterminate rather than showing a wrong percentage.
              percent: total > 0 ? Math.round((downloaded / total) * 100) : null,
            });
          }
        })
        .catch(() => {
          // A failed download just means no banner appears; the next launch
          // tries the whole thing again from scratch.
          pending.current = null;
        });

      if (!cancelled && pending.current) {
        setState({ status: "ready", version: update.version, body: update.body ?? null });
      }
    })();

    return () => {
      cancelled = true;
    };
  }, []);

  const install = useCallback(async () => {
    const update = pending.current;
    if (!update) return;

    setState({ status: "installing" });
    await update.install();
    await relaunch();
  }, []);

  return { state, install };
}
