import { useTranslate, type MessageKey } from "@/shared/i18n";
import { Badge, Dot, type BadgeTone } from "@/shared/ui";
import type { WatcherView } from "@/shared/types/WatcherView";

import { getChannelStatus, type ChannelStatus } from "./channels";

const TONES: Record<ChannelStatus, BadgeTone> = {
  ready: "good",
  waiting: "warn",
  noFile: "neutral",
  trackingError: "bad",
};

const LABELS: Record<ChannelStatus, MessageKey> = {
  ready: "party.channel.ready",
  waiting: "party.channel.waiting",
  noFile: "party.channel.noFile",
  trackingError: "party.channel.trackingError",
};

/** One person in the room: who, what they have open, and whether it lines up. */
export function ChannelRow({
  watcher,
  filesCompatible,
}: {
  watcher: WatcherView;
  filesCompatible: boolean;
}) {
  const t = useTranslate();
  const status = getChannelStatus(watcher, filesCompatible);

  return (
    <li className="flex items-center gap-3 border-b border-line/50 py-2.5 last:border-b-0">
      <Dot tone={TONES[status]} />

      <div className="min-w-0 flex-1">
        <p className="truncate text-sm font-medium text-ink">
          {watcher.name}
          {watcher.isController && (
            <span aria-hidden className="ml-1.5 text-accent">
              ★
            </span>
          )}
        </p>
        <p className="truncate font-mono text-[11px] text-ink-faint">
          {watcher.file?.name ?? "—"}
        </p>
      </div>

      <Badge tone={TONES[status]}>{t(LABELS[status])}</Badge>
    </li>
  );
}
