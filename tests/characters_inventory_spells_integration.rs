use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use axum_extra::extract::cookie::Key;
use backend::{db, routes, state::AppState};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tower::ServiceExt;

async fn test_app() -> (Router, SqlitePool) {
    let pool = db::connect("sqlite::memory:")
        .await
        .expect("connect sqlite");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("run migrations");

    let mut bytes = [0_u8; 64];
    let seed = b"characters-integration-tests-key-seed";
    bytes[..seed.len()].copy_from_slice(seed);

    let state = AppState::new(
        pool.clone(),
        Key::from(&bytes),
        backend::config::AppConfig {
            app_host: "127.0.0.1".to_string(),
            app_port: 3001,
            database_url: "sqlite::memory:".to_string(),
            allowed_origin: "http://localhost:5173".to_string(),
            cookie_secret: "characters-integration-tests-key-seed".to_string(),
            cookie_secure: false,
        },
    );

    (routes::router().with_state(state), pool)
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("read response body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("parse response json")
}

fn first_cookie(response: &axum::response::Response) -> String {
    response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .expect("set-cookie header")
        .to_string()
}

async fn register_and_get_cookie(app: &Router, email: &str) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/register")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "email": email,
                        "password": "supersecure-password"
                    })
                    .to_string(),
                ))
                .expect("build register request"),
        )
        .await
        .expect("register request");

    assert_eq!(response.status(), StatusCode::CREATED);
    first_cookie(&response)
}

#[tokio::test]
async fn character_payload_roundtrips_inventory_spells_and_abilities() {
    let (app, _pool) = test_app().await;
    let cookie = register_and_get_cookie(&app, "char-owner@example.com").await;

    let create_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &cookie)
                .body(Body::from(
                    json!({
                        "name": "My Test Character",
                        "race": "human",
                        "classId": "wizard",
                        "level": 3,
                        "background": "Sage",
                        "alignment": "Neutral Good",
                        "experiencePoints": 900,
                        "abilityScores": {
                            "str": 8,
                            "dex": 14,
                            "con": 12,
                            "int": 16,
                            "wis": 13,
                            "cha": 10
                        },
                        "proficiencyBonusOverride": null,
                        "skillProficiencies": ["arcana", "history"],
                        "savingThrowProficiencies": ["int", "wis"],
                        "languages": ["Common", "Draconic"],
                        "toolProficiencies": [],
                        "hitPointsMax": 18,
                        "hitPointsCurrent": 18,
                        "hitPointsTemp": 0,
                        "hitDie": 6,
                        "hitDiceTotal": 3,
                        "hitDiceUsed": 0,
                        "deathSaveSuccesses": 0,
                        "deathSaveFailures": 0,
                        "armorClass": 12,
                        "initiative": 2,
                        "speed": 30,
                        "equipment": ["Quarterstaff", "Spellbook"],
                        "inventory": [
                            { "id": "inv-1", "name": "Quarterstaff", "quantity": 1, "category": "weapon", "source": "custom" },
                            { "id": "inv-2", "name": "Rations (1 day)", "quantity": 5, "category": "consumable", "source": "srd" }
                        ],
                        "spells": [
                            { "id": "fire-bolt", "name": "Fire Bolt", "level": 0, "school": "Evocation", "prepared": true, "source": "srd" },
                            { "id": "magic-missile", "name": "Magic Missile", "level": 1, "school": "Evocation", "prepared": true, "source": "srd" }
                        ],
                        "abilities": [
                            { "id": "arcane-recovery", "name": "Arcane Recovery", "source": "custom", "level": 1, "description": "Recover slots on short rest" }
                        ],
                        "treasure": "25 gp",
                        "features": "Arcane Recovery",
                        "ideals": "Knowledge",
                        "bonds": "Library",
                        "flaws": "Obsessive",
                        "personalityTraits": "Curious",
                        "backstory": "Test backstory",
                        "spriteKey": "wizard",
                        "avatarUrl": null
                    })
                    .to_string(),
                ))
                .expect("build create character request"),
        )
        .await
        .expect("create character request");

    assert_eq!(create_response.status(), StatusCode::CREATED);
    let created = response_json(create_response).await;
    let character_id = created
        .get("id")
        .and_then(Value::as_str)
        .expect("character id")
        .to_string();

    assert_eq!(
        created
            .get("inventory")
            .and_then(Value::as_array)
            .map(|items| items.len()),
        Some(2)
    );
    assert_eq!(
        created
            .get("spells")
            .and_then(Value::as_array)
            .map(|items| items.len()),
        Some(2)
    );
    assert_eq!(
        created
            .get("abilities")
            .and_then(Value::as_array)
            .map(|items| items.len()),
        Some(1)
    );

    let patch_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/characters/{character_id}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &cookie)
                .body(Body::from(
                    json!({
                        "inventory": [
                            { "id": "inv-1", "name": "Quarterstaff", "quantity": 1, "category": "weapon", "source": "custom" },
                            { "id": "inv-3", "name": "Potion of Healing", "quantity": 2, "category": "consumable", "source": "srd" }
                        ],
                        "spells": [
                            { "id": "fire-bolt", "name": "Fire Bolt", "level": 0, "school": "Evocation", "prepared": true, "source": "srd" },
                            { "id": "shield", "name": "Shield", "level": 1, "school": "Abjuration", "prepared": true, "source": "srd" }
                        ]
                    })
                    .to_string(),
                ))
                .expect("build patch character request"),
        )
        .await
        .expect("patch character request");

    assert_eq!(patch_response.status(), StatusCode::OK);
    let patched = response_json(patch_response).await;
    assert!(
        patched
            .get("inventory")
            .and_then(Value::as_array)
            .expect("patched inventory")
            .iter()
            .any(|item| item.get("name").and_then(Value::as_str) == Some("Potion of Healing"))
    );

    let list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/characters")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("build list characters request"),
        )
        .await
        .expect("list characters request");

    assert_eq!(list_response.status(), StatusCode::OK);
    let listed = response_json(list_response).await;
    let list = listed.as_array().expect("characters list");
    assert_eq!(list.len(), 1);
    assert_eq!(
        list[0]
            .get("spells")
            .and_then(Value::as_array)
            .map(|spells| spells.len()),
        Some(2)
    );
}

#[tokio::test]
async fn legacy_equipment_payload_deserializes_with_default_new_fields() {
    let (app, pool) = test_app().await;
    let cookie = register_and_get_cookie(&app, "legacy-owner@example.com").await;

    let me_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/auth/me")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("build me request"),
        )
        .await
        .expect("me request");
    assert_eq!(me_response.status(), StatusCode::OK);
    let me = response_json(me_response).await;
    let user_id = me
        .get("user")
        .and_then(|u| u.get("id"))
        .and_then(Value::as_str)
        .expect("user id");

    let now = chrono::Utc::now().to_rfc3339();
    let legacy_character_id = uuid::Uuid::new_v4().to_string();
    let payload = json!({
        "id": legacy_character_id,
        "name": "Legacy Hero",
        "race": "human",
        "classId": "fighter",
        "level": 1,
        "background": "Soldier",
        "alignment": "Lawful Neutral",
        "experiencePoints": 0,
        "abilityScores": { "str": 15, "dex": 10, "con": 14, "int": 8, "wis": 12, "cha": 10 },
        "skillProficiencies": ["athletics"],
        "savingThrowProficiencies": ["str", "con"],
        "languages": ["Common"],
        "toolProficiencies": [],
        "hitPointsMax": 12,
        "hitPointsCurrent": 12,
        "hitPointsTemp": 0,
        "hitDie": 10,
        "hitDiceTotal": 1,
        "hitDiceUsed": 0,
        "deathSaveSuccesses": 0,
        "deathSaveFailures": 0,
        "armorClass": 16,
        "initiative": 0,
        "speed": 30,
        "equipment": ["Longsword", "Shield"],
        "treasure": "",
        "features": "",
        "ideals": "",
        "bonds": "",
        "flaws": "",
        "personalityTraits": "",
        "backstory": "",
        "spriteKey": "fighter",
        "createdAt": now,
        "updatedAt": now
    })
    .to_string();

    sqlx::query(
        "INSERT INTO characters (id, user_id, payload, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&legacy_character_id)
    .bind(user_id)
    .bind(payload)
    .bind(&now)
    .bind(&now)
    .execute(&pool)
    .await
    .expect("insert legacy character row");

    let list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/characters")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("build list characters request"),
        )
        .await
        .expect("list characters request");

    assert_eq!(list_response.status(), StatusCode::OK);
    let listed = response_json(list_response).await;
    let list = listed.as_array().expect("characters list");
    assert_eq!(list.len(), 1);

    assert_eq!(
        list[0]
            .get("inventory")
            .and_then(Value::as_array)
            .map(|items| items.len()),
        Some(0)
    );
    assert_eq!(
        list[0]
            .get("spells")
            .and_then(Value::as_array)
            .map(|items| items.len()),
        Some(0)
    );
    assert_eq!(
        list[0]
            .get("abilities")
            .and_then(Value::as_array)
            .map(|items| items.len()),
        Some(0)
    );
}
