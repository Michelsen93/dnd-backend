use axum::extract::FromRef;
use axum_extra::extract::cookie::Key;
use sqlx::SqlitePool;
use tokio::sync::broadcast;

use crate::config::AppConfig;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub cookie_key: Key,
    pub config: AppConfig,
    pub encounter_sync_tx: broadcast::Sender<String>,
}

impl FromRef<AppState> for Key {
    fn from_ref(state: &AppState) -> Self {
        state.cookie_key.clone()
    }
}
