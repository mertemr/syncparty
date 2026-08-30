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
import type { DiscoverFilter } from "@/shared/types/DiscoverFilter";
import type { Genre } from "@/shared/types/Genre";
import type { HostingInfo } from "@/shared/types/HostingInfo";
import type { Invite } from "@/shared/types/Invite";
import type { MovieCandidate } from "@/shared/types/MovieCandidate";
import type { MovieDetails } from "@/shared/types/MovieDetails";
import type { MovieSummary } from "@/shared/types/MovieSummary";
import type { MovieVoteSnapshot } from "@/shared/types/MovieVoteSnapshot";
import type { ParticipationStatus } from "@/shared/types/ParticipationStatus";
import type { PlayerChoice } from "@/shared/types/PlayerChoice";
import type { PreflightReport } from "@/shared/types/PreflightReport";
import type { SessionHistoryEntry } from "@/shared/types/SessionHistoryEntry";
import type { SessionState } from "@/shared/types/SessionState";
import type { SettingsPatch } from "@/shared/types/SettingsPatch";
import type { WatchedMovie } from "@/shared/types/WatchedMovie";

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

  tmdbStatus: () => invoke<boolean>("tmdb_status"),
  setTmdbApiKey: (key: string) => invoke<void>("set_tmdb_api_key", { key }),
  clearTmdbApiKey: () => invoke<void>("clear_tmdb_api_key"),

  searchMovies: (query: string, page?: number) =>
    invoke<MovieSummary[]>("search_movies", { query, page }),
  getPopularMovies: (page?: number) =>
    invoke<MovieSummary[]>("get_popular_movies", { page }),
  getNowPlayingMovies: (page?: number) =>
    invoke<MovieSummary[]>("get_now_playing_movies", { page }),
  getUpcomingMovies: (page?: number) =>
    invoke<MovieSummary[]>("get_upcoming_movies", { page }),
  getTopRatedMovies: (page?: number) =>
    invoke<MovieSummary[]>("get_top_rated_movies", { page }),
  discoverMovies: (filter: DiscoverFilter, page?: number) =>
    invoke<MovieSummary[]>("discover_movies", { filter, page }),
  getGenres: () => invoke<Genre[]>("get_genres"),
  getMovieDetails: (tmdbId: bigint) =>
    invoke<MovieDetails>("get_movie_details", { tmdbId }),

  startMovieVote: (schedule: string | null) =>
    invoke<MovieVoteSnapshot>("start_movie_vote", { schedule }),
  addMovieCandidate: (candidate: MovieCandidate) =>
    invoke<MovieVoteSnapshot>("add_movie_candidate", { candidate }),
  removeMovieCandidate: (tmdbId: bigint) =>
    invoke<MovieVoteSnapshot>("remove_movie_candidate", { tmdbId }),
  openMovieVote: () => invoke<MovieVoteSnapshot>("open_movie_vote"),
  closeMovieVote: () => invoke<MovieVoteSnapshot>("close_movie_vote"),
  resolveMovieVoteTie: (tmdbId: bigint) =>
    invoke<MovieVoteSnapshot>("resolve_movie_vote_tie", { tmdbId }),
  cancelMovieVote: () => invoke<void>("cancel_movie_vote"),
  castMovieVote: (tmdbId: bigint) =>
    invoke<void>("cast_movie_vote", { tmdbId }),
  setMovieVoteParticipation: (status: ParticipationStatus | null) =>
    invoke<void>("set_movie_vote_participation", { status }),
  getMovieVote: () => invoke<MovieVoteSnapshot | null>("get_movie_vote"),

  getSessionHistory: () =>
    invoke<SessionHistoryEntry[]>("get_session_history"),
  getWatchedMovies: () => invoke<WatchedMovie[]>("get_watched_movies"),
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
