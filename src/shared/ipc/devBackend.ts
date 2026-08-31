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
import type { Genre } from "@/shared/types/Genre";
import type { HostingInfo } from "@/shared/types/HostingInfo";
import type { Invite } from "@/shared/types/Invite";
import type { MovieCandidate } from "@/shared/types/MovieCandidate";
import type { MovieDetails } from "@/shared/types/MovieDetails";
import type { MovieSummary } from "@/shared/types/MovieSummary";
import type { MovieVoteSnapshot } from "@/shared/types/MovieVoteSnapshot";
import type { ParticipationStatus } from "@/shared/types/ParticipationStatus";
import type { PreflightReport } from "@/shared/types/PreflightReport";
import type { RoomSnapshot } from "@/shared/types/RoomSnapshot";
import type { SessionState } from "@/shared/types/SessionState";
import type { SettingsPatch } from "@/shared/types/SettingsPatch";
import type { StartupStep } from "@/shared/types/StartupStep";
import type { PartyLogEntry } from "@/shared/types/PartyLogEntry";
import type { UpdatePolicy } from "@/shared/types/UpdatePolicy";
import type { UserMovie } from "@/shared/types/UserMovie";
import type { VoteParticipant } from "@/shared/types/VoteParticipant";

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

const GENRES: Genre[] = [
  { id: 28, name: "Action" },
  { id: 35, name: "Comedy" },
  { id: 18, name: "Drama" },
  { id: 27, name: "Horror" },
  { id: 878, name: "Sci-Fi" },
];

const MOVIES: MovieSummary[] = [
  {
    tmdbId: 27205n,
    title: "Inception",
    originalTitle: "Inception",
    poster: null,
    backdrop: null,
    releaseDate: "2010-07-16",
    overview: "A thief who steals corporate secrets through dream-sharing technology.",
    genreIds: [28, 878],
    rating: 8.4,
    voteCount: 35000n,
    popularity: 130,
  },
  {
    tmdbId: 157336n,
    title: "Interstellar",
    originalTitle: "Interstellar",
    poster: null,
    backdrop: null,
    releaseDate: "2014-11-05",
    overview: "A team of explorers travel through a wormhole in space.",
    genreIds: [18, 878],
    rating: 8.4,
    voteCount: 34000n,
    popularity: 150,
  },
  {
    tmdbId: 496243n,
    title: "Parasite",
    originalTitle: "기생충",
    poster: null,
    backdrop: null,
    releaseDate: "2019-05-30",
    overview: "Greed and class discrimination threaten the newly formed relationship.",
    genreIds: [35, 18],
    rating: 8.5,
    voteCount: 20000n,
    popularity: 90,
  },
  {
    tmdbId: 419430n,
    title: "Get Out",
    originalTitle: "Get Out",
    poster: null,
    backdrop: null,
    releaseDate: "2017-02-24",
    overview: "A young man visits his girlfriend's parents for the weekend.",
    genreIds: [27, 18],
    rating: 7.7,
    voteCount: 18000n,
    popularity: 70,
  },
  {
    tmdbId: 361743n,
    title: "Top Gun: Maverick",
    originalTitle: "Top Gun: Maverick",
    poster: null,
    backdrop: null,
    releaseDate: "2022-05-24",
    overview: "After more than thirty years of service, Maverick is still pushing the envelope.",
    genreIds: [28],
    rating: 8.2,
    voteCount: 9000n,
    popularity: 200,
  },
  {
    tmdbId: 129n,
    title: "Spirited Away",
    originalTitle: "千と千尋の神隠し",
    poster: null,
    backdrop: null,
    releaseDate: "2001-07-20",
    overview: "A young girl wanders into a world ruled by gods and witches.",
    genreIds: [18],
    rating: 8.5,
    voteCount: 15000n,
    popularity: 60,
  },
];

const WATCHED_TMDB_IDS = new Set<string>(["129"]);

/** Favourites and manual marks, for the length of the page. */
const devUserMovies = new Map<string, UserMovie>();

/** One night still running plus one already over, so both shapes of row in
 * the party log can be looked at without hosting anything. */
const devPartyLog: PartyLogEntry[] = [
  {
    id: "dev-tonight",
    startedAt: BigInt(Date.now() - 45 * 60_000),
    endedAt: null,
    movieTmdbId: null,
    movieTitle: null,
    moviePoster: null,
    participants: ["taha", "mert"],
  },
  {
    id: "dev-last-week",
    startedAt: BigInt(Date.now() - 7 * 86_400_000),
    endedAt: BigInt(Date.now() - 7 * 86_400_000 + 107 * 60_000),
    movieTmdbId: 27205n,
    movieTitle: "Inception",
    moviePoster: null,
    participants: ["taha", "mert", "ada"],
  },
];

/** Fakes a second page of results (offset ids so React keys stay unique) and
 * an end of catalogue after that, so infinite scroll has something to do
 * and somewhere to stop. */
function paginate(base: MovieSummary[], page: number): MovieSummary[] {
  if (page === 1) return base;
  if (page === 2) {
    return base.map((movie) => ({
      ...movie,
      tmdbId: movie.tmdbId + 1000000n,
      title: `${movie.title} II`,
    }));
  }
  return [];
}

/** Host-authoritative movie vote, mirroring `core::movie_vote`'s rules
 * closely enough to drive every screen — not a claim about exact backend
 * behaviour, same as the rest of this file. */
let movieVote: MovieVoteSnapshot | null = null;

function participant(peer: string): VoteParticipant {
  const found = movieVote?.participants.find((p) => p.peer === peer);
  if (found) return found;
  return {
    peer,
    displayName: peer === "host" ? "taha" : peer,
    participation: null,
    selectedMovie: null,
    respondedAt: null,
  };
}

function upsertParticipant(peer: string, patch: Partial<VoteParticipant>) {
  if (!movieVote) return;
  const existing = movieVote.participants.find((p) => p.peer === peer);
  const updated = { ...participant(peer), ...patch };
  if (existing) {
    movieVote.participants = movieVote.participants.map((p) => (p.peer === peer ? updated : p));
  } else {
    movieVote.participants = [...movieVote.participants, updated];
  }
}

/** A fresh, independent copy of the current vote — every mutating handler
 * below updates the module-level `movieVote` in place (for brevity), but
 * each *emitted* snapshot has to stop reflecting later mutations the moment
 * it goes out, exactly like the real backend's JSON-serialised snapshots do.
 * Skipping this made two consecutive events look identical to anything
 * comparing "previous" and "next" by reference, since both would actually be
 * the same live, still-mutating object. */
function cloneSnapshot(snapshot: MovieVoteSnapshot | null): MovieVoteSnapshot | null {
  if (!snapshot) return null;
  return {
    ...snapshot,
    candidates: [...snapshot.candidates],
    participants: [...snapshot.participants],
    result: snapshot.result
      ? { ...snapshot.result, tally: [...snapshot.result.tally], tied: [...snapshot.result.tied] }
      : null,
  };
}

function emitMovieVote() {
  emit({ kind: "movieVoteChanged", snapshot: cloneSnapshot(movieVote) });
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

  tmdb_status: () => true,
  set_tmdb_api_key: () => undefined,
  clear_tmdb_api_key: () => undefined,

  search_movies: ({ query, page }) => {
    const needle = String(query).toLowerCase();
    return paginate(
      MOVIES.filter((movie) => movie.title.toLowerCase().includes(needle)),
      Number(page ?? 1),
    );
  },
  get_popular_movies: ({ page }) => paginate(MOVIES, Number(page ?? 1)),
  get_now_playing_movies: ({ page }) => paginate([...MOVIES].reverse(), Number(page ?? 1)),
  get_upcoming_movies: ({ page }) => paginate(MOVIES.slice(0, 3), Number(page ?? 1)),
  get_top_rated_movies: ({ page }) =>
    paginate(
      [...MOVIES].sort((a, b) => b.rating - a.rating),
      Number(page ?? 1),
    ),
  discover_movies: ({ page }) => paginate(MOVIES, Number(page ?? 1)),
  get_genres: () => GENRES,

  get_movie_details: ({ tmdbId }): MovieDetails => {
    const movie = MOVIES.find((item) => item.tmdbId === BigInt(tmdbId as never));
    if (!movie) fail("other", "unknown movie");

    return {
      tmdbId: movie!.tmdbId,
      title: movie!.title,
      originalTitle: movie!.originalTitle,
      poster: movie!.poster,
      backdrop: movie!.backdrop,
      releaseDate: movie!.releaseDate,
      runtimeMinutes: 128,
      genres: movie!.genreIds.map((id) => GENRES.find((genre) => genre.id === id)!),
      rating: movie!.rating,
      voteCount: movie!.voteCount,
      overview: movie!.overview,
      trailer: { key: "dQw4w9WgXcQ", site: "YouTube", kind: "Trailer", official: true },
      watchProviders: [{ name: "Netflix", logo: null }],
      watchLink: "https://www.themoviedb.org",
    };
  },

  get_watched_movies: () =>
    MOVIES.filter((movie) => WATCHED_TMDB_IDS.has(movie.tmdbId.toString())).map((movie) => ({
      tmdbId: movie.tmdbId,
      title: movie.title,
      sessionId: "dev-session",
      watchedAt: BigInt(Date.now()),
      participants: ["taha", "mert"],
    })),

  get_session_history: () => [],

  get_party_log: () => devPartyLog,

  set_now_watching: ({ movie }) => {
    const candidate = movie as MovieCandidate | null;
    const open = devPartyLog.find((entry) => entry.endedAt === null);
    if (!open) return;
    open.movieTmdbId = candidate ? BigInt(candidate.tmdbId) : null;
    open.movieTitle = candidate?.title ?? null;
    open.moviePoster = candidate?.poster ?? null;
  },

  // The real store is SQLite; here it is a module-level map that resets with
  // the page, which is all a design pass on the marked-up states needs.
  list_user_movies: () => [...devUserMovies.values()],

  set_user_movie: ({ movie, favorite, watched }) => {
    const summary = movie as MovieSummary;
    const key = summary.tmdbId.toString();
    if (!favorite && !watched) devUserMovies.delete(key);
    else
      devUserMovies.set(key, {
        movie: summary,
        favorite: favorite as boolean,
        watched: watched as boolean,
        markedAt: BigInt(Date.now()),
      });
  },

  get_movie_vote: () => cloneSnapshot(movieVote),

  start_movie_vote: ({ schedule }) => {
    movieVote = {
      id: `dev-${Date.now()}`,
      phase: "draft",
      createdAt: BigInt(Date.now()),
      schedule: (schedule as string | null) ?? null,
      candidates: [],
      participants: [],
      result: null,
    };
    emitMovieVote();
    return cloneSnapshot(movieVote);
  },

  add_movie_candidate: ({ candidate }) => {
    if (!movieVote) fail("other", "no movie vote is in progress");
    const next = candidate as MovieCandidate;
    if (movieVote!.candidates.some((c) => c.tmdbId === next.tmdbId)) {
      fail("other", "that movie is already a candidate");
    }
    if (movieVote!.candidates.length >= 10) fail("other", "a vote can hold at most 10 movies");
    movieVote!.candidates = [...movieVote!.candidates, next];
    emitMovieVote();
    return cloneSnapshot(movieVote);
  },

  remove_movie_candidate: ({ tmdbId }) => {
    if (!movieVote) fail("other", "no movie vote is in progress");
    const id = BigInt(tmdbId as never);
    movieVote!.candidates = movieVote!.candidates.filter((c) => c.tmdbId !== id);
    emitMovieVote();
    return cloneSnapshot(movieVote);
  },

  open_movie_vote: () => {
    if (!movieVote) fail("other", "no movie vote is in progress");
    if (movieVote!.candidates.length < 2) fail("other", "at least 2 movies are needed");
    movieVote!.phase = "open";
    emitMovieVote();
    return cloneSnapshot(movieVote);
  },

  cast_movie_vote: ({ tmdbId }) => {
    if (!movieVote) fail("other", "no movie vote is in progress");
    upsertParticipant("host", { selectedMovie: BigInt(tmdbId as never), respondedAt: BigInt(Date.now()) });
    emitMovieVote();
  },

  set_movie_vote_participation: ({ status }) => {
    if (!movieVote) fail("other", "no movie vote is in progress");
    upsertParticipant("host", {
      participation: (status as ParticipationStatus | null) ?? null,
      respondedAt: BigInt(Date.now()),
    });
    emitMovieVote();
  },

  close_movie_vote: () => {
    if (!movieVote) fail("other", "no movie vote is in progress");
    const tally = movieVote!.candidates.map((candidate) => ({
      tmdbId: candidate.tmdbId,
      votes: movieVote!.participants.filter((p) => p.selectedMovie === candidate.tmdbId).length,
    }));
    const top = Math.max(0, ...tally.map((t) => t.votes));
    const leaders = tally.filter((t) => top > 0 && t.votes === top).map((t) => t.tmdbId);
    movieVote!.phase = "completed";
    movieVote!.result = {
      tally,
      winner: leaders.length === 1 ? leaders[0] : null,
      tied: leaders.length > 1 ? leaders : [],
    };
    emitMovieVote();
    return cloneSnapshot(movieVote);
  },

  resolve_movie_vote_tie: ({ tmdbId }) => {
    if (!movieVote?.result) fail("other", "the vote has not closed yet");
    movieVote.result.winner = BigInt(tmdbId as never);
    movieVote.result.tied = [];
    emitMovieVote();
    return cloneSnapshot(movieVote);
  },

  cancel_movie_vote: () => {
    if (movieVote && (movieVote.phase === "draft" || movieVote.phase === "open")) {
      movieVote.phase = "cancelled";
      emitMovieVote();
    }
  },
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
