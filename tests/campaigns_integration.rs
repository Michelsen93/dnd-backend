use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use axum_extra::extract::cookie::Key;
use backend::{db, routes, state::AppState};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn test_app() -> Router {
    let pool = db::connect("sqlite::memory:")
        .await
        .expect("connect sqlite");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("run migrations");

    let mut bytes = [0_u8; 64];
    let seed = b"campaign-integration-tests-key-seed";
    bytes[..seed.len()].copy_from_slice(seed);

    let state = AppState::new(
        pool,
        Key::from(&bytes),
        backend::config::AppConfig {
            app_host: "127.0.0.1".to_string(),
            app_port: 3001,
            database_url: "sqlite::memory:".to_string(),
            allowed_origin: "http://localhost:5173".to_string(),
            cookie_secret: "campaign-integration-tests-key-seed".to_string(),
            cookie_secure: false,
            firebase_project_id: None,
        },
    );

    routes::router().with_state(state)
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
                .uri("/api/auth/dev-login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "email": email
                    })
                    .to_string(),
                ))
                .expect("build register request"),
        )
        .await
        .expect("register request");

    assert_eq!(response.status(), StatusCode::OK);
    first_cookie(&response)
}

async fn create_character(app: &Router, cookie: &str, name: &str, sprite_key: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/characters")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, cookie)
                .body(Body::from(
                    json!({
                        "name": name,
                        "race": "human",
                        "classId": "fighter",
                        "level": 1,
                        "background": "Soldier",
                        "alignment": "Neutral",
                        "experiencePoints": 0,
                        "abilityScores": {
                            "str": 14,
                            "dex": 12,
                            "con": 13,
                            "int": 10,
                            "wis": 10,
                            "cha": 8
                        },
                        "proficiencyBonusOverride": null,
                        "skillProficiencies": [],
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
                        "initiative": 1,
                        "speed": 30,
                        "equipment": [],
                        "treasure": "",
                        "features": "",
                        "ideals": "",
                        "bonds": "",
                        "flaws": "",
                        "personalityTraits": "",
                        "backstory": "",
                        "spriteKey": sprite_key,
                        "avatarUrl": null
                    })
                    .to_string(),
                ))
                .expect("build create-character request"),
        )
        .await
        .expect("create-character request");

    assert_eq!(response.status(), StatusCode::CREATED);
    response_json(response).await
}

#[tokio::test]
async fn campaign_owner_flow_create_add_and_regenerate_invite() {
    let app = test_app().await;

    let owner_cookie = register_and_get_cookie(&app, "owner@example.com").await;
    let owner_character = create_character(&app, &owner_cookie, "Owner Tank", "fighter").await;
    let owner_character_id = owner_character
        .get("id")
        .and_then(Value::as_str)
        .expect("owner character id");

    let created_campaign_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/campaigns")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &owner_cookie)
                .body(Body::from(json!({ "name": "Stormreach" }).to_string()))
                .expect("build create-campaign request"),
        )
        .await
        .expect("create-campaign request");

    assert_eq!(created_campaign_response.status(), StatusCode::CREATED);
    let created_campaign = response_json(created_campaign_response).await;
    let campaign_id = created_campaign
        .get("id")
        .and_then(Value::as_str)
        .expect("campaign id");
    let invite_before = created_campaign
        .get("inviteCode")
        .and_then(Value::as_str)
        .expect("invite code")
        .to_string();

    let add_member_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/campaigns/{campaign_id}/members"))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &owner_cookie)
                .body(Body::from(
                    json!({ "characterId": owner_character_id }).to_string(),
                ))
                .expect("build add-member request"),
        )
        .await
        .expect("add-member request");

    assert_eq!(add_member_response.status(), StatusCode::CREATED);

    let list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/campaigns")
                .header(header::COOKIE, &owner_cookie)
                .body(Body::empty())
                .expect("build list-campaigns request"),
        )
        .await
        .expect("list-campaigns request");

    assert_eq!(list_response.status(), StatusCode::OK);
    let listed = response_json(list_response).await;
    let members = listed
        .get("members")
        .and_then(Value::as_array)
        .expect("members array");
    assert!(members.iter().any(|member| {
        member.get("campaignId").and_then(Value::as_str) == Some(campaign_id)
            && member.get("characterId").and_then(Value::as_str) == Some(owner_character_id)
            && member.get("characterName").and_then(Value::as_str) == Some("Owner Tank")
    }));

    let regenerate_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/campaigns/{campaign_id}/invite/regenerate"))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &owner_cookie)
                .body(Body::from("{}"))
                .expect("build regenerate-invite request"),
        )
        .await
        .expect("regenerate-invite request");

    assert_eq!(regenerate_response.status(), StatusCode::OK);
    let regenerated = response_json(regenerate_response).await;
    let invite_after = regenerated
        .get("inviteCode")
        .and_then(Value::as_str)
        .expect("regenerated invite code");
    assert_ne!(invite_before, invite_after);
}

#[tokio::test]
async fn invite_join_flow_allows_second_user_to_join_campaign() {
    let app = test_app().await;

    let owner_cookie = register_and_get_cookie(&app, "owner2@example.com").await;
    let owner_character = create_character(&app, &owner_cookie, "Owner Rogue", "rogue").await;
    let owner_character_id = owner_character
        .get("id")
        .and_then(Value::as_str)
        .expect("owner character id");

    let created_campaign_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/campaigns")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &owner_cookie)
                .body(Body::from(json!({ "name": "Frozen March" }).to_string()))
                .expect("build create-campaign request"),
        )
        .await
        .expect("create-campaign request");

    assert_eq!(created_campaign_response.status(), StatusCode::CREATED);
    let created_campaign = response_json(created_campaign_response).await;
    let campaign_id = created_campaign
        .get("id")
        .and_then(Value::as_str)
        .expect("campaign id")
        .to_string();
    let invite_code = created_campaign
        .get("inviteCode")
        .and_then(Value::as_str)
        .expect("invite code")
        .to_string();

    let add_owner_member_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/campaigns/{campaign_id}/members"))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &owner_cookie)
                .body(Body::from(
                    json!({ "characterId": owner_character_id }).to_string(),
                ))
                .expect("build add-owner-member request"),
        )
        .await
        .expect("add-owner-member request");
    assert_eq!(add_owner_member_response.status(), StatusCode::CREATED);

    let joiner_cookie = register_and_get_cookie(&app, "joiner@example.com").await;
    let joiner_character = create_character(&app, &joiner_cookie, "Joiner Cleric", "cleric").await;
    let joiner_character_id = joiner_character
        .get("id")
        .and_then(Value::as_str)
        .expect("joiner character id");

    let join_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/campaigns/join")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &joiner_cookie)
                .body(Body::from(
                    json!({
                        "inviteCode": invite_code,
                        "characterId": joiner_character_id
                    })
                    .to_string(),
                ))
                .expect("build join-by-code request"),
        )
        .await
        .expect("join-by-code request");

    assert_eq!(join_response.status(), StatusCode::OK);
    let joined = response_json(join_response).await;
    assert_eq!(
        joined
            .get("campaign")
            .and_then(|campaign| campaign.get("id"))
            .and_then(Value::as_str),
        Some(campaign_id.as_str())
    );
    assert_eq!(
        joined
            .get("member")
            .and_then(|member| member.get("characterName"))
            .and_then(Value::as_str),
        Some("Joiner Cleric")
    );

    let owner_list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/campaigns")
                .header(header::COOKIE, &owner_cookie)
                .body(Body::empty())
                .expect("build owner list-campaigns request"),
        )
        .await
        .expect("owner list-campaigns request");

    assert_eq!(owner_list_response.status(), StatusCode::OK);
    let owner_listed = response_json(owner_list_response).await;
    let owner_members = owner_listed
        .get("members")
        .and_then(Value::as_array)
        .expect("owner members array");
    assert!(owner_members.iter().any(|member| {
        member.get("campaignId").and_then(Value::as_str) == Some(campaign_id.as_str())
            && member.get("characterId").and_then(Value::as_str) == Some(joiner_character_id)
            && member.get("characterName").and_then(Value::as_str) == Some("Joiner Cleric")
    }));
}

#[tokio::test]
async fn add_member_is_idempotent_for_same_character() {
    let app = test_app().await;

    let owner_cookie = register_and_get_cookie(&app, "owner3@example.com").await;
    let owner_character = create_character(&app, &owner_cookie, "Owner Wizard", "wizard").await;
    let owner_character_id = owner_character
        .get("id")
        .and_then(Value::as_str)
        .expect("owner character id")
        .to_string();

    let created_campaign_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/campaigns")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &owner_cookie)
                .body(Body::from(
                    json!({ "name": "Idempotency Check" }).to_string(),
                ))
                .expect("build create-campaign request"),
        )
        .await
        .expect("create-campaign request");

    assert_eq!(created_campaign_response.status(), StatusCode::CREATED);
    let created_campaign = response_json(created_campaign_response).await;
    let campaign_id = created_campaign
        .get("id")
        .and_then(Value::as_str)
        .expect("campaign id")
        .to_string();

    let first_add_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/campaigns/{campaign_id}/members"))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &owner_cookie)
                .body(Body::from(
                    json!({ "characterId": owner_character_id }).to_string(),
                ))
                .expect("build first add-member request"),
        )
        .await
        .expect("first add-member request");

    assert_eq!(first_add_response.status(), StatusCode::CREATED);

    let second_add_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/campaigns/{campaign_id}/members"))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &owner_cookie)
                .body(Body::from(
                    json!({ "characterId": owner_character_id }).to_string(),
                ))
                .expect("build second add-member request"),
        )
        .await
        .expect("second add-member request");

    assert_eq!(second_add_response.status(), StatusCode::CREATED);

    let list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/campaigns")
                .header(header::COOKIE, &owner_cookie)
                .body(Body::empty())
                .expect("build list-campaigns request"),
        )
        .await
        .expect("list-campaigns request");

    assert_eq!(list_response.status(), StatusCode::OK);
    let listed = response_json(list_response).await;
    let members = listed
        .get("members")
        .and_then(Value::as_array)
        .expect("members array");

    let duplicates = members
        .iter()
        .filter(|member| {
            member.get("campaignId").and_then(Value::as_str) == Some(campaign_id.as_str())
                && member.get("characterId").and_then(Value::as_str)
                    == Some(owner_character_id.as_str())
        })
        .count();

    assert_eq!(duplicates, 1);
}

#[tokio::test]
async fn non_owner_cannot_regenerate_invite_or_remove_members() {
    let app = test_app().await;

    let owner_cookie = register_and_get_cookie(&app, "owner4@example.com").await;
    let owner_character = create_character(&app, &owner_cookie, "Owner Bard", "bard").await;
    let owner_character_id = owner_character
        .get("id")
        .and_then(Value::as_str)
        .expect("owner character id")
        .to_string();

    let created_campaign_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/campaigns")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &owner_cookie)
                .body(Body::from(
                    json!({ "name": "Private Campaign" }).to_string(),
                ))
                .expect("build create-campaign request"),
        )
        .await
        .expect("create-campaign request");

    assert_eq!(created_campaign_response.status(), StatusCode::CREATED);
    let created_campaign = response_json(created_campaign_response).await;
    let campaign_id = created_campaign
        .get("id")
        .and_then(Value::as_str)
        .expect("campaign id")
        .to_string();

    let add_member_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/campaigns/{campaign_id}/members"))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &owner_cookie)
                .body(Body::from(
                    json!({ "characterId": owner_character_id }).to_string(),
                ))
                .expect("build add-member request"),
        )
        .await
        .expect("add-member request");
    assert_eq!(add_member_response.status(), StatusCode::CREATED);

    let attacker_cookie = register_and_get_cookie(&app, "attacker@example.com").await;

    let regen_attempt = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/campaigns/{campaign_id}/invite/regenerate"))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &attacker_cookie)
                .body(Body::from("{}"))
                .expect("build regen attempt request"),
        )
        .await
        .expect("regen attempt request");
    assert_eq!(regen_attempt.status(), StatusCode::NOT_FOUND);

    let remove_attempt = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!(
                    "/api/campaigns/{campaign_id}/members/{owner_character_id}"
                ))
                .header(header::COOKIE, &attacker_cookie)
                .body(Body::empty())
                .expect("build remove attempt request"),
        )
        .await
        .expect("remove attempt request");
    assert_eq!(remove_attempt.status(), StatusCode::NOT_FOUND);
}
