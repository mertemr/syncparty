//! The TMDB v3 [`MovieProvider`] implementation.
//!
//! Everything TMDB-shaped — snake_case field names, `poster_path` fragments,
//! `append_to_response` — stays in this file. Callers only ever see the
//! domain types in [`super`].

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use crate::core::config::{SecretKey, SecretStore};
use crate::core::error::{Result, SyncPartyError};

use super::{
    DiscoverFilter, Genre, MovieDetails, MovieProvider, MovieSummary, MovieVideo, WatchProvider,
};

const BASE_URL: &str = "https://api.themoviedb.org/3";
const IMAGE_BASE: &str = "https://image.tmdb.org/t/p/w500";
const BACKDROP_BASE: &str = "https://image.tmdb.org/t/p/w1280";

pub struct TmdbClient {
    secrets: Arc<SecretStore>,
    client: reqwest::Client,
}

impl TmdbClient {
    pub fn new(secrets: Arc<SecretStore>) -> Self {
        Self {
            secrets,
            client: reqwest::Client::new(),
        }
    }

    pub fn is_configured(&self) -> bool {
        matches!(self.secrets.get(SecretKey::TmdbApiKey), Ok(Some(_)))
    }

    pub fn set_api_key(&self, key: &str) -> Result<()> {
        let trimmed = key.trim();
        if trimmed.is_empty() {
            return Err(SyncPartyError::Config(
                "a TMDB API key cannot be empty".to_owned(),
            ));
        }
        self.secrets.set(SecretKey::TmdbApiKey, trimmed)
    }

    pub fn clear_api_key(&self) -> Result<()> {
        self.secrets.delete(SecretKey::TmdbApiKey)
    }

    fn api_key(&self) -> Result<String> {
        self.secrets
            .get(SecretKey::TmdbApiKey)?
            .ok_or(SyncPartyError::MovieProviderNotConfigured)
    }

    async fn get<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        params: &[(String, String)],
    ) -> Result<T> {
        let mut query = vec![("api_key".to_owned(), self.api_key()?)];
        query.extend_from_slice(params);

        let response = self
            .client
            .get(format!("{BASE_URL}{path}"))
            .query(&query)
            .send()
            .await?
            .error_for_status()?;

        Ok(response.json::<T>().await?)
    }

    async fn list(&self, path: &str, language: &str, page: u32) -> Result<Vec<MovieSummary>> {
        let raw: RawPage<RawMovieSummary> = self
            .get(
                path,
                &[
                    ("language".to_owned(), language.to_owned()),
                    ("page".to_owned(), page.to_string()),
                ],
            )
            .await?;
        Ok(raw.results.into_iter().map(MovieSummary::from).collect())
    }
}

#[async_trait]
impl MovieProvider for TmdbClient {
    async fn search_movies(
        &self,
        query: &str,
        language: &str,
        page: u32,
    ) -> Result<Vec<MovieSummary>> {
        let raw: RawPage<RawMovieSummary> = self
            .get(
                "/search/movie",
                &[
                    ("query".to_owned(), query.to_owned()),
                    ("language".to_owned(), language.to_owned()),
                    ("page".to_owned(), page.to_string()),
                ],
            )
            .await?;
        Ok(raw.results.into_iter().map(MovieSummary::from).collect())
    }

    async fn popular_movies(&self, language: &str, page: u32) -> Result<Vec<MovieSummary>> {
        self.list("/movie/popular", language, page).await
    }

    async fn now_playing_movies(&self, language: &str, page: u32) -> Result<Vec<MovieSummary>> {
        self.list("/movie/now_playing", language, page).await
    }

    async fn upcoming_movies(&self, language: &str, page: u32) -> Result<Vec<MovieSummary>> {
        self.list("/movie/upcoming", language, page).await
    }

    async fn top_rated_movies(&self, language: &str, page: u32) -> Result<Vec<MovieSummary>> {
        self.list("/movie/top_rated", language, page).await
    }

    async fn discover_movies(
        &self,
        filter: &DiscoverFilter,
        language: &str,
        page: u32,
    ) -> Result<Vec<MovieSummary>> {
        let mut query = vec![
            ("language".to_owned(), language.to_owned()),
            ("page".to_owned(), page.to_string()),
        ];
        if let Some(genre) = filter.genre_id {
            query.push(("with_genres".to_owned(), genre.to_string()));
        }
        if let Some(year) = filter.year {
            query.push(("primary_release_year".to_owned(), year.to_string()));
        }
        if let Some(sort_by) = &filter.sort_by {
            query.push(("sort_by".to_owned(), sort_by.clone()));
        }
        if let Some(min_rating) = filter.min_rating {
            query.push(("vote_average.gte".to_owned(), min_rating.to_string()));
        }

        let raw: RawPage<RawMovieSummary> = self.get("/discover/movie", &query).await?;
        Ok(raw.results.into_iter().map(MovieSummary::from).collect())
    }

    async fn genres(&self, language: &str) -> Result<Vec<Genre>> {
        #[derive(Deserialize)]
        struct Wrapper {
            genres: Vec<Genre>,
        }

        let raw: Wrapper = self
            .get(
                "/genre/movie/list",
                &[("language".to_owned(), language.to_owned())],
            )
            .await?;
        Ok(raw.genres)
    }

    async fn movie_details(&self, tmdb_id: i64, language: &str) -> Result<MovieDetails> {
        let raw: RawMovieDetails = self
            .get(
                &format!("/movie/{tmdb_id}"),
                &[
                    ("language".to_owned(), language.to_owned()),
                    (
                        "append_to_response".to_owned(),
                        "videos,watch/providers".to_owned(),
                    ),
                ],
            )
            .await?;

        Ok(MovieDetails::from(raw))
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.is_empty())
}

#[derive(Deserialize)]
struct RawPage<T> {
    results: Vec<T>,
}

#[derive(Deserialize)]
struct RawMovieSummary {
    id: i64,
    title: String,
    original_title: String,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    release_date: Option<String>,
    overview: String,
    #[serde(default)]
    genre_ids: Vec<i32>,
    vote_average: f64,
    vote_count: i64,
    popularity: f64,
}

impl From<RawMovieSummary> for MovieSummary {
    fn from(raw: RawMovieSummary) -> Self {
        MovieSummary {
            tmdb_id: raw.id,
            title: raw.title,
            original_title: raw.original_title,
            poster: raw.poster_path.map(|path| format!("{IMAGE_BASE}{path}")),
            backdrop: raw
                .backdrop_path
                .map(|path| format!("{BACKDROP_BASE}{path}")),
            release_date: non_empty(raw.release_date),
            overview: raw.overview,
            genre_ids: raw.genre_ids,
            rating: raw.vote_average,
            vote_count: raw.vote_count,
            popularity: raw.popularity,
        }
    }
}

#[derive(Deserialize)]
struct RawVideo {
    key: String,
    site: String,
    #[serde(rename = "type")]
    kind: String,
    official: bool,
}

#[derive(Deserialize)]
struct RawVideos {
    results: Vec<RawVideo>,
}

/// Picks the trailer worth showing: an official YouTube trailer first, then
/// any YouTube trailer, then any YouTube video at all — falling back rather
/// than showing nothing just because nobody marked one "official".
fn pick_trailer(videos: Option<RawVideos>) -> Option<MovieVideo> {
    let list = videos?.results;

    let best = list
        .iter()
        .find(|video| video.site == "YouTube" && video.kind == "Trailer" && video.official)
        .or_else(|| {
            list.iter()
                .find(|video| video.site == "YouTube" && video.kind == "Trailer")
        })
        .or_else(|| list.iter().find(|video| video.site == "YouTube"))?;

    Some(MovieVideo {
        key: best.key.clone(),
        site: best.site.clone(),
        kind: best.kind.clone(),
        official: best.official,
    })
}

#[derive(Deserialize)]
struct RawProvider {
    provider_name: String,
    logo_path: Option<String>,
}

#[derive(Deserialize)]
struct RawRegionProviders {
    link: Option<String>,
    #[serde(default)]
    flatrate: Vec<RawProvider>,
}

#[derive(Deserialize)]
struct RawWatchProvidersWrapper {
    results: std::collections::HashMap<String, RawRegionProviders>,
}

/// Picks one region's providers to show. TMDB has no notion of "the user's
/// region" in this response — it returns every region it has data for — so
/// this prefers the US listing, which is the most complete, and otherwise
/// takes whichever region happens to be first rather than showing nothing.
fn pick_watch_providers(
    wrapper: Option<RawWatchProvidersWrapper>,
) -> (Vec<WatchProvider>, Option<String>) {
    let Some(wrapper) = wrapper else {
        return (Vec::new(), None);
    };

    let Some(region) = wrapper
        .results
        .get("US")
        .or_else(|| wrapper.results.values().next())
    else {
        return (Vec::new(), None);
    };

    let providers = region
        .flatrate
        .iter()
        .map(|provider| WatchProvider {
            name: provider.provider_name.clone(),
            logo: provider
                .logo_path
                .clone()
                .map(|path| format!("{IMAGE_BASE}{path}")),
        })
        .collect();

    (providers, region.link.clone())
}

#[derive(Deserialize)]
struct RawMovieDetails {
    id: i64,
    title: String,
    original_title: String,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    release_date: Option<String>,
    runtime: Option<i32>,
    #[serde(default)]
    genres: Vec<Genre>,
    vote_average: f64,
    vote_count: i64,
    overview: String,
    videos: Option<RawVideos>,
    #[serde(rename = "watch/providers")]
    watch_providers: Option<RawWatchProvidersWrapper>,
}

impl From<RawMovieDetails> for MovieDetails {
    fn from(raw: RawMovieDetails) -> Self {
        let (watch_providers, watch_link) = pick_watch_providers(raw.watch_providers);

        MovieDetails {
            tmdb_id: raw.id,
            title: raw.title,
            original_title: raw.original_title,
            poster: raw.poster_path.map(|path| format!("{IMAGE_BASE}{path}")),
            backdrop: raw
                .backdrop_path
                .map(|path| format!("{BACKDROP_BASE}{path}")),
            release_date: non_empty(raw.release_date),
            runtime_minutes: raw.runtime,
            genres: raw.genres,
            rating: raw.vote_average,
            vote_count: raw.vote_count,
            overview: raw.overview,
            trailer: pick_trailer(raw.videos),
            watch_providers,
            watch_link,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_movie_summary_gets_full_image_urls_and_drops_an_empty_release_date() {
        let raw: RawMovieSummary = serde_json::from_str(
            r#"{
                "id": 27205,
                "title": "Inception",
                "original_title": "Inception",
                "poster_path": "/poster.jpg",
                "backdrop_path": null,
                "release_date": "",
                "overview": "A thief who steals corporate secrets.",
                "genre_ids": [28, 878],
                "vote_average": 8.4,
                "vote_count": 35000,
                "popularity": 123.4
            }"#,
        )
        .expect("deserialize");

        let summary = MovieSummary::from(raw);
        assert_eq!(
            summary.poster.as_deref(),
            Some("https://image.tmdb.org/t/p/w500/poster.jpg")
        );
        assert_eq!(summary.backdrop, None);
        assert_eq!(summary.release_date, None, "an empty string is not a date");
        assert_eq!(summary.genre_ids, vec![28, 878]);
    }

    #[test]
    fn a_missing_translation_leaves_the_overview_empty_rather_than_panicking() {
        let raw: RawMovieSummary = serde_json::from_str(
            r#"{
                "id": 1, "title": "Original Title", "original_title": "Original Title",
                "poster_path": null, "backdrop_path": null, "release_date": null,
                "overview": "", "genre_ids": [], "vote_average": 0.0,
                "vote_count": 0, "popularity": 0.0
            }"#,
        )
        .expect("deserialize");

        assert_eq!(MovieSummary::from(raw).overview, "");
    }

    #[test]
    fn genres_deserialize_directly_from_tmdbs_wrapper_shape() {
        let genres: Vec<Genre> =
            serde_json::from_value(serde_json::json!([{"id": 28, "name": "Action"}]))
                .expect("deserialize");

        assert_eq!(genres[0].name, "Action");
    }

    #[test]
    fn an_official_trailer_is_preferred_over_an_unofficial_one() {
        let videos: RawVideos = serde_json::from_str(
            r#"{"results": [
                {"key": "fan-cut", "site": "YouTube", "type": "Trailer", "official": false},
                {"key": "real-trailer", "site": "YouTube", "type": "Trailer", "official": true}
            ]}"#,
        )
        .expect("deserialize");

        let trailer = pick_trailer(Some(videos)).expect("a trailer");
        assert_eq!(trailer.key, "real-trailer");
    }

    #[test]
    fn a_movie_with_no_videos_at_all_has_no_trailer() {
        assert!(pick_trailer(None).is_none());
    }

    #[test]
    fn a_movie_with_only_non_youtube_videos_has_no_trailer() {
        let videos: RawVideos = serde_json::from_str(
            r#"{"results": [{"key": "x", "site": "Vimeo", "type": "Trailer", "official": true}]}"#,
        )
        .expect("deserialize");

        assert!(pick_trailer(Some(videos)).is_none());
    }

    #[test]
    fn watch_providers_prefer_the_us_region() {
        let wrapper: RawWatchProvidersWrapper = serde_json::from_str(
            r#"{"results": {
                "DE": {"link": "https://example.com/de", "flatrate": [{"provider_name": "Netflix DE", "logo_path": null}]},
                "US": {"link": "https://example.com/us", "flatrate": [{"provider_name": "Netflix", "logo_path": "/n.png"}]}
            }}"#,
        )
        .expect("deserialize");

        let (providers, link) = pick_watch_providers(Some(wrapper));
        assert_eq!(providers[0].name, "Netflix");
        assert_eq!(link.as_deref(), Some("https://example.com/us"));
    }

    #[test]
    fn a_movie_with_no_watch_providers_response_has_none_rather_than_erroring() {
        let (providers, link) = pick_watch_providers(None);
        assert!(providers.is_empty());
        assert!(link.is_none());
    }
}
