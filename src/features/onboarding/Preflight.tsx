import { useCallback, useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";

import { useAppState } from "@/app/AppState";
import { useTranslate, type Translate } from "@/shared/i18n";
import { errorMessage, ipc } from "@/shared/ipc";
import { Badge, Button, Card, Dot, PageHeader } from "@/shared/ui";
import type { AppMode } from "@/shared/types/AppMode";
import type { DependencyId } from "@/shared/types/DependencyId";
import type { PreflightItem } from "@/shared/types/PreflightItem";
import type { PreflightReport } from "@/shared/types/PreflightReport";

/**
 * The setup checklist.
 *
 * Runs on every visit rather than caching a "setup done" flag, because the
 * things it checks for can be uninstalled between launches.
 */
export function Preflight({
  mode,
  onReady,
}: {
  mode: AppMode;
  onReady: () => void;
}) {
  const t = useTranslate();
  const { installs, reportFailure } = useAppState();

  const [report, setReport] = useState<PreflightReport | null>(null);
  const [checking, setChecking] = useState(true);
  const [installing, setInstalling] = useState<DependencyId | null>(null);
  const [locateErrors, setLocateErrors] = useState<
    Partial<Record<DependencyId, string>>
  >({});

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

  async function install(id: DependencyId) {
    setInstalling(id);
    try {
      await ipc.installDependency(id);
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

  const satisfied =
    report !== null && report.items.every((item) => item.status.state !== "missing");

  return (
    <div className="mx-auto max-w-3xl space-y-6 px-8 py-10">
      <PageHeader title={t("preflight.title")} description={t("preflight.subtitle")} />

      <Card>
        {checking && !report ? (
          <p className="py-6 text-center text-sm text-ink-faint">
            {t("preflight.checking")}
          </p>
        ) : (
          <ul className="divide-y divide-line">
            {report?.items.map((item) => (
              <DependencyRow
                key={item.id}
                item={item}
                busy={installing === item.id}
                progress={installs[item.id]?.stage}
                disabled={installing !== null}
                locateError={locateErrors[item.id] ?? null}
                onInstall={() => void install(item.id)}
                onLocate={() => void locate(item.id, item.displayName)}
                onForgetPath={() => void applyPath(item.id, null)}
              />
            ))}
          </ul>
        )}
      </Card>

      <div className="flex items-center justify-between gap-3">
        <Button
          variant="ghost"
          onClick={() => void check()}
          disabled={checking || installing !== null}
        >
          {checking ? t("preflight.checking") : t("preflight.recheck")}
        </Button>

        <div className="flex items-center gap-3">
          {satisfied && (
            <span className="text-sm text-good">{t("preflight.allReady")}</span>
          )}
          <Button variant="primary" onClick={onReady} disabled={!satisfied}>
            {t("preflight.continue")}
          </Button>
        </div>
      </div>
    </div>
  );
}

function DependencyRow({
  item,
  busy,
  progress,
  disabled,
  onInstall,
  onLocate,
  onForgetPath,
  locateError,
}: {
  item: PreflightItem;
  busy: boolean;
  progress: string | undefined;
  disabled: boolean;
  onInstall: () => void;
  onLocate: () => void;
  onForgetPath: () => void;
  locateError: string | null;
}) {
  const t = useTranslate();
  const installed = item.status.state === "installed";

  return (
    <li className="py-3 first:pt-0 last:pb-0">
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

      {locateError && (
        <p className="mt-2 pl-5 text-xs text-bad">{locateError}</p>
      )}
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
