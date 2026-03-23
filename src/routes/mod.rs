use axum::{routing::get, Json, Router};

use crate::{models::HealthResponse, state::AppState};

pub mod auth;
pub mod campaigns;
pub mod characters;
pub mod combat;
pub mod encounters;
pub mod notes;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .nest("/api/auth", auth::router())
        .route("/api/characters", get(characters::list).post(characters::create))
        .route("/api/characters/{id}", get(characters::get_one).patch(characters::update).delete(characters::delete_character))
        .route("/api/characters/{id}/notes", get(notes::list_for_character).post(notes::create_for_character))
        .route("/api/notes/{id}", axum::routing::patch(notes::update).delete(notes::delete_note))
        .nest("/api/combat", combat::router())
        .nest("/api/encounters", encounters::router())
        .nest("/api/campaigns", campaigns::router())
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
    })
}
