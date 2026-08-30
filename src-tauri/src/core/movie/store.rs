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

use super::{MovieDetails, MovieSummary};

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

/// One person's own marks on a movie — a favourite, a "seen it", or both.
///
/// The summary is stored alongside the flags rather than looked up again:
/// a favourites list or a watched grid is a list of posters, and re-fetching
/// every one of them from TMDB to draw a screen the user has already seen
/// turns a local list into a network round trip per movie.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct UserMovie {
    pub movie: MovieSummary,
    pub favorite: bool,
    pub watched: bool,
    /// When the flags last changed. Sorts a list newest first.
    pub marked_at: i64,
}

/// One night: when it ran, what was on, and who was in the room.
///
/// Separate from `SessionHistoryEntry`, which is a *vote's* record — a party
/// can run without a vote ever being held, and a vote's clock starts when the
/// ballot was drafted rather than when anyone actually sat down.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct PartyLogEntry {
    pub id: String,
    pub started_at: i64,
    /// `None` while the party is still running.
    pub ended_at: Option<i64>,
    pub movie_tmdb_id: Option<i64>,
    pub movie_title: Option<String>,
    pub movie_poster: Option<String>,
    /// Everyone seen in the room over the whole night, not just whoever was
    /// still there at the end — people drop out of a call and come back.
    pub participants: Vec<String>,
}

/// Every schema step in order. The list is the migration: a new one is
/// appended and nothing else needs touching, which is one fewer thing to get
/// wrong than a version constant kept in step with it by hand.
const MIGRATIONS: [&str; 3] = [SCHEMA_V1, SCHEMA_V2, SCHEMA_V3];

const SCHEMA_V1: &str = "
    CREATE TABLE IF NOT EXISTS movie_cache (
        tmdb_id INTEGER NOT NULL,
        language TEXT NOT NULL,
        payload TEXT NOT NULL,
        fetched_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL,
        PRIMARY KEY (tmdb_id, language)
    );

    CREATE TABLE IF NOT EXISTS session_history (
        id TEXT PRIMARY KEY,
        started_at INTEGER NOT NULL,
        ended_at INTEGER,
        payload TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS watched_movies (
        tmdb_id INTEGER NOT NULL,
        session_id TEXT NOT NULL,
        title TEXT NOT NULL,
        watched_at INTEGER NOT NULL,
        participants TEXT NOT NULL,
        PRIMARY KEY (tmdb_id, session_id)
    );
";

/// Favourites and manual "seen it" marks. Both used to live in the
/// webview's `localStorage`, which is neither backed up with the rest of the
/// app's data nor readable by anything but the window that wrote it.
const SCHEMA_V2: &str = "
    CREATE TABLE IF NOT EXISTS user_movies (
        tmdb_id INTEGER PRIMARY KEY,
        favorite INTEGER NOT NULL,
        watched INTEGER NOT NULL,
        payload TEXT NOT NULL,
        marked_at INTEGER NOT NULL
    );
";

/// The party log. `user_movies` (v2) is one person's marks on a movie; this
/// is the record of an evening, which nothing until now wrote down.
const SCHEMA_V3: &str = "
    CREATE TABLE IF NOT EXISTS party_sessions (
        id TEXT PRIMARY KEY,
        started_at INTEGER NOT NULL,
        ended_at INTEGER,
        movie_tmdb_id INTEGER,
        movie_title TEXT,
        movie_poster TEXT,
        participants TEXT NOT NULL
    );
";

/// Brings the database up to the last step in [`MIGRATIONS`], one at a time.
///
/// Each step commits its own schema *and* its own version number in a single
/// transaction. Bumping the version once at the end instead — which is what
/// this did — is not the same thing: a step that lands its tables and then
/// fails to record that it ran leaves a database whose schema is ahead of its
/// version, and the next launch re-runs `CREATE TABLE` against tables that
/// are already there. That is a permanent crash on startup, not a retry,
/// which is exactly what shipping v3 did to anyone who had v1 on disk.
///
/// `IF NOT EXISTS` on every table is the second half of the same guarantee:
/// it lets a database already caught out that way heal on the next launch
/// rather than needing to be deleted.
fn migrate(conn: &Connection) -> Result<()> {
    let version: i32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    for (index, schema) in MIGRATIONS.iter().enumerate() {
        let target = index as i32 + 1;
        if version >= target {
            continue;
        }

        // `PRAGMA user_version` is part of the database header, so it is
        // written transactionally along with the tables above it.
        conn.execute_batch(&format!(
            "BEGIN;{schema}PRAGMA user_version = {target};COMMIT;"
        ))?;
    }

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
        self.conn
            .lock()
            .expect("the movie store connection mutex is never poisoned")
    }

    /// A cached movie's details, if one is on file and has not expired.
    pub fn cached_movie(
        &self,
        tmdb_id: i64,
        language: &str,
        now: i64,
    ) -> Result<Option<MovieDetails>> {
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

    /// Every movie the user has marked, most recently marked first.
    pub fn list_user_movies(&self) -> Result<Vec<UserMovie>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT favorite, watched, payload, marked_at FROM user_movies ORDER BY marked_at DESC",
        )?;

        let rows = statement.query_map([], |row| {
            let favorite: i64 = row.get(0)?;
            let watched: i64 = row.get(1)?;
            let payload: String = row.get(2)?;
            let marked_at: i64 = row.get(3)?;
            Ok((favorite != 0, watched != 0, payload, marked_at))
        })?;

        let mut movies = Vec::new();
        for row in rows {
            let (favorite, watched, payload, marked_at) = row?;
            movies.push(UserMovie {
                movie: serde_json::from_str(&payload)?,
                favorite,
                watched,
                marked_at,
            });
        }
        Ok(movies)
    }

    /// Sets both flags for one movie. Clearing both deletes the row rather
    /// than keeping a record that says nothing — the movie is simply not
    /// marked any more, and the summary is re-fetchable at any time.
    pub fn set_user_movie(
        &self,
        movie: &MovieSummary,
        favorite: bool,
        watched: bool,
        now: i64,
    ) -> Result<()> {
        if !favorite && !watched {
            self.conn().execute(
                "DELETE FROM user_movies WHERE tmdb_id = ?1",
                params![movie.tmdb_id],
            )?;
            return Ok(());
        }

        let payload = serde_json::to_string(movie)?;
        self.conn().execute(
            "INSERT INTO user_movies (tmdb_id, favorite, watched, payload, marked_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (tmdb_id) DO UPDATE SET
                favorite = excluded.favorite,
                watched = excluded.watched,
                payload = excluded.payload,
                marked_at = excluded.marked_at",
            params![movie.tmdb_id, favorite as i64, watched as i64, payload, now],
        )?;
        Ok(())
    }

    /// Starts a party's record. Called the moment hosting comes up, so a
    /// night that is never given a movie still leaves a trace of having
    /// happened.
    pub fn open_party_log(&self, id: &str, started_at: i64) -> Result<()> {
        self.conn().execute(
            "INSERT INTO party_sessions (id, started_at, ended_at, participants)
             VALUES (?1, ?2, NULL, '[]')
             ON CONFLICT (id) DO NOTHING",
            params![id, started_at],
        )?;
        Ok(())
    }

    /// Sets what the room is watching. Overwrites — changing your mind
    /// halfway through the evening is the normal case, not an error.
    pub fn set_party_movie(
        &self,
        id: &str,
        tmdb_id: Option<i64>,
        title: Option<&str>,
        poster: Option<&str>,
    ) -> Result<()> {
        self.conn().execute(
            "UPDATE party_sessions
             SET movie_tmdb_id = ?2, movie_title = ?3, movie_poster = ?4
             WHERE id = ?1",
            params![id, tmdb_id, title, poster],
        )?;
        Ok(())
    }

    /// Closes a party's record with its end time and its roster.
    pub fn close_party_log(&self, id: &str, ended_at: i64, participants: &[String]) -> Result<()> {
        let participants = serde_json::to_string(participants)?;
        self.conn().execute(
            "UPDATE party_sessions SET ended_at = ?2, participants = ?3 WHERE id = ?1",
            params![id, ended_at, participants],
        )?;
        Ok(())
    }

    /// Every night on file, most recent first.
    pub fn list_party_logs(&self) -> Result<Vec<PartyLogEntry>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT id, started_at, ended_at, movie_tmdb_id, movie_title, movie_poster, participants
             FROM party_sessions ORDER BY started_at DESC",
        )?;

        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?;

        let mut entries = Vec::new();
        for row in rows {
            let (id, started_at, ended_at, movie_tmdb_id, movie_title, movie_poster, participants) =
                row?;
            entries.push(PartyLogEntry {
                id,
                started_at,
                ended_at,
                movie_tmdb_id,
                movie_title,
                movie_poster,
                participants: serde_json::from_str(&participants)?,
            });
        }
        Ok(entries)
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

        assert!(store
            .cached_movie(27205, "en-US", 0)
            .expect("read")
            .is_none());

        store
            .cache_movie(27205, "en-US", &details, 0, 3600)
            .expect("write");
        let cached = store
            .cached_movie(27205, "en-US", 100)
            .expect("read")
            .expect("hit");

        assert_eq!(cached, details);
    }

    #[test]
    fn an_expired_cache_entry_is_not_returned() {
        let store = store_at("cache-expiry");
        let details = sample_details(1);

        store
            .cache_movie(1, "en-US", &details, 0, 60)
            .expect("write");

        assert!(store.cached_movie(1, "en-US", 61).expect("read").is_none());
    }

    #[test]
    fn caching_the_same_movie_again_overwrites_rather_than_duplicating() {
        let store = store_at("cache-overwrite");
        let mut details = sample_details(1);

        store
            .cache_movie(1, "en-US", &details, 0, 3600)
            .expect("first write");
        details.rating = 9.0;
        store
            .cache_movie(1, "en-US", &details, 0, 3600)
            .expect("second write");

        let cached = store
            .cached_movie(1, "en-US", 1)
            .expect("read")
            .expect("hit");
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
            store
                .cache_movie(1, "en-US", &sample_details(1), 0, 3600)
                .expect("write");
        }

        let store = MovieStore::open(&path).expect("reopen");
        assert!(store.cached_movie(1, "en-US", 1).expect("read").is_some());
    }

    fn sample_summary(tmdb_id: i64) -> MovieSummary {
        MovieSummary {
            tmdb_id,
            title: "Inception".to_owned(),
            original_title: "Inception".to_owned(),
            poster: Some("/poster.jpg".to_owned()),
            backdrop: None,
            release_date: Some("2010-07-16".to_owned()),
            overview: "A thief who steals corporate secrets.".to_owned(),
            genre_ids: vec![28],
            rating: 8.4,
            vote_count: 35000,
            popularity: 1.0,
        }
    }

    #[test]
    fn user_marks_round_trip_newest_first() {
        let store = store_at("user-movies-order");

        store
            .set_user_movie(&sample_summary(1), true, false, 100)
            .expect("write");
        store
            .set_user_movie(&sample_summary(2), false, true, 300)
            .expect("write");

        let marked = store.list_user_movies().expect("read");
        assert_eq!(marked.len(), 2);
        assert_eq!(marked[0].movie.tmdb_id, 2);
        assert!(marked[0].watched && !marked[0].favorite);
        assert_eq!(marked[1].movie.tmdb_id, 1);
        assert!(marked[1].favorite && !marked[1].watched);
        assert_eq!(marked[1].movie.poster.as_deref(), Some("/poster.jpg"));
    }

    #[test]
    fn setting_a_second_flag_updates_the_same_row() {
        let store = store_at("user-movies-update");

        store
            .set_user_movie(&sample_summary(7), true, false, 100)
            .expect("write");
        store
            .set_user_movie(&sample_summary(7), true, true, 200)
            .expect("write");

        let marked = store.list_user_movies().expect("read");
        assert_eq!(marked.len(), 1);
        assert!(marked[0].favorite && marked[0].watched);
        assert_eq!(marked[0].marked_at, 200);
    }

    #[test]
    fn clearing_both_flags_removes_the_row() {
        let store = store_at("user-movies-clear");

        store
            .set_user_movie(&sample_summary(9), true, true, 100)
            .expect("write");
        store
            .set_user_movie(&sample_summary(9), false, false, 200)
            .expect("write");

        assert!(store.list_user_movies().expect("read").is_empty());
    }

    /// The exact state v0.7.0 left on disk: the v2 and v3 tables created, and
    /// a version number that never caught up with them. Every launch after
    /// that re-ran `CREATE TABLE` and panicked before the window opened.
    #[test]
    fn a_database_whose_schema_ran_ahead_of_its_version_still_opens() {
        let path = std::env::temp_dir().join("syncparty-movies-half-migrated.sqlite3");
        let _ = std::fs::remove_file(&path);

        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(SCHEMA_V1).expect("v1");
            conn.execute_batch(SCHEMA_V2).expect("v2");
            conn.execute_batch(SCHEMA_V3).expect("v3");
            conn.pragma_update(None, "user_version", 1)
                .expect("stale version");
        }

        let store = MovieStore::open(&path).expect("a half-migrated database still opens");
        assert!(store.list_user_movies().expect("read").is_empty());
        assert!(store.list_party_logs().expect("read").is_empty());

        let conn = Connection::open(&path).expect("reopen");
        let version: i32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(
            version,
            MIGRATIONS.len() as i32,
            "the version is caught up afterwards"
        );
    }

    #[test]
    fn migrating_from_v1_keeps_what_was_already_there() {
        let path = std::env::temp_dir().join("syncparty-movies-from-v1.sqlite3");
        let _ = std::fs::remove_file(&path);

        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(SCHEMA_V1).expect("v1");
            conn.pragma_update(None, "user_version", 1)
                .expect("version");
            conn.execute(
                "INSERT INTO watched_movies (tmdb_id, session_id, title, watched_at, participants)
                 VALUES (1, 'old', 'Stalker', 10, '[]')",
                [],
            )
            .expect("seed");
        }

        let store = MovieStore::open(&path).expect("open");
        assert_eq!(store.list_watched_movies().expect("read").len(), 1);
        assert!(store.list_party_logs().expect("read").is_empty());
    }
}
