//! Regression tests for the security review: each test is the proof of concept of a finding,
//! asserting it no longer works.
mod common;

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use common::{TestApp, corridor_encounter, test_app};
use serde_json::{Value, json};
use tower::ServiceExt;

struct Table {
    app: TestApp,
    dm: String,
    player: String,
    campaign_id: String,
    character_id: String,
    encounter_id: String,
}

impl Table {
    fn url(&self, path: &str) -> String {
        format!("/api/campaigns/{}{}", self.campaign_id, path)
    }
}

/// A DM, a player with a joined character, and the corridor map live on the table.
async fn table() -> Table {
    let app = test_app().await;
    let dm = app.register("dm@example.com").await;
    let player = app.register("player@example.com").await;
    let campaign = app
        .ok(
            "POST",
            "/api/campaigns",
            &dm,
            Some(json!({ "name": "Stormreach" })),
        )
        .await;
    let campaign_id = campaign["id"].as_str().unwrap().to_string();
    let character = app.create_character(&player, "Pip").await;
    let character_id = character["id"].as_str().unwrap().to_string();
    app.ok(
        "POST",
        "/api/campaigns/join",
        &player,
        Some(json!({ "inviteCode": campaign["inviteCode"], "characterId": character_id })),
    )
    .await;
    let encounter = app
        .ok(
            "POST",
            &format!("/api/campaigns/{campaign_id}/encounters"),
            &dm,
            Some(corridor_encounter(&character_id)),
        )
        .await;
    let encounter_id = encounter["id"].as_str().unwrap().to_string();
    app.ok(
        "POST",
        &format!("/api/campaigns/{campaign_id}/actions"),
        &dm,
        Some(json!({ "type": "set_active_encounter", "encounterId": encounter_id })),
    )
    .await;
    Table {
        app,
        dm,
        player,
        campaign_id,
        character_id,
        encounter_id,
    }
}

#[tokio::test]
async fn rolls_too_big_to_store_are_refused() {
    let t = table().await;
    let roll = |notation: String| json!({ "notation": notation, "label": "x", "characterId": t.character_id });
    for notation in [
        "100d1000+".repeat(25_000),
        "100d1000+100d1000+100d1000".to_string(),
        "1+1+1+1+1+1+1+1+1+1+1+1".to_string(),
    ] {
        let (status, _) = t
            .app
            .call(
                "POST",
                &t.url("/rolls"),
                Some(&t.player),
                Some(roll(notation)),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    for extra in [
        json!({ "rollKind": "x".repeat(10_000) }),
        json!({ "advantage": "x".repeat(10_000) }),
        json!({ "requestId": "x".repeat(10_000) }),
    ] {
        let mut body = roll("1d20".into());
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let (status, _) = t
            .app
            .call("POST", &t.url("/rolls"), Some(&t.player), Some(body))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (status, _) = t
        .app
        .call(
            "POST",
            &t.url("/actions"),
            Some(&t.dm),
            Some(json!({ "type": "toggle_condition", "targetKind": "monster", "targetId": "gob", "condition": "x".repeat(50_000) })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Normal rolls still work.
    t.app
        .ok(
            "POST",
            &t.url("/rolls"),
            &t.player,
            Some(roll("2d6+3".into())),
        )
        .await;
}

#[tokio::test]
async fn a_map_id_from_another_campaign_cannot_be_overwritten() {
    let t = table().await;
    // The player learns the live map's id from their snapshot and makes their own campaign.
    let snapshot = t.app.ok("GET", &t.url("/table"), &t.player, None).await;
    let victim_id = snapshot["table"]["activeEncounterId"].as_str().unwrap();
    let own = t
        .app
        .ok(
            "POST",
            "/api/campaigns",
            &t.player,
            Some(json!({ "name": "Mine" })),
        )
        .await;
    let mut evil = corridor_encounter(&t.character_id);
    evil["id"] = json!(victim_id);
    evil["name"] = json!("PWNED");
    evil["monsters"] = json!([]);
    let created = t
        .app
        .ok(
            "POST",
            &format!("/api/campaigns/{}/encounters", own["id"].as_str().unwrap()),
            &t.player,
            Some(evil),
        )
        .await;
    assert_ne!(created["id"], json!(victim_id), "the server picks the id");
    let maps = t.app.ok("GET", &t.url("/encounters"), &t.dm, None).await;
    assert_eq!(maps[0]["id"], json!(t.encounter_id));
    assert_eq!(maps[0]["name"], "Goblin Ambush");
    assert_eq!(maps[0]["monsters"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn tokens_off_the_map_are_refused() {
    let t = table().await;
    for token in [
        json!({ "id": "t", "name": "x", "spriteKey": "rogue", "x": 46000, "y": 0, "visionRadius": 6 }),
        json!({ "id": "t", "name": "x", "spriteKey": "rogue", "x": 0, "y": 0, "visionRadius": 46340 }),
        json!({ "id": "t", "name": "x", "spriteKey": "rogue", "x": -1, "y": 0, "visionRadius": 6 }),
    ] {
        let mut map = corridor_encounter(&t.character_id);
        map["playerTokens"] = json!([token]);
        let (status, _) = t
            .app
            .call("POST", &t.url("/encounters"), Some(&t.dm), Some(map))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let mut map = corridor_encounter(&t.character_id);
    map["monsters"][0]["x"] = json!(99);
    let (status, _) = t
        .app
        .call("POST", &t.url("/encounters"), Some(&t.dm), Some(map))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn players_never_receive_monster_hp_in_the_feed() {
    let t = table().await;
    t.app
        .ok(
            "POST",
            &t.url("/actions"),
            &t.dm,
            Some(json!({ "type": "apply_damage", "targetKind": "monster", "targetId": "gob", "amount": 3 })),
        )
        .await;
    let player_feed = t.app.ok("GET", &t.url("/events"), &t.player, None).await;
    let damage: Vec<&Value> = player_feed
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "damage")
        .collect();
    assert_eq!(damage.len(), 1, "players see that the goblin was hurt");
    assert!(
        damage[0]["payload"].get("dmDetail").is_none(),
        "but not its HP"
    );
    let dm_feed = t.app.ok("GET", &t.url("/events"), &t.dm, None).await;
    assert!(dm_feed.to_string().contains("4/7"), "the DM still sees it");
}

#[tokio::test]
async fn one_user_cannot_hold_every_stream() {
    let t = table().await;
    let open = || {
        let request = Request::builder()
            .uri(t.url("/stream"))
            .header(header::COOKIE, &t.player)
            .body(Body::empty())
            .unwrap();
        t.app.router.clone().oneshot(request)
    };
    let mut held = Vec::new();
    for _ in 0..backend::limits::MAX_STREAMS_PER_HOLDER {
        let response = open().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        held.push(response); // keep the body (and its slot) alive
    }
    assert_eq!(
        open().await.unwrap().status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    drop(held);
    assert_eq!(
        open().await.unwrap().status(),
        StatusCode::OK,
        "closing frees the slots"
    );
}
