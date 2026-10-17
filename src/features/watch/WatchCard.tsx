import { useState } from "react";

import { useAppState } from "@/app/AppState";
import { VIDEO_EXTENSIONS } from "@/features/movie/fileTitle";
import { useTranslate } from "@/shared/i18n";
import { ipc, pickFile } from "@/shared/ipc";
import { Badge, Button, Card, Dot, Rewind, Toggle, cx } from "@/shared/ui";
import type { RoomSnapshot } from "@/shared/types/RoomSnapshot";
import type { WatchState } from "@/shared/types/WatchState";

import { describeWatch, type WatchTone } from "./watchState";

const NOTE_TONES: Record<WatchTone, string> = {
  neutral: "border-line bg-surface-raised/35 text-ink-muted",
  good: "border-good/35 bg-good/10 text-good",
  warn: "border-warn/40 bg-warn/10 text-warn",
  bad: "border-bad/40 bg-bad/10 text-bad",
};

/**
 * The party as this machine is watching it: whether the connection is up,
 * what the player has open, and the few things a viewer does about either.
 *
 * Shared by both sides. A guest and a host watch the same way — only where
 * the sync client connects differs — so the only thing the screens pass in
 * is how to stop: a guest leaves the party, a host just closes its player.
 */
export function WatchCard({
  watch,
  room,
  onLeave,
}: {
  watch: WatchState;
  room: RoomSnapshot | null;
  onLeave: () => Promise<unknown>;
}) {
  const t = useTranslate();
  const { reportFailure } = useAppState();
  const [busy, setBusy] = useState(false);

  const view = describeWatch(watch, room, t);

  async function run(action: () => Promise<unknown>) {
    setBusy(true);
    try {
      await action();
    } catch (error) {
      reportFailure(error);
    } finally {
      setBusy(false);
    }
  }

  async function chooseFile() {
    const path = await pickFile({
      title: t("watch.file.dialogTitle"),
      filters: [{ name: t("watch.file.filter"), extensions: VIDEO_EXTENSIONS }],
    });
    if (path) await ipc.openWatchFile(path);
  }

  return (
    <Card
      title={t("watch.title")}
      action={
        <Badge tone={view.tone}>
          <Dot tone={view.tone} />
          {view.status}
        </Badge>
      }
    >
      <div className="space-y-4">
        <div className="min-w-0">
          <p className="truncate font-display text-lg font-extrabold text-ink [font-stretch:110%]">
            {watch.room}
          </p>
          {watch.username && (
            <p className="font-mono text-xs break-words text-ink-faint">
              {t("watch.username")} {watch.username}
            </p>
          )}
        </div>

        {view.banner && (
          <div
            className={cx(
              "rounded-[var(--radius-control)] border p-3",
              NOTE_TONES[view.banner.tone],
            )}
          >
            <p className="text-sm font-medium">{view.banner.title}</p>
            {view.banner.detail && (
              <p className="mt-0.5 text-xs break-words text-ink-muted">
                {view.banner.detail}
              </p>
            )}
          </div>
        )}

        {view.loading && <Rewind label={view.loading} />}

        {view.body.kind === "idle" && (
          <p className="text-sm leading-relaxed text-ink-muted">
            {t("watch.file.hint")}
          </p>
        )}

        {view.body.kind === "loaded" && (
          <div className="min-w-0">
            {/* Release names run longer than any rail is wide; the tooltip
                is what makes the cut-off part readable. */}
            <p
              className="selectable truncate font-mono text-sm text-ink"
              title={view.body.name}
            >
              {view.body.name}
            </p>
            {view.body.duration && (
              <p className="mt-0.5 font-mono text-[11px] text-ink-faint">
                {view.body.duration}
              </p>
            )}
          </div>
        )}

        {view.compatibility && (
          <p
            className={cx(
              "rounded-[var(--radius-control)] border p-3 text-sm",
              NOTE_TONES[view.compatibility.tone],
            )}
          >
            {view.compatibility.label}
          </p>
        )}

        {view.body.kind === "closed" && (
          <p className="text-sm leading-relaxed text-ink-muted">
            {t("watch.player.closed")}
          </p>
        )}

        {view.body.kind === "failed" && (
          <div>
            <p className="text-sm font-medium text-bad">{t("watch.player.failed")}</p>
            <p className="mt-0.5 text-xs break-words text-ink-muted">
              {view.body.message}
            </p>
          </div>
        )}

        {view.ready && (
          <Toggle
            checked={view.ready.checked}
            label={t("watch.ready")}
            hint={t("watch.ready.hint")}
            disabled={busy || !view.ready.enabled}
            onChange={(ready) => void run(() => ipc.setReady(ready))}
          />
        )}

        <div className="space-y-2">
          {view.fileAction && (
            <Button
              variant={view.body.kind === "idle" ? "primary" : "secondary"}
              className="w-full"
              disabled={busy}
              onClick={() => void run(chooseFile)}
            >
              {view.fileAction}
            </Button>
          )}

          {view.reopen && (
            <Button
              variant="primary"
              className="w-full"
              disabled={busy}
              onClick={() => void run(ipc.reopenPlayer)}
            >
              {t("watch.player.reopen")}
            </Button>
          )}

          {/* Offered in every state, connecting included: a party that will
              not answer has to be something the viewer can walk away from. */}
          <Button
            variant="ghost"
            className="w-full"
            disabled={busy}
            onClick={() => void run(onLeave)}
          >
            {view.leave}
          </Button>
        </div>
      </div>
    </Card>
  );
}
