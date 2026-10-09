use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, patch, post},
};
use axum_extra::extract::PrivateCookieJar;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;

use crate::{
    access::{CampaignRole, campaign_access},
    auth::require_user,
    error::ApiError,
    repo,
    state::AppState,
};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CampaignRecord {
    id: String,
    name: String,
    /// Only the DM sees the invite code.
    invite_code: Option<String>,
    owner_user_id: String,
    role: CampaignRole,
    /// A game session is running right now.
    live: bool,
    created_at: String,
}

#[derive(Clone, Debug, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct CampaignMemberRecord {
    id: String,
    campaign_id: String,
    user_id: String,
    character_id: String,
    character_name: String,
    sprite_key: String,
    joined_at: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CampaignSyncResponse {
    campaigns: Vec<CampaignRecord>,
    members: Vec<CampaignMemberRecord>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CampaignNameInput {
    name: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddMemberInput {
    character_id: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JoinByCodeInput {
    invite_code: String,
    character_id: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct JoinByCodeResponse {
    campaign: CampaignRecord,
    member: CampaignMemberRecord,
}

#[derive(Clone, Debug, FromRow)]
struct CampaignRow {
    id: String,
    owner_user_id: String,
    name: String,
    invite_code: String,
    created_at: String,
}

impl CampaignRow {
    fn into_record(self, user_id: &str) -> CampaignRecord {
        let is_dm = self.owner_user_id == user_id;
        CampaignRecord {
            id: self.id,
            name: self.name,
            invite_code: is_dm.then_some(self.invite_code),
            role: if is_dm {
                CampaignRole::Dm
            } else {
                CampaignRole::Player
            },
            owner_user_id: self.owner_user_id,
            live: false,
            created_at: self.created_at,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CharacterPayload {
    id: String,
    name: String,
    sprite_key: String,
}

const CAMPAIGN_COLUMNS: &str = "id, owner_user_id, name, invite_code, created_at";
const MEMBER_COLUMNS: &str =
    "id, campaign_id, user_id, character_id, character_name, sprite_key, joined_at";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_campaigns).post(create_campaign))
        .route("/join", post(join_by_code))
        .route("/{id}", patch(rename_campaign).delete(delete_campaign))
        .route("/{id}/members", post(add_member))
        .route("/{id}/members/{character_id}", delete(remove_member))
        .route("/{id}/invite/regenerate", post(regenerate_invite_code))
        .route("/{id}/characters", get(list_party))
        .route(
            "/{id}/characters/{character_id}",
            patch(dm_update_character),
        )
}

async fn list_campaigns(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
) -> Result<Json<CampaignSyncResponse>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;

    let campaign_rows = sqlx::query_as::<_, CampaignRow>(&format!(
        "SELECT DISTINCT {cols} FROM campaigns
         WHERE owner_user_id = ? OR id IN (SELECT campaign_id FROM campaign_members WHERE user_id = ?)
         ORDER BY updated_at DESC",
        cols = CAMPAIGN_COLUMNS
    ))
    .bind(&user.id)
    .bind(&user.id)
    .fetch_all(&state.pool)
    .await?;

    let members = sqlx::query_as::<_, CampaignMemberRecord>(&format!(
        "SELECT {cols} FROM campaign_members
         WHERE campaign_id IN (
             SELECT id FROM campaigns WHERE owner_user_id = ?
             UNION SELECT campaign_id FROM campaign_members WHERE user_id = ?
         )
         ORDER BY joined_at ASC",
        cols = MEMBER_COLUMNS
    ))
    .bind(&user.id)
    .bind(&user.id)
    .fetch_all(&state.pool)
    .await?;

    let live_ids = sqlx::query_scalar::<_, String>(
        "SELECT campaign_id FROM table_states WHERE json_extract(payload, '$.sessionId') IS NOT NULL",
    )
    .fetch_all(&state.pool)
    .await?;
    let campaigns = campaign_rows
        .into_iter()
        .map(|row| {
            let mut record = row.into_record(&user.id);
            record.live = live_ids.contains(&record.id);
            record
        })
        .collect();
    Ok(Json(CampaignSyncResponse { campaigns, members }))
}

async fn create_campaign(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Json(input): Json<CampaignNameInput>,
) -> Result<(StatusCode, Json<CampaignRecord>), ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let name = input.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request("Campaign name is required"));
    }
    let now = crate::models::now_iso();
    let row = CampaignRow {
        id: uuid::Uuid::new_v4().to_string(),
        owner_user_id: user.id.clone(),
        name: name.to_string(),
        invite_code: new_invite_code(),
        created_at: now.clone(),
    };

    sqlx::query(
        "INSERT INTO campaigns (id, owner_user_id, name, invite_code, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&row.id)
    .bind(&row.owner_user_id)
    .bind(&row.name)
    .bind(&row.invite_code)
    .bind(&row.created_at)
    .bind(&now)
    .execute(&state.pool)
    .await?;

    Ok((StatusCode::CREATED, Json(row.into_record(&user.id))))
}

async fn rename_campaign(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
    Json(input): Json<CampaignNameInput>,
) -> Result<Json<CampaignRecord>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let access = campaign_access(&state.pool, &id, &user.id).await?;
    access.require_dm()?;
    let name = input.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request("Campaign name is required"));
    }
    sqlx::query("UPDATE campaigns SET name = ?, updated_at = ? WHERE id = ?")
        .bind(name)
        .bind(crate::models::now_iso())
        .bind(&id)
        .execute(&state.pool)
        .await?;
    state.notify(&id, "campaign");
    Ok(Json(campaign_row(&state, &id).await?.into_record(&user.id)))
}

async fn delete_campaign(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    campaign_access(&state.pool, &id, &user.id)
        .await?
        .require_dm()?;
    sqlx::query("DELETE FROM campaigns WHERE id = ?")
        .bind(&id)
        .execute(&state.pool)
        .await?;
    state.notify(&id, "campaign");
    Ok(StatusCode::NO_CONTENT)
}

async fn add_member(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
    Json(input): Json<AddMemberInput>,
) -> Result<(StatusCode, Json<CampaignMemberRecord>), ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    campaign_access(&state.pool, &id, &user.id)
        .await?
        .require_dm()?;
    let character = owned_character_payload(&state, &user.id, &input.character_id).await?;
    let member = insert_member(&state, &id, &user.id, character).await?;
    Ok((StatusCode::CREATED, Json(member)))
}

/// The DM can remove anyone; a player can remove (leave with) their own character.
async fn remove_member(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path((id, character_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let access = campaign_access(&state.pool, &id, &user.id).await?;

    let query = if access.is_dm() {
        sqlx::query("DELETE FROM campaign_members WHERE campaign_id = ? AND character_id = ?")
            .bind(&id)
            .bind(&character_id)
    } else {
        sqlx::query("DELETE FROM campaign_members WHERE campaign_id = ? AND character_id = ? AND user_id = ?")
            .bind(&id)
            .bind(&character_id)
            .bind(&user.id)
    };
    if query.execute(&state.pool).await?.rows_affected() == 0 {
        return Err(if access.is_dm() {
            ApiError::not_found("Member not found")
        } else {
            ApiError::forbidden("You can only remove your own characters")
        });
    }

    touch_campaign(&state, &id).await?;
    state.notify(&id, "party");
    Ok(StatusCode::NO_CONTENT)
}

async fn regenerate_invite_code(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
) -> Result<Json<CampaignRecord>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    campaign_access(&state.pool, &id, &user.id)
        .await?
        .require_dm()?;

    sqlx::query("UPDATE campaigns SET invite_code = ?, updated_at = ? WHERE id = ?")
        .bind(new_invite_code())
        .bind(crate::models::now_iso())
        .bind(&id)
        .execute(&state.pool)
        .await?;

    Ok(Json(campaign_row(&state, &id).await?.into_record(&user.id)))
}

async fn join_by_code(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Json(input): Json<JoinByCodeInput>,
) -> Result<Json<JoinByCodeResponse>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let campaign = sqlx::query_as::<_, CampaignRow>(&format!(
        "SELECT {cols} FROM campaigns WHERE invite_code = ?",
        cols = CAMPAIGN_COLUMNS
    ))
    .bind(input.invite_code.trim().to_uppercase())
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::not_found("Invite code not found"))?;

    let character = owned_character_payload(&state, &user.id, &input.character_id).await?;
    let member = insert_member(&state, &campaign.id, &user.id, character).await?;

    Ok(Json(JoinByCodeResponse {
        campaign: campaign.into_record(&user.id),
        member,
    }))
}

/// Every member (and the DM) can read the whole party's sheets: they are allies.
async fn list_party(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
) -> Result<Json<Vec<Value>>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    campaign_access(&state.pool, &id, &user.id).await?;
    Ok(Json(repo::member_characters(&state.pool, &id).await?))
}

/// Fields the DM may change on a player's character during play.
const DM_EDITABLE_CHARACTER_FIELDS: &[&str] = &[
    "hitPointsCurrent",
    "hitPointsTemp",
    "conditions",
    "concentration",
    "deathSaveSuccesses",
    "deathSaveFailures",
    "experiencePoints",
    "spellSlots",
    "hitDiceUsed",
    "inventory",
];

async fn dm_update_character(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path((id, character_id)): Path<(String, String)>,
    Json(patch): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    campaign_access(&state.pool, &id, &user.id)
        .await?
        .require_dm()?;
    if !repo::member_character_ids(&state.pool, &id)
        .await?
        .contains(&character_id)
    {
        return Err(ApiError::not_found("Character not found"));
    }
    let Value::Object(patch) = patch else {
        return Err(ApiError::bad_request("Patch payload must be a JSON object"));
    };
    if let Some(key) = patch
        .keys()
        .find(|k| !DM_EDITABLE_CHARACTER_FIELDS.contains(&k.as_str()))
    {
        return Err(ApiError::bad_request(format!(
            "The DM cannot change '{key}'"
        )));
    }

    let mut record = repo::load_character(&state.pool, &character_id).await?;
    if let Value::Object(map) = &mut record.value {
        map.extend(patch);
    }
    repo::save_character_value(&state, &character_id, &mut record.value).await?;
    Ok(Json(record.value))
}

async fn insert_member(
    state: &AppState,
    campaign_id: &str,
    user_id: &str,
    character: CharacterPayload,
) -> Result<CampaignMemberRecord, ApiError> {
    sqlx::query(
        "INSERT OR IGNORE INTO campaign_members (id, campaign_id, user_id, character_id, character_name, sprite_key, joined_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(campaign_id)
    .bind(user_id)
    .bind(&character.id)
    .bind(&character.name)
    .bind(&character.sprite_key)
    .bind(crate::models::now_iso())
    .execute(&state.pool)
    .await?;

    touch_campaign(state, campaign_id).await?;
    state.notify(campaign_id, "party");

    Ok(sqlx::query_as::<_, CampaignMemberRecord>(&format!(
        "SELECT {cols} FROM campaign_members WHERE campaign_id = ? AND character_id = ?",
        cols = MEMBER_COLUMNS
    ))
    .bind(campaign_id)
    .bind(&character.id)
    .fetch_one(&state.pool)
    .await?)
}

async fn campaign_row(state: &AppState, campaign_id: &str) -> Result<CampaignRow, ApiError> {
    sqlx::query_as::<_, CampaignRow>(&format!(
        "SELECT {cols} FROM campaigns WHERE id = ?",
        cols = CAMPAIGN_COLUMNS
    ))
    .bind(campaign_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::not_found("Campaign not found"))
}

async fn owned_character_payload(
    state: &AppState,
    user_id: &str,
    character_id: &str,
) -> Result<CharacterPayload, ApiError> {
    let payload = sqlx::query_scalar::<_, String>(
        "SELECT payload FROM characters WHERE user_id = ? AND id = ?",
    )
    .bind(user_id)
    .bind(character_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::not_found("Character not found"))?;
    Ok(serde_json::from_str::<CharacterPayload>(&payload)?)
}

async fn touch_campaign(state: &AppState, campaign_id: &str) -> Result<(), ApiError> {
    sqlx::query("UPDATE campaigns SET updated_at = ? WHERE id = ?")
        .bind(crate::models::now_iso())
        .bind(campaign_id)
        .execute(&state.pool)
        .await?;
    Ok(())
}

fn new_invite_code() -> String {
    uuid::Uuid::new_v4()
        .to_string()
        .replace('-', "")
        .chars()
        .take(8)
        .collect::<String>()
        .to_uppercase()
}
