use axum::extract::FromRef;
use axum_extra::extract::cookie::Key;
use sqlx::SqlitePool;
use tokio::sync::broadcast;

use crate::config::AppConfig;

/// Something changed in a campaign; SSE subscribers of that campaign refetch.
#[derive(Clone, Debug)]
pub struct CampaignSignal {
    pub campaign_id: String,
    pub kind: String,
}

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub cookie_key: Key,
    pub config: AppConfig,
    pub campaign_tx: broadcast::Sender<CampaignSignal>,
}

impl AppState {
    pub fn new(pool: SqlitePool, cookie_key: Key, config: AppConfig) -> Self {
        let (campaign_tx, _rx) = broadcast::channel(256);
        Self {
            pool,
            cookie_key,
            config,
            campaign_tx,
        }
    }

    pub fn notify(&self, campaign_id: &str, kind: &str) {
        let _ = self.campaign_tx.send(CampaignSignal {
            campaign_id: campaign_id.to_string(),
            kind: kind.to_string(),
        });
    }
}

impl FromRef<AppState> for Key {
    fn from_ref(state: &AppState) -> Self {
        state.cookie_key.clone()
    }
}
