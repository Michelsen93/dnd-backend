//! Shared helpers for integration tests: an in-memory app and a tiny JSON client.
#![allow(dead_code)]

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use axum_extra::extract::cookie::Key;
use backend::{config::AppConfig, db, routes, state::AppState};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

pub struct TestApp {
    pub router: Router,
    pub pool: sqlx::SqlitePool,
}

pub async fn test_app() -> TestApp {
    let pool = db::connect("sqlite::memory:")
        .await
        .expect("connect sqlite");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("run migrations");
    let config = AppConfig {
        app_host: "127.0.0.1".into(),
        app_port: 3001,
        database_url: "sqlite::memory:".into(),
        allowed_origin: "http://localhost:5173".into(),
        cookie_secret: "test".into(),
    };
    let state = AppState::new(pool.clone(), Key::from(&[7_u8; 64]), config);
    TestApp {
        router: routes::router().with_state(state),
        pool,
    }
}

impl TestApp {
    pub async fn call(
        &self,
        method: &str,
        uri: &str,
        cookie: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        let body = match body {
            Some(value) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        let response = self
            .router
            .clone()
            .oneshot(builder.body(body).expect("build request"))
            .await
            .expect("request");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, value)
    }

    pub async fn ok(&self, method: &str, uri: &str, cookie: &str, body: Option<Value>) -> Value {
        let (status, value) = self.call(method, uri, Some(cookie), body).await;
        assert!(status.is_success(), "{method} {uri} -> {status}: {value}");
        value
    }

    pub async fn register(&self, email: &str) -> String {
        let response = self
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/register")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({ "email": email, "password": "supersecure-password" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        response
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .expect("cookie")
            .to_string()
    }

    pub async fn create_character(&self, cookie: &str, name: &str) -> Value {
        self.ok(
            "POST",
            "/api/characters",
            cookie,
            Some(character_json(name)),
        )
        .await
    }
}

pub fn character_json(name: &str) -> Value {
    json!({
        "name": name, "race": "human", "classId": "fighter", "level": 1, "background": "Soldier",
        "alignment": "Neutral", "experiencePoints": 0,
        "abilityScores": { "str": 14, "dex": 12, "con": 13, "int": 10, "wis": 10, "cha": 8 },
        "proficiencyBonusOverride": null, "skillProficiencies": [], "savingThrowProficiencies": ["str", "con"],
        "languages": ["Common"], "toolProficiencies": [], "hitPointsMax": 12, "hitPointsCurrent": 12,
        "hitPointsTemp": 0, "hitDie": 10, "hitDiceTotal": 1, "hitDiceUsed": 0, "deathSaveSuccesses": 0,
        "deathSaveFailures": 0, "armorClass": 16, "initiative": 1, "speed": 30, "equipment": [],
        "treasure": "", "features": "", "ideals": "", "bonds": "", "flaws": "", "personalityTraits": "",
        "backstory": "", "spriteKey": "fighter", "avatarUrl": null,
        "attacks": [{ "id": "a1", "name": "Longsword", "damageDice": "1d8" }],
        "conditions": []
    })
}

/// A 6x1 corridor with a wall at x=2: a goblin at x=1 (visible) and an orc at x=4 (hidden).
pub fn corridor_encounter(character_id: &str) -> Value {
    json!({
        "id": "", "campaignId": "", "name": "Goblin Ambush", "gridCols": 6, "gridRows": 1,
        "terrain": [["grass", "grass", "wall", "grass", "grass", "grass"]],
        "visibility": [[false, false, false, false, false, false]],
        "monsters": [
            { "id": "gob", "name": "Goblin", "spriteKey": "goblin", "x": 1, "y": 0, "hitPointsMax": 7, "hitPointsCurrent": 7, "armorClass": 15, "conditions": [], "dexMod": 2, "xp": 50 },
            { "id": "orc", "name": "Orc", "spriteKey": "orc", "x": 4, "y": 0, "hitPointsMax": 15, "hitPointsCurrent": 15, "armorClass": 13, "conditions": [], "dexMod": 1, "xp": 100 }
        ],
        "playerTokens": [
            { "id": "tok", "characterId": character_id, "name": "Pip", "spriteKey": "rogue", "x": 0, "y": 0, "visionRadius": 10 }
        ],
        "losBlockByWalls": true
    })
}
