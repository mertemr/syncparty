import { useCallback, useState } from "react";

import { useTranslate } from "@/shared/i18n";
import type { AppMode } from "@/shared/types/AppMode";

import { SystemStrip } from "./SystemStrip";
import type { StripState } from "./stripState";

/**
 * The opening surface: pick a side, with the setup check underneath.
 *
 * The check used to be a step of its own, which meant everyone walked past a
 * screen that had nothing to tell them. Here it only takes space when it has
 * something to say, and the slots stay shut until it does not.
 */
export function LaunchScreen({
  onChoose,
}: {
  onChoose: (mode: AppMode) => void;
}) {
  const t = useTranslate();
  // The strip has to check against *some* mode, and host is the stricter of
  // the two — so a guest is never blocked by a tool only a host needs.
  const [strip, setStrip] = useState<StripState>("checking");

  const blocked = strip === "blocked";

  const handleStripState = useCallback((next: StripState) => {
    setStrip(next);
  }, []);

  return (
    <div className="flex min-h-full flex-col">
      <div className="mx-auto flex w-full max-w-3xl flex-1 flex-col justify-center px-8 py-10">
        <div className="mb-8 flex items-center gap-3 font-mono text-[11px] tracking-[0.24em] text-accent uppercase">
          <span aria-hidden className="h-px w-8 bg-accent/60" />
          {t("onboarding.eyebrow")}
        </div>

        <h1 className="chroma font-display text-4xl font-extrabold tracking-[-0.03em] text-ink [font-stretch:115%]">
          {t("onboarding.title")}
        </h1>
        <p className="mt-3 max-w-xl text-sm leading-relaxed text-ink-muted">
          {t("onboarding.subtitle")}
        </p>

        <div className="mt-9 grid gap-4 sm:grid-cols-2">
          <Slot
            kind="host"
            title={t("onboarding.host.title")}
            detail={t("onboarding.host.detail")}
            disabled={blocked}
            onClick={() => onChoose("host")}
          />
          <Slot
            kind="guest"
            title={t("onboarding.guest.title")}
            detail={t("onboarding.guest.detail")}
            disabled={blocked}
            onClick={() => onChoose("guest")}
          />
        </div>
      </div>

      <SystemStrip mode="host" onStateChange={handleStripState} />
    </div>
  );
}

/**
 * One side of the choice, shaped like a cassette bay.
 *
 * `REC` and `PLAY` are deliberately untranslated: they are the labels printed
 * on the hardware this borrows from, and they read the same in every language
 * the app ships.
 */
function Slot({
  kind,
  title,
  detail,
  disabled,
  onClick,
}: {
  kind: "host" | "guest";
  title: string;
  detail: string;
  disabled: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      className="group relative overflow-hidden rounded-panel border border-line bg-surface/80 p-6 text-left transition-colors duration-[var(--duration-base)] hover:border-accent/70 hover:bg-surface-raised/80 disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:border-line disabled:hover:bg-surface/80"
    >
      {/* The slot mouth. Nothing but a line, until you hover it. */}
      <span
        aria-hidden
        className="absolute inset-x-6 top-0 h-0.5 bg-line transition-colors group-hover:bg-accent group-disabled:bg-line"
      />
      <span
        aria-hidden
        className="font-mono text-[10px] tracking-[0.22em] text-ink-faint uppercase"
      >
        {kind === "host" ? "REC" : "PLAY"}
      </span>
      <h2 className="mt-4 font-display text-lg font-extrabold tracking-tight text-ink [font-stretch:110%]">
        {title}
      </h2>
      <p className="mt-2 text-sm leading-relaxed text-ink-muted">{detail}</p>
    </button>
  );
}
