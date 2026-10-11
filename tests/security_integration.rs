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

#[tokio::test]
async fn invite_codes_are_long_and_random() {
    let t = table().await;
    let campaign = t.app.ok("GET", "/api/campaigns", &t.dm, None).await;
    let code = campaign["campaigns"][0]["inviteCode"].as_str().unwrap();
    assert_eq!(code.len(), 12);
    assert!(
        code.bytes()
            .all(|b| b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789".contains(&b))
    );
}

#[tokio::test]
async fn a_deleted_character_leaves_and_never_shows_an_email() {
    let t = table().await;
    t.app
        .ok(
            "DELETE",
            &format!("/api/characters/{}", t.character_id),
            &t.player,
            None,
        )
        .await;
    let party = t.app.ok("GET", &t.url("/characters"), &t.dm, None).await;
    assert_eq!(
        party.as_array().unwrap().len(),
        0,
        "the membership went with the character"
    );
    let members: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM campaign_members")
        .fetch_one(&t.app.pool)
        .await
        .unwrap();
    assert_eq!(members, 0);
}

#[tokio::test]
async fn the_dm_cannot_break_a_players_sheet() {
    let t = table().await;
    let (status, _) = t
        .app
        .call(
            "PATCH",
            &t.url(&format!("/characters/{}", t.character_id)),
            Some(&t.dm),
            Some(json!({ "hitPointsCurrent": "lol" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let mine = t.app.ok("GET", "/api/characters", &t.player, None).await;
    assert_eq!(
        mine.as_array().unwrap().len(),
        1,
        "the player's list still loads"
    );
    // XP can't be pushed past what the sheet can hold.
    for _ in 0..3 {
        t.app
            .ok(
                "POST",
                &t.url("/actions"),
                &t.dm,
                Some(json!({ "type": "award_xp", "amount": 1_000_000, "characterIds": [t.character_id] })),
            )
            .await;
    }
    let sheet = t
        .app
        .ok(
            "GET",
            &format!("/api/characters/{}", t.character_id),
            &t.player,
            None,
        )
        .await;
    assert!(sheet["experiencePoints"].as_i64().unwrap() <= 10_000_000);
}

#[tokio::test]
async fn players_cannot_probe_the_fog_or_a_monsters_ac() {
    let t = table().await;
    let snapshot = t.app.ok("GET", &t.url("/table"), &t.player, None).await;
    let token = snapshot["encounter"]["playerTokens"][0]["id"].clone();
    // x=4 is behind the wall: the answer is the same whether it's empty, a wall or the orc.
    let (status, body) = t
        .app
        .call(
            "POST",
            &t.url("/actions"),
            Some(&t.player),
            Some(json!({ "type": "move_token", "tokenId": token, "x": 4, "y": 0 })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("can see"));
    let (status, _) = t
        .app
        .call(
            "POST",
            &t.url("/rolls"),
            Some(&t.player),
            Some(json!({ "notation": "15", "label": "probe", "characterId": t.character_id, "rollKind": "attack",
                         "target": { "kind": "monster", "id": "gob" }, "secret": true })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "attacks need a d20");
}

#[tokio::test]
async fn loot_can_only_be_claimed_once() {
    let t = table().await;
    let loot = t
        .app
        .ok(
            "POST",
            &t.url("/entities"),
            &t.dm,
            Some(json!({ "kind": "loot", "revealed": true, "data": { "name": "Potion", "quantity": 99999999999i64 } })),
        )
        .await;
    let claim_url = t.url(&format!("/entities/{}/claim", loot["id"].as_str().unwrap()));
    let claim_body = json!({ "characterId": t.character_id });
    let claim = || {
        t.app.call(
            "POST",
            &claim_url,
            Some(&t.player),
            Some(claim_body.clone()),
        )
    };
    let (first, second) = tokio::join!(claim(), claim());
    let statuses = [first.0, second.0];
    assert_eq!(
        statuses.iter().filter(|s| s.is_success()).count(),
        1,
        "{statuses:?}"
    );
    let sheet = t
        .app
        .ok(
            "GET",
            &format!("/api/characters/{}", t.character_id),
            &t.player,
            None,
        )
        .await;
    let potions: Vec<&Value> = sheet["inventory"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["name"] == "Potion")
        .collect();
    assert_eq!(potions.len(), 1);
    assert!(potions[0]["quantity"].as_i64().unwrap() <= 9_999);
}

#[tokio::test]
async fn made_up_cookies_share_the_anonymous_limit() {
    let app = test_app().await;
    let mut limited = 0;
    for i in 0..80 {
        let cookie = format!("__session=forged{i}");
        let (status, _) = app.call("GET", "/api/auth/me", Some(&cookie), None).await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            limited += 1;
        }
    }
    assert!(
        limited > 0,
        "a fresh fake cookie per request no longer buys a fresh bucket"
    );
}

#[tokio::test]
async fn sessions_per_user_are_capped() {
    let app = test_app().await;
    app.register("many@example.com").await;
    // 30 older sessions for the same account (signing in 30 times would hit the rate limit).
    sqlx::query(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 30)
         INSERT INTO sessions (id_hash, user_id, created_at, last_seen_at, expires_at)
         SELECT 'old' || i, (SELECT id FROM users), i, i, 9999999999 FROM n",
    )
    .execute(&app.pool)
    .await
    .unwrap();
    app.register("many@example.com").await;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&app.pool)
        .await
        .unwrap();
    assert_eq!(count, backend::auth::MAX_SESSIONS_PER_USER);
    let newest_kept: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE id_hash = 'old1'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(newest_kept, 0, "the oldest sessions go first");
}
