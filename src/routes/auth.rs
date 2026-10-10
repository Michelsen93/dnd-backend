//! Sign-in, sessions and account management.
//!
//! Production (FIREBASE_PROJECT_ID set): the web app signs in with Firebase Authentication and
//! exchanges the Firebase ID token for our own session (`POST /session`). Local development and
//! tests (no Firebase): `POST /dev-login` with just an email. The two never coexist.

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
};
use axum_extra::extract::PrivateCookieJar;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    auth::{USER_COLUMNS, clear_session_cookie, end_session, require_user, start_session},
    db::UserRow,
    error::ApiError,
    models::{AuthResponse, MessageResponse, UserResponse},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/config", get(config))
        .route("/session", post(firebase_session))
        .route("/dev-login", post(dev_login))
        .route("/logout", post(logout))
        .route("/logout-all", post(logout_all))
        .route("/me", get(me))
        .route("/export", get(export))
        .route("/account", delete(delete_account))
}

fn user_response(user: UserRow) -> AuthResponse {
    AuthResponse {
        user: UserResponse {
            id: user.id,
            email: user.email,
            created_at: user.created_at,
            updated_at: user.updated_at,
        },
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthConfig {
    /// "firebase" in production, "dev" for local development.
    mode: &'static str,
    project_id: Option<String>,
}

async fn config(State(state): State<AppState>) -> Json<AuthConfig> {
    Json(AuthConfig {
        mode: if state.firebase.is_some() {
            "firebase"
        } else {
            "dev"
        },
        project_id: state.firebase.as_ref().map(|f| f.project_id().to_string()),
    })
}

async fn user_by(state: &AppState, column: &str, value: &str) -> Result<Option<UserRow>, ApiError> {
    Ok(sqlx::query_as::<_, UserRow>(&format!(
        "SELECT {USER_COLUMNS} FROM users WHERE {column} = ?"
    ))
    .bind(value)
    .fetch_optional(&state.pool)
    .await?)
}

async fn create_user(
    state: &AppState,
    email: &str,
    firebase_uid: Option<&str>,
) -> Result<UserRow, ApiError> {
    let now = crate::models::now_iso();
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO users (id, email, password_hash, firebase_uid, created_at, updated_at) VALUES (?, ?, '', ?, ?, ?)",
    )
    .bind(&id)
    .bind(email)
    .bind(firebase_uid)
    .bind(&now)
    .bind(&now)
    .execute(&state.pool)
    .await?;
    user_by(state, "id", &id)
        .await?
        .ok_or_else(|| ApiError::unauthorized("Could not create account"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionInput {
    id_token: String,
}

/// Exchange a Firebase ID token for a session cookie.
async fn firebase_session(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Json(input): Json<SessionInput>,
) -> Result<impl IntoResponse, ApiError> {
    let verifier = state
        .firebase
        .clone()
        .ok_or_else(|| ApiError::not_found("Not found"))?;
    let identity = verifier.verify(&input.id_token).await?;
    if !identity.email_verified {
        return Err(ApiError::forbidden(
            "Verify your email address first — check your inbox",
        ));
    }

    let user = match user_by(&state, "firebase_uid", &identity.uid).await? {
        Some(user) => {
            if user.email != identity.email {
                sqlx::query("UPDATE users SET email = ?, updated_at = ? WHERE id = ?")
                    .bind(&identity.email)
                    .bind(crate::models::now_iso())
                    .bind(&user.id)
                    .execute(&state.pool)
                    .await?;
            }
            user
        }
        // An existing account with this (verified) email is linked, so data carries over.
        None => match user_by(&state, "email", &identity.email).await? {
            Some(user) if user.firebase_uid.is_none() => {
                sqlx::query("UPDATE users SET firebase_uid = ?, updated_at = ? WHERE id = ?")
                    .bind(&identity.uid)
                    .bind(crate::models::now_iso())
                    .bind(&user.id)
                    .execute(&state.pool)
                    .await?;
                user
            }
            Some(_) => return Err(ApiError::conflict("This email belongs to another account")),
            None => create_user(&state, &identity.email, Some(&identity.uid)).await?,
        },
    };

    let jar = start_session(&state.pool, jar, &user.id, state.config.cookie_secure).await?;
    Ok((jar, Json(user_response(user))))
}

#[derive(Deserialize)]
struct DevLoginInput {
    email: String,
}

/// Local development and tests only: sign in with an email, no password. Disabled whenever
/// Firebase is configured (and the server refuses to start in production without Firebase).
async fn dev_login(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Json(input): Json<DevLoginInput>,
) -> Result<impl IntoResponse, ApiError> {
    if state.firebase.is_some() {
        return Err(ApiError::not_found("Not found"));
    }
    let email = input.email.trim().to_lowercase();
    if email.is_empty() || email.len() > 254 || !email.contains('@') {
        return Err(ApiError::bad_request("Enter an email address"));
    }
    let user = match user_by(&state, "email", &email).await? {
        Some(user) => user,
        None => create_user(&state, &email, None).await?,
    };
    let jar = start_session(&state.pool, jar, &user.id, state.config.cookie_secure).await?;
    Ok((jar, Json(user_response(user))))
}

async fn logout(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
) -> Result<impl IntoResponse, ApiError> {
    end_session(&state.pool, &jar).await?;
    Ok((
        clear_session_cookie(jar),
        Json(MessageResponse {
            message: "Logged out".to_string(),
        }),
    ))
}

/// Revoke every session of this account (e.g. a lost phone).
async fn logout_all(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
) -> Result<impl IntoResponse, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    sqlx::query("DELETE FROM sessions WHERE user_id = ?")
        .bind(&user.id)
        .execute(&state.pool)
        .await?;
    Ok((
        clear_session_cookie(jar),
        Json(MessageResponse {
            message: "Logged out everywhere".to_string(),
        }),
    ))
}

async fn me(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
) -> Result<Json<AuthResponse>, ApiError> {
    Ok(Json(user_response(require_user(&state.pool, &jar).await?)))
}

async fn payloads(state: &AppState, sql: &str, bind: &str) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query_scalar::<_, String>(sql)
        .bind(bind)
        .fetch_all(&state.pool)
        .await?;
    Ok(rows
        .iter()
        .filter_map(|p| serde_json::from_str(p).ok())
        .collect())
}

/// GDPR data export: everything stored about the account, as JSON.
async fn export(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
) -> Result<impl IntoResponse, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let characters = payloads(
        &state,
        "SELECT payload FROM characters WHERE user_id = ?",
        &user.id,
    )
    .await?;
    let notes = payloads(
        &state,
        "SELECT payload FROM notes WHERE user_id = ?",
        &user.id,
    )
    .await?;

    let owned = sqlx::query_as::<_, (String, String, String)>(
        "SELECT id, name, created_at FROM campaigns WHERE owner_user_id = ?",
    )
    .bind(&user.id)
    .fetch_all(&state.pool)
    .await?;
    let mut campaigns = Vec::new();
    for (id, name, created_at) in owned {
        campaigns.push(json!({
            "id": id,
            "name": name,
            "createdAt": created_at,
            "encounters": payloads(&state, "SELECT payload FROM encounters WHERE campaign_id = ?", &id).await?,
            "entities": payloads(&state, "SELECT payload FROM campaign_entities WHERE campaign_id = ?", &id).await?,
            "sessions": payloads(&state, "SELECT payload FROM game_sessions WHERE campaign_id = ?", &id).await?,
        }));
    }
    let memberships = sqlx::query_as::<_, (String, String, String)>(
        "SELECT campaign_id, character_id, joined_at FROM campaign_members WHERE user_id = ?",
    )
    .bind(&user.id)
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|(campaign_id, character_id, joined_at)| json!({ "campaignId": campaign_id, "characterId": character_id, "joinedAt": joined_at }))
    .collect::<Vec<_>>();
    let activity = payloads(
        &state,
        "SELECT payload FROM campaign_events WHERE actor_user_id = ?",
        &user.id,
    )
    .await?;

    let body = json!({
        "exportedAt": crate::models::now_iso(),
        "account": { "id": user.id, "email": user.email, "createdAt": user.created_at },
        "characters": characters,
        "notes": notes,
        "campaignsYouRun": campaigns,
        "memberships": memberships,
        "tableActivity": activity,
    });
    Ok((
        [(
            axum::http::header::CONTENT_DISPOSITION,
            "attachment; filename=\"pixel-quest-export.json\"",
        )],
        Json(body),
    ))
}

/// GDPR erasure: delete the account and everything it owns (characters, notes, campaigns it runs,
/// memberships, sessions), detach its name from other campaigns' feeds, and delete the Firebase user.
async fn delete_account(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
) -> Result<impl IntoResponse, ApiError> {
    let user = require_user(&state.pool, &jar).await?;

    let campaigns = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT campaign_id FROM campaign_members WHERE user_id = ?",
    )
    .bind(&user.id)
    .fetch_all(&state.pool)
    .await?;

    let mut tx = state.pool.begin().await?;
    sqlx::query("UPDATE campaign_events SET actor_user_id = NULL WHERE actor_user_id = ?")
        .bind(&user.id)
        .execute(&mut *tx)
        .await?;
    // ON DELETE CASCADE removes sessions, characters, notes, owned campaigns (and their maps,
    // entities, sessions, events) and memberships.
    sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(&user.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    for campaign_id in campaigns {
        state.notify(&campaign_id, "party");
    }
    if let (Some(verifier), Some(uid)) = (&state.firebase, &user.firebase_uid)
        && let Err(error) = verifier.delete_user(uid).await
    {
        // Local data is already gone; the orphaned sign-in can be removed in the console.
        tracing::error!(?error, uid, "could not delete the Firebase user");
    }
    Ok((
        StatusCode::OK,
        clear_session_cookie(jar),
        Json(MessageResponse {
            message: "Account deleted".to_string(),
        }),
    ))
}
