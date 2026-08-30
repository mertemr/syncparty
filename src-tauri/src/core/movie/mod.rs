//! Movie metadata: search/browse via TMDB, and local caching/history.
//!
//! `MovieProvider` is the boundary nothing outside this module reaches past —
//! commands and the movie-vote domain talk to it, never to `reqwest` or to
//! TMDB's response shapes directly. Only one implementation exists today
//! ([`tmdb::TmdbClient`]), but the trait is what would let a second provider
//! sit alongside it later without touching a caller.

mod store;
mod tmdb;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::core::error::Result;

pub use store::{MovieStore, PartyLogEntry, SessionHistoryEntry, UserMovie, WatchedMovie};
pub use tmdb::TmdbClient;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct Genre {
    pub id: i32,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MovieSummary {
    pub tmdb_id: i64,
    pub title: String,
    pub original_title: String,
    pub poster: Option<String>,
    pub backdrop: Option<String>,
    pub release_date: Option<String>,
    pub overview: String,
    pub genre_ids: Vec<i32>,
    pub rating: f64,
    pub vote_count: i64,
    pub popularity: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MovieVideo {
    pub key: String,
    pub site: String,
    pub kind: String,
    pub official: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct WatchProvider {
    pub name: String,
    pub logo: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct MovieDetails {
    pub tmdb_id: i64,
    pub title: String,
    pub original_title: String,
    pub poster: Option<String>,
    pub backdrop: Option<String>,
    pub release_date: Option<String>,
    pub runtime_minutes: Option<i32>,
    pub genres: Vec<Genre>,
    pub rating: f64,
    pub vote_count: i64,
    pub overview: String,
    pub trailer: Option<MovieVideo>,
    /// Never used to stream anything — only to point a person at wherever
    /// TMDB (via JustWatch) says the movie is available, per JustWatch's
    /// attribution requirement.
    pub watch_providers: Vec<WatchProvider>,
    pub watch_link: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverFilter {
    pub genre_id: Option<i32>,
    pub year: Option<i32>,
    /// A TMDB `sort_by` value verbatim (e.g. `"popularity.desc"`) — passed
    /// straight through rather than modelled, since the set TMDB accepts
    /// changes independently of this app.
    pub sort_by: Option<String>,
    pub min_rating: Option<f64>,
}

#[async_trait]
pub trait MovieProvider: Send + Sync {
    async fn search_movies(
        &self,
        query: &str,
        language: &str,
        page: u32,
    ) -> Result<Vec<MovieSummary>>;
    async fn popular_movies(&self, language: &str, page: u32) -> Result<Vec<MovieSummary>>;
    async fn now_playing_movies(&self, language: &str, page: u32) -> Result<Vec<MovieSummary>>;
    async fn upcoming_movies(&self, language: &str, page: u32) -> Result<Vec<MovieSummary>>;
    async fn top_rated_movies(&self, language: &str, page: u32) -> Result<Vec<MovieSummary>>;
    async fn discover_movies(
        &self,
        filter: &DiscoverFilter,
        language: &str,
        page: u32,
    ) -> Result<Vec<MovieSummary>>;
    async fn genres(&self, language: &str) -> Result<Vec<Genre>>;
    async fn movie_details(&self, tmdb_id: i64, language: &str) -> Result<MovieDetails>;
}
