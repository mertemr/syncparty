//! The movie discovery and voting command surface. Same shape as
//! `commands.rs` — thin wrappers, all the logic in `core`.

use tauri::State;

use crate::core::config::AppSettings;
use crate::core::error::Result;
use crate::core::movie::{
    DiscoverFilter, Genre, MovieDetails, MovieProvider, MovieSummary, PartyLogEntry,
    SessionHistoryEntry, UserMovie, WatchedMovie,
};
use crate::core::movie_vote::{MovieCandidate, MovieVoteSnapshot, ParticipationStatus};
use crate::core::session::SessionState;
use crate::ipc::AppState;

/// Movie details are the one thing worth caching across a whole day: the
/// same tmdb id is looked up again every time it is added to a vote, and
/// nothing about a released movie's details changes minute to minute.
const MOVIE_DETAILS_TTL_SECONDS: i64 = 24 * 60 * 60;

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

async fn is_hosting(state: &AppState) -> bool {
    matches!(state.session.state().await, SessionState::Hosting(_))
}

fn app_language(settings: &AppSettings) -> String {
    // TMDB wants `xx-XX`; the app's own setting is the bare language tag
    // (`tr`, `en`) used by `shared/i18n`, so the country half is filled in
    // for the two languages the app actually ships rather than guessing at
    // every locale TMDB might support.
    match settings.language.as_str() {
        "tr" => "tr-TR".to_owned(),
        _ => "en-US".to_owned(),
    }
}

#[tauri::command]
pub async fn tmdb_status(state: State<'_, AppState>) -> Result<bool> {
    Ok(state.tmdb.is_configured())
}

#[tauri::command]
pub fn set_tmdb_api_key(state: State<'_, AppState>, key: String) -> Result<()> {
    state.tmdb.set_api_key(&key)
}

#[tauri::command]
pub fn clear_tmdb_api_key(state: State<'_, AppState>) -> Result<()> {
    state.tmdb.clear_api_key()
}

#[tauri::command]
pub async fn search_movies(
    state: State<'_, AppState>,
    query: String,
    page: Option<u32>,
) -> Result<Vec<MovieSummary>> {
    let language = app_language(&state.settings.get());
    state
        .tmdb
        .search_movies(&query, &language, page.unwrap_or(1))
        .await
}

#[tauri::command]
pub async fn get_popular_movies(
    state: State<'_, AppState>,
    page: Option<u32>,
) -> Result<Vec<MovieSummary>> {
    let language = app_language(&state.settings.get());
    state
        .tmdb
        .popular_movies(&language, page.unwrap_or(1))
        .await
}

#[tauri::command]
pub async fn get_now_playing_movies(
    state: State<'_, AppState>,
    page: Option<u32>,
) -> Result<Vec<MovieSummary>> {
    let language = app_language(&state.settings.get());
    state
        .tmdb
        .now_playing_movies(&language, page.unwrap_or(1))
        .await
}

#[tauri::command]
pub async fn get_upcoming_movies(
    state: State<'_, AppState>,
    page: Option<u32>,
) -> Result<Vec<MovieSummary>> {
    let language = app_language(&state.settings.get());
    state
        .tmdb
        .upcoming_movies(&language, page.unwrap_or(1))
        .await
}

#[tauri::command]
pub async fn get_top_rated_movies(
    state: State<'_, AppState>,
    page: Option<u32>,
) -> Result<Vec<MovieSummary>> {
    let language = app_language(&state.settings.get());
    state
        .tmdb
        .top_rated_movies(&language, page.unwrap_or(1))
        .await
}

#[tauri::command]
pub async fn discover_movies(
    state: State<'_, AppState>,
    filter: DiscoverFilter,
    page: Option<u32>,
) -> Result<Vec<MovieSummary>> {
    let language = app_language(&state.settings.get());
    state
        .tmdb
        .discover_movies(&filter, &language, page.unwrap_or(1))
        .await
}

#[tauri::command]
pub async fn get_genres(state: State<'_, AppState>) -> Result<Vec<Genre>> {
    let language = app_language(&state.settings.get());
    state.tmdb.genres(&language).await
}

#[tauri::command]
pub async fn get_movie_details(state: State<'_, AppState>, tmdb_id: i64) -> Result<MovieDetails> {
    let language = app_language(&state.settings.get());

    if let Some(cached) = state
        .movie_store
        .cached_movie(tmdb_id, &language, now_seconds())?
    {
        return Ok(cached);
    }

    let details = state.tmdb.movie_details(tmdb_id, &language).await?;
    state.movie_store.cache_movie(
        tmdb_id,
        &language,
        &details,
        now_seconds(),
        MOVIE_DETAILS_TTL_SECONDS,
    )?;
    Ok(details)
}

#[tauri::command]
pub async fn start_movie_vote(
    state: State<'_, AppState>,
    schedule: Option<String>,
) -> Result<MovieVoteSnapshot> {
    let host = is_hosting(&state).await;
    state.movie_vote.start(host, schedule).await
}

#[tauri::command]
pub async fn add_movie_candidate(
    state: State<'_, AppState>,
    candidate: MovieCandidate,
) -> Result<MovieVoteSnapshot> {
    let host = is_hosting(&state).await;
    state.movie_vote.add_candidate(host, candidate).await
}

#[tauri::command]
pub async fn remove_movie_candidate(
    state: State<'_, AppState>,
    tmdb_id: i64,
) -> Result<MovieVoteSnapshot> {
    let host = is_hosting(&state).await;
    state.movie_vote.remove_candidate(host, tmdb_id).await
}

#[tauri::command]
pub async fn open_movie_vote(state: State<'_, AppState>) -> Result<MovieVoteSnapshot> {
    let host = is_hosting(&state).await;
    state.movie_vote.open(host).await
}

#[tauri::command]
pub async fn close_movie_vote(state: State<'_, AppState>) -> Result<MovieVoteSnapshot> {
    let host = is_hosting(&state).await;
    state.movie_vote.close(host).await
}

#[tauri::command]
pub async fn resolve_movie_vote_tie(
    state: State<'_, AppState>,
    tmdb_id: i64,
) -> Result<MovieVoteSnapshot> {
    let host = is_hosting(&state).await;
    state.movie_vote.resolve_tie(host, tmdb_id).await
}

#[tauri::command]
pub async fn cancel_movie_vote(state: State<'_, AppState>) -> Result<()> {
    let host = is_hosting(&state).await;
    state.movie_vote.cancel(host).await
}

/// The literal peer id the host's own vote/participation is stored under —
/// the host has no guest connection to key on. Guest calls ignore this: a
/// guest's action is forwarded to the host, which keys it on the connection
/// the message actually arrived on instead.
const HOST_PEER: &str = "host";

#[tauri::command]
pub async fn cast_movie_vote(state: State<'_, AppState>, tmdb_id: i64) -> Result<()> {
    let host = is_hosting(&state).await;
    state.movie_vote.cast_vote(host, HOST_PEER, tmdb_id).await
}

#[tauri::command]
pub async fn set_movie_vote_participation(
    state: State<'_, AppState>,
    status: Option<ParticipationStatus>,
) -> Result<()> {
    let host = is_hosting(&state).await;
    state
        .movie_vote
        .set_participation(host, HOST_PEER, status)
        .await
}

#[tauri::command]
pub async fn get_movie_vote(state: State<'_, AppState>) -> Result<Option<MovieVoteSnapshot>> {
    Ok(state.movie_vote.snapshot().await)
}

#[tauri::command]
pub fn get_session_history(state: State<'_, AppState>) -> Result<Vec<SessionHistoryEntry>> {
    state.movie_store.list_session_history()
}

#[tauri::command]
pub fn get_watched_movies(state: State<'_, AppState>) -> Result<Vec<WatchedMovie>> {
    state.movie_store.list_watched_movies()
}

#[tauri::command]
pub fn list_user_movies(state: State<'_, AppState>) -> Result<Vec<UserMovie>> {
    state.movie_store.list_user_movies()
}

/// Sets the favourite and watched marks for one movie in one call. Both
/// flags travel together because they share a row: sending only the one that
/// changed would mean reading the other back first to avoid clearing it.
#[tauri::command]
pub fn set_user_movie(
    state: State<'_, AppState>,
    movie: MovieSummary,
    favorite: bool,
    watched: bool,
) -> Result<()> {
    state
        .movie_store
        .set_user_movie(&movie, favorite, watched, now_seconds())
}

#[tauri::command]
pub fn get_party_log(state: State<'_, AppState>) -> Result<Vec<PartyLogEntry>> {
    state.movie_store.list_party_logs()
}

/// Records what the room is watching tonight, against the running party.
/// `movie` of `None` clears it.
#[tauri::command]
pub async fn set_now_watching(
    state: State<'_, AppState>,
    movie: Option<MovieCandidate>,
) -> Result<()> {
    match movie {
        Some(movie) => {
            state
                .session
                .set_now_watching(
                    Some(movie.tmdb_id),
                    Some(&movie.title),
                    movie.poster.as_deref(),
                )
                .await
        }
        None => state.session.set_now_watching(None, None, None).await,
    }
}
