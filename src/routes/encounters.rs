//! Campaign encounters (maps). Only the DM reads or writes them directly; players see the
//! live encounter through the projected table snapshot.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, put},
};
use axum_extra::extract::PrivateCookieJar;

use crate::{
    access::campaign_access, auth::require_user, error::ApiError, models::Encounter, repo,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/{id}/encounters", get(list).post(create))
        .route(
            "/{id}/encounters/{encounter_id}",
            put(replace).delete(remove),
        )
}

async fn list(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
) -> Result<Json<Vec<Encounter>>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    campaign_access(&state.pool, &id, &user.id)
        .await?
        .require_dm()?;
    Ok(Json(repo::list_encounters(&state.pool, &id).await?))
}

fn validate(encounter: &Encounter) -> Result<(), ApiError> {
    if !(1..=40).contains(&encounter.grid_cols) || !(1..=40).contains(&encounter.grid_rows) {
        return Err(ApiError::bad_request(
            "Grid must be between 1 and 40 cells per side",
        ));
    }
    let rows_ok = |rows: usize, cols: Vec<usize>| {
        rows == encounter.grid_rows as usize
            && cols.iter().all(|&c| c == encounter.grid_cols as usize)
    };
    if !rows_ok(
        encounter.terrain.len(),
        encounter.terrain.iter().map(Vec::len).collect(),
    ) || !rows_ok(
        encounter.visibility.len(),
        encounter.visibility.iter().map(Vec::len).collect(),
    ) {
        return Err(ApiError::bad_request(
            "terrain and visibility must match the grid size",
        ));
    }
    Ok(())
}

async fn create(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
    Json(mut encounter): Json<Encounter>,
) -> Result<(StatusCode, Json<Encounter>), ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    campaign_access(&state.pool, &id, &user.id)
        .await?
        .require_dm()?;
    validate(&encounter)?;
    if encounter.id.trim().is_empty() {
        encounter.id = uuid::Uuid::new_v4().to_string();
    }
    encounter.campaign_id = id.clone();
    encounter.revision = 0;
    encounter.created_at = String::new();
    repo::save_encounter(&state.pool, &mut encounter).await?;
    state.notify(&id, "encounters");
    Ok((StatusCode::CREATED, Json(encounter)))
}

/// Full replace with optimistic concurrency: the body's `revision` must match the stored one,
/// otherwise 409 with the current encounter so the client can rebase.
async fn replace(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path((id, encounter_id)): Path<(String, String)>,
    Json(mut encounter): Json<Encounter>,
) -> Result<Response, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    campaign_access(&state.pool, &id, &user.id)
        .await?
        .require_dm()?;
    validate(&encounter)?;
    let current = repo::load_encounter(&state.pool, &id, &encounter_id).await?;
    if current.revision != encounter.revision {
        return Ok((StatusCode::CONFLICT, Json(current)).into_response());
    }
    encounter.id = current.id;
    encounter.campaign_id = id.clone();
    encounter.created_at = current.created_at;
    repo::save_encounter(&state.pool, &mut encounter).await?;
    state.notify(&id, "encounters");
    Ok(Json(encounter).into_response())
}

async fn remove(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path((id, encounter_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    campaign_access(&state.pool, &id, &user.id)
        .await?
        .require_dm()?;
    let result = sqlx::query("DELETE FROM encounters WHERE id = ? AND campaign_id = ?")
        .bind(&encounter_id)
        .bind(&id)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("Encounter not found"));
    }
    let mut table = repo::load_table_state(&state.pool, &id).await?;
    if table.active_encounter_id.as_deref() == Some(encounter_id.as_str()) {
        table.active_encounter_id = None;
        table.combat = None;
        repo::save_table_state(&state.pool, &id, &table).await?;
    }
    state.notify(&id, "encounters");
    Ok(StatusCode::NO_CONTENT)
}
