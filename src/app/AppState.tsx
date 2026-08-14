/**
 * The one place backend state lands.
 *
 * Everything here is push-driven: settings are read once, and from then on the
 * backend's events are the only thing that moves the UI. Nothing polls.
 */
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";

import { errorMessage, ipc, onAppEvent } from "@/shared/ipc";
import type { AppEvent } from "@/shared/types/AppEvent";
import type { AppSettings } from "@/shared/types/AppSettings";
import type { DependencyId } from "@/shared/types/DependencyId";
import type { Invite } from "@/shared/types/Invite";
import type { RoomSnapshot } from "@/shared/types/RoomSnapshot";
import type { SessionState } from "@/shared/types/SessionState";
import type { SettingsPatch } from "@/shared/types/SettingsPatch";

/** How many server log lines to keep. Enough to diagnose a failed start. */
const LOG_LIMIT = 300;

export interface InstallProgress {
  stage: string;
  percent: number | null;
  detail: string | null;
}

export interface AppFailure {
  kind: string;
  message: string;
}

interface AppStateValue {
  settings: AppSettings | null;
  patchSettings: (patch: SettingsPatch) => Promise<void>;

  session: SessionState;
  room: RoomSnapshot | null;
  serverLog: string[];

  /** Keyed by dependency; present only while that install is running. */
  installs: Partial<Record<DependencyId, InstallProgress>>;

  /** Set when the app was opened through a `syncparty://` link. */
  pendingInvite: Invite | null;
  clearPendingInvite: () => void;

  failure: AppFailure | null;
  reportFailure: (error: unknown) => void;
  dismissFailure: () => void;
}

const AppStateContext = createContext<AppStateValue | null>(null);

export function AppStateProvider({ children }: { children: ReactNode }) {
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [session, setSession] = useState<SessionState>({ phase: "idle" });
  const [room, setRoom] = useState<RoomSnapshot | null>(null);
  const [serverLog, setServerLog] = useState<string[]>([]);
  const [installs, setInstalls] = useState<
    Partial<Record<DependencyId, InstallProgress>>
  >({});
  const [pendingInvite, setPendingInvite] = useState<Invite | null>(null);
  const [failure, setFailure] = useState<AppFailure | null>(null);

  // Held in a ref so the event handler never needs to be re-registered when
  // an unrelated piece of state changes.
  const handleEvent = useRef<(event: AppEvent) => void>(() => {});

  handleEvent.current = (event: AppEvent) => {
    switch (event.kind) {
      case "sessionChanged":
        setSession(event.state);
        if (event.state.phase === "starting") setServerLog([]);
        // A party that has stopped has no room to show.
        if (event.state.phase === "idle") setRoom(null);
        break;

      case "roomUpdated":
        setRoom(event.snapshot);
        break;

      case "serverLog":
        setServerLog((lines) =>
          [...lines, event.line].slice(-LOG_LIMIT),
        );
        break;

      case "installProgress":
        setInstalls((current) => ({
          ...current,
          [event.dependency]: {
            stage: event.stage,
            percent: event.percent,
            detail: event.detail,
          },
        }));
        break;

      case "inviteReceived":
        setPendingInvite(event.invite);
        break;

      case "failed":
        setFailure({ kind: event.errorKind, message: event.message });
        break;

      case "preflightCompleted":
        // The preflight screen owns this report; it asks for it directly.
        break;
    }
  };

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    void onAppEvent((event) => handleEvent.current(event)).then((stop) => {
      // Guards against strict mode's double mount leaving a live listener.
      if (cancelled) stop();
      else unlisten = stop;
    });

    void ipc.getSettings().then(setSettings).catch(reportInitialFailure);
    void ipc.sessionState().then(setSession).catch(reportInitialFailure);

    return () => {
      cancelled = true;
      unlisten?.();
    };

    function reportInitialFailure(error: unknown) {
      setFailure({ kind: "startup", message: errorMessage(error) });
    }
  }, []);

  const patchSettings = useCallback(async (patch: SettingsPatch) => {
    setSettings(await ipc.updateSettings(patch));
  }, []);

  const reportFailure = useCallback((error: unknown) => {
    setFailure({
      kind:
        typeof error === "object" && error !== null && "kind" in error
          ? String((error as { kind: unknown }).kind)
          : "other",
      message: errorMessage(error),
    });
  }, []);

  const value = useMemo<AppStateValue>(
    () => ({
      settings,
      patchSettings,
      session,
      room,
      serverLog,
      installs,
      pendingInvite,
      clearPendingInvite: () => setPendingInvite(null),
      failure,
      reportFailure,
      dismissFailure: () => setFailure(null),
    }),
    [
      settings,
      patchSettings,
      session,
      room,
      serverLog,
      installs,
      pendingInvite,
      failure,
      reportFailure,
    ],
  );

  return (
    <AppStateContext.Provider value={value}>
      {children}
    </AppStateContext.Provider>
  );
}

export function useAppState(): AppStateValue {
  const value = useContext(AppStateContext);
  if (!value) {
    throw new Error("useAppState must be used inside AppStateProvider");
  }
  return value;
}
