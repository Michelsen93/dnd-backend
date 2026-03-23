use axum::{
    extract::State,
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    routing::get,
    Json, Router,
};
use axum_extra::extract::PrivateCookieJar;
use serde_json::{json, Value};
use std::{convert::Infallible, time::Duration};
use tokio_stream::{wrappers::BroadcastStream, StreamExt};

use crate::{
    auth::require_user,
    error::ApiError,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/state", get(get_state).patch(set_state))
        .route("/stream", get(stream_updates))
}

pub async fn get_state(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
) -> Result<Json<Value>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;

    let row = sqlx::query_as::<_, crate::db::PayloadRow>(
        "SELECT payload FROM encounter_states WHERE user_id = ?",
    )
    .bind(&user.id)
    .fetch_optional(&state.pool)
    .await?;

    if let Some(row) = row {
        let payload: Value = serde_json::from_str(&row.payload)?;
        return Ok(Json(payload));
    }

    Ok(Json(json!({ "encounters": [] })))
}

pub async fn set_state(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Json(payload): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let user = require_user(&state.pool, &jar).await?;

    let Some(encounters) = payload.get("encounters") else {
        return Err(ApiError::bad_request("Payload must include 'encounters'"));
    };
    if !encounters.is_array() {
        return Err(ApiError::bad_request("'encounters' must be an array"));
    }

    let now = crate::models::now_iso();
    sqlx::query(
        "INSERT INTO encounter_states (user_id, payload, updated_at)
         VALUES (?, ?, ?)
         ON CONFLICT(user_id) DO UPDATE SET payload = excluded.payload, updated_at = excluded.updated_at",
    )
    .bind(&user.id)
    .bind(serde_json::to_string(&payload)?)
    .bind(&now)
    .execute(&state.pool)
    .await?;

    let _ = state.encounter_sync_tx.send(user.id.clone());

    Ok((StatusCode::OK, Json(payload)))
}

pub async fn stream_updates(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let user_id = user.id;
    let rx = state.encounter_sync_tx.subscribe();

    let stream = BroadcastStream::new(rx).filter_map(move |msg| match msg {
        Ok(changed_user_id) if changed_user_id == user_id => {
            Some(Ok(Event::default().event("updated").data("encounters-updated")))
        }
        _ => None,
    });

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    ))
}
