/**
 * Typed wrappers over the Tauri command surface.
 *
 * Every call the UI makes goes through here, so the argument and return types
 * are checked against the generated bindings in one place instead of being
 * re-stated at each call site.
 */
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";

import { devInvoke, devListen } from "./devBackend";
import type { AppEvent } from "@/shared/types/AppEvent";
import type { AppMode } from "@/shared/types/AppMode";
import type { AppSettings } from "@/shared/types/AppSettings";
import type { DependencyId } from "@/shared/types/DependencyId";
import type { DiagnosticsReport } from "@/shared/types/DiagnosticsReport";
import type { HostingInfo } from "@/shared/types/HostingInfo";
import type { Invite } from "@/shared/types/Invite";
import type { PlayerChoice } from "@/shared/types/PlayerChoice";
import type { PreflightReport } from "@/shared/types/PreflightReport";
import type { SessionState } from "@/shared/types/SessionState";
import type { SettingsPatch } from "@/shared/types/SettingsPatch";

/** Must match `ipc::EVENT_CHANNEL`. */
const EVENT_CHANNEL = "syncparty://event";

/**
 * `pnpm dev` serves the frontend without the Tauri shell, where `invoke` has
 * nothing to talk to. Falling back to the fake backend there keeps every
 * screen reachable in a browser during design work.
 *
 * `import.meta.env.DEV` is replaced with `false` in a production build, so the
 * branch and the module behind it are both dropped from the bundle.
 */
export const insideTauri =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const useDevBackend = import.meta.env.DEV && !insideTauri;

const invoke = useDevBackend ? devInvoke : tauriInvoke;

/**
 * The shape `SyncPartyError` serialises to.
 *
 * `kind` is a stable discriminant, so recovery logic can branch on it rather
 * than matching on a translated message.
 */
export interface BackendError {
  kind: string;
  message: string;
}

export function isBackendError(value: unknown): value is BackendError {
  return (
    typeof value === "object" &&
    value !== null &&
    "kind" in value &&
    "message" in value
  );
}

/** Turns anything thrown by `invoke` into a message worth showing. */
export function errorMessage(error: unknown): string {
  if (isBackendError(error)) return error.message;
  if (error instanceof Error) return error.message;
  return String(error);
}

export const ipc = {
  getSettings: () => invoke<AppSettings>("get_settings"),

  updateSettings: (patch: SettingsPatch) =>
    invoke<AppSettings>("update_settings", { patch }),

  runPreflight: (mode: AppMode) =>
    invoke<PreflightReport>("run_preflight", { mode }),

  runDiagnostics: () => invoke<DiagnosticsReport>("run_diagnostics"),

  /** Progress arrives as `installProgress` events while this is in flight. */
  installDependency: (id: DependencyId, choice?: PlayerChoice) =>
    invoke<void>("install_dependency", { id, choice }),

  /**
   * Points a dependency at a program the user chose, for portable builds
   * automatic detection cannot see. `path` may be the executable or the
   * folder holding it; `null` clears the choice.
   *
   * Rejects when the program is not actually there, leaving the previous
   * setting untouched.
   */
  setDependencyPath: (id: DependencyId, path: string | null) =>
    invoke<void>("set_dependency_path", { id, path }),

  startHosting: () => invoke<HostingInfo>("start_hosting"),
  stopHosting: () => invoke<void>("stop_hosting"),
  sessionState: () => invoke<SessionState>("session_state"),

  /** Accepts a bare code, a deep link, or a whole chat message. */
  decodeInvite: (text: string) => invoke<Invite>("decode_invite", { text }),
  joinParty: (invite: Invite) => invoke<void>("join_party", { invite }),

  /**
   * Closes the tunnel this guest is connected through.
   *
   * syncparty carries the connection rather than standing beside it, so a
   * guest that has finished with a party has to say so — otherwise the tunnel
   * stays open for as long as the window does.
   */
  leaveParty: () => invoke<void>("leave_party"),

  resumeLastSession: () => invoke<Invite | null>("resume_last_session"),
  clearLastSession: () => invoke<void>("clear_last_session"),

  /**
   * Opens the host's own Syncplay client on the party they are running.
   *
   * Not the same as `joinParty` with the shared invite: the host connects
   * straight to its own server on loopback, with no tunnel in between.
   */
  joinHostedParty: () => invoke<void>("join_hosted_party"),

  discordStatus: () => invoke<boolean>("discord_status"),
  setDiscordWebhook: (url: string) =>
    invoke<void>("set_discord_webhook", { url }),
  clearDiscordWebhook: () => invoke<void>("clear_discord_webhook"),
  testDiscordWebhook: () => invoke<void>("test_discord_webhook"),
};

/**
 * Subscribes to backend events.
 *
 * Returns the unlisten function, which callers must invoke on teardown —
 * React strict mode mounts effects twice, and a leaked listener would double
 * every event.
 */
export function onAppEvent(handler: (event: AppEvent) => void) {
  if (useDevBackend) return devListen(handler);

  return tauriListen<AppEvent>(EVENT_CHANNEL, ({ payload }) =>
    handler(payload),
  );
}
