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
pub const MAX_ENCOUNTERS_PER_CAMPAIGN: i64 = 30;
pub const MAX_ENTITIES_PER_CAMPAIGN: i64 = 300;
pub const MAX_NOTES_PER_CHARACTER: i64 = 300;
/// Notes across all of a user's characters (keeps one account's storage bounded).
pub const MAX_NOTES_PER_USER: i64 = 600;
/// Older feed events are pruned beyond this (session logs are kept separately).
pub const MAX_EVENTS_PER_CAMPAIGN: i64 = 3000;
/// Any single feed event (a roll with 200 dice is ~2 KB).
pub const MAX_EVENT_BYTES: usize = 8 * 1024;
/// SQLite refuses writes past this size instead of letting the server run out of memory (Cloud
/// Run keeps the database on its in-memory filesystem).
pub const MAX_DATABASE_BYTES: u64 = 384 * 1024 * 1024;

// ── Sizes ─────────────────────────────────────────────────────────────────────

/// Whole request bodies. A 40×40 map is ~30 KB; a full character sheet is a few KB.
pub const MAX_BODY_BYTES: usize = 256 * 1024;
/// A full 40×40 map with tokens and notes is about 40 KB.
pub const MAX_ENCOUNTER_BYTES: usize = 96 * 1024;
pub const MAX_CHARACTER_BYTES: usize = 64 * 1024;
pub const MAX_ENTITY_BYTES: usize = 16 * 1024;
pub const MAX_NOTE_BYTES: usize = 20 * 1024;
pub const MAX_SESSION_LOG_BYTES: usize = 20 * 1024;
pub const MAX_NAME_CHARS: usize = 80;
pub const MAX_LABEL_CHARS: usize = 120;
/// Ids, kinds and other short machine strings sent by clients.
pub const MAX_ID_CHARS: usize = 64;
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

/// Sprite keys, condition ids and the like: short, lowercase, `a-z0-9_-` only (empty allowed).
pub fn ensure_key(value: &str, what: &str) -> Result<(), ApiError> {
    if value.len() > 40
        || !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    {
        return Err(ApiError::bad_request(format!("{what} is not a valid key")));
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

// ── Open streams ──────────────────────────────────────────────────────────────

/// Live streams (SSE) each hold a request slot for as long as they are open, so they are
/// counted per holder (a user or a TV screen) and in total, not per minute.
pub const MAX_STREAMS_PER_HOLDER: usize = 6;
pub const MAX_STREAMS_TOTAL: usize = 400;

#[derive(Default)]
pub struct StreamCounter {
    open: Mutex<HashMap<String, usize>>,
}

/// One open stream; dropping it (the client disconnected) frees the slot.
pub struct StreamSlot {
    counter: std::sync::Arc<StreamCounter>,
    holder: String,
}

impl StreamCounter {
    pub fn acquire(
        counter: &std::sync::Arc<StreamCounter>,
        holder: &str,
    ) -> Result<StreamSlot, ApiError> {
        let mut open = counter.open.lock().expect("stream counter lock");
        let total: usize = open.values().sum();
        let mine = open.get(holder).copied().unwrap_or(0);
        if total >= MAX_STREAMS_TOTAL || mine >= MAX_STREAMS_PER_HOLDER {
            return Err(ApiError::too_many(
                "Too many live connections open — close some tabs and try again",
            ));
        }
        *open.entry(holder.to_string()).or_insert(0) += 1;
        Ok(StreamSlot {
            counter: counter.clone(),
            holder: holder.to_string(),
        })
    }
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        if let Ok(mut open) = self.counter.open.lock()
            && let Some(count) = open.get_mut(&self.holder)
        {
            *count -= 1;
            if *count == 0 {
                open.remove(&self.holder);
            }
        }
    }
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
        let retry = window_secs - now % window_secs;
        let mut windows = self.windows.lock().expect("rate limiter lock");
        if windows.len() >= MAX_TRACKED_KEYS && !windows.contains_key(key) {
            // Drop stale windows once; if a flood still fills the map, refuse new keys rather
            // than grow without bound (existing clients keep working).
            windows.retain(|_, (w, _)| *w == window);
            if windows.len() >= MAX_TRACKED_KEYS {
                return Err(retry);
            }
        }
        let entry = windows.entry(key.to_string()).or_insert((window, 0));
        if entry.0 != window {
            *entry = (window, 0);
        }
        entry.1 += 1;
        if entry.1 > limit { Err(retry) } else { Ok(()) }
    }
}

const MAX_TRACKED_KEYS: usize = 100_000;

/// Requests per minute.
const SIGNED_IN_PER_MINUTE: u32 = 300;
/// Backstops across all clients, for traffic whose per-client key can be faked (IP headers).
const ALL_ANONYMOUS_PER_MINUTE: u32 = 1_200;
const ALL_SIGN_IN_PER_MINUTE: u32 = 120;
const ALL_JOIN_PER_MINUTE: u32 = 300;
const ALL_SCREENS_PER_MINUTE: u32 = 3_000;
const ANONYMOUS_PER_MINUTE: u32 = 60;
const SIGN_IN_PER_MINUTE: u32 = 10;
const JOIN_PER_MINUTE: u32 = 10;

/// How many proxies in front of us append to X-Forwarded-For (`TRUSTED_PROXY_HOPS`). When set,
/// the client's address is that many entries from the right; anything further left was sent by
/// the client and can't be trusted. Unset, the left-most entry is used: spoofable, but it never
/// lumps every visitor into one bucket if the hop count were wrong. Check a production request
/// log and set it (the global caps bound load either way).
fn trusted_proxy_hops() -> Option<usize> {
    static HOPS: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    *HOPS.get_or_init(|| {
        std::env::var("TRUSTED_PROXY_HOPS")
            .ok()
            .and_then(|v| v.parse().ok())
    })
}

fn client_ip(request: &Request) -> String {
    let forwarded: Vec<String> = request
        .headers()
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|ip| ip.trim().to_string())
        .filter(|ip| !ip.is_empty())
        .collect();
    let picked = match trusted_proxy_hops() {
        Some(hops) if forwarded.len() > hops => forwarded.get(forwarded.len() - 1 - hops),
        Some(_) => None,
        None => forwarded.first(),
    };
    if let Some(ip) = picked {
        return ip.clone();
    }
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// The session token, only if the cookie decrypts with our key: a made-up cookie must not buy
/// its own rate-limit bucket.
fn session_cookie(request: &Request, key: &axum_extra::extract::cookie::Key) -> Option<String> {
    axum_extra::extract::PrivateCookieJar::from_headers(request.headers(), key.clone())
        .get(SESSION_COOKIE_NAME)
        .map(|c| c.value().to_string())
}

fn looks_like_screen_token(token: &str) -> bool {
    token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit())
}

pub async fn rate_limit(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    let ip = client_ip(&request);
    let session = session_cookie(&request, &state.cookie_key);

    // A paired TV screen carries its token in the path: limit it like a signed-in user.
    let screen_token = path
        .strip_prefix("/api/screen/")
        .and_then(|rest| rest.split('/').next())
        .filter(|token| looks_like_screen_token(token));

    let mut checks: Vec<(String, u32)> = Vec::with_capacity(4);
    match (&session, screen_token) {
        (_, Some(token)) => {
            checks.push((
                format!("screen:{}", crate::auth::hash_token(token)),
                SIGNED_IN_PER_MINUTE,
            ));
            checks.push(("all:screens".into(), ALL_SCREENS_PER_MINUTE));
        }
        (Some(token), None) => checks.push((
            format!("s:{}", crate::auth::hash_token(token)),
            SIGNED_IN_PER_MINUTE,
        )),
        (None, None) => {
            checks.push((format!("ip:{ip}"), ANONYMOUS_PER_MINUTE));
            checks.push(("all:anonymous".into(), ALL_ANONYMOUS_PER_MINUTE));
        }
    }
    if path.ends_with("/auth/session")
        || path.ends_with("/auth/dev-login")
        || path == "/api/screen/pair"
    {
        checks.push((format!("signin:{ip}"), SIGN_IN_PER_MINUTE));
        checks.push(("all:signin".into(), ALL_SIGN_IN_PER_MINUTE));
    }
    if path.ends_with("/campaigns/join") {
        let who = session
            .as_deref()
            .map(crate::auth::hash_token)
            .unwrap_or(ip.clone());
        checks.push((format!("join:{who}"), JOIN_PER_MINUTE));
        checks.push((format!("join-ip:{ip}"), JOIN_PER_MINUTE * 3));
        checks.push(("all:join".into(), ALL_JOIN_PER_MINUTE));
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
    fn streams_are_counted_while_open() {
        let counter = std::sync::Arc::new(StreamCounter::default());
        let slots: Vec<_> = (0..MAX_STREAMS_PER_HOLDER)
            .map(|_| StreamCounter::acquire(&counter, "u:1").unwrap())
            .collect();
        assert!(StreamCounter::acquire(&counter, "u:1").is_err());
        assert!(StreamCounter::acquire(&counter, "u:2").is_ok());
        drop(slots);
        assert!(StreamCounter::acquire(&counter, "u:1").is_ok());
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
