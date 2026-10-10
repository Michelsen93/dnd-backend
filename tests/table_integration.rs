mod common;

use axum::http::StatusCode;
use common::{TestApp, corridor_encounter, test_app};
use serde_json::{Value, json};

struct Table {
    app: TestApp,
    dm: String,
    player: String,
    campaign_id: String,
    character_id: String,
}

impl Table {
    fn url(&self, path: &str) -> String {
        format!("/api/campaigns/{}{}", self.campaign_id, path)
    }

    async fn action(&self, cookie: &str, body: Value) -> (StatusCode, Value) {
        self.app
            .call("POST", &self.url("/actions"), Some(cookie), Some(body))
            .await
    }

    async fn snapshot(&self, cookie: &str) -> Value {
        self.app.ok("GET", &self.url("/table"), cookie, None).await
    }
}

/// DM creates a campaign, a player joins with a fresh character.
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
    let character = app.create_character(&player, "Pip").await;
    let code = campaign["inviteCode"].as_str().unwrap().to_string();
    let joined = app
        .ok(
            "POST",
            "/api/campaigns/join",
            &player,
            Some(json!({ "inviteCode": code, "characterId": character["id"] })),
        )
        .await;
    assert_eq!(joined["campaign"]["role"], "player");
    assert!(
        joined["campaign"]["inviteCode"].is_null(),
        "players never see the invite code"
    );
    Table {
        campaign_id: campaign["id"].as_str().unwrap().to_string(),
        character_id: character["id"].as_str().unwrap().to_string(),
        app,
        dm,
        player,
    }
}

async fn live_encounter(t: &Table) -> Value {
    let encounter = t
        .app
        .ok(
            "POST",
            &t.url("/encounters"),
            &t.dm,
            Some(corridor_encounter(&t.character_id)),
        )
        .await;
    let (status, _) = t
        .action(
            &t.dm,
            json!({ "type": "set_active_encounter", "encounterId": encounter["id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    encounter
}

#[tokio::test]
async fn roles_and_party_access() {
    let t = table().await;
    let outsider = t.app.register("outsider@example.com").await;

    let party = t.app.ok("GET", &t.url("/characters"), &t.dm, None).await;
    assert_eq!(party[0]["name"], "Pip");
    assert_eq!(
        party[0]["attacks"][0]["name"], "Longsword",
        "frontend-owned fields survive the round trip"
    );

    let (status, _) = t
        .app
        .call("GET", &t.url("/characters"), Some(&outsider), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = t.app.call("GET", &t.url("/table"), None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let dm_view = t.snapshot(&t.dm).await;
    assert_eq!(dm_view["role"], "dm");
    let player_view = t.snapshot(&t.player).await;
    assert_eq!(player_view["role"], "player");
    assert_eq!(player_view["myCharacterIds"][0], t.character_id.as_str());
    assert!(player_view["campaign"]["inviteCode"].is_null());

    // Players can't run the table or read DM-only content.
    let (status, _) = t.action(&t.player, json!({ "type": "start_combat" })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = t
        .app
        .call("GET", &t.url("/encounters"), Some(&t.player), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The DM may only touch play-state fields on a player's character.
    let (status, _) = t
        .app
        .call(
            "PATCH",
            &t.url(&format!("/characters/{}", t.character_id)),
            Some(&t.dm),
            Some(json!({ "name": "Hacked" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    t.app
        .ok(
            "PATCH",
            &t.url(&format!("/characters/{}", t.character_id)),
            &t.dm,
            Some(json!({ "hitPointsTemp": 5 })),
        )
        .await;

    // A player can leave with their own character.
    let (status, _) = t
        .app
        .call(
            "DELETE",
            &t.url(&format!("/members/{}", t.character_id)),
            Some(&t.player),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = t
        .app
        .call("GET", &t.url("/table"), Some(&t.player), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn players_get_a_projected_battlefield() {
    let t = table().await;
    live_encounter(&t).await;

    let dm_view = t.snapshot(&t.dm).await;
    assert_eq!(
        dm_view["encounter"]["monsters"].as_array().unwrap().len(),
        2
    );
    assert_eq!(dm_view["encounter"]["monsters"][0]["hitPointsCurrent"], 7);

    let player_view = t.snapshot(&t.player).await;
    let monsters = player_view["encounter"]["monsters"].as_array().unwrap();
    assert_eq!(
        monsters.len(),
        1,
        "the orc behind the wall is not sent at all"
    );
    assert_eq!(monsters[0]["name"], "Goblin");
    assert_eq!(monsters[0]["status"], "unhurt");
    assert!(monsters[0].get("hitPointsCurrent").is_none());
    assert_eq!(player_view["encounter"]["terrain"][0][4], "fog");
}

#[tokio::test]
async fn combat_turns_initiative_movement_and_damage() {
    let t = table().await;
    live_encounter(&t).await;

    let (status, _) = t.action(&t.dm, json!({ "type": "start_combat" })).await;
    assert_eq!(status, StatusCode::OK);

    // Player rolls initiative from their sheet; the server slots them into the order.
    let roll = t
        .app
        .ok(
            "POST",
            &t.url("/rolls"),
            &t.player,
            Some(json!({ "notation": "1d20+1", "label": "Initiative", "characterId": t.character_id, "rollKind": "initiative" })),
        )
        .await;
    let total = roll["payload"]["result"]["total"].as_i64().unwrap();
    let dm_view = t.snapshot(&t.dm).await;
    let combatants = dm_view["table"]["combat"]["combatants"].as_array().unwrap();
    assert_eq!(combatants.len(), 3);
    let pip = combatants.iter().find(|c| c["kind"] == "pc").unwrap();
    assert_eq!(pip["initiative"].as_i64().unwrap(), total);
    assert_eq!(
        dm_view["table"]["combat"]["turnIndex"], 0,
        "before anyone acts, the turn stays at the top of the order"
    );

    // The hidden orc's turn and wounds are announced to the DM only.
    t.action(
        &t.dm,
        json!({ "type": "apply_damage", "targetKind": "monster", "targetId": "orc", "amount": 1 }),
    )
    .await;
    let player_feed = t.snapshot(&t.player).await["events"].to_string();
    assert!(
        !player_feed.contains("Orc"),
        "hidden monsters never appear in the player feed"
    );
    assert!(
        t.snapshot(&t.dm).await["events"]
            .to_string()
            .contains("Orc takes 1")
    );

    // Players don't see the hidden orc in the initiative order.
    let player_view = t.snapshot(&t.player).await;
    assert_eq!(
        player_view["table"]["combat"]["combatants"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    // Advance until it's Pip's turn; before that the player may not move or end the turn.
    let pip_id = pip["id"].as_str().unwrap().to_string();
    for _ in 0..3 {
        let view = t.snapshot(&t.dm).await;
        let combat = &view["table"]["combat"];
        let current = &combat["combatants"][combat["turnIndex"].as_u64().unwrap() as usize];
        if current["id"] == pip_id.as_str() {
            break;
        }
        let (status, _) = t.action(&t.player, json!({ "type": "end_turn" })).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = t
            .action(
                &t.player,
                json!({ "type": "move_token", "tokenId": "tok", "x": 0, "y": 0 }),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        t.action(&t.dm, json!({ "type": "next_turn" })).await;
    }

    // On their turn: walls and occupied squares are rejected, a legal move works.
    let (status, _) = t
        .action(
            &t.player,
            json!({ "type": "move_token", "tokenId": "tok", "x": 2, "y": 0 }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = t
        .action(
            &t.player,
            json!({ "type": "move_token", "tokenId": "tok", "x": 1, "y": 0 }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = t.action(&t.player, json!({ "type": "end_turn" })).await;
    assert_eq!(status, StatusCode::OK);

    // Damage flows to the source of truth: the encounter for monsters, the sheet for PCs.
    t.action(
        &t.dm,
        json!({ "type": "apply_damage", "targetKind": "monster", "targetId": "gob", "amount": 4 }),
    )
    .await;
    let player_view = t.snapshot(&t.player).await;
    assert_eq!(
        player_view["encounter"]["monsters"][0]["status"],
        "bloodied"
    );

    t.action(&t.dm, json!({ "type": "apply_damage", "targetKind": "pc", "targetId": t.character_id, "amount": 5 })).await;
    let me = t
        .app
        .ok(
            "GET",
            &format!("/api/characters/{}", t.character_id),
            &t.player,
            None,
        )
        .await;
    assert_eq!(me["hitPointsCurrent"], 7);

    t.action(
        &t.dm,
        json!({ "type": "apply_damage", "targetKind": "monster", "targetId": "gob", "amount": 10 }),
    )
    .await;
    let (status, body) = t.action(&t.dm, json!({ "type": "end_encounter" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["event"]["payload"]["xpEach"], 50);
    let me = t
        .app
        .ok(
            "GET",
            &format!("/api/characters/{}", t.character_id),
            &t.player,
            None,
        )
        .await;
    assert_eq!(me["experiencePoints"], 50);
    assert!(t.snapshot(&t.dm).await["table"]["combat"].is_null());

    t.action(&t.dm, json!({ "type": "rest", "kind": "long" }))
        .await;
    let me = t
        .app
        .ok(
            "GET",
            &format!("/api/characters/{}", t.character_id),
            &t.player,
            None,
        )
        .await;
    assert_eq!(me["hitPointsCurrent"], 12);
}

#[tokio::test]
async fn feed_visibility_secret_rolls_and_hidden_dc() {
    let t = table().await;
    let other = t.app.register("other@example.com").await;
    let other_character = t.app.create_character(&other, "Bram").await;
    let code = t.snapshot(&t.dm).await["campaign"]["inviteCode"]
        .as_str()
        .unwrap()
        .to_string();
    t.app
        .ok(
            "POST",
            "/api/campaigns/join",
            &other,
            Some(json!({ "inviteCode": code, "characterId": other_character["id"] })),
        )
        .await;

    // Can't roll for someone else's character.
    let (status, _) = t
        .app
        .call(
            "POST",
            &t.url("/rolls"),
            Some(&other),
            Some(json!({ "notation": "1d20", "characterId": t.character_id })),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = t
        .app
        .call(
            "POST",
            &t.url("/rolls"),
            Some(&t.player),
            Some(json!({ "notation": "banana" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    t.app
        .ok(
            "POST",
            &t.url("/rolls"),
            &t.player,
            Some(json!({ "notation": "1d20+3", "label": "Stealth", "characterId": t.character_id, "secret": true, "advantage": "adv" })),
        )
        .await;
    t.action(
        &t.dm,
        json!({ "type": "request_roll", "label": "Perception", "skill": "perception", "dc": 15, "hiddenDc": true, "characterIds": [t.character_id] }),
    )
    .await;

    let count_rolls = |events: &Value| {
        events
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "roll")
            .count()
    };
    let dm_events = t.snapshot(&t.dm).await["events"].clone();
    let player_events = t.snapshot(&t.player).await["events"].clone();
    let other_events = t.snapshot(&other).await["events"].clone();
    assert_eq!(count_rolls(&dm_events), 1);
    assert_eq!(
        count_rolls(&player_events),
        1,
        "the roller sees their own secret roll"
    );
    assert_eq!(count_rolls(&other_events), 0);

    let request = |events: &Value| {
        events
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["kind"] == "roll_request")
            .cloned()
            .unwrap()
    };
    assert_eq!(request(&dm_events)["payload"]["dc"], 15);
    assert!(request(&other_events)["payload"].get("dc").is_none());

    let roll = dm_events
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "roll")
        .unwrap();
    assert_eq!(
        roll["payload"]["result"]["groups"][0]["rolls"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "advantage rolls two d20s"
    );
}

#[tokio::test]
async fn sessions_entities_and_loot() {
    let t = table().await;

    let session = t.app.ok("POST", &t.url("/sessions"), &t.dm, None).await;
    assert_eq!(session["number"], 1);
    let (status, _) = t
        .app
        .call("POST", &t.url("/sessions"), Some(&t.dm), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(t.snapshot(&t.player).await["session"]["number"], 1);
    let list = t.app.ok("GET", "/api/campaigns", &t.player, None).await;
    assert_eq!(list["campaigns"][0]["live"], true);

    let hidden_quest = t
        .app
        .ok("POST", &t.url("/entities"), &t.dm, Some(json!({ "kind": "quest", "data": { "title": "Find the lich", "dmNotes": "twist" } })))
        .await;
    let loot = t
        .app
        .ok("POST", &t.url("/entities"), &t.dm, Some(json!({ "kind": "loot", "revealed": true, "data": { "name": "Potion of Healing", "quantity": 2 } })))
        .await;

    let player_entities = t.snapshot(&t.player).await["entities"].clone();
    assert_eq!(
        player_entities.as_array().unwrap().len(),
        1,
        "unrevealed quest stays hidden"
    );

    t.app
        .ok(
            "PATCH",
            &t.url(&format!(
                "/entities/{}",
                hidden_quest["id"].as_str().unwrap()
            )),
            &t.dm,
            Some(json!({ "revealed": true })),
        )
        .await;
    let player_entities = t.snapshot(&t.player).await["entities"].clone();
    let quest = player_entities
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "quest")
        .unwrap();
    assert!(quest["data"].get("dmNotes").is_none());

    let character = t
        .app
        .ok(
            "POST",
            &t.url(&format!("/entities/{}/claim", loot["id"].as_str().unwrap())),
            &t.player,
            Some(json!({ "characterId": t.character_id })),
        )
        .await;
    assert_eq!(character["inventory"][0]["name"], "Potion of Healing");
    assert_eq!(character["inventory"][0]["quantity"], 2);

    let ended = t
        .app
        .ok(
            "POST",
            &t.url(&format!(
                "/sessions/{}/end",
                session["id"].as_str().unwrap()
            )),
            &t.dm,
            Some(json!({ "summary": "The party found a potion.", "nextSteps": "Hunt the lich" })),
        )
        .await;
    assert!(ended["endedAt"].is_string());
    let view = t.snapshot(&t.player).await;
    assert!(view["session"].is_null());
    assert_eq!(
        view["sessions"][0]["log"]["summary"],
        "The party found a potion."
    );

    let events = t
        .app
        .ok(
            "GET",
            &t.url(&format!(
                "/events?sessionId={}",
                session["id"].as_str().unwrap()
            )),
            &t.dm,
            None,
        )
        .await;
    let kinds: Vec<&str> = events
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds.first(), Some(&"session"));
    assert_eq!(kinds.last(), Some(&"session"));
    assert!(kinds.contains(&"loot"));
}

#[tokio::test]
async fn encounter_writes_use_optimistic_concurrency() {
    let t = table().await;
    let encounter = live_encounter(&t).await;
    let id = encounter["id"].as_str().unwrap();

    let mut edit = encounter.clone();
    edit["name"] = json!("Renamed");
    let saved = t
        .app
        .ok(
            "PUT",
            &t.url(&format!("/encounters/{id}")),
            &t.dm,
            Some(edit.clone()),
        )
        .await;
    assert_eq!(
        saved["revision"].as_i64().unwrap(),
        encounter["revision"].as_i64().unwrap() + 1
    );

    // Re-sending the stale revision conflicts and returns the current encounter.
    let (status, current) = t
        .app
        .call(
            "PUT",
            &t.url(&format!("/encounters/{id}")),
            Some(&t.dm),
            Some(edit),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(current["name"], "Renamed");

    let (status, _) = t
        .app
        .call(
            "DELETE",
            &t.url(&format!("/encounters/{id}")),
            Some(&t.dm),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(t.snapshot(&t.dm).await["table"]["activeEncounterId"].is_null());
}

fn expected_outcome(roll: &Value, hits: bool) -> &'static str {
    match roll["result"]["natural"].as_u64() {
        Some(20) => "crit",
        Some(1) => "miss",
        _ if hits => "hit",
        _ => "miss",
    }
}

#[tokio::test]
async fn targeted_attacks_resolve_hit_or_miss() {
    let t = table().await;
    live_encounter(&t).await;
    let roll = |notation: &str, target: Value, kind: &str| {
        json!({ "notation": notation, "label": "Shortsword attack", "characterId": t.character_id,
                "rollKind": kind, "target": target })
    };

    // Pip attacks the visible goblin (AC 15): +100 always hits, -100 always misses (nat 20/1 aside).
    let hit = t
        .app
        .ok(
            "POST",
            &t.url("/rolls"),
            &t.player,
            Some(roll(
                "1d20+100",
                json!({ "kind": "monster", "id": "gob" }),
                "attack",
            )),
        )
        .await;
    assert_eq!(hit["payload"]["target"]["name"], "Goblin");
    assert_eq!(
        hit["payload"]["outcome"],
        expected_outcome(&hit["payload"], true)
    );
    assert!(
        !hit.to_string().contains("armorClass"),
        "the target's AC is never sent"
    );
    let miss = t
        .app
        .ok(
            "POST",
            &t.url("/rolls"),
            &t.player,
            Some(roll(
                "1d20-100",
                json!({ "kind": "monster", "id": "gob" }),
                "attack",
            )),
        )
        .await;
    assert_eq!(
        miss["payload"]["outcome"],
        expected_outcome(&miss["payload"], false)
    );

    // Damage rolls carry the target (so the DM's APPLY is pre-filled) but no outcome.
    let damage = t
        .app
        .ok(
            "POST",
            &t.url("/rolls"),
            &t.player,
            Some(roll(
                "1d6+2",
                json!({ "kind": "monster", "id": "gob" }),
                "damage",
            )),
        )
        .await;
    assert_eq!(damage["payload"]["target"]["id"], "gob");
    assert!(damage["payload"]["outcome"].is_null());

    // Players can't aim at the orc behind the wall, or at nonsense.
    for target in [
        json!({ "kind": "monster", "id": "orc" }),
        json!({ "kind": "monster", "id": "nope" }),
        json!({ "kind": "pc", "id": "nope" }),
    ] {
        let (status, _) = t
            .app
            .call(
                "POST",
                &t.url("/rolls"),
                Some(&t.player),
                Some(roll("1d20", target, "attack")),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // The DM's goblin attacks Pip: resolved against Pip's AC.
    let dm_attack = t
        .app
        .ok(
            "POST",
            &t.url("/rolls"),
            &t.dm,
            Some(
                json!({ "notation": "1d20+100", "label": "Goblin: Scimitar", "rollKind": "attack",
                         "target": { "kind": "pc", "id": t.character_id } }),
            ),
        )
        .await;
    assert_eq!(dm_attack["visibility"], "public");
    assert_eq!(dm_attack["payload"]["target"]["name"], "Pip");
    assert_eq!(
        dm_attack["payload"]["outcome"],
        expected_outcome(&dm_attack["payload"], true)
    );

    // The DM aiming at the hidden orc stays behind the screen.
    let secret = t
        .app
        .ok(
            "POST",
            &t.url("/rolls"),
            &t.dm,
            Some(
                json!({ "notation": "1d20", "label": "Test", "rollKind": "attack",
                         "target": { "kind": "monster", "id": "orc" } }),
            ),
        )
        .await;
    assert_eq!(secret["visibility"], "dm");
    let player_view = t.snapshot(&t.player).await;
    assert!(!player_view["events"].to_string().contains("Orc"));
}

// ── Table screens (TV) ────────────────────────────────────────────────────────

#[tokio::test]
async fn tv_screen_pairs_and_sees_only_the_party_view() {
    let t = table().await;
    let outsider = t.app.register("outsider@example.com").await;
    live_encounter(&t).await;
    t.app.ok("POST", &t.url("/sessions"), &t.dm, None).await;
    t.app
        .ok(
            "POST",
            &t.url("/events"),
            &t.dm,
            Some(json!({ "text": "The orc is the mayor's brother", "visibility": "dm" })),
        )
        .await;
    t.app
        .ok(
            "POST",
            &t.url("/rolls"),
            &t.player,
            Some(json!({ "notation": "1d20", "label": "Sneaky", "characterId": t.character_id, "secret": true })),
        )
        .await;
    t.app
        .ok(
            "POST",
            &t.url("/rolls"),
            &t.player,
            Some(json!({ "notation": "1d20+5", "label": "Perception", "characterId": t.character_id })),
        )
        .await;

    // Only the DM can make a pairing code.
    let (status, _) = t
        .app
        .call("POST", &t.url("/screens"), Some(&t.player), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = t
        .app
        .call("POST", &t.url("/screens"), Some(&outsider), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = t.app.call("POST", &t.url("/screens"), None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let code = t.app.ok("POST", &t.url("/screens"), &t.dm, None).await;
    let code = code["code"].as_str().unwrap().to_string();
    assert_eq!(code.len(), 6);

    // The TV pairs without an account; lower-case and spaces are forgiven. Codes are single-use.
    let typed = format!(" {} ", code.to_lowercase());
    let (status, paired) = t
        .app
        .call(
            "POST",
            "/api/screen/pair",
            None,
            Some(json!({ "code": typed })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(paired["campaignName"], "Stormreach");
    let token = paired["token"].as_str().unwrap().to_string();
    let (status, _) = t
        .app
        .call(
            "POST",
            "/api/screen/pair",
            None,
            Some(json!({ "code": code })),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let status_view = t.app.ok("GET", &t.url("/screens"), &t.dm, None).await;
    assert_eq!(status_view["screens"], 1);

    let (status, screen) = t
        .app
        .call("GET", &format!("/api/screen/{token}/table"), None, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let text = screen.to_string();
    assert_eq!(screen["campaign"]["name"], "Stormreach");
    assert_eq!(screen["party"][0]["name"], "Pip");
    assert!(
        screen["party"][0].get("attacks").is_none(),
        "a summary, not the sheet"
    );
    assert_eq!(screen["encounter"]["monsters"].as_array().unwrap().len(), 1);
    assert!(!text.contains("Orc"), "no hidden monsters");
    assert!(!text.contains("hitPointsMax\":7"), "no monster HP");
    assert!(!text.contains("mayor's brother"), "no DM notes");
    assert!(!text.contains("Sneaky"), "no private rolls");
    assert!(text.contains("Perception"), "public rolls are there");
    assert!(screen["campaign"].get("inviteCode").is_none());

    // Unknown tokens are refused; the DM can unpair every screen.
    let (status, _) = t
        .app
        .call("GET", "/api/screen/not-a-token/table", None, None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = t
        .app
        .call("DELETE", &t.url("/screens"), Some(&t.dm), None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = t
        .app
        .call("GET", &format!("/api/screen/{token}/table"), None, None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
