use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};
use axum_extra::extract::PrivateCookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use sqlx::SqlitePool;

use crate::db::UserRow;
use crate::error::ApiError;

/// Firebase Hosting forwards only the cookie named `__session` to Cloud Run, so that's our name.
pub const SESSION_COOKIE_NAME: &str = "__session";

pub fn hash_password(password: &str) -> Result<String, ApiError> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default().hash_password(password.as_bytes(), &salt)?;
    Ok(hash.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> Result<bool, ApiError> {
    let parsed = PasswordHash::new(hash)?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

pub fn add_session_cookie(jar: PrivateCookieJar, user_id: &str, secure: bool) -> PrivateCookieJar {
    let mut cookie = Cookie::new(SESSION_COOKIE_NAME, user_id.to_string());
    cookie.set_secure(secure);
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_path("/");
    jar.add(cookie)
}

pub fn clear_session_cookie(jar: PrivateCookieJar) -> PrivateCookieJar {
    let mut cookie = Cookie::new(SESSION_COOKIE_NAME, "");
    cookie.set_path("/");
    jar.remove(cookie)
}

pub async fn require_user(pool: &SqlitePool, jar: &PrivateCookieJar) -> Result<UserRow, ApiError> {
    let cookie = jar
        .get(SESSION_COOKIE_NAME)
        .ok_or_else(|| ApiError::unauthorized("Authentication required"))?;

    let user = sqlx::query_as::<_, UserRow>(
        "SELECT id, email, password_hash, created_at, updated_at FROM users WHERE id = ?",
    )
    .bind(cookie.value())
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::unauthorized("Session is no longer valid"))?;

    Ok(user)
}
