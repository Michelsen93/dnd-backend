use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use axum_extra::extract::PrivateCookieJar;

use crate::{
    auth::require_user,
    db::{PayloadRow, deserialize_payload, serialize_payload},
    error::ApiError,
    limits,
    models::{NewNote, Note, UpdateNote},
    state::AppState,
};

pub async fn list_for_character(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(character_id): Path<String>,
) -> Result<Json<Vec<Note>>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    ensure_character_access(&state, &user.id, &character_id).await?;

    let rows = sqlx::query_as::<_, PayloadRow>(
        "SELECT payload FROM notes WHERE user_id = ? AND character_id = ? ORDER BY updated_at DESC",
    )
    .bind(&user.id)
    .bind(&character_id)
    .fetch_all(&state.pool)
    .await?;

    let notes = rows
        .into_iter()
        .map(|row| deserialize_payload::<Note>(&row.payload))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(Json(notes))
}

pub async fn create_for_character(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(character_id): Path<String>,
    Json(input): Json<NewNote>,
) -> Result<(StatusCode, Json<Note>), ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    ensure_character_access(&state, &user.id, &character_id).await?;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notes WHERE character_id = ?")
        .bind(&character_id)
        .fetch_one(&state.pool)
        .await?;
    limits::ensure_quota(
        count,
        limits::MAX_NOTES_PER_CHARACTER,
        "journal entries per character",
    )?;

    let note = input.into_note(character_id.clone());
    limits::ensure_size(&note, limits::MAX_NOTE_BYTES, "Journal entry")?;
    sqlx::query(
        "INSERT INTO notes (id, user_id, character_id, payload, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&note.id)
    .bind(&user.id)
    .bind(&character_id)
    .bind(serialize_payload(&note)?)
    .bind(&note.created_at)
    .bind(&note.updated_at)
    .execute(&state.pool)
    .await?;

    Ok((StatusCode::CREATED, Json(note)))
}

pub async fn update(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
    Json(patch): Json<UpdateNote>,
) -> Result<Json<Note>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let row =
        sqlx::query_as::<_, PayloadRow>("SELECT payload FROM notes WHERE id = ? AND user_id = ?")
            .bind(&id)
            .bind(&user.id)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(|| ApiError::not_found("Note not found"))?;

    let mut note: Note = deserialize_payload(&row.payload)?;
    if let Some(title) = patch.title {
        note.title = title;
    }
    if let Some(content) = patch.content {
        note.content = content;
    }
    note.updated_at = crate::models::now_iso();
    limits::ensure_size(&note, limits::MAX_NOTE_BYTES, "Journal entry")?;

    sqlx::query("UPDATE notes SET payload = ?, updated_at = ? WHERE id = ? AND user_id = ?")
        .bind(serialize_payload(&note)?)
        .bind(&note.updated_at)
        .bind(&id)
        .bind(&user.id)
        .execute(&state.pool)
        .await?;

    Ok(Json(note))
}

pub async fn delete_note(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let result = sqlx::query("DELETE FROM notes WHERE id = ? AND user_id = ?")
        .bind(&id)
        .bind(&user.id)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("Note not found"));
    }

    Ok(StatusCode::NO_CONTENT)
}

async fn ensure_character_access(
    state: &AppState,
    user_id: &str,
    character_id: &str,
) -> Result<(), ApiError> {
    let exists =
        sqlx::query_scalar::<_, String>("SELECT id FROM characters WHERE id = ? AND user_id = ?")
            .bind(character_id)
            .bind(user_id)
            .fetch_optional(&state.pool)
            .await?;

    if exists.is_none() {
        return Err(ApiError::not_found("Character not found"));
    }

    Ok(())
}
