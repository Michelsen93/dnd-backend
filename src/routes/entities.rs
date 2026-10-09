//! Campaign prep content: quests, NPCs, locations, handouts and loot.
//! Players only see entities the DM has revealed, without DM notes.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, patch, post},
};
use axum_extra::extract::PrivateCookieJar;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, SqlitePool};

use crate::{
    access::campaign_access, auth::require_user, error::ApiError, models::now_iso, repo,
    state::AppState,
};

const KINDS: &[&str] = &["quest", "npc", "location", "handout", "loot"];

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/{id}/entities", get(list).post(create))
        .route("/{id}/entities/{entity_id}", patch(update).delete(remove))
        .route("/{id}/entities/{entity_id}/claim", post(claim))
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityRecord {
    pub id: String,
    pub kind: String,
    pub revealed: bool,
    pub data: Value,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(FromRow)]
struct EntityRow {
    id: String,
    kind: String,
    revealed: bool,
    payload: String,
    created_at: String,
    updated_at: String,
}

impl EntityRow {
    fn into_record(self, is_dm: bool) -> EntityRecord {
        let mut data: Value = serde_json::from_str(&self.payload).unwrap_or_else(|_| json!({}));
        if !is_dm && let Value::Object(map) = &mut data {
            map.remove("dmNotes");
        }
        EntityRecord {
            id: self.id,
            kind: self.kind,
            revealed: self.revealed,
            data,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

const COLUMNS: &str = "id, kind, revealed, payload, created_at, updated_at";

pub async fn list_entities_for(
    pool: &SqlitePool,
    campaign_id: &str,
    is_dm: bool,
) -> Result<Vec<EntityRecord>, ApiError> {
    let rows = sqlx::query_as::<_, EntityRow>(&format!(
        "SELECT {COLUMNS} FROM campaign_entities WHERE campaign_id = ? AND (? OR revealed = 1) ORDER BY created_at ASC"
    ))
    .bind(campaign_id)
    .bind(is_dm)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| r.into_record(is_dm)).collect())
}

async fn load(
    pool: &SqlitePool,
    campaign_id: &str,
    entity_id: &str,
) -> Result<EntityRecord, ApiError> {
    sqlx::query_as::<_, EntityRow>(&format!(
        "SELECT {COLUMNS} FROM campaign_entities WHERE id = ? AND campaign_id = ?"
    ))
    .bind(entity_id)
    .bind(campaign_id)
    .fetch_optional(pool)
    .await?
    .map(|r| r.into_record(true))
    .ok_or_else(|| ApiError::not_found("Entry not found"))
}

fn entity_name(data: &Value) -> String {
    data.get("name")
        .or_else(|| data.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("Something")
        .to_string()
}

async fn list(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
) -> Result<Json<Vec<EntityRecord>>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let access = campaign_access(&state.pool, &id, &user.id).await?;
    Ok(Json(
        list_entities_for(&state.pool, &id, access.is_dm()).await?,
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateInput {
    kind: String,
    #[serde(default)]
    revealed: bool,
    data: Value,
}

async fn create(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
    Json(input): Json<CreateInput>,
) -> Result<(StatusCode, Json<EntityRecord>), ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    campaign_access(&state.pool, &id, &user.id)
        .await?
        .require_dm()?;
    if !KINDS.contains(&input.kind.as_str()) {
        return Err(ApiError::bad_request(format!(
            "kind must be one of {}",
            KINDS.join(", ")
        )));
    }
    if !input.data.is_object() {
        return Err(ApiError::bad_request("data must be a JSON object"));
    }
    let now = now_iso();
    let record = EntityRecord {
        id: uuid::Uuid::new_v4().to_string(),
        kind: input.kind,
        revealed: input.revealed,
        data: input.data,
        created_at: now.clone(),
        updated_at: now,
    };
    sqlx::query(
        "INSERT INTO campaign_entities (id, campaign_id, kind, revealed, payload, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&record.id)
    .bind(&id)
    .bind(&record.kind)
    .bind(record.revealed)
    .bind(serde_json::to_string(&record.data)?)
    .bind(&record.created_at)
    .bind(&record.updated_at)
    .execute(&state.pool)
    .await?;
    if record.revealed {
        reveal_event(&state, &id, &user.id, &record).await?;
    }
    state.notify(&id, "entities");
    Ok((StatusCode::CREATED, Json(record)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateInput {
    #[serde(default)]
    revealed: Option<bool>,
    /// Shallow-merged into the stored data.
    #[serde(default)]
    data: Option<Value>,
}

async fn update(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path((id, entity_id)): Path<(String, String)>,
    Json(input): Json<UpdateInput>,
) -> Result<Json<EntityRecord>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    campaign_access(&state.pool, &id, &user.id)
        .await?
        .require_dm()?;
    let mut record = load(&state.pool, &id, &entity_id).await?;
    let newly_revealed = input.revealed == Some(true) && !record.revealed;
    if let Some(revealed) = input.revealed {
        record.revealed = revealed;
    }
    if let Some(Value::Object(patch)) = input.data
        && let Value::Object(map) = &mut record.data
    {
        map.extend(patch);
    }
    record.updated_at = now_iso();
    sqlx::query(
        "UPDATE campaign_entities SET revealed = ?, payload = ?, updated_at = ? WHERE id = ?",
    )
    .bind(record.revealed)
    .bind(serde_json::to_string(&record.data)?)
    .bind(&record.updated_at)
    .bind(&record.id)
    .execute(&state.pool)
    .await?;
    if newly_revealed {
        reveal_event(&state, &id, &user.id, &record).await?;
    }
    state.notify(&id, "entities");
    Ok(Json(record))
}

async fn reveal_event(
    state: &AppState,
    campaign_id: &str,
    user_id: &str,
    record: &EntityRecord,
) -> Result<(), ApiError> {
    let name = entity_name(&record.data);
    let text = match record.kind.as_str() {
        "quest" => format!("New quest: {name}"),
        "npc" => format!("You meet {name}"),
        "loot" => format!("Loot found: {name}"),
        "location" => format!("Discovered: {name}"),
        _ => format!("Revealed: {name}"),
    };
    repo::record_event(
        state,
        campaign_id,
        Some(user_id),
        "reveal",
        "public",
        json!({ "text": text, "entityId": record.id, "kind": record.kind }),
    )
    .await?;
    Ok(())
}

async fn remove(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path((id, entity_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    campaign_access(&state.pool, &id, &user.id)
        .await?
        .require_dm()?;
    let result = sqlx::query("DELETE FROM campaign_entities WHERE id = ? AND campaign_id = ?")
        .bind(&entity_id)
        .bind(&id)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("Entry not found"));
    }
    state.notify(&id, "entities");
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaimInput {
    character_id: String,
}

/// A player takes revealed loot from the party stash into their character's inventory.
async fn claim(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path((id, entity_id)): Path<(String, String)>,
    Json(input): Json<ClaimInput>,
) -> Result<Json<Value>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let access = campaign_access(&state.pool, &id, &user.id).await?;
    let record = load(&state.pool, &id, &entity_id).await?;
    if record.kind != "loot" || (!record.revealed && !access.is_dm()) {
        return Err(ApiError::not_found("Entry not found"));
    }
    if !repo::member_character_ids(&state.pool, &id)
        .await?
        .contains(&input.character_id)
    {
        return Err(ApiError::not_found("Character not found"));
    }
    let mut character = repo::load_character(&state.pool, &input.character_id).await?;
    if !access.is_dm() && character.user_id != user.id {
        return Err(ApiError::forbidden(
            "You can only claim loot for your own characters",
        ));
    }

    let name = entity_name(&record.data);
    let quantity = record
        .data
        .get("quantity")
        .and_then(Value::as_i64)
        .unwrap_or(1)
        .max(1);
    let item = json!({
        "id": format!("item-{}", uuid::Uuid::new_v4()),
        "name": name,
        "quantity": quantity,
        "category": record.data.get("category").and_then(Value::as_str).unwrap_or("misc"),
        "source": "custom",
        "notes": record.data.get("description").and_then(Value::as_str).unwrap_or(""),
    });
    match character
        .value
        .get_mut("inventory")
        .and_then(Value::as_array_mut)
    {
        Some(inventory) => inventory.push(item),
        None => character.value["inventory"] = json!([item]),
    }
    repo::save_character_value(&state, &input.character_id, &mut character.value).await?;

    sqlx::query("DELETE FROM campaign_entities WHERE id = ?")
        .bind(&entity_id)
        .execute(&state.pool)
        .await?;
    let who = character
        .value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("Someone")
        .to_string();
    repo::record_event(
        &state,
        &id,
        Some(&user.id),
        "loot",
        "public",
        json!({ "text": format!("{who} takes {name}") }),
    )
    .await?;
    state.notify(&id, "entities");
    Ok(Json(character.value))
}
