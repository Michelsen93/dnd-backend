//! Campaign-level authorization. Every campaign-scoped handler starts here.

use serde::Serialize;
use sqlx::{FromRow, SqlitePool};

use crate::error::ApiError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CampaignRole {
    Dm,
    Player,
}

#[derive(Clone, Debug, FromRow)]
pub struct CampaignRow {
    pub id: String,
    pub owner_user_id: String,
    pub name: String,
    pub invite_code: String,
    pub created_at: String,
}

pub struct CampaignAccess {
    pub campaign: CampaignRow,
    pub role: CampaignRole,
}

impl CampaignAccess {
    pub fn is_dm(&self) -> bool {
        self.role == CampaignRole::Dm
    }

    pub fn require_dm(&self) -> Result<(), ApiError> {
        if self.is_dm() {
            Ok(())
        } else {
            Err(ApiError::forbidden("Only the Dungeon Master can do that"))
        }
    }
}

/// DM if the user owns the campaign, Player if they have a member row, otherwise 404.
pub async fn campaign_access(
    pool: &SqlitePool,
    campaign_id: &str,
    user_id: &str,
) -> Result<CampaignAccess, ApiError> {
    let campaign = sqlx::query_as::<_, CampaignRow>(
        "SELECT id, owner_user_id, name, invite_code, created_at FROM campaigns WHERE id = ?",
    )
    .bind(campaign_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::not_found("Campaign not found"))?;

    if campaign.owner_user_id == user_id {
        return Ok(CampaignAccess {
            campaign,
            role: CampaignRole::Dm,
        });
    }

    let is_member = sqlx::query_scalar::<_, String>(
        "SELECT id FROM campaign_members WHERE campaign_id = ? AND user_id = ? LIMIT 1",
    )
    .bind(campaign_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?
    .is_some();

    if is_member {
        Ok(CampaignAccess {
            campaign,
            role: CampaignRole::Player,
        })
    } else {
        Err(ApiError::not_found("Campaign not found"))
    }
}

/// Character ids the user plays in this campaign.
pub async fn my_character_ids(
    pool: &SqlitePool,
    campaign_id: &str,
    user_id: &str,
) -> Result<Vec<String>, ApiError> {
    Ok(sqlx::query_scalar::<_, String>(
        "SELECT character_id FROM campaign_members WHERE campaign_id = ? AND user_id = ?",
    )
    .bind(campaign_id)
    .bind(user_id)
    .fetch_all(pool)
    .await?)
}
