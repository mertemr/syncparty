//! Local persistence: cached movie metadata, and movie-night history.
//!
//! SQLite rather than another JSON file — this is the one place the app
//! accumulates a genuinely unbounded amount of data (every movie ever looked
//! up, every night ever run), which is exactly what a flat file starts to
//! struggle with. `rusqlite`'s `bundled` feature compiles SQLite in, so this
//! costs nothing to install and nothing to configure.
//!
//! Schema changes go through [`migrate`], keyed off `PRAGMA user_version` —
//! there is only one version so far, but the shape is there for the next one.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::core::error::{Result, SyncPartyError};
use crate::core::movie_vote::MovieVoteSnapshot;

use super::MovieDetails;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct SessionHistoryEntry {
    pub id: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub snapshot: MovieVoteSnapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WatchedMovie {
    pub tmdb_id: i64,
    pub title: String,
    pub session_id: String,
    pub watched_at: i64,
    pub participants: Vec<String>,
}

const CURRENT_VERSION: i32 = 1;

const SCHEMA_V1: &str = "
    CREATE TABLE movie_cache (
        tmdb_id INTEGER NOT NULL,
        language TEXT NOT NULL,
        payload TEXT NOT NULL,
        fetched_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL,
        PRIMARY KEY (tmdb_id, language)
    );

    CREATE TABLE session_history (
        id TEXT PRIMARY KEY,
        started_at INTEGER NOT NULL,
        ended_at INTEGER,
        payload TEXT NOT NULL
    );

    CREATE TABLE watched_movies (
        tmdb_id INTEGER NOT NULL,
        session_id TEXT NOT NULL,
        title TEXT NOT NULL,
        watched_at INTEGER NOT NULL,
        participants TEXT NOT NULL,
        PRIMARY KEY (tmdb_id, session_id)
    );
";

fn migrate(conn: &Connection) -> Result<()> {
    let version: i32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    if version < 1 {
        conn.execute_batch(SCHEMA_V1)?;
    }

    conn.pragma_update(None, "user_version", CURRENT_VERSION)?;
    Ok(())
}

pub struct MovieStore {
    conn: Mutex<Connection>,
}

impl MovieStore {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("the movie store connection mutex is never poisoned")
    }

    /// A cached movie's details, if one is on file and has not expired.
    pub fn cached_movie(&self, tmdb_id: i64, language: &str, now: i64) -> Result<Option<MovieDetails>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT payload FROM movie_cache WHERE tmdb_id = ?1 AND language = ?2 AND expires_at > ?3",
        )?;

        let mut rows = statement.query(params![tmdb_id, language, now])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };

        let payload: String = row.get(0)?;
        Ok(Some(serde_json::from_str(&payload)?))
    }

    pub fn cache_movie(
        &self,
        tmdb_id: i64,
        language: &str,
        details: &MovieDetails,
        now: i64,
        ttl_seconds: i64,
    ) -> Result<()> {
        let payload = serde_json::to_string(details)?;
        self.conn().execute(
            "INSERT INTO movie_cache (tmdb_id, language, payload, fetched_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (tmdb_id, language) DO UPDATE SET
                payload = excluded.payload,
                fetched_at = excluded.fetched_at,
                expires_at = excluded.expires_at",
            params![tmdb_id, language, payload, now, now + ttl_seconds],
        )?;
        Ok(())
    }

    /// Records a movie night, replacing any entry already stored under the
    /// same id — a vote's history row is written more than once as it
    /// progresses (opened, then completed), not only at the very end.
    pub fn save_session_history(&self, entry: &SessionHistoryEntry) -> Result<()> {
        let payload = serde_json::to_string(&entry.snapshot)?;
        self.conn().execute(
            "INSERT INTO session_history (id, started_at, ended_at, payload)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (id) DO UPDATE SET
                ended_at = excluded.ended_at,
                payload = excluded.payload",
            params![entry.id, entry.started_at, entry.ended_at, payload],
        )?;
        Ok(())
    }

    /// Every movie night on file, most recent first.
    pub fn list_session_history(&self) -> Result<Vec<SessionHistoryEntry>> {
        let conn = self.conn();
        let mut statement =
            conn.prepare("SELECT id, started_at, ended_at, payload FROM session_history ORDER BY started_at DESC")?;

        let rows = statement.query_map([], |row| {
            let id: String = row.get(0)?;
            let started_at: i64 = row.get(1)?;
            let ended_at: Option<i64> = row.get(2)?;
            let payload: String = row.get(3)?;
            Ok((id, started_at, ended_at, payload))
        })?;

        let mut entries = Vec::new();
        for row in rows {
            let (id, started_at, ended_at, payload) = row?;
            let snapshot: MovieVoteSnapshot = serde_json::from_str(&payload)?;
            entries.push(SessionHistoryEntry {
                id,
                started_at,
                ended_at,
                snapshot,
            });
        }
        Ok(entries)
    }

    pub fn record_watched_movie(&self, watched: &WatchedMovie) -> Result<()> {
        let participants = serde_json::to_string(&watched.participants)?;
        self.conn().execute(
            "INSERT INTO watched_movies (tmdb_id, session_id, title, watched_at, participants)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (tmdb_id, session_id) DO NOTHING",
            params![
                watched.tmdb_id,
                watched.session_id,
                watched.title,
                watched.watched_at,
                participants,
            ],
        )?;
        Ok(())
    }

    pub fn list_watched_movies(&self) -> Result<Vec<WatchedMovie>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT tmdb_id, session_id, title, watched_at, participants FROM watched_movies ORDER BY watched_at DESC",
        )?;

        let rows = statement.query_map([], |row| {
            let tmdb_id: i64 = row.get(0)?;
            let session_id: String = row.get(1)?;
            let title: String = row.get(2)?;
            let watched_at: i64 = row.get(3)?;
            let participants: String = row.get(4)?;
            Ok((tmdb_id, session_id, title, watched_at, participants))
        })?;

        let mut watched = Vec::new();
        for row in rows {
            let (tmdb_id, session_id, title, watched_at, participants) = row?;
            let participants: Vec<String> = serde_json::from_str(&participants)?;
            watched.push(WatchedMovie {
                tmdb_id,
                title,
                session_id,
                watched_at,
                participants,
            });
        }
        Ok(watched)
    }

    /// Whether `tmdb_id` has been watched in any past session — for marking
    /// the browse grid.
    pub fn has_been_watched(&self, tmdb_id: i64) -> Result<bool> {
        let count: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM watched_movies WHERE tmdb_id = ?1",
            params![tmdb_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }
}

impl From<rusqlite::Error> for SyncPartyError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Other(format!("movie database error: {value}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_at(label: &str) -> MovieStore {
        let path = std::env::temp_dir().join(format!("syncparty-movies-{label}.sqlite3"));
        let _ = std::fs::remove_file(&path);
        MovieStore::open(&path).expect("open")
    }

    fn sample_details(tmdb_id: i64) -> MovieDetails {
        MovieDetails {
            tmdb_id,
            title: "Inception".to_owned(),
            original_title: "Inception".to_owned(),
            poster: None,
            backdrop: None,
            release_date: Some("2010-07-16".to_owned()),
            runtime_minutes: Some(148),
            genres: Vec::new(),
            rating: 8.4,
            vote_count: 35000,
            overview: "A thief who steals corporate secrets.".to_owned(),
            trailer: None,
            watch_providers: Vec::new(),
            watch_link: None,
        }
    }

    #[test]
    fn a_cached_movie_round_trips() {
        let store = store_at("cache-roundtrip");
        let details = sample_details(27205);

        assert!(store.cached_movie(27205, "en-US", 0).expect("read").is_none());

        store.cache_movie(27205, "en-US", &details, 0, 3600).expect("write");
        let cached = store.cached_movie(27205, "en-US", 100).expect("read").expect("hit");

        assert_eq!(cached, details);
    }

    #[test]
    fn an_expired_cache_entry_is_not_returned() {
        let store = store_at("cache-expiry");
        let details = sample_details(1);

        store.cache_movie(1, "en-US", &details, 0, 60).expect("write");

        assert!(store.cached_movie(1, "en-US", 61).expect("read").is_none());
    }

    #[test]
    fn caching_the_same_movie_again_overwrites_rather_than_duplicating() {
        let store = store_at("cache-overwrite");
        let mut details = sample_details(1);

        store.cache_movie(1, "en-US", &details, 0, 3600).expect("first write");
        details.rating = 9.0;
        store.cache_movie(1, "en-US", &details, 0, 3600).expect("second write");

        let cached = store.cached_movie(1, "en-US", 1).expect("read").expect("hit");
        assert_eq!(cached.rating, 9.0);
    }

    fn sample_snapshot() -> MovieVoteSnapshot {
        use crate::core::movie_vote::VotePhase;
        MovieVoteSnapshot {
            id: "vote-1".to_owned(),
            phase: VotePhase::Completed,
            created_at: 0,
            schedule: None,
            candidates: Vec::new(),
            participants: Vec::new(),
            result: None,
        }
    }

    #[test]
    fn session_history_round_trips_and_orders_most_recent_first() {
        let store = store_at("history-roundtrip");

        store
            .save_session_history(&SessionHistoryEntry {
                id: "a".to_owned(),
                started_at: 100,
                ended_at: Some(200),
                snapshot: sample_snapshot(),
            })
            .expect("save a");
        store
            .save_session_history(&SessionHistoryEntry {
                id: "b".to_owned(),
                started_at: 300,
                ended_at: Some(400),
                snapshot: sample_snapshot(),
            })
            .expect("save b");

        let history = store.list_session_history().expect("list");
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].id, "b", "most recent first");
    }

    #[test]
    fn saving_a_session_again_under_the_same_id_updates_it_in_place() {
        let store = store_at("history-update");

        store
            .save_session_history(&SessionHistoryEntry {
                id: "a".to_owned(),
                started_at: 100,
                ended_at: None,
                snapshot: sample_snapshot(),
            })
            .expect("save open");
        store
            .save_session_history(&SessionHistoryEntry {
                id: "a".to_owned(),
                started_at: 100,
                ended_at: Some(500),
                snapshot: sample_snapshot(),
            })
            .expect("save closed");

        let history = store.list_session_history().expect("list");
        assert_eq!(history.len(), 1, "the same id must not duplicate");
        assert_eq!(history[0].ended_at, Some(500));
    }

    #[test]
    fn watched_movies_round_trip() {
        let store = store_at("watched-roundtrip");

        assert!(!store.has_been_watched(27205).expect("check"));

        store
            .record_watched_movie(&WatchedMovie {
                tmdb_id: 27205,
                title: "Inception".to_owned(),
                session_id: "vote-1".to_owned(),
                watched_at: 1000,
                participants: vec!["host".to_owned(), "guest-1".to_owned()],
            })
            .expect("record");

        assert!(store.has_been_watched(27205).expect("check"));
        let watched = store.list_watched_movies().expect("list");
        assert_eq!(watched.len(), 1);
        assert_eq!(watched[0].participants.len(), 2);
    }

    #[test]
    fn reopening_the_database_keeps_what_was_there() {
        let path = std::env::temp_dir().join("syncparty-movies-reopen.sqlite3");
        let _ = std::fs::remove_file(&path);

        {
            let store = MovieStore::open(&path).expect("open");
            store.cache_movie(1, "en-US", &sample_details(1), 0, 3600).expect("write");
        }

        let store = MovieStore::open(&path).expect("reopen");
        assert!(store.cached_movie(1, "en-US", 1).expect("read").is_some());
    }
}
