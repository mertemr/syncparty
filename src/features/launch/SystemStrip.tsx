import { useCallback, useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";

import { useAppState } from "@/app/AppState";
import { useTranslate, type Translate } from "@/shared/i18n";
import { errorMessage, ipc } from "@/shared/ipc";
import { Badge, Button, Choice, Dot, Rewind } from "@/shared/ui";
import type { AppMode } from "@/shared/types/AppMode";
import type { DependencyId } from "@/shared/types/DependencyId";
import type { PlayerChoice } from "@/shared/types/PlayerChoice";
import type { PreflightItem } from "@/shared/types/PreflightItem";
import type { PreflightReport } from "@/shared/types/PreflightReport";

import { getStripState, summariseReady, type StripState } from "./systemStrip";

/**
 * The setup check, as a strip under the launch slots rather than a screen.
 *
 * Runs on every visit rather than caching a "setup done" flag, because the
 * things it checks for can be uninstalled between launches. When there is
 * nothing to report it collapses to a single line — the check has to happen,
 * but on a healthy machine it does not deserve a whole step.
 */
export function SystemStrip({
  mode,
  onStateChange,
}: {
  mode: AppMode;
  onStateChange: (state: StripState) => void;
}) {
  const t = useTranslate();
  const { installs, reportFailure } = useAppState();

  const [report, setReport] = useState<PreflightReport | null>(null);
  const [checking, setChecking] = useState(true);
  const [installing, setInstalling] = useState<DependencyId | null>(null);
  const [locateErrors, setLocateErrors] = useState<
    Partial<Record<DependencyId, string>>
  >({});
  // Not persisted: it decides what this click downloads and nothing else.
  const [playerChoice, setPlayerChoice] = useState<PlayerChoice>("mpv");

  const check = useCallback(async () => {
    setChecking(true);
    try {
      setReport(await ipc.runPreflight(mode));
    } catch (error) {
      reportFailure(error);
    } finally {
      setChecking(false);
    }
  }, [mode, reportFailure]);

  useEffect(() => {
    void check();
  }, [check]);

  const state = getStripState(report);

  useEffect(() => {
    onStateChange(state);
  }, [state, onStateChange]);

  async function install(id: DependencyId) {
    setInstalling(id);
    try {
      await ipc.installDependency(id, id === "mpv" ? playerChoice : undefined);
    } catch (error) {
      reportFailure(error);
    } finally {
      setInstalling(null);
      // Re-check rather than assuming: the install may have half-worked.
      await check();
    }
  }

  /**
   * Asks the user where a program is.
   *
   * A file picker, not a folder one — Tauri's dialog is one or the other, and
   * a portable build is reachable either way: the user opens the folder and
   * picks the executable inside it. The backend accepts a folder too, for
   * paths that arrive from somewhere other than this dialog.
   */
  async function locate(id: DependencyId, displayName: string) {
    const chosen = await open({
      title: `${displayName} — ${t("preflight.locate.title")}`,
      multiple: false,
      directory: false,
    });

    if (typeof chosen !== "string") return;
    await applyPath(id, chosen);
  }

  async function applyPath(id: DependencyId, path: string | null) {
    setLocateErrors((current) => ({ ...current, [id]: undefined }));

    try {
      await ipc.setDependencyPath(id, path);
    } catch (error) {
      // A rejected path is the user's mistake to correct, not an app-level
      // failure, so it stays next to the row rather than in the top banner.
      setLocateErrors((current) => ({
        ...current,
        [id]: errorMessage(error),
      }));
    } finally {
      await check();
    }
  }

  return (
    <div className="shrink-0 border-t border-line bg-surface/60 px-6 py-3">
      {state === "checking" ? (
        <Rewind label={t("system.checking")} />
      ) : (
        <>
          <div className="flex items-center gap-3">
            <Dot tone={state === "ready" ? "good" : "warn"} />
            <p className="min-w-0 flex-1 truncate font-mono text-[11px] tracking-[0.14em] text-ink-faint uppercase">
              {state === "ready" && report
                ? `${t("system.ready")} — ${summariseReady(report)}`
                : t("system.blocked")}
            </p>
            <Button
              variant="ghost"
              onClick={() => void check()}
              disabled={checking || installing !== null}
            >
              {checking ? t("system.checking") : t("system.recheck")}
            </Button>
          </div>

          {state === "blocked" && (
            <ul className="mt-2 divide-y divide-line/60">
              {report?.items
                .filter((item) => item.status.state === "missing")
                .map((item) => (
                  <DependencyRow
                    key={item.id}
                    item={item}
                    busy={installing === item.id}
                    progress={installs[item.id]?.stage}
                    disabled={installing !== null}
                    locateError={locateErrors[item.id] ?? null}
                    playerChoice={item.id === "mpv" ? playerChoice : null}
                    onPlayerChoice={setPlayerChoice}
                    onInstall={() => void install(item.id)}
                    onLocate={() => void locate(item.id, item.displayName)}
                    onForgetPath={() => void applyPath(item.id, null)}
                  />
                ))}
            </ul>
          )}
        </>
      )}
    </div>
  );
}

function DependencyRow({
  item,
  busy,
  progress,
  disabled,
  playerChoice,
  onPlayerChoice,
  onInstall,
  onLocate,
  onForgetPath,
  locateError,
}: {
  item: PreflightItem;
  busy: boolean;
  progress: string | undefined;
  disabled: boolean;
  playerChoice: PlayerChoice | null;
  onPlayerChoice: (choice: PlayerChoice) => void;
  onInstall: () => void;
  onLocate: () => void;
  onForgetPath: () => void;
  locateError: string | null;
}) {
  const t = useTranslate();
  const installed = item.status.state === "installed";

  return (
    <li className="py-2.5">
      <div className="flex items-center gap-3">
        <Dot tone={installed ? "good" : "warn"} />

        <div className="min-w-0 flex-1">
          <p className="truncate text-sm font-medium text-ink">
            {item.displayName}
          </p>
          <p className="truncate text-xs text-ink-faint">
            {detailFor(item, busy, progress, t)}
          </p>
        </div>

        {installed ? (
          <Badge tone="good">{t("preflight.installed")}</Badge>
        ) : (
          <div className="flex items-center gap-2">
            {playerChoice && item.canAutoInstall && (
              <Choice
                ariaLabel={t("preflight.player")}
                value={playerChoice}
                options={[
                  { value: "mpv", label: "mpv" },
                  { value: "vlc", label: "VLC" },
                ]}
                onChange={(choice) => onPlayerChoice(choice as PlayerChoice)}
                disabled={disabled}
              />
            )}

            {/* Offered alongside installing, not instead of it: a portable
                build already on disk is quicker than a download, and the
                user is the only one who knows where they put it. */}
            {item.supportsManualPath && (
              <Button variant="ghost" onClick={onLocate} disabled={disabled}>
                {t("preflight.locate")}
              </Button>
            )}

            {item.canAutoInstall ? (
              <Button variant="primary" onClick={onInstall} disabled={disabled}>
                {busy ? t("preflight.installing") : t("preflight.install")}
              </Button>
            ) : (
              <Button onClick={() => void openUrl(item.manualUrl)}>
                {t("preflight.manual")}
              </Button>
            )}
          </div>
        )}
      </div>

      {item.overridePath && (
        <div className="mt-2 flex items-center gap-2 pl-5">
          <p className="min-w-0 flex-1 truncate font-mono text-xs text-ink-faint">
            {item.overridePath}
          </p>
          <Button variant="ghost" onClick={onForgetPath} disabled={disabled}>
            {t("preflight.clearPath")}
          </Button>
        </div>
      )}

      {locateError && <p className="mt-2 pl-5 text-xs text-bad">{locateError}</p>}
    </li>
  );
}

/**
 * The line under a dependency's name.
 *
 * For something already installed this is the version, and an empty string
 * when the tool would not report one — the badge beside it already says
 * "Ready", so repeating that word here would be noise.
 */
function detailFor(
  item: PreflightItem,
  busy: boolean,
  progress: string | undefined,
  t: Translate,
): string {
  if (busy) return progress ?? t("preflight.installing");

  if (item.status.state === "installed") return item.status.version ?? "";

  if (!item.canAutoInstall) return t("preflight.noAutoInstall");

  return item.needsElevation
    ? t("preflight.elevation")
    : t("preflight.missing");
}
