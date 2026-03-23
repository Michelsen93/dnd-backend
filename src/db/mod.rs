use serde::{de::DeserializeOwned, Serialize};
use serde_json::{Map, Value};
use sqlx::{sqlite::SqlitePoolOptions, FromRow, SqlitePool};

use crate::error::ApiError;
use crate::models::{Character, CombatEntry};

#[derive(Clone, Debug, FromRow)]
pub struct UserRow {
    pub id: String,
    pub email: String,
    pub password_hash: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, FromRow)]
pub struct PayloadRow {
    pub payload: String,
}

#[derive(Clone, Debug, FromRow)]
pub struct CombatSessionRow {
    pub id: String,
    pub user_id: String,
    pub name: Option<String>,
    pub round: i32,
    pub active_index: i32,
    pub started: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, FromRow)]
pub struct CombatEntryRow {
    pub session_id: String,
    pub payload: String,
}

pub async fn connect(database_url: &str) -> Result<SqlitePool, sqlx::Error> {
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await?;

    sqlx::query("PRAGMA foreign_keys = ON;")
        .execute(&pool)
        .await?;
    sqlx::query("PRAGMA journal_mode = WAL;")
        .execute(&pool)
        .await?;

    Ok(pool)
}

pub fn serialize_payload<T: Serialize>(value: &T) -> Result<String, ApiError> {
    Ok(serde_json::to_string(value)?)
}

pub fn deserialize_payload<T: DeserializeOwned>(payload: &str) -> Result<T, ApiError> {
    Ok(serde_json::from_str(payload)?)
}

pub fn merge_json<T>(current: &T, patch: Value) -> Result<T, ApiError>
where
    T: Serialize + DeserializeOwned,
{
    let mut base = serde_json::to_value(current)?;
    let patch_object = match patch {
        Value::Object(object) => object,
        _ => return Err(ApiError::bad_request("Patch payload must be a JSON object")),
    };

    let base_object = match &mut base {
        Value::Object(object) => object,
        _ => return Err(ApiError::bad_request("Target payload must be a JSON object")),
    };

    merge_objects(base_object, patch_object);
    Ok(serde_json::from_value(base)?)
}

fn merge_objects(base: &mut Map<String, Value>, patch: Map<String, Value>) {
    for (key, value) in patch {
        base.insert(key, value);
    }
}

pub fn character_from_row(row: &PayloadRow) -> Result<Character, ApiError> {
    deserialize_payload(&row.payload)
}

pub fn combat_entry_from_row(row: &CombatEntryRow) -> Result<CombatEntry, ApiError> {
    deserialize_payload(&row.payload)
}
