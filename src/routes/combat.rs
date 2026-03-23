use axum::{extract::{Path, State}, http::StatusCode, routing::{get, post}, Json, Router};
use axum_extra::extract::PrivateCookieJar;
use serde_json::Value;

use crate::{
    auth::require_user,
    db::{combat_entry_from_row, deserialize_payload, merge_json, serialize_payload, CombatEntryRow, CombatSessionRow},
    error::ApiError,
    models::{CombatEntry, CombatSession, CreateCombatSession, NewCombatEntry, UpdateCombatSession},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/{id}", get(get_session).patch(update_session).delete(delete_session))
        .route("/sessions/{id}/entries", post(create_entry))
        .route("/entries/{id}", axum::routing::patch(update_entry).delete(delete_entry))
}

pub async fn list_sessions(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
) -> Result<Json<Vec<CombatSession>>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let rows = sqlx::query_as::<_, CombatSessionRow>(
        "SELECT id, user_id, name, round, active_index, started, created_at, updated_at FROM combat_sessions WHERE user_id = ? ORDER BY updated_at DESC",
    )
    .bind(&user.id)
    .fetch_all(&state.pool)
    .await?;

    let mut sessions = Vec::with_capacity(rows.len());
    for row in rows {
        sessions.push(build_session(&state, row).await?);
    }

    Ok(Json(sessions))
}

pub async fn create_session(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Json(input): Json<CreateCombatSession>,
) -> Result<(StatusCode, Json<CombatSession>), ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let session_id = uuid::Uuid::new_v4().to_string();
    let now = crate::models::now_iso();

    sqlx::query(
        "INSERT INTO combat_sessions (id, user_id, name, round, active_index, started, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&session_id)
    .bind(&user.id)
    .bind(&input.name)
    .bind(1)
    .bind(0)
    .bind(false)
    .bind(&now)
    .bind(&now)
    .execute(&state.pool)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(CombatSession {
            id: session_id,
            name: input.name,
            entries: vec![],
            round: 1,
            active_index: 0,
            started: false,
            created_at: now.clone(),
            updated_at: now,
        }),
    ))
}

pub async fn get_session(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
) -> Result<Json<CombatSession>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let row = owned_session_row(&state, &user.id, &id).await?;
    Ok(Json(build_session(&state, row).await?))
}

pub async fn update_session(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
    Json(patch): Json<UpdateCombatSession>,
) -> Result<Json<CombatSession>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let current = owned_session_row(&state, &user.id, &id).await?;
    let updated_at = crate::models::now_iso();
    let name = patch.name.or(current.name);
    let round = patch.round.unwrap_or(current.round);
    let active_index = patch.active_index.unwrap_or(current.active_index);
    let started = patch.started.unwrap_or(current.started);

    sqlx::query(
        "UPDATE combat_sessions SET name = ?, round = ?, active_index = ?, started = ?, updated_at = ? WHERE id = ? AND user_id = ?",
    )
    .bind(&name)
    .bind(round)
    .bind(active_index)
    .bind(started)
    .bind(&updated_at)
    .bind(&id)
    .bind(&user.id)
    .execute(&state.pool)
    .await?;

    Ok(Json(build_session(
        &state,
        CombatSessionRow {
            id: current.id,
            user_id: current.user_id,
            name,
            round,
            active_index,
            started,
            created_at: current.created_at,
            updated_at,
        },
    )
    .await?))
}

pub async fn delete_session(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let result = sqlx::query("DELETE FROM combat_sessions WHERE id = ? AND user_id = ?")
        .bind(&id)
        .bind(&user.id)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("Combat session not found"));
    }

    Ok(StatusCode::NO_CONTENT)
}

pub async fn create_entry(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(session_id): Path<String>,
    Json(input): Json<NewCombatEntry>,
) -> Result<(StatusCode, Json<CombatEntry>), ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let _session = owned_session_row(&state, &user.id, &session_id).await?;
    let entry = input.into_entry();
    let now = crate::models::now_iso();

    sqlx::query(
        "INSERT INTO combat_entries (id, session_id, payload, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&entry.id)
    .bind(&session_id)
    .bind(serialize_payload(&entry)?)
    .bind(&now)
    .bind(&now)
    .execute(&state.pool)
    .await?;

    touch_session(&state, &session_id).await?;
    Ok((StatusCode::CREATED, Json(entry)))
}

pub async fn update_entry(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
    Json(patch): Json<Value>,
) -> Result<Json<CombatEntry>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let row = owned_entry_row(&state, &user.id, &id).await?;
    let current: CombatEntry = deserialize_payload(&row.payload)?;
    let mut updated: CombatEntry = merge_json(&current, patch)?;
    updated.id = current.id;

    let updated_at = crate::models::now_iso();
    sqlx::query("UPDATE combat_entries SET payload = ?, updated_at = ? WHERE id = ?")
        .bind(serialize_payload(&updated)?)
        .bind(&updated_at)
        .bind(&id)
        .execute(&state.pool)
        .await?;

    touch_session(&state, &row.session_id).await?;
    Ok(Json(updated))
}

pub async fn delete_entry(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let row = owned_entry_row(&state, &user.id, &id).await?;

    let result = sqlx::query("DELETE FROM combat_entries WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("Combat entry not found"));
    }

    touch_session(&state, &row.session_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn build_session(state: &AppState, row: CombatSessionRow) -> Result<CombatSession, ApiError> {
    let entry_rows = sqlx::query_as::<_, CombatEntryRow>(
        "SELECT session_id, payload FROM combat_entries WHERE session_id = ? ORDER BY created_at ASC",
    )
    .bind(&row.id)
    .fetch_all(&state.pool)
    .await?;

    let entries = entry_rows
        .iter()
        .map(combat_entry_from_row)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(CombatSession {
        id: row.id,
        name: row.name,
        entries,
        round: row.round,
        active_index: row.active_index,
        started: row.started,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

async fn owned_session_row(state: &AppState, user_id: &str, session_id: &str) -> Result<CombatSessionRow, ApiError> {
    sqlx::query_as::<_, CombatSessionRow>(
        "SELECT id, user_id, name, round, active_index, started, created_at, updated_at FROM combat_sessions WHERE id = ? AND user_id = ?",
    )
    .bind(session_id)
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::not_found("Combat session not found"))
}

async fn owned_entry_row(state: &AppState, user_id: &str, entry_id: &str) -> Result<CombatEntryRow, ApiError> {
    sqlx::query_as::<_, CombatEntryRow>(
        "SELECT combat_entries.session_id, combat_entries.payload
         FROM combat_entries
         INNER JOIN combat_sessions ON combat_sessions.id = combat_entries.session_id
         WHERE combat_entries.id = ? AND combat_sessions.user_id = ?",
    )
    .bind(entry_id)
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::not_found("Combat entry not found"))
}

async fn touch_session(state: &AppState, session_id: &str) -> Result<(), ApiError> {
    let now = crate::models::now_iso();
    sqlx::query("UPDATE combat_sessions SET updated_at = ? WHERE id = ?")
        .bind(&now)
        .bind(session_id)
        .execute(&state.pool)
        .await?;
    Ok(())
}
