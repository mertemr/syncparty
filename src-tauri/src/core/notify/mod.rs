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
    movie_selected, movie_vote_cancelled, movie_vote_completed, movie_vote_started, party_ready,
    party_stopped, webhook_test,
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

    /// Posts `content` to the configured channel.
    ///
    /// Returns `false` when no webhook is set, which is a normal state rather
    /// than a failure — most people never turn this on.
    pub async fn send(&self, content: &str) -> Result<bool> {
        let Some(webhook) = self.secrets.get(SecretKey::DiscordWebhook)? else {
            return Ok(false);
        };

        self.client
            .post(webhook)
            .json(&serde_json::json!({ "content": content }))
            .send()
            .await?
            .error_for_status()?;

        Ok(true)
    }
}
