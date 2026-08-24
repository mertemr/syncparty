/**
 * A fake backend, for running the UI in a plain browser.
 *
 * `pnpm dev` serves the frontend without the Tauri shell, so `invoke` has
 * nothing to talk to and every screen stops at "Loading…". That makes the UI
 * impossible to look at anywhere except a compiled desktop build, which is a
 * poor trade during design work.
 *
 * This stands in for the Rust side well enough to drive every screen: it holds
 * settings, runs a scripted start-up, and pushes room snapshots. It is not a
 * simulator — nothing here is a claim about how the real backend behaves, and
 * nothing may depend on it outside development.
 *
 * Scenarios are chosen with a query parameter, so a screenshot of any state is
 * one URL away:
 *
 *   ?dev=missing    a dependency is not installed
 *   ?dev=mismatch   the room is watching different files
 *   ?dev=guest      start as a guest rather than a host
 */
import type { AppEvent } from "@/shared/types/AppEvent";
import type { AppSettings } from "@/shared/types/AppSettings";
import type { DiagnosticsReport } from "@/shared/types/DiagnosticsReport";
import type { HostingInfo } from "@/shared/types/HostingInfo";
import type { Invite } from "@/shared/types/Invite";
import type { PreflightReport } from "@/shared/types/PreflightReport";
import type { RoomSnapshot } from "@/shared/types/RoomSnapshot";
import type { SessionState } from "@/shared/types/SessionState";
import type { SettingsPatch } from "@/shared/types/SettingsPatch";
import type { StartupStep } from "@/shared/types/StartupStep";
import type { UpdatePolicy } from "@/shared/types/UpdatePolicy";

const scenario = new URLSearchParams(globalThis.location?.search ?? "").get(
  "dev",
);

const ENDPOINT =
  "k57qxc3jrqvbnl2mzp4wt6yhd8fgs9auk3e7rvxm2cqbnjh4pldw";

const INVITE: Invite = {
  endpoint: ENDPOINT,
  password: "tape-deck-42",
  room: "movie-night",
};

const listeners = new Set<(event: AppEvent) => void>();

function emit(event: AppEvent) {
  for (const listener of listeners) listener(event);
}

let settings: AppSettings = {
  mode: scenario === "guest" ? "guest" : null,
  port: 8999,
  room: "movie-night",
  nickname: "taha",
  language: "en",
  monitorEnabled: true,
  skipSetupWhenReady: false,
  discordEnabled: false,
  executableOverrides: {},
};

let session: SessionState = { phase: "idle" };
const overrides: Record<string, string | null> = {};
/** Dependencies the fake machine is missing. Installing one clears it. */
const missing = new Set(scenario === "missing" ? ["mpv"] : []);

function preflight(): PreflightReport {
  return {
    mode: settings.mode ?? "host",
    items: [
      {
        id: "syncplayClient",
        displayName: "Syncplay",
        status: missing.has("syncplayClient")
          ? { state: "missing" }
          : { state: "installed", version: "1.7.2", path: "C:/syncplay" },
        canAutoInstall: true,
        needsElevation: false,
        manualUrl: "https://syncplay.pl/download/",
        supportsManualPath: true,
        overridePath: overrides.syncplayClient ?? null,
      },
      {
        id: "mpv",
        displayName: "mpv",
        status: missing.has("mpv")
          ? { state: "missing" }
          : { state: "installed", version: "0.38.0", path: "C:/mpv" },
        canAutoInstall: true,
        needsElevation: false,
        manualUrl: "https://mpv.io/installation/",
        supportsManualPath: true,
        overridePath: overrides.mpv ?? null,
      },
    ],
  };
}

function hosting(): HostingInfo {
  return {
    invite: INVITE,
    inviteCode: `SP1-${ENDPOINT.slice(0, 24)}`,
    deepLink: `syncparty://join/${ENDPOINT.slice(0, 24)}`,
    serverAddress: "127.0.0.1:8999",
    server: { state: "running", port: 8999 },
    monitorAttached: true,
  };
}

function room(): RoomSnapshot {
  const mismatched = scenario === "mismatch";

  return {
    connected: true,
    rooms: [
      {
        name: "movie-night",
        everyoneOnTheSameFile: !mismatched,
        fileCompatibility: mismatched ? "mismatch" : "exact",
        watchers: [
          {
            name: "taha",
            file: { name: "Stalker.1979.2160p.mkv", durationSeconds: 9660 },
            isReady: true,
            isController: true,
          },
          {
            name: "mert",
            file: {
              name: mismatched
                ? "Solaris.1972.1080p.mkv"
                : "Stalker.1979.1080p.mkv",
              durationSeconds: mismatched ? 9420 : 9661,
            },
            isReady: true,
            isController: false,
          },
          {
            name: "ada",
            file: null,
            isReady: false,
            isController: false,
          },
        ],
      },
    ],
  };
}

const STARTUP: StartupStep[] = [
  "joiningNetwork",
  "openingTunnel",
  "startingServer",
  "attachingMonitor",
];

/** Walks the start-up steps, then goes live and starts pushing the room. */
async function startHosting(): Promise<HostingInfo> {
  for (const step of STARTUP) {
    session = { phase: "starting", step };
    emit({ kind: "sessionChanged", state: session });
    emit({
      kind: "serverLog",
      line: `[dev] ${step}`,
      isError: false,
    });
    await sleep(700);
  }

  session = { phase: "hosting", ...hosting() };
  emit({ kind: "sessionChanged", state: session });
  emit({ kind: "roomUpdated", snapshot: room() });

  return hosting();
}

function sleep(ms: number) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function fail(kind: string, message: string): never {
  throw { kind, message };
}

const COMMANDS: Record<string, (args: Args) => unknown | Promise<unknown>> = {
  get_settings: () => settings,

  update_settings: ({ patch }) => {
    settings = { ...settings, ...(patch as SettingsPatch) };
    return settings;
  },

  // The dev backend stands in for a desktop install, so it claims the
  // self-installing behaviour. The updater plugin is unreachable in a browser
  // anyway, so this never gets as far as a download.
  update_policy: (): UpdatePolicy => ({ checks: true, selfInstalls: true }),

  run_preflight: () => {
    const report = preflight();
    emit({ kind: "preflightCompleted", report });
    return report;
  },

  install_dependency: async ({ id }) => {
    const dependency = id as "mpv";
    for (const [index, stage] of ["Downloading", "Extracting", "Verifying"].entries()) {
      emit({
        kind: "installProgress",
        dependency,
        stage,
        percent: (index + 1) * 33,
        detail: null,
      });
      await sleep(600);
    }
    missing.delete(dependency);
  },

  set_dependency_path: ({ id, path }) => {
    if (typeof path === "string" && !path.trim()) {
      fail("dependency_missing", "Nothing is installed at that path.");
    }
    overrides[id as string] = (path as string | null) ?? null;
  },

  start_hosting: () => startHosting(),

  stop_hosting: () => {
    session = { phase: "idle" };
    emit({ kind: "sessionChanged", state: session });
  },

  session_state: () => session,

  decode_invite: ({ text }) => {
    if (!String(text).trim().toUpperCase().startsWith("SP1-")) {
      fail("invalid_invite", "That does not look like a syncparty invite.");
    }
    return INVITE;
  },

  join_party: () => undefined,
  leave_party: () => undefined,
  resume_last_session: () => null,
  clear_last_session: () => undefined,
  join_hosted_party: () => undefined,

  run_diagnostics: (): DiagnosticsReport => ({
    appVersion: "0.5.3-dev",
    operatingSystem: "Windows 11 (dev backend)",
    secretStorage: "keychain",
    dependencies: preflight(),
    endpoint: ENDPOINT,
    session,
    transport: {
      endpointId: ENDPOINT,
      addresses: ["192.168.1.24:52311", "81.214.9.7:52311"],
      behindCarrierNat: false,
      relays: [
        { url: "https://euw1-1.relay.iroh.network", connected: true, lastError: null },
      ],
      peers: [],
    },
    transportError: null,
  }),

  discord_status: () => false,
  set_discord_webhook: () => undefined,
  clear_discord_webhook: () => undefined,
  test_discord_webhook: () => undefined,
};

type Args = Record<string, unknown>;

export function devInvoke<T>(command: string, args?: Args): Promise<T> {
  const handler = COMMANDS[command];

  if (!handler) {
    return Promise.reject({
      kind: "unimplemented",
      message: `dev backend has no ${command}`,
    });
  }

  return Promise.resolve(handler(args ?? {})).then((value) => value as T);
}

export function devListen(handler: (event: AppEvent) => void) {
  listeners.add(handler);
  return Promise.resolve(() => {
    listeners.delete(handler);
  });
}
