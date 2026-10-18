import { useCallback, useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";

import { AppStateProvider, useAppState } from "@/app/AppState";
import { leaveReason, type LeaveReason } from "@/app/leaveReason";
import { GuestScreen } from "@/features/guest/GuestScreen";
import { guestJoinInFlight } from "@/features/guest/guestJoin";
import { leaveGuestParty, useGuestJoin } from "@/features/guest/useGuestJoin";
import { HostScreen } from "@/features/host/HostScreen";
import { LaunchScreen } from "@/features/launch/LaunchScreen";
import { SystemStrip } from "@/features/launch/SystemStrip";
import type { StripState } from "@/features/launch/stripState";
import { SettingsScreen } from "@/features/settings/SettingsScreen";
import {
  TranslationProvider,
  useTranslate,
  type MessageKey,
} from "@/shared/i18n";
import { useAppUpdate } from "@/shared/hooks/useAppUpdate";
import { ipc } from "@/shared/ipc";
import { Badge, Button, Rewind, ToastHost, Wordmark } from "@/shared/ui";
import type { AppMode } from "@/shared/types/AppMode";

/** Where a package-managed install is pointed for release notes. */
const RELEASES_URL = "https://github.com/mertemr/syncparty/releases/latest";

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

/** The ways stepping back can break something, each with its own words. */
const LEAVE_PROMPTS = {
  hosting: {
    title: "nav.leaveHosting.title",
    detail: "nav.leaveHosting.detail",
    confirm: "nav.leaveHosting.confirm",
  },
  joining: {
    title: "nav.leaveJoining.title",
    detail: "nav.leaveJoining.detail",
    confirm: "nav.leaveJoining.confirm",
  },
  watching: {
    title: "nav.leaveWatching.title",
    detail: "nav.leaveWatching.detail",
    confirm: "nav.leaveWatching.confirm",
  },
} satisfies Record<LeaveReason, Record<"title" | "detail" | "confirm", MessageKey>>;

function Shell() {
  const t = useTranslate();
  const {
    settings,
    patchSettings,
    pendingInvite,
    reportFailure,
    session,
    watch,
    failure,
    dismissFailure,
  } = useAppState();

  const [showSettings, setShowSettings] = useState(false);
  const [confirmingLeave, setConfirmingLeave] = useState<LeaveReason | null>(null);
  // A leave sent mid-join waits for the join to land before undoing it, so
  // the prompt can sit there a while; it must not take a second click.
  const [leaving, setLeaving] = useState(false);
  const guestJoin = useGuestJoin();
  // Set when the user steps back to the launch surface. The stored mode stays
  // as their last answer — `SettingsPatch` cannot clear it — but the slots
  // come back, so a mistaken pick is one click to undo.
  const [atLaunch, setAtLaunch] = useState(false);

  // An invite arriving by link means the user is a guest tonight, whatever
  // they picked last time — and settles the mode question outright.
  useEffect(() => {
    if (!pendingInvite || !settings) return;

    setAtLaunch(false);
    if (settings.mode !== "guest") {
      void patchSettings({ mode: "guest" }).catch(reportFailure);
    }
  }, [pendingInvite, settings, patchSettings, reportFailure]);

  const chooseMode = (mode: AppMode) => {
    setAtLaunch(false);
    void patchSettings({ mode }).catch(reportFailure);
  };

  const mode = atLaunch ? null : (settings?.mode ?? null);

  // A party screen reached without the launch screen — a saved mode, an
  // invite link — never ran the setup check. When joining or watching then
  // fails for want of mpv, the check comes to the party screen instead, with
  // its Install and Locate, and goes again once mpv is there.
  const [settingUp, setSettingUp] = useState(false);
  useEffect(() => {
    if (failure?.kind === "dependency_missing" && mode !== null) {
      setSettingUp(true);
    }
  }, [failure, mode]);
  const missingPlayer = failure?.kind === "dependency_missing";
  const handleSetupState = useCallback(
    (state: StripState) => {
      if (state !== "ready") return;
      setSettingUp(false);
      // The banner said mpv was missing; it is not any more.
      if (missingPlayer) dismissFailure();
    },
    [missingPlayer, dismissFailure],
  );

  const canGoBack = settings !== null && !showSettings && mode !== null;

  function stepBack() {
    setConfirmingLeave(null);
    setAtLaunch(true);
  }

  /**
   * A step back that would break something asks first — which ones, and
   * why, is `leaveReason`'s to say. Confirming ends what it named, then
   * steps back: said once, then done.
   */
  function goBack() {
    const reason = leaveReason({ mode, session, guestJoin, watch });
    if (reason) setConfirmingLeave(reason);
    else stepBack();
  }

  async function leaveAndGoBack(reason: LeaveReason) {
    if (leaving) return;

    setLeaving(true);
    try {
      if (reason === "hosting") {
        await ipc.stopHosting();
      } else {
        // The same as the watch card's Leave: a party left on purpose is not
        // one to reopen on the next launch.
        await leaveGuestParty();
      }
    } catch (error) {
      reportFailure(error);
      setConfirmingLeave(null);
      return;
    } finally {
      setLeaving(false);
    }
    stepBack();
  }

  return (
    <div className="crt-scan crt-vignette relative flex h-full flex-col">
      <Header
        mode={mode}
        settingsOpen={showSettings}
        canGoBack={canGoBack}
        onBack={goBack}
        onToggleSettings={() => setShowSettings((open) => !open)}
      />

      {confirmingLeave && (
        <LeavePrompt
          reason={confirmingLeave}
          busy={leaving}
          onConfirm={() => void leaveAndGoBack(confirmingLeave)}
          onCancel={() => setConfirmingLeave(null)}
        />
      )}

      {/* Above the loading state on purpose: if settings fail to load, the
          reason has to be visible rather than hidden behind a spinner that
          never resolves. */}
      <FailureBanner />
      {settingUp && mode !== null && !showSettings && (
        <SystemStrip mode={mode} onStateChange={handleSetupState} />
      )}
      <UpdateBanner />

      <main className="min-h-0 flex-1 overflow-y-auto scroll-smooth">
        {!settings ? (
          <div className="mx-auto max-w-sm px-8 py-16">
            <Rewind label={t("common.loading")} />
          </div>
        ) : showSettings ? (
          <SettingsScreen />
        ) : mode === null ? (
          <LaunchScreen onChoose={chooseMode} />
        ) : mode === "host" ? (
          <HostScreen />
        ) : (
          <GuestScreen />
        )}
      </main>

      <ToastHost />
    </div>
  );
}

/**
 * The one confirmation in the app, worded for whichever step back asked.
 *
 * A strip rather than a modal: it matches the other two things that interrupt
 * from the top of the window, and a modal layer exists nowhere else here.
 */
function LeavePrompt({
  reason,
  busy,
  onConfirm,
  onCancel,
}: {
  reason: LeaveReason;
  busy: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const t = useTranslate();
  const words = LEAVE_PROMPTS[reason];

  return (
    <div className="shrink-0 border-b border-warn/40 bg-warn/10 px-5 py-3">
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <p className="font-mono text-[11px] tracking-[0.16em] text-warn uppercase">
            {t(words.title)}
          </p>
          <p className="mt-0.5 text-xs text-ink-muted">
            {t(words.detail)}
          </p>
        </div>

        <div className="flex shrink-0 items-center gap-2">
          <Button variant="ghost" disabled={busy} onClick={onCancel}>
            {t("common.cancel")}
          </Button>
          <Button variant="danger" disabled={busy} onClick={onConfirm}>
            {t(words.confirm)}
          </Button>
        </div>
      </div>
    </div>
  );
}

function Header({
  mode,
  settingsOpen,
  canGoBack,
  onBack,
  onToggleSettings,
}: {
  mode: AppMode | null;
  settingsOpen: boolean;
  canGoBack: boolean;
  onBack: () => void;
  onToggleSettings: () => void;
}) {
  const t = useTranslate();

  return (
    <header className="z-10 flex shrink-0 items-center justify-between border-b border-line/60 bg-canvas/55 px-6 py-4 backdrop-blur-2xl">
      <div className="flex items-center gap-3">
        <Wordmark />

        {canGoBack && (
          <>
            <span aria-hidden className="h-5 w-px bg-line/70" />
            <Button variant="ghost" onClick={onBack} className="px-3">
              <svg
                viewBox="0 0 24 24"
                className="size-4 fill-none stroke-current"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
                aria-hidden
              >
                <path d="M15 5.5 8.5 12l6.5 6.5" />
              </svg>
              {t("common.back")}
            </Button>
          </>
        )}

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
    player_failed: "error.player_failed",
    sync_rejected: "error.sync_rejected",
    sync_failed: "error.sync_failed",
    not_watching: "error.not_watching",
  };
  const headline = knownKeys[failure.kind];

  return (
    // An error is exactly the moment a tape would skip.
    <div className="tracking-glitch shrink-0 border-b border-bad/40 bg-bad/10 px-5 py-3">
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <p className="font-mono text-[11px] tracking-[0.16em] text-bad uppercase">
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
 * The restart button is withheld while a party is running or being watched —
 * installing replaces the running binary and relaunches it, which would take
 * the server down along with everyone still watching, or close the player
 * syncparty is keeping in step. A guest join still connecting counts as
 * watching: it has no watch state yet, but it will open the player the
 * moment the party answers. Once hosting and watching both stop, the button
 * appears on its own; nothing needs to be re-triggered manually.
 */
function UpdateBanner() {
  const t = useTranslate();
  const { session, watch } = useAppState();
  const guestJoin = useGuestJoin();
  const { state, install } = useAppUpdate();

  if (state.status === "packageManaged") {
    return <PackageManagedBanner version={state.version} />;
  }

  if (state.status !== "ready") return null;

  const hosting = session.phase === "hosting";
  const watching = watch !== null || guestJoinInFlight(guestJoin);
  const notice = hosting
    ? t("update.hostingNotice")
    : watching
      ? t("update.watchingNotice")
      : null;

  return (
    <div className="shrink-0 border-b border-accent/40 bg-accent/10 px-5 py-3">
      <div className="flex items-center justify-between gap-4">
        <div className="min-w-0">
          <p className="font-mono text-[11px] tracking-[0.16em] text-accent uppercase">
            {t("update.title")} — v{state.version}
          </p>
          {notice && (
            <p className="mt-0.5 text-xs text-ink-muted">{notice}</p>
          )}
        </div>

        {!notice && (
          <Button variant="primary" onClick={() => void install()}>
            {t("update.restart")}
          </Button>
        )}
      </div>
    </div>
  );
}

/**
 * The Linux banner: says a version exists and stops there.
 *
 * No package-manager command is printed. Working out which one this machine
 * uses means reading `/etc/os-release` and guessing, and a wrong command is
 * worse than no command — the release page is correct everywhere.
 */
function PackageManagedBanner({ version }: { version: string }) {
  const t = useTranslate();

  return (
    <div className="shrink-0 border-b border-accent/40 bg-accent/10 px-5 py-3">
      <div className="flex items-center justify-between gap-4">
        <div className="min-w-0">
          <p className="font-mono text-[11px] tracking-[0.16em] text-accent uppercase">
            {t("update.available")} — v{version}
          </p>
          <p className="mt-0.5 text-xs text-ink-muted">
            {t("update.packageManaged")}
          </p>
        </div>

        <Button onClick={() => void openUrl(RELEASES_URL)}>
          {t("update.releaseNotes")}
        </Button>
      </div>
    </div>
  );
}
