//! Data access helpers shared by the campaign-scoped routes.

use serde::Serialize;
use serde_json::Value;
use sqlx::{FromRow, SqlitePool};

use crate::{
    db::{deserialize_payload, serialize_payload},
    error::ApiError,
    models::{Encounter, TableState, now_iso},
    state::AppState,
};

// ── Table state ───────────────────────────────────────────────────────────────

pub async fn load_table_state(
    pool: &SqlitePool,
    campaign_id: &str,
) -> Result<TableState, ApiError> {
    let payload =
        sqlx::query_scalar::<_, String>("SELECT payload FROM table_states WHERE campaign_id = ?")
            .bind(campaign_id)
            .fetch_optional(pool)
            .await?;
    Ok(match payload {
        Some(payload) => deserialize_payload(&payload)?,
        None => TableState::default(),
    })
}

pub async fn save_table_state(
    pool: &SqlitePool,
    campaign_id: &str,
    table: &TableState,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO table_states (campaign_id, payload, updated_at) VALUES (?, ?, ?)
         ON CONFLICT(campaign_id) DO UPDATE SET payload = excluded.payload, updated_at = excluded.updated_at",
    )
    .bind(campaign_id)
    .bind(serialize_payload(table)?)
    .bind(now_iso())
    .execute(pool)
    .await?;
    Ok(())
}

// ── Encounters ────────────────────────────────────────────────────────────────

pub async fn list_encounters(
    pool: &SqlitePool,
    campaign_id: &str,
) -> Result<Vec<Encounter>, ApiError> {
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT payload FROM encounters WHERE campaign_id = ? ORDER BY created_at ASC",
    )
    .bind(campaign_id)
    .fetch_all(pool)
    .await?;
    rows.iter().map(|p| deserialize_payload(p)).collect()
}

pub async fn load_encounter(
    pool: &SqlitePool,
    campaign_id: &str,
    encounter_id: &str,
) -> Result<Encounter, ApiError> {
    let payload = sqlx::query_scalar::<_, String>(
        "SELECT payload FROM encounters WHERE id = ? AND campaign_id = ?",
    )
    .bind(encounter_id)
    .bind(campaign_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::not_found("Encounter not found"))?;
    deserialize_payload(&payload)
}

/// Insert or update, bumping the revision.
pub async fn save_encounter(pool: &SqlitePool, encounter: &mut Encounter) -> Result<(), ApiError> {
    encounter.revision += 1;
    let now = now_iso();
    if encounter.created_at.is_empty() {
        encounter.created_at = now.clone();
    }
    sqlx::query(
        "INSERT INTO encounters (id, campaign_id, payload, created_at, updated_at) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET payload = excluded.payload, updated_at = excluded.updated_at",
    )
    .bind(&encounter.id)
    .bind(&encounter.campaign_id)
    .bind(serialize_payload(encounter)?)
    .bind(&encounter.created_at)
    .bind(&now)
    .execute(pool)
    .await?;
    Ok(())
}

// ── Characters ────────────────────────────────────────────────────────────────

#[derive(FromRow)]
struct CharacterRow {
    user_id: String,
    payload: String,
}

pub struct CharacterRecord {
    pub user_id: String,
    pub value: Value,
}

pub async fn load_character(
    pool: &SqlitePool,
    character_id: &str,
) -> Result<CharacterRecord, ApiError> {
    let row =
        sqlx::query_as::<_, CharacterRow>("SELECT user_id, payload FROM characters WHERE id = ?")
            .bind(character_id)
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| ApiError::not_found("Character not found"))?;
    Ok(CharacterRecord {
        user_id: row.user_id,
        value: serde_json::from_str(&row.payload)?,
    })
}

pub async fn save_character_value(
    state: &AppState,
    character_id: &str,
    value: &mut Value,
) -> Result<(), ApiError> {
    let now = now_iso();
    if let Value::Object(map) = value {
        map.insert("updatedAt".into(), Value::String(now.clone()));
    }
    sqlx::query("UPDATE characters SET payload = ?, updated_at = ? WHERE id = ?")
        .bind(serde_json::to_string(value)?)
        .bind(&now)
        .bind(character_id)
        .execute(&state.pool)
        .await?;
    notify_character_campaigns(state, character_id).await
}

/// Tell every campaign this character belongs to that party data changed.
pub async fn notify_character_campaigns(
    state: &AppState,
    character_id: &str,
) -> Result<(), ApiError> {
    let campaign_ids = sqlx::query_scalar::<_, String>(
        "SELECT campaign_id FROM campaign_members WHERE character_id = ?",
    )
    .bind(character_id)
    .fetch_all(&state.pool)
    .await?;
    for campaign_id in campaign_ids {
        state.notify(&campaign_id, "party");
    }
    Ok(())
}

pub async fn member_character_ids(
    pool: &SqlitePool,
    campaign_id: &str,
) -> Result<Vec<String>, ApiError> {
    Ok(sqlx::query_scalar::<_, String>(
        "SELECT character_id FROM campaign_members WHERE campaign_id = ? ORDER BY joined_at ASC",
    )
    .bind(campaign_id)
    .fetch_all(pool)
    .await?)
}

pub async fn member_characters(
    pool: &SqlitePool,
    campaign_id: &str,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT characters.payload FROM campaign_members
         JOIN characters ON characters.id = campaign_members.character_id
         WHERE campaign_members.campaign_id = ? ORDER BY campaign_members.joined_at ASC",
    )
    .bind(campaign_id)
    .fetch_all(pool)
    .await?;
    rows.iter().map(|p| Ok(serde_json::from_str(p)?)).collect()
}

pub fn json_i64(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

// ── Events ────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct EventRow {
    pub id: String,
    pub campaign_id: String,
    pub session_id: Option<String>,
    pub actor_user_id: Option<String>,
    pub kind: String,
    pub visibility: String,
    pub payload: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventRecord {
    pub id: String,
    pub session_id: Option<String>,
    pub actor_user_id: Option<String>,
    pub kind: String,
    /// "public" (everyone), "private" (actor + DM) or "dm" (DM only)
    pub visibility: String,
    pub payload: Value,
    pub created_at: String,
}

impl EventRow {
    pub fn into_record(self) -> EventRecord {
        EventRecord {
            id: self.id,
            session_id: self.session_id,
            actor_user_id: self.actor_user_id,
            kind: self.kind,
            visibility: self.visibility,
            payload: serde_json::from_str(&self.payload).unwrap_or(Value::Null),
            created_at: self.created_at,
        }
    }
}

pub async fn record_event(
    state: &AppState,
    campaign_id: &str,
    actor_user_id: Option<&str>,
    kind: &str,
    visibility: &str,
    payload: Value,
) -> Result<EventRecord, ApiError> {
    let session_id = load_table_state(&state.pool, campaign_id).await?.session_id;
    let record = EventRecord {
        id: uuid::Uuid::new_v4().to_string(),
        session_id,
        actor_user_id: actor_user_id.map(str::to_string),
        kind: kind.to_string(),
        visibility: visibility.to_string(),
        payload,
        created_at: now_iso(),
    };
    sqlx::query(
        "INSERT INTO campaign_events (id, campaign_id, session_id, actor_user_id, kind, visibility, payload, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&record.id)
    .bind(campaign_id)
    .bind(&record.session_id)
    .bind(&record.actor_user_id)
    .bind(&record.kind)
    .bind(&record.visibility)
    .bind(serde_json::to_string(&record.payload)?)
    .bind(&record.created_at)
    .execute(&state.pool)
    .await?;
    // Keep the feed bounded: drop the oldest events beyond the cap.
    sqlx::query(
        "DELETE FROM campaign_events WHERE campaign_id = ? AND rowid <= (
             SELECT rowid FROM campaign_events WHERE campaign_id = ? ORDER BY rowid DESC LIMIT 1 OFFSET ?
         )",
    )
    .bind(campaign_id)
    .bind(campaign_id)
    .bind(crate::limits::MAX_EVENTS_PER_CAMPAIGN)
    .execute(&state.pool)
    .await?;
    Ok(record)
}

/// Events the caller may see, oldest first. `session_id` narrows to one session.
pub async fn visible_events(
    pool: &SqlitePool,
    campaign_id: &str,
    user_id: &str,
    is_dm: bool,
    session_id: Option<&str>,
    limit: i64,
) -> Result<Vec<EventRecord>, ApiError> {
    let rows = sqlx::query_as::<_, EventRow>(
        "SELECT * FROM (
             SELECT rowid AS seq, id, campaign_id, session_id, actor_user_id, kind, visibility, payload, created_at
             FROM campaign_events
             WHERE campaign_id = ?
               AND (? IS NULL OR session_id = ?)
               AND (? OR visibility = 'public' OR (visibility = 'private' AND actor_user_id = ?))
             ORDER BY rowid DESC
             LIMIT ?
         ) ORDER BY seq ASC",
    )
    .bind(campaign_id)
    .bind(session_id)
    .bind(session_id)
    .bind(is_dm)
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| {
            let mut record = row.into_record();
            if !is_dm {
                strip_secret_fields(&mut record.payload);
            }
            record
        })
        .collect())
}

/// Roll requests can carry a hidden DC; players only see it if the DM made it public.
fn strip_secret_fields(payload: &mut Value) {
    if let Value::Object(map) = payload
        && map
            .get("hiddenDc")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        map.remove("dc");
    }
}
