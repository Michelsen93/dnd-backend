use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use axum_extra::extract::PrivateCookieJar;
use serde_json::Value;

use crate::{
    auth::require_user,
    db::{PayloadRow, character_from_row, deserialize_payload, merge_json, serialize_payload},
    error::ApiError,
    models::{Character, NewCharacter},
    state::AppState,
};

pub async fn list(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
) -> Result<Json<Vec<Character>>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let rows = sqlx::query_as::<_, PayloadRow>(
        "SELECT payload FROM characters WHERE user_id = ? ORDER BY updated_at DESC",
    )
    .bind(&user.id)
    .fetch_all(&state.pool)
    .await?;

    let characters = rows
        .iter()
        .map(character_from_row)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Json(characters))
}

pub async fn get_one(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
) -> Result<Json<Character>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let row = sqlx::query_as::<_, PayloadRow>(
        "SELECT payload FROM characters WHERE id = ? AND user_id = ?",
    )
    .bind(&id)
    .bind(&user.id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::not_found("Character not found"))?;

    Ok(Json(character_from_row(&row)?))
}

pub async fn create(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Json(input): Json<NewCharacter>,
) -> Result<(StatusCode, Json<Character>), ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let character = input.into_character();
    let payload = serialize_payload(&character)?;

    sqlx::query(
        "INSERT INTO characters (id, user_id, payload, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&character.id)
    .bind(&user.id)
    .bind(payload)
    .bind(&character.created_at)
    .bind(&character.updated_at)
    .execute(&state.pool)
    .await?;

    Ok((StatusCode::CREATED, Json(character)))
}

pub async fn update(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
    Json(patch): Json<Value>,
) -> Result<Json<Character>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let row = sqlx::query_as::<_, PayloadRow>(
        "SELECT payload FROM characters WHERE id = ? AND user_id = ?",
    )
    .bind(&id)
    .bind(&user.id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::not_found("Character not found"))?;

    let current: Character = deserialize_payload(&row.payload)?;
    let mut updated: Character = merge_json(&current, patch)?;
    updated.id = current.id;
    updated.created_at = current.created_at;
    updated.updated_at = crate::models::now_iso();

    sqlx::query("UPDATE characters SET payload = ?, updated_at = ? WHERE id = ? AND user_id = ?")
        .bind(serialize_payload(&updated)?)
        .bind(&updated.updated_at)
        .bind(&id)
        .bind(&user.id)
        .execute(&state.pool)
        .await?;

    sqlx::query(
        "UPDATE campaign_members SET character_name = ?, sprite_key = ? WHERE character_id = ?",
    )
    .bind(&updated.name)
    .bind(&updated.sprite_key)
    .bind(&id)
    .execute(&state.pool)
    .await?;
    crate::repo::notify_character_campaigns(&state, &id).await?;

    Ok(Json(updated))
}

pub async fn delete_character(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    crate::repo::notify_character_campaigns(&state, &id).await?;
    let result = sqlx::query("DELETE FROM characters WHERE id = ? AND user_id = ?")
        .bind(&id)
        .bind(&user.id)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("Character not found"));
    }

    Ok(StatusCode::NO_CONTENT)
}
