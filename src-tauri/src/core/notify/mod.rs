//! Announcing a party on Discord.
//!
//! Optional, and off until the user pastes a webhook URL. The URL itself is a
//! secret — anyone holding it can post to the channel — so it lives in the OS
//! keychain like every other credential.

mod message;

use std::sync::Arc;

use crate::core::config::{SecretKey, SecretStore};
use crate::core::error::{Result, SyncPartyError};

pub use message::{
    movie_selected, movie_selected_card, movie_vote_cancelled, movie_vote_completed,
    movie_vote_started, movie_vote_started_card, party_ready, party_stopped, webhook_test,
    PosterCard,
};

pub struct DiscordNotifier {
    secrets: Arc<SecretStore>,
    client: reqwest::Client,
}

impl DiscordNotifier {
    pub fn new(secrets: Arc<SecretStore>) -> Self {
        Self {
            secrets,
            client: reqwest::Client::new(),
        }
    }

    pub fn is_configured(&self) -> bool {
        matches!(self.secrets.get(SecretKey::DiscordWebhook), Ok(Some(_)))
    }

    pub fn set_webhook(&self, url: &str) -> Result<()> {
        let trimmed = url.trim();

        if !trimmed.starts_with("https://") {
            return Err(SyncPartyError::Config(
                "a Discord webhook URL must start with https://".to_owned(),
            ));
        }

        self.secrets.set(SecretKey::DiscordWebhook, trimmed)
    }

    pub fn clear_webhook(&self) -> Result<()> {
        self.secrets.delete(SecretKey::DiscordWebhook)
    }

    /// The app's accent color (`--color-accent` in `src/styles.css`),
    /// converted from OkLCH to sRGB, as the Discord embed color int.
    const ACCENT_COLOR: u32 = 0xff61b7;

    /// Posts `content` to the configured channel as an embed, so it carries
    /// the app's accent color instead of showing up as bare text.
    ///
    /// Returns `false` when no webhook is set, which is a normal state rather
    /// than a failure — most people never turn this on.
    pub async fn send(&self, content: &str) -> Result<bool> {
        self.post(&serde_json::json!({
            "embeds": [{ "description": content, "color": Self::ACCENT_COLOR }]
        }))
        .await
    }

    /// Posts a prepared webhook payload — an embed rather than a line of
    /// text. Same contract as [`send`]: `false` means no webhook is set.
    pub async fn send_payload(&self, payload: &serde_json::Value) -> Result<bool> {
        self.post(payload).await
    }

    async fn post(&self, payload: &serde_json::Value) -> Result<bool> {
        let Some(webhook) = self.secrets.get(SecretKey::DiscordWebhook)? else {
            return Ok(false);
        };

        self.client
            .post(webhook)
            .json(payload)
            .send()
            .await?
            .error_for_status()?;

        Ok(true)
    }
}
