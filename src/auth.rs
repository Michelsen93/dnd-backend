//! Server-side sessions. The cookie holds a random token; the database stores only its SHA-256.
//! Sessions slide forward while used and expire after inactivity or a hard maximum age, and they
//! can be revoked (logout, "log out everywhere", account deletion).

use axum_extra::extract::PrivateCookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::db::UserRow;
use crate::error::ApiError;

/// Firebase Hosting forwards only the cookie named `__session` to Cloud Run, so that's our name.
pub const SESSION_COOKIE_NAME: &str = "__session";
/// A session expires after this long without use…
pub const SESSION_IDLE_SECS: i64 = 30 * 24 * 3600;
/// …and never lives longer than this.
pub const SESSION_MAX_SECS: i64 = 90 * 24 * 3600;
/// Don't write `last_seen_at` on every request.
const TOUCH_EVERY_SECS: i64 = 3600;

pub const USER_COLUMNS: &str = "id, email, firebase_uid, created_at, updated_at";

pub fn hash_token(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Create a session for the user and put its token in the cookie jar.
pub async fn start_session(
    pool: &SqlitePool,
    jar: PrivateCookieJar,
    user_id: &str,
    secure: bool,
) -> Result<PrivateCookieJar, ApiError> {
    let mut bytes = [0_u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let now = now();
    sqlx::query(
        "INSERT INTO sessions (id_hash, user_id, created_at, last_seen_at, expires_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(hash_token(&token))
    .bind(user_id)
    .bind(now)
    .bind(now)
    .bind(now + SESSION_IDLE_SECS)
    .execute(pool)
    .await?;
    // Opportunistic cleanup of expired sessions.
    sqlx::query("DELETE FROM sessions WHERE expires_at < ?")
        .bind(now)
        .execute(pool)
        .await?;

    let mut cookie = Cookie::new(SESSION_COOKIE_NAME, token);
    cookie.set_secure(secure);
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_path("/");
    cookie.set_max_age(Some(cookie::time::Duration::seconds(SESSION_MAX_SECS)));
    Ok(jar.add(cookie))
}

pub fn clear_session_cookie(jar: PrivateCookieJar) -> PrivateCookieJar {
    let mut cookie = Cookie::new(SESSION_COOKIE_NAME, "");
    cookie.set_path("/");
    jar.remove(cookie)
}

/// Revoke the session in this cookie (if any).
pub async fn end_session(pool: &SqlitePool, jar: &PrivateCookieJar) -> Result<(), ApiError> {
    if let Some(cookie) = jar.get(SESSION_COOKIE_NAME) {
        sqlx::query("DELETE FROM sessions WHERE id_hash = ?")
            .bind(hash_token(cookie.value()))
            .execute(pool)
            .await?;
    }
    Ok(())
}

pub async fn require_user(pool: &SqlitePool, jar: &PrivateCookieJar) -> Result<UserRow, ApiError> {
    let cookie = jar
        .get(SESSION_COOKIE_NAME)
        .ok_or_else(|| ApiError::unauthorized("Authentication required"))?;
    let id_hash = hash_token(cookie.value());

    let session = sqlx::query_as::<_, (String, i64, i64, i64)>(
        "SELECT user_id, created_at, last_seen_at, expires_at FROM sessions WHERE id_hash = ?",
    )
    .bind(&id_hash)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::unauthorized("Session is no longer valid"))?;
    let (user_id, created_at, last_seen_at, expires_at) = session;

    let now = now();
    if expires_at <= now || created_at + SESSION_MAX_SECS <= now {
        sqlx::query("DELETE FROM sessions WHERE id_hash = ?")
            .bind(&id_hash)
            .execute(pool)
            .await?;
        return Err(ApiError::unauthorized(
            "Session expired, please sign in again",
        ));
    }
    if now - last_seen_at > TOUCH_EVERY_SECS {
        sqlx::query("UPDATE sessions SET last_seen_at = ?, expires_at = ? WHERE id_hash = ?")
            .bind(now)
            .bind((now + SESSION_IDLE_SECS).min(created_at + SESSION_MAX_SECS))
            .bind(&id_hash)
            .execute(pool)
            .await?;
    }

    sqlx::query_as::<_, UserRow>(&format!("SELECT {USER_COLUMNS} FROM users WHERE id = ?"))
        .bind(&user_id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| ApiError::unauthorized("Session is no longer valid"))
}
