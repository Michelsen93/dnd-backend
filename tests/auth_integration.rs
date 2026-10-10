mod common;

use std::{collections::HashMap, sync::OnceLock};

use axum::http::StatusCode;
use backend::firebase::FirebaseVerifier;
use common::{TestApp, test_app, test_app_with};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, encode};
use rsa::pkcs1::{EncodeRsaPrivateKey, EncodeRsaPublicKey, LineEnding};
use serde_json::{Value, json};

const PROJECT: &str = "pq-test";

struct Signer {
    key: EncodingKey,
}

impl Signer {
    fn token(&self, kid: &str, claims: Value) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(kid.to_string());
        encode(&header, &claims, &self.key).unwrap()
    }
}

fn claims(uid: &str, email: &str, verified: bool) -> Value {
    let now = chrono::Utc::now().timestamp();
    json!({
        "iss": format!("https://securetoken.google.com/{PROJECT}"),
        "aud": PROJECT,
        "sub": uid,
        "iat": now,
        "exp": now + 3600,
        "auth_time": now,
        "email": email,
        "email_verified": verified,
    })
}

/// An app in "firebase" mode whose verifier trusts a freshly generated key (kid "k1").
/// (private PEM, public PEM), generated once per test run — no key material lives in the repo.
fn keypair() -> &'static (String, String) {
    static KEYS: OnceLock<(String, String)> = OnceLock::new();
    KEYS.get_or_init(|| {
        let private = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
        let public = rsa::RsaPublicKey::from(&private);
        (
            private.to_pkcs1_pem(LineEnding::LF).unwrap().to_string(),
            public.to_pkcs1_pem(LineEnding::LF).unwrap(),
        )
    })
}

async fn firebase_app() -> (TestApp, Signer) {
    let (private, public) = keypair();
    let encoding = EncodingKey::from_rsa_pem(private.as_bytes()).unwrap();
    let decoding = DecodingKey::from_rsa_pem(public.as_bytes()).unwrap();
    let verifier =
        FirebaseVerifier::with_static_keys(PROJECT, HashMap::from([("k1".to_string(), decoding)]));
    let app = test_app_with(|state| state.with_firebase(verifier)).await;
    (app, Signer { key: encoding })
}

#[tokio::test]
async fn firebase_tokens_are_verified_strictly() {
    let (app, signer) = firebase_app().await;

    let (_, config) = app.call("GET", "/api/auth/config", None, None).await;
    assert_eq!(config["mode"], "firebase");
    let (status, _) = app
        .call(
            "POST",
            "/api/auth/dev-login",
            None,
            Some(json!({ "email": "a@b.c" })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "dev login is off when Firebase is configured"
    );

    let session = |token: String| json!({ "idToken": token });
    let mut bad = claims("u1", "pip@example.com", true);
    bad["aud"] = json!("someone-else");
    let mut expired = claims("u1", "pip@example.com", true);
    expired["exp"] = json!(chrono::Utc::now().timestamp() - 3600);
    let mut wrong_issuer = claims("u1", "pip@example.com", true);
    wrong_issuer["iss"] = json!("https://evil.example.com");
    for (label, token) in [
        ("wrong audience", signer.token("k1", bad)),
        ("expired", signer.token("k1", expired)),
        ("wrong issuer", signer.token("k1", wrong_issuer)),
        (
            "unknown key",
            signer.token("k2", claims("u1", "pip@example.com", true)),
        ),
        ("garbage", "not.a.jwt".to_string()),
    ] {
        let (status, _) = app
            .call("POST", "/api/auth/session", None, Some(session(token)))
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{label} must be rejected");
    }

    let (status, body) = app
        .call(
            "POST",
            "/api/auth/session",
            None,
            Some(session(
                signer.token("k1", claims("u1", "pip@example.com", false)),
            )),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "unverified email: {body}");

    let (status, body, cookie) = app
        .call_with_cookie(
            "POST",
            "/api/auth/session",
            None,
            Some(session(
                signer.token("k1", claims("u1", "Pip@Example.com", true)),
            )),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["user"]["email"], "pip@example.com");
    let cookie = cookie.expect("session cookie");
    assert!(cookie.starts_with("__session="));
    let me = app.ok("GET", "/api/auth/me", &cookie, None).await;
    assert_eq!(me["user"]["id"], body["user"]["id"]);

    // Signing in again with the same Firebase user lands on the same account.
    let (_, again, _) = app
        .call_with_cookie(
            "POST",
            "/api/auth/session",
            None,
            Some(session(
                signer.token("k1", claims("u1", "pip@example.com", true)),
            )),
        )
        .await;
    assert_eq!(again["user"]["id"], body["user"]["id"]);
}

#[tokio::test]
async fn existing_accounts_are_linked_by_verified_email() {
    let (app, signer) = firebase_app().await;
    // An account from before Firebase (created directly, as the old password flow did).
    sqlx::query("INSERT INTO users (id, email, password_hash, created_at, updated_at) VALUES ('old', 'dm@example.com', 'x', '', '')")
        .execute(&app.pool)
        .await
        .unwrap();
    let (status, body, _) = app
        .call_with_cookie(
            "POST",
            "/api/auth/session",
            None,
            Some(json!({ "idToken": signer.token("k1", claims("fb-dm", "dm@example.com", true)) })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["user"]["id"], "old",
        "the old account (and its data) is kept"
    );

    // A different Firebase user can't take over an already-linked email.
    let (status, _) = app
        .call("POST", "/api/auth/session", None, Some(json!({ "idToken": signer.token("k1", claims("fb-other", "dm@example.com", true)) })))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn sessions_can_be_revoked_and_expire() {
    let app = test_app().await;
    let (_, config) = app.call("GET", "/api/auth/config", None, None).await;
    assert_eq!(config["mode"], "dev");
    let (status, _) = app
        .call(
            "POST",
            "/api/auth/session",
            None,
            Some(json!({ "idToken": "x" })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "no Firebase exchange in dev mode"
    );

    let laptop = app.register("pip@example.com").await;
    let phone = app.register("pip@example.com").await;
    assert_ne!(laptop, phone, "each sign-in is its own session");

    app.ok("POST", "/api/auth/logout", &laptop, None).await;
    let (status, _) = app.call("GET", "/api/auth/me", Some(&laptop), None).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "logout revokes the session server-side"
    );
    app.ok("GET", "/api/auth/me", &phone, None).await;

    let tablet = app.register("pip@example.com").await;
    app.ok("POST", "/api/auth/logout-all", &tablet, None).await;
    for cookie in [&phone, &tablet] {
        let (status, _) = app.call("GET", "/api/auth/me", Some(cookie), None).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "log out everywhere revokes every session"
        );
    }

    let fresh = app.register("pip@example.com").await;
    sqlx::query("UPDATE sessions SET expires_at = 0")
        .execute(&app.pool)
        .await
        .unwrap();
    let (status, _) = app.call("GET", "/api/auth/me", Some(&fresh), None).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "expired sessions are rejected"
    );
}

#[tokio::test]
async fn export_and_delete_account() {
    let app = test_app().await;
    let dm = app.register("dm@example.com").await;
    let player = app.register("player@example.com").await;
    let campaign = app
        .ok(
            "POST",
            "/api/campaigns",
            &dm,
            Some(json!({ "name": "Keep" })),
        )
        .await;
    let character = app.create_character(&player, "Pip").await;
    app.ok(
        "POST",
        "/api/campaigns/join",
        &player,
        Some(json!({ "inviteCode": campaign["inviteCode"], "characterId": character["id"] })),
    )
    .await;
    app.ok(
        "POST",
        &format!("/api/campaigns/{}/rolls", campaign["id"].as_str().unwrap()),
        &player,
        Some(json!({ "notation": "1d20", "characterId": character["id"] })),
    )
    .await;

    let export = app.ok("GET", "/api/auth/export", &player, None).await;
    assert_eq!(export["account"]["email"], "player@example.com");
    assert_eq!(export["characters"][0]["name"], "Pip");
    assert_eq!(export["memberships"].as_array().unwrap().len(), 1);
    assert_eq!(export["tableActivity"].as_array().unwrap().len(), 1);

    app.ok("DELETE", "/api/auth/account", &player, None).await;
    let (status, _) = app.call("GET", "/api/auth/me", Some(&player), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let party = app
        .ok(
            "GET",
            &format!(
                "/api/campaigns/{}/characters",
                campaign["id"].as_str().unwrap()
            ),
            &dm,
            None,
        )
        .await;
    assert_eq!(
        party.as_array().unwrap().len(),
        0,
        "the deleted player's character left the DM's campaign"
    );
    let leftover: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM campaign_events WHERE actor_user_id IS NOT NULL AND actor_user_id NOT IN (SELECT id FROM users)")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(
        leftover, 0,
        "feed entries no longer point at the deleted account"
    );

    // Signing up again with the same email starts from scratch.
    let again = app.register("player@example.com").await;
    let characters = app.ok("GET", "/api/characters", &again, None).await;
    assert_eq!(characters.as_array().unwrap().len(), 0);
}
