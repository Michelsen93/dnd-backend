//! Game sessions: the spine of a campaign (start → play → end with a published log).

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, patch, post},
};
use axum_extra::extract::PrivateCookieJar;
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{FromRow, SqlitePool};

use crate::{
    access::campaign_access, auth::require_user, error::ApiError, models::now_iso, repo,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/{id}/sessions", get(list).post(start))
        .route("/{id}/sessions/{session_id}", patch(update_log))
        .route("/{id}/sessions/{session_id}/end", post(end))
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    pub id: String,
    pub number: i64,
    pub started_at: String,
    pub ended_at: Option<String>,
    /// { title, summary, nextSteps, loot, highlights }
    pub log: Value,
}

#[derive(FromRow)]
struct SessionRow {
    id: String,
    number: i64,
    started_at: String,
    ended_at: Option<String>,
    payload: String,
}

impl SessionRow {
    fn into_record(self) -> SessionRecord {
        SessionRecord {
            id: self.id,
            number: self.number,
            started_at: self.started_at,
            ended_at: self.ended_at,
            log: serde_json::from_str(&self.payload).unwrap_or_else(|_| json!({})),
        }
    }
}

pub async fn list_sessions_for(
    pool: &SqlitePool,
    campaign_id: &str,
) -> Result<Vec<SessionRecord>, ApiError> {
    let rows = sqlx::query_as::<_, SessionRow>(
        "SELECT id, number, started_at, ended_at, payload FROM game_sessions WHERE campaign_id = ? ORDER BY number DESC",
    )
    .bind(campaign_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(SessionRow::into_record).collect())
}

async fn load_session(
    pool: &SqlitePool,
    campaign_id: &str,
    session_id: &str,
) -> Result<SessionRecord, ApiError> {
    sqlx::query_as::<_, SessionRow>(
        "SELECT id, number, started_at, ended_at, payload FROM game_sessions WHERE id = ? AND campaign_id = ?",
    )
    .bind(session_id)
    .bind(campaign_id)
    .fetch_optional(pool)
    .await?
    .map(SessionRow::into_record)
    .ok_or_else(|| ApiError::not_found("Session not found"))
}

async fn list(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
) -> Result<Json<Vec<SessionRecord>>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    campaign_access(&state.pool, &id, &user.id).await?;
    Ok(Json(list_sessions_for(&state.pool, &id).await?))
}

async fn start(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<SessionRecord>), ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    campaign_access(&state.pool, &id, &user.id)
        .await?
        .require_dm()?;

    let mut table = repo::load_table_state(&state.pool, &id).await?;
    if table.session_id.is_some() {
        return Err(ApiError::conflict("A session is already running"));
    }

    let number = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(MAX(number), 0) + 1 FROM game_sessions WHERE campaign_id = ?",
    )
    .bind(&id)
    .fetch_one(&state.pool)
    .await?;
    let record = SessionRecord {
        id: uuid::Uuid::new_v4().to_string(),
        number,
        started_at: now_iso(),
        ended_at: None,
        log: json!({ "title": format!("Session {number}") }),
    };
    sqlx::query("INSERT INTO game_sessions (id, campaign_id, number, started_at, payload) VALUES (?, ?, ?, ?, ?)")
        .bind(&record.id)
        .bind(&id)
        .bind(record.number)
        .bind(&record.started_at)
        .bind(serde_json::to_string(&record.log)?)
        .execute(&state.pool)
        .await?;

    table.session_id = Some(record.id.clone());
    repo::save_table_state(&state.pool, &id, &table).await?;
    repo::record_event(
        &state,
        &id,
        Some(&user.id),
        "session",
        "public",
        json!({ "text": format!("Session {number} begins!"), "phase": "start" }),
    )
    .await?;
    state.notify(&id, "table");
    Ok((StatusCode::CREATED, Json(record)))
}

async fn end(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path((id, session_id)): Path<(String, String)>,
    Json(log): Json<Value>,
) -> Result<Json<SessionRecord>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    campaign_access(&state.pool, &id, &user.id)
        .await?
        .require_dm()?;
    let mut session = load_session(&state.pool, &id, &session_id).await?;
    if !log.is_object() {
        return Err(ApiError::bad_request("Session log must be a JSON object"));
    }

    // The closing event belongs to the session it ends.
    repo::record_event(
        &state,
        &id,
        Some(&user.id),
        "session",
        "public",
        json!({ "text": format!("Session {} ends.", session.number), "phase": "end" }),
    )
    .await?;

    session.ended_at = Some(now_iso());
    merge_object(&mut session.log, log);
    crate::limits::ensure_size(
        &session.log,
        crate::limits::MAX_SESSION_LOG_BYTES,
        "Session log",
    )?;
    sqlx::query("UPDATE game_sessions SET ended_at = ?, payload = ? WHERE id = ?")
        .bind(&session.ended_at)
        .bind(serde_json::to_string(&session.log)?)
        .bind(&session.id)
        .execute(&state.pool)
        .await?;

    let mut table = repo::load_table_state(&state.pool, &id).await?;
    if table.session_id.as_deref() == Some(session.id.as_str()) {
        table.session_id = None;
        table.combat = None;
        table.spotlight = None;
        repo::save_table_state(&state.pool, &id, &table).await?;
    }
    state.notify(&id, "table");
    Ok(Json(session))
}

async fn update_log(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path((id, session_id)): Path<(String, String)>,
    Json(log): Json<Value>,
) -> Result<Json<SessionRecord>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    campaign_access(&state.pool, &id, &user.id)
        .await?
        .require_dm()?;
    let mut session = load_session(&state.pool, &id, &session_id).await?;
    if !log.is_object() {
        return Err(ApiError::bad_request("Session log must be a JSON object"));
    }
    merge_object(&mut session.log, log);
    crate::limits::ensure_size(
        &session.log,
        crate::limits::MAX_SESSION_LOG_BYTES,
        "Session log",
    )?;
    sqlx::query("UPDATE game_sessions SET payload = ? WHERE id = ?")
        .bind(serde_json::to_string(&session.log)?)
        .bind(&session.id)
        .execute(&state.pool)
        .await?;
    state.notify(&id, "sessions");
    Ok(Json(session))
}

fn merge_object(target: &mut Value, patch: Value) {
    if let (Value::Object(target), Value::Object(patch)) = (target, patch) {
        target.extend(patch);
    }
}
