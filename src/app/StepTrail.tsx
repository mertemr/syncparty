import { useTranslate, type MessageKey } from "@/shared/i18n";
import { cx } from "@/shared/ui";

/** The three things that happen between opening the app and watching a film. */
export type Step = "mode" | "setup" | "party";

const ORDER: Step[] = ["mode", "setup", "party"];

const LABELS: Record<Step, MessageKey> = {
  mode: "nav.step.mode",
  setup: "nav.step.setup",
  party: "nav.step.party",
};

const CIRCLES = {
  current: "bg-accent text-accent-ink",
  done: "border border-accent/40 bg-accent/12 text-accent",
  todo: "border border-line text-ink-faint",
} as const;

const LABELS_TONE = {
  current: "text-ink",
  done: "text-ink-muted",
  todo: "text-ink-faint",
} as const;

/**
 * Where you are in getting to a film.
 *
 * Deliberately not clickable: the header's back button already moves between
 * steps, and a second control that can only go one direction would be a
 * worse version of it.
 */
export function StepTrail({ current }: { current: Step }) {
  const t = useTranslate();
  const position = ORDER.indexOf(current);

  return (
    <nav
      aria-label={t("nav.steps")}
      className="flex shrink-0 items-center gap-2 border-b border-line/40 bg-canvas/35 px-6 py-2.5 backdrop-blur-xl"
    >
      {ORDER.map((step, index) => {
        const state =
          index === position ? "current" : index < position ? "done" : "todo";

        return (
          <div key={step} className="flex items-center gap-2">
            {index > 0 && (
              <span
                aria-hidden
                className={cx(
                  "h-px w-5",
                  state === "todo" ? "bg-line/70" : "bg-accent/50",
                )}
              />
            )}

            <span
              aria-current={state === "current" ? "step" : undefined}
              className={cx(
                "flex items-center gap-1.5 text-xs font-semibold transition-colors",
                LABELS_TONE[state],
              )}
            >
              <span
                aria-hidden
                className={cx(
                  "grid size-5 place-items-center rounded-full text-[10px] transition-colors",
                  CIRCLES[state],
                )}
              >
                {index + 1}
              </span>
              {t(LABELS[step])}
            </span>
          </div>
        );
      })}
    </nav>
  );
}
