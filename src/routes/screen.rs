//! Table screens: a TV (or any shared display) paired to a campaign with a short code. It gets a
//! read-only view of what the whole party can see: the fogged map, initiative, party HP,
//! spotlight and the public feed. Nothing DM-only and nothing private ever reaches it.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use axum_extra::extract::PrivateCookieJar;
use rand::{Rng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    access::campaign_access,
    auth::{hash_token, require_user},
    error::ApiError,
    models::TableState,
    projection::project_encounter_for_player,
    repo::{self, EventRecord},
    routes::{
        sessions,
        table::{campaign_stream, hide_unseen_combatants},
    },
    state::AppState,
};

/// Pairing codes are short-lived; paired screens stay paired while used (sliding expiry).
const CODE_TTL_SECS: i64 = 10 * 60;
const SCREEN_IDLE_SECS: i64 = 30 * 24 * 3600;
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const SCREEN_EVENTS: i64 = 30;
/// Character fields a screen may show (a summary, not the whole sheet).
const PARTY_FIELDS: &[&str] = &[
    "id",
    "name",
    "spriteKey",
    "level",
    "classId",
    "race",
    "hitPointsCurrent",
    "hitPointsMax",
    "hitPointsTemp",
    "armorClass",
    "conditions",
    "concentration",
    "deathSaveSuccesses",
    "deathSaveFailures",
];

/// DM-side routes, merged under /api/campaigns.
pub fn campaign_router() -> Router<AppState> {
    Router::new().route(
        "/{id}/screens",
        get(list_screens).post(create_code).delete(revoke_screens),
    )
}

/// Screen-side routes, nested under /api/screen.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/pair", post(pair))
        .route("/{token}/table", get(screen_snapshot))
        .route("/{token}/stream", get(screen_stream))
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

// ── DM: pairing codes ─────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScreenStatus {
    screens: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PairingCode {
    code: String,
    expires_in_seconds: i64,
}

async fn list_screens(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
) -> Result<Json<ScreenStatus>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    campaign_access(&state.pool, &id, &user.id)
        .await?
        .require_dm()?;
    let screens = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM table_screens WHERE campaign_id = ? AND expires_at > ?",
    )
    .bind(&id)
    .bind(now())
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(ScreenStatus { screens }))
}

async fn create_code(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<PairingCode>), ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    campaign_access(&state.pool, &id, &user.id)
        .await?
        .require_dm()?;
    let now = now();
    sqlx::query("DELETE FROM screen_codes WHERE expires_at <= ? OR campaign_id = ?")
        .bind(now)
        .bind(&id)
        .execute(&state.pool)
        .await?;
    // 32^6 ≈ 10^9 codes, live for 10 minutes, with pairing rate-limited per IP.
    let code: String = {
        let mut rng = rand::thread_rng();
        (0..6)
            .map(|_| CODE_ALPHABET[rng.gen_range(0..CODE_ALPHABET.len())] as char)
            .collect()
    };
    sqlx::query("INSERT INTO screen_codes (code, campaign_id, expires_at) VALUES (?, ?, ?)")
        .bind(&code)
        .bind(&id)
        .bind(now + CODE_TTL_SECS)
        .execute(&state.pool)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(PairingCode {
            code,
            expires_in_seconds: CODE_TTL_SECS,
        }),
    ))
}

async fn revoke_screens(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    campaign_access(&state.pool, &id, &user.id)
        .await?
        .require_dm()?;
    sqlx::query("DELETE FROM table_screens WHERE campaign_id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;
    sqlx::query("DELETE FROM screen_codes WHERE campaign_id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;
    state.notify(&id, "screens");
    Ok(StatusCode::NO_CONTENT)
}

// ── Screen: pairing and reading ───────────────────────────────────────────────

#[derive(Deserialize)]
struct PairInput {
    code: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Paired {
    token: String,
    campaign_name: String,
}

async fn pair(
    State(state): State<AppState>,
    Json(input): Json<PairInput>,
) -> Result<(StatusCode, Json<Paired>), ApiError> {
    let code: String = input
        .code
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_uppercase();
    let now = now();
    let campaign_id = sqlx::query_scalar::<_, String>(
        "DELETE FROM screen_codes WHERE code = ? AND expires_at > ? RETURNING campaign_id",
    )
    .bind(&code)
    .bind(now)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::not_found("That code is wrong or has expired"))?;

    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    sqlx::query(
        "INSERT INTO table_screens (id_hash, campaign_id, created_at, last_seen_at, expires_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(hash_token(&token))
    .bind(&campaign_id)
    .bind(now)
    .bind(now)
    .bind(now + SCREEN_IDLE_SECS)
    .execute(&state.pool)
    .await?;
    let campaign_name = sqlx::query_scalar::<_, String>("SELECT name FROM campaigns WHERE id = ?")
        .bind(&campaign_id)
        .fetch_one(&state.pool)
        .await?;
    state.notify(&campaign_id, "screens");
    Ok((
        StatusCode::CREATED,
        Json(Paired {
            token,
            campaign_name,
        }),
    ))
}

/// The campaign a screen token belongs to; touches the sliding expiry at most hourly.
async fn screen_campaign(state: &AppState, token: &str) -> Result<String, ApiError> {
    let hash = hash_token(token);
    let now = now();
    let row = sqlx::query_as::<_, (String, i64)>(
        "SELECT campaign_id, last_seen_at FROM table_screens WHERE id_hash = ? AND expires_at > ?",
    )
    .bind(&hash)
    .bind(now)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::unauthorized("This screen is not paired"))?;
    if now - row.1 > 3600 {
        sqlx::query("UPDATE table_screens SET last_seen_at = ?, expires_at = ? WHERE id_hash = ?")
            .bind(now)
            .bind(now + SCREEN_IDLE_SECS)
            .bind(&hash)
            .execute(&state.pool)
            .await?;
    }
    Ok(row.0)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScreenSnapshot {
    campaign: Value,
    party: Vec<Value>,
    table: TableState,
    encounter: Option<Value>,
    session: Option<sessions::SessionRecord>,
    /// The last finished session's log, for a "previously…" card.
    previously: Option<sessions::SessionRecord>,
    events: Vec<EventRecord>,
}

pub fn party_summary(character: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for key in PARTY_FIELDS {
        if let Some(value) = character.get(*key) {
            out.insert((*key).to_string(), value.clone());
        }
    }
    Value::Object(out)
}

async fn screen_snapshot(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Json<ScreenSnapshot>, ApiError> {
    let id = screen_campaign(&state, &token).await?;
    let name = sqlx::query_scalar::<_, String>("SELECT name FROM campaigns WHERE id = ?")
        .bind(&id)
        .fetch_one(&state.pool)
        .await?;

    let mut table = repo::load_table_state(&state.pool, &id).await?;
    let encounter = match &table.active_encounter_id {
        Some(eid) => repo::load_encounter(&state.pool, &id, eid).await.ok(),
        None => None,
    };
    if let (Some(combat), Some(encounter)) = (&mut table.combat, &encounter) {
        hide_unseen_combatants(combat, encounter);
    }
    let all_sessions = sessions::list_sessions_for(&state.pool, &id).await?;
    let session = table
        .session_id
        .as_ref()
        .and_then(|sid| all_sessions.iter().find(|s| &s.id == sid).cloned());
    let previously = all_sessions.iter().find(|s| s.ended_at.is_some()).cloned();
    let party = repo::member_characters(&state.pool, &id)
        .await?
        .iter()
        .map(party_summary)
        .collect();
    // No user id matches the empty string, so only public events come back (secrets stripped).
    let events = repo::visible_events(&state.pool, &id, "", false, None, SCREEN_EVENTS).await?;

    Ok(Json(ScreenSnapshot {
        campaign: json!({ "id": id, "name": name }),
        party,
        table,
        encounter: encounter.map(|e| project_encounter_for_player(&e)),
        session,
        previously,
        events,
    }))
}

async fn screen_stream(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    let id = screen_campaign(&state, &token).await?;
    Ok(campaign_stream(&state, id))
}
