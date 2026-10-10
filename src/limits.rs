//! Abuse protection for a public deployment: request rate limits, payload size caps and
//! per-account / per-campaign quotas. Numbers are generous for real tables and tight for scripts.

use std::{collections::HashMap, net::SocketAddr, sync::Mutex};

use axum::{
    extract::{ConnectInfo, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::Serialize;

use crate::{auth::SESSION_COOKIE_NAME, error::ApiError, state::AppState};

// ── Quotas ────────────────────────────────────────────────────────────────────

pub const MAX_CHARACTERS_PER_USER: i64 = 30;
pub const MAX_CAMPAIGNS_OWNED: i64 = 10;
pub const MAX_MEMBERS_PER_CAMPAIGN: i64 = 12;
pub const MAX_ENCOUNTERS_PER_CAMPAIGN: i64 = 50;
pub const MAX_ENTITIES_PER_CAMPAIGN: i64 = 300;
pub const MAX_NOTES_PER_CHARACTER: i64 = 300;
/// Older feed events are pruned beyond this (session logs are kept separately).
pub const MAX_EVENTS_PER_CAMPAIGN: i64 = 5000;

// ── Sizes ─────────────────────────────────────────────────────────────────────

/// Whole request bodies. A 40×40 map is ~30 KB; a full character sheet is a few KB.
pub const MAX_BODY_BYTES: usize = 256 * 1024;
pub const MAX_CHARACTER_BYTES: usize = 64 * 1024;
pub const MAX_ENTITY_BYTES: usize = 16 * 1024;
pub const MAX_NOTE_BYTES: usize = 20 * 1024;
pub const MAX_SESSION_LOG_BYTES: usize = 20 * 1024;
pub const MAX_NAME_CHARS: usize = 80;
pub const MAX_LABEL_CHARS: usize = 120;
pub const MAX_FEED_TEXT_CHARS: usize = 500;
pub const MAX_SPOTLIGHT_BODY_CHARS: usize = 2000;

pub fn ensure_size<T: Serialize>(value: &T, max: usize, what: &str) -> Result<(), ApiError> {
    let size = serde_json::to_vec(value)
        .map(|v| v.len())
        .unwrap_or(usize::MAX);
    if size > max {
        return Err(ApiError::bad_request(format!(
            "{what} is too large ({} KB max)",
            max / 1024
        )));
    }
    Ok(())
}

pub fn ensure_chars(text: &str, max: usize, what: &str) -> Result<(), ApiError> {
    if text.chars().count() > max {
        return Err(ApiError::bad_request(format!(
            "{what} is too long ({max} characters max)"
        )));
    }
    Ok(())
}

pub fn ensure_quota(current: i64, max: i64, what: &str) -> Result<(), ApiError> {
    if current >= max {
        return Err(ApiError::bad_request(format!(
            "Limit reached: at most {max} {what}"
        )));
    }
    Ok(())
}

// ── Rate limiting ─────────────────────────────────────────────────────────────

/// Fixed-window counters kept in memory (the API runs as a single instance).
#[derive(Default)]
pub struct RateLimiter {
    windows: Mutex<HashMap<String, (i64, u32)>>,
}

impl RateLimiter {
    /// Count one hit for `key`; Err(seconds until the window resets) when over `limit`.
    pub fn hit(&self, key: &str, limit: u32, window_secs: i64) -> Result<(), i64> {
        let now = chrono::Utc::now().timestamp();
        let window = now / window_secs;
        let mut windows = self.windows.lock().expect("rate limiter lock");
        if windows.len() > 50_000 {
            windows.retain(|_, (w, _)| *w == window);
        }
        let entry = windows.entry(key.to_string()).or_insert((window, 0));
        if entry.0 != window {
            *entry = (window, 0);
        }
        entry.1 += 1;
        if entry.1 > limit {
            Err(window_secs - now % window_secs)
        } else {
            Ok(())
        }
    }
}

/// Requests per minute.
const SIGNED_IN_PER_MINUTE: u32 = 300;
const ANONYMOUS_PER_MINUTE: u32 = 60;
const SIGN_IN_PER_MINUTE: u32 = 10;
const JOIN_PER_MINUTE: u32 = 10;

fn client_ip(request: &Request) -> String {
    // Behind Firebase Hosting / Cloud Run the client address arrives in X-Forwarded-For. It can
    // be spoofed, so it only guards anonymous endpoints; signed-in traffic is keyed by session.
    request
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|ip| ip.trim().to_string())
        .or_else(|| {
            request
                .extensions()
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| c.0.ip().to_string())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn session_cookie(request: &Request) -> Option<String> {
    let cookies = request.headers().get(header::COOKIE)?.to_str().ok()?;
    cookies
        .split(';')
        .filter_map(|c| c.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE_NAME)
        .map(|(_, value)| value.to_string())
}

pub async fn rate_limit(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    let ip = client_ip(&request);
    let session = session_cookie(&request);

    let mut checks: Vec<(String, u32)> = Vec::with_capacity(2);
    match &session {
        Some(cookie) => checks.push((
            format!("s:{}", crate::auth::hash_token(cookie)),
            SIGNED_IN_PER_MINUTE,
        )),
        None => checks.push((format!("ip:{ip}"), ANONYMOUS_PER_MINUTE)),
    }
    if path.ends_with("/auth/session") || path.ends_with("/auth/dev-login") {
        checks.push((format!("signin:{ip}"), SIGN_IN_PER_MINUTE));
    }
    if path.ends_with("/campaigns/join") {
        let who = session
            .as_deref()
            .map(crate::auth::hash_token)
            .unwrap_or(ip.clone());
        checks.push((format!("join:{who}"), JOIN_PER_MINUTE));
    }

    for (key, limit) in checks {
        if let Err(retry_after) = state.limiter.hit(&key, limit, 60) {
            let mut response = (
                StatusCode::TOO_MANY_REQUESTS,
                axum::Json(serde_json::json!({ "error": "Too many requests — slow down a little and try again" })),
            )
                .into_response();
            if let Ok(value) = HeaderValue::from_str(&retry_after.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
            return response;
        }
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_counts_per_key() {
        let limiter = RateLimiter::default();
        for _ in 0..3 {
            assert!(limiter.hit("a", 3, 60).is_ok());
        }
        assert!(limiter.hit("a", 3, 60).is_err());
        assert!(limiter.hit("b", 3, 60).is_ok());
    }

    #[test]
    fn size_and_quota_checks() {
        assert!(ensure_chars("hello", 5, "x").is_ok());
        assert!(ensure_chars("hello!", 5, "x").is_err());
        assert!(ensure_size(&"x".repeat(2000), 1024, "x").is_err());
        assert!(ensure_quota(9, 10, "things").is_ok());
        assert!(ensure_quota(10, 10, "things").is_err());
    }
}
