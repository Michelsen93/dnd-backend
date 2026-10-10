use std::sync::Arc;

use axum::extract::FromRef;
use axum_extra::extract::cookie::Key;
use sqlx::SqlitePool;
use tokio::sync::broadcast;

use crate::{config::AppConfig, firebase::FirebaseVerifier, limits::RateLimiter};

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
    /// Present when FIREBASE_PROJECT_ID is configured.
    pub firebase: Option<Arc<FirebaseVerifier>>,
    pub limiter: Arc<RateLimiter>,
}

impl AppState {
    pub fn new(pool: SqlitePool, cookie_key: Key, config: AppConfig) -> Self {
        let (campaign_tx, _rx) = broadcast::channel(256);
        let firebase = config
            .firebase_project_id
            .clone()
            .map(|project| Arc::new(FirebaseVerifier::new(project)));
        Self {
            pool,
            cookie_key,
            config,
            campaign_tx,
            firebase,
            limiter: Arc::new(RateLimiter::default()),
        }
    }

    /// Swap in a verifier (tests use one with static keys).
    pub fn with_firebase(mut self, verifier: FirebaseVerifier) -> Self {
        self.firebase = Some(Arc::new(verifier));
        self
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
