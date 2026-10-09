use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use std::str::FromStr;

use sqlx::{
    FromRow, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

use crate::error::ApiError;
use crate::models::Character;

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

pub async fn connect(database_url: &str) -> Result<SqlitePool, sqlx::Error> {
    let options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal);

    // Every `:memory:` connection is its own database, so in-memory pools must be single-connection.
    let max_connections = if database_url.contains(":memory:") {
        1
    } else {
        5
    };

    SqlitePoolOptions::new()
        .max_connections(max_connections)
        .connect_with(options)
        .await
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
        _ => {
            return Err(ApiError::bad_request(
                "Target payload must be a JSON object",
            ));
        }
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
