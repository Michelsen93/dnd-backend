use axum::{Json, Router, extract::DefaultBodyLimit, middleware, routing::get};

use crate::{limits, models::HealthResponse, state::AppState};

pub mod auth;
pub mod campaigns;
pub mod characters;
pub mod encounters;
pub mod entities;
pub mod notes;
pub mod sessions;
pub mod table;

/// The complete application: routes plus rate limiting and body size limits.
pub fn app(state: AppState) -> Router {
    router()
        .layer(middleware::from_fn_with_state(
            state.clone(),
            limits::rate_limit,
        ))
        .layer(DefaultBodyLimit::max(limits::MAX_BODY_BYTES))
        .with_state(state)
}

pub fn router() -> Router<AppState> {
    let campaigns = campaigns::router()
        .merge(encounters::router())
        .merge(entities::router())
        .merge(sessions::router())
        .merge(table::router());

    Router::new()
        .route("/health", get(health))
        .nest("/api/auth", auth::router())
        .route(
            "/api/characters",
            get(characters::list).post(characters::create),
        )
        .route(
            "/api/characters/{id}",
            get(characters::get_one)
                .patch(characters::update)
                .delete(characters::delete_character),
        )
        .route(
            "/api/characters/{id}/notes",
            get(notes::list_for_character).post(notes::create_for_character),
        )
        .route(
            "/api/notes/{id}",
            axum::routing::patch(notes::update).delete(notes::delete_note),
        )
        .nest("/api/campaigns", campaigns)
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
    })
}
