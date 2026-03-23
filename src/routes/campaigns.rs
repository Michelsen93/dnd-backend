use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, post},
    Json, Router,
};
use axum_extra::extract::PrivateCookieJar;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::{
    auth::require_user,
    error::ApiError,
    state::AppState,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CampaignRecord {
    id: String,
    name: String,
    invite_code: String,
    created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CampaignMemberRecord {
    id: String,
    campaign_id: String,
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
struct CreateCampaignInput {
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
    name: String,
    invite_code: String,
    created_at: String,
}

#[derive(Clone, Debug, FromRow)]
struct CampaignMemberRow {
    id: String,
    campaign_id: String,
    character_id: String,
    character_name: String,
    sprite_key: String,
    joined_at: String,
}

#[derive(Clone, Debug, FromRow)]
struct CharacterPayloadRow {
    payload: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CharacterPayload {
    id: String,
    name: String,
    sprite_key: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_campaigns).post(create_campaign))
        .route("/{id}/members", post(add_member))
        .route("/{id}/members/{character_id}", delete(remove_member))
        .route("/{id}/invite/regenerate", post(regenerate_invite_code))
        .route("/join", post(join_by_code))
}

async fn list_campaigns(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
) -> Result<Json<CampaignSyncResponse>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;

    let campaign_rows = sqlx::query_as::<_, CampaignRow>(
        "SELECT DISTINCT campaigns.id, campaigns.name, campaigns.invite_code, campaigns.created_at
         FROM campaigns
         LEFT JOIN campaign_members ON campaign_members.campaign_id = campaigns.id
         WHERE campaigns.owner_user_id = ? OR campaign_members.user_id = ?
         ORDER BY campaigns.updated_at DESC",
    )
    .bind(&user.id)
    .bind(&user.id)
    .fetch_all(&state.pool)
    .await?;

    let campaign_ids: Vec<String> = campaign_rows.iter().map(|c| c.id.clone()).collect();
    let mut members: Vec<CampaignMemberRecord> = Vec::new();

    for campaign_id in campaign_ids {
        let rows = sqlx::query_as::<_, CampaignMemberRow>(
            "SELECT id, campaign_id, character_id, character_name, sprite_key, joined_at
             FROM campaign_members WHERE campaign_id = ? ORDER BY joined_at ASC",
        )
        .bind(&campaign_id)
        .fetch_all(&state.pool)
        .await?;

        members.extend(rows.into_iter().map(|row| CampaignMemberRecord {
            id: row.id,
            campaign_id: row.campaign_id,
            character_id: row.character_id,
            character_name: row.character_name,
            sprite_key: row.sprite_key,
            joined_at: row.joined_at,
        }));
    }

    let campaigns = campaign_rows
        .into_iter()
        .map(|row| CampaignRecord {
            id: row.id,
            name: row.name,
            invite_code: row.invite_code,
            created_at: row.created_at,
        })
        .collect();

    Ok(Json(CampaignSyncResponse { campaigns, members }))
}

async fn create_campaign(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Json(input): Json<CreateCampaignInput>,
) -> Result<(StatusCode, Json<CampaignRecord>), ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let now = crate::models::now_iso();

    let campaign = CampaignRecord {
        id: uuid::Uuid::new_v4().to_string(),
        name: input.name.trim().to_string(),
        invite_code: new_invite_code(),
        created_at: now.clone(),
    };

    sqlx::query(
        "INSERT INTO campaigns (id, owner_user_id, name, invite_code, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&campaign.id)
    .bind(&user.id)
    .bind(&campaign.name)
    .bind(&campaign.invite_code)
    .bind(&campaign.created_at)
    .bind(&now)
    .execute(&state.pool)
    .await?;

    Ok((StatusCode::CREATED, Json(campaign)))
}

async fn add_member(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
    Json(input): Json<AddMemberInput>,
) -> Result<(StatusCode, Json<CampaignMemberRecord>), ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let campaign = owned_campaign(&state, &id, &user.id).await?;
    let character = owned_character_payload(&state, &user.id, &input.character_id).await?;

    let member = CampaignMemberRecord {
        id: uuid::Uuid::new_v4().to_string(),
        campaign_id: campaign.id.clone(),
        character_id: character.id,
        character_name: character.name,
        sprite_key: character.sprite_key,
        joined_at: crate::models::now_iso(),
    };

    sqlx::query(
        "INSERT OR IGNORE INTO campaign_members (id, campaign_id, user_id, character_id, character_name, sprite_key, joined_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&member.id)
    .bind(&member.campaign_id)
    .bind(&user.id)
    .bind(&member.character_id)
    .bind(&member.character_name)
    .bind(&member.sprite_key)
    .bind(&member.joined_at)
    .execute(&state.pool)
    .await?;

    touch_campaign(&state, &campaign.id).await?;
    Ok((StatusCode::CREATED, Json(member)))
}

async fn remove_member(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path((id, character_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let campaign = owned_campaign(&state, &id, &user.id).await?;

    sqlx::query("DELETE FROM campaign_members WHERE campaign_id = ? AND character_id = ?")
        .bind(&campaign.id)
        .bind(&character_id)
        .execute(&state.pool)
        .await?;

    touch_campaign(&state, &campaign.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn regenerate_invite_code(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
) -> Result<Json<CampaignRecord>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let mut campaign = owned_campaign(&state, &id, &user.id).await?;

    campaign.invite_code = new_invite_code();
    let now = crate::models::now_iso();

    sqlx::query("UPDATE campaigns SET invite_code = ?, updated_at = ? WHERE id = ?")
        .bind(&campaign.invite_code)
        .bind(&now)
        .bind(&campaign.id)
        .execute(&state.pool)
        .await?;

    Ok(Json(CampaignRecord {
        id: campaign.id,
        name: campaign.name,
        invite_code: campaign.invite_code,
        created_at: campaign.created_at,
    }))
}

async fn join_by_code(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Json(input): Json<JoinByCodeInput>,
) -> Result<Json<JoinByCodeResponse>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let campaign = campaign_by_invite_code(&state, &input.invite_code).await?;
    let character = owned_character_payload(&state, &user.id, &input.character_id).await?;

    let member = CampaignMemberRecord {
        id: uuid::Uuid::new_v4().to_string(),
        campaign_id: campaign.id.clone(),
        character_id: character.id,
        character_name: character.name,
        sprite_key: character.sprite_key,
        joined_at: crate::models::now_iso(),
    };

    sqlx::query(
        "INSERT OR IGNORE INTO campaign_members (id, campaign_id, user_id, character_id, character_name, sprite_key, joined_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&member.id)
    .bind(&member.campaign_id)
    .bind(&user.id)
    .bind(&member.character_id)
    .bind(&member.character_name)
    .bind(&member.sprite_key)
    .bind(&member.joined_at)
    .execute(&state.pool)
    .await?;

    touch_campaign(&state, &campaign.id).await?;

    Ok(Json(JoinByCodeResponse {
        campaign: CampaignRecord {
            id: campaign.id,
            name: campaign.name,
            invite_code: campaign.invite_code,
            created_at: campaign.created_at,
        },
        member,
    }))
}

async fn owned_campaign(state: &AppState, campaign_id: &str, owner_user_id: &str) -> Result<CampaignRow, ApiError> {
    sqlx::query_as::<_, CampaignRow>(
        "SELECT id, name, invite_code, created_at
         FROM campaigns
         WHERE id = ? AND owner_user_id = ?",
    )
    .bind(campaign_id)
    .bind(owner_user_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::not_found("Campaign not found"))
}

async fn campaign_by_invite_code(state: &AppState, invite_code: &str) -> Result<CampaignRow, ApiError> {
    sqlx::query_as::<_, CampaignRow>(
        "SELECT id, name, invite_code, created_at
         FROM campaigns
         WHERE invite_code = ?",
    )
    .bind(invite_code.trim().to_uppercase())
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::not_found("Invite code not found"))
}

async fn owned_character_payload(state: &AppState, user_id: &str, character_id: &str) -> Result<CharacterPayload, ApiError> {
    let row = sqlx::query_as::<_, CharacterPayloadRow>(
        "SELECT payload FROM characters WHERE user_id = ? AND id = ?",
    )
    .bind(user_id)
    .bind(character_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::not_found("Character not found"))?;

    Ok(serde_json::from_str::<CharacterPayload>(&row.payload)?)
}

async fn touch_campaign(state: &AppState, campaign_id: &str) -> Result<(), ApiError> {
    let now = crate::models::now_iso();
    sqlx::query("UPDATE campaigns SET updated_at = ? WHERE id = ?")
        .bind(&now)
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
