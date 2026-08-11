import { useEffect, useState } from "react";

import { AppStateProvider, useAppState } from "@/app/AppState";
import { GuestScreen } from "@/features/guest/GuestScreen";
import { HostScreen } from "@/features/host/HostScreen";
import { ModeChooser } from "@/features/onboarding/ModeChooser";
import { Preflight } from "@/features/onboarding/Preflight";
import { SettingsScreen } from "@/features/settings/SettingsScreen";
import {
  TranslationProvider,
  useTranslate,
  type MessageKey,
} from "@/shared/i18n";
import { useAppUpdate } from "@/shared/hooks/useAppUpdate";
import { Badge, Button } from "@/shared/ui";
import type { AppMode } from "@/shared/types/AppMode";

export default function App() {
  return (
    <AppStateProvider>
      <Localised />
    </AppStateProvider>
  );
}

/** Sits inside the state provider so it can read the chosen language. */
function Localised() {
  const { settings } = useAppState();

  return (
    <TranslationProvider language={settings?.language ?? "en"}>
      <Shell />
    </TranslationProvider>
  );
}

function Shell() {
  const t = useTranslate();
  const { settings, patchSettings, pendingInvite, reportFailure } =
    useAppState();

  const [showSettings, setShowSettings] = useState(false);
  const [setupConfirmed, setSetupConfirmed] = useState(false);

  // An invite arriving by link means the user is a guest tonight, whatever
  // they picked last time.
  useEffect(() => {
    if (pendingInvite && settings && settings.mode !== "guest") {
      void patchSettings({ mode: "guest" }).catch(reportFailure);
    }
  }, [pendingInvite, settings, patchSettings, reportFailure]);

  const chooseMode = (mode: AppMode) => {
    setSetupConfirmed(false);
    void patchSettings({ mode }).catch(reportFailure);
  };

  return (
    <div className="relative flex h-full flex-col">
      <Header
        mode={settings?.mode ?? null}
        settingsOpen={showSettings}
        onToggleSettings={() => setShowSettings((open) => !open)}
      />

      {/* Above the loading state on purpose: if settings fail to load, the
          reason has to be visible rather than hidden behind a spinner that
          never resolves. */}
      <FailureBanner />
      <UpdateBanner />

      <main className="min-h-0 flex-1 overflow-y-auto scroll-smooth">
        {!settings ? (
          <p className="p-10 text-center text-sm text-ink-faint">
            {t("common.loading")}
          </p>
        ) : showSettings ? (
          <SettingsScreen />
        ) : settings.mode === null ? (
          <ModeChooser onChoose={chooseMode} />
        ) : !setupConfirmed ? (
          <Preflight
            mode={settings.mode}
            onReady={() => setSetupConfirmed(true)}
          />
        ) : settings.mode === "host" ? (
          <HostScreen />
        ) : (
          <GuestScreen />
        )}
      </main>
    </div>
  );
}

function Header({
  mode,
  settingsOpen,
  onToggleSettings,
}: {
  mode: AppMode | null;
  settingsOpen: boolean;
  onToggleSettings: () => void;
}) {
  const t = useTranslate();

  return (
    <header className="z-10 flex shrink-0 items-center justify-between border-b border-line/60 bg-canvas/55 px-6 py-4 backdrop-blur-2xl">
      <div className="flex items-center gap-3">
        <span aria-hidden className="relative grid size-9 place-items-center overflow-hidden rounded-xl bg-accent text-accent-ink shadow-[0_8px_28px_oklch(0.65_0.18_42/0.25)]">
          <svg viewBox="0 0 24 24" className="size-5 fill-current" aria-hidden>
            <path d="M8.2 6.1a1 1 0 0 1 1.52-.85l8.25 5.3a1 1 0 0 1 0 1.7l-8.25 5.3a1 1 0 0 1-1.52-.84V6.1Z" />
          </svg>
        </span>
        <span className="text-[15px] font-bold tracking-[-0.02em] text-ink">
          {t("appName")}
        </span>
        {mode && (
          <Badge tone="neutral">
            {t(mode === "host" ? "mode.host" : "mode.guest")}
          </Badge>
        )}
      </div>

      <Button variant="ghost" onClick={onToggleSettings} aria-pressed={settingsOpen}>
        <svg viewBox="0 0 24 24" className="size-4 fill-none stroke-current" strokeWidth="1.8" aria-hidden>
          <path d="M12 15.25A3.25 3.25 0 1 0 12 8.75a3.25 3.25 0 0 0 0 6.5Z" />
          <path d="M19.1 13.2a7.8 7.8 0 0 0 0-2.4l2-1.55-2-3.46-2.48 1a8.5 8.5 0 0 0-2.08-1.2L14.2 3h-4l-.34 2.6a8.5 8.5 0 0 0-2.08 1.2l-2.48-1-2 3.46 2 1.55a7.8 7.8 0 0 0 0 2.4l-2 1.55 2 3.46 2.48-1a8.5 8.5 0 0 0 2.08 1.2l.34 2.6h4l.34-2.6a8.5 8.5 0 0 0 2.08-1.2l2.48 1 2-3.46-2-1.55Z" />
        </svg>
        {settingsOpen ? t("common.close") : t("common.settings")}
      </Button>
    </header>
  );
}

/**
 * Failures that arrive outside a command call, so there was no `Result` to
 * surface them on. Known kinds get a translated headline; the rest fall back
 * to the message the backend wrote.
 */
function FailureBanner() {
  const t = useTranslate();
  const { failure, dismissFailure } = useAppState();

  if (!failure) return null;

  const knownKeys: Record<string, MessageKey> = {
    dependency_missing: "error.dependency_missing",
    endpoint_offline: "error.endpoint_offline",
    party_unreachable: "error.party_unreachable",
  };
  const headline = knownKeys[failure.kind];

  return (
    <div className="shrink-0 border-b border-warn/40 bg-warn/10 px-5 py-3">
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <p className="text-sm font-medium text-warn">
            {headline ? t(headline) : t("error.title")}
          </p>
          <p className="mt-0.5 text-xs break-words text-ink-muted">
            {failure.message}
          </p>
        </div>

        <div className="flex shrink-0 items-center gap-2">
          <Button variant="ghost" onClick={dismissFailure}>
            {t("common.close")}
          </Button>
        </div>
      </div>
    </div>
  );
}

/**
 * Tells the user a downloaded update is ready to install.
 *
 * The restart button is withheld while a party is running — installing
 * replaces the running binary and relaunches it, which would take the
 * Syncplay server down along with everyone still watching. Once hosting
 * stops (or on the guest side, where this never applies), the button appears
 * on its own; nothing needs to be re-triggered manually.
 */
function UpdateBanner() {
  const t = useTranslate();
  const { session } = useAppState();
  const { state, install } = useAppUpdate();

  if (state.status !== "ready") return null;

  const hosting = session.phase === "hosting";

  return (
    <div className="shrink-0 border-b border-accent/40 bg-accent/10 px-5 py-3">
      <div className="flex items-center justify-between gap-4">
        <div className="min-w-0">
          <p className="text-sm font-medium text-accent">
            {t("update.title")} — v{state.version}
          </p>
          {hosting && (
            <p className="mt-0.5 text-xs text-ink-muted">
              {t("update.hostingNotice")}
            </p>
          )}
        </div>

        {!hosting && (
          <Button variant="primary" onClick={() => void install()}>
            {t("update.restart")}
          </Button>
        )}
      </div>
    </div>
  );
}
