//! The live table: snapshot (projected per role), SSE stream, dice rolls, feed and actions.

use std::{convert::Infallible, time::Duration};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post},
};
use axum_extra::extract::PrivateCookieJar;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

use crate::{
    access::{CampaignAccess, CampaignRole, campaign_access, my_character_ids},
    auth::require_user,
    db::UserRow,
    dice::{self, Advantage},
    error::ApiError,
    models::{Combat, Combatant, Encounter, Spotlight, TableState},
    projection::{self, project_encounter_for_player},
    repo::{self, EventRecord},
    routes::{entities, sessions},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/{id}/table", get(snapshot))
        .route("/{id}/stream", get(stream))
        .route("/{id}/events", get(list_events).post(post_note))
        .route("/{id}/rolls", post(roll))
        .route("/{id}/actions", post(action))
}

// ── Snapshot ──────────────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CampaignInfo {
    id: String,
    name: String,
    owner_user_id: String,
    invite_code: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TableSnapshot {
    campaign: CampaignInfo,
    role: CampaignRole,
    user_id: String,
    my_character_ids: Vec<String>,
    party: Vec<Value>,
    table: TableState,
    encounter: Option<Value>,
    session: Option<sessions::SessionRecord>,
    sessions: Vec<sessions::SessionRecord>,
    entities: Vec<entities::EntityRecord>,
    events: Vec<EventRecord>,
}

async fn snapshot(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
) -> Result<Json<TableSnapshot>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let access = campaign_access(&state.pool, &id, &user.id).await?;
    let is_dm = access.is_dm();

    let mut table = repo::load_table_state(&state.pool, &id).await?;
    let encounter = match &table.active_encounter_id {
        Some(encounter_id) => repo::load_encounter(&state.pool, &id, encounter_id)
            .await
            .ok(),
        None => None,
    };

    if !is_dm && let (Some(combat), Some(encounter)) = (&mut table.combat, &encounter) {
        hide_unseen_combatants(combat, encounter);
    }

    let encounter_value = encounter.map(|e| {
        if is_dm {
            serde_json::to_value(&e).unwrap_or(Value::Null)
        } else {
            project_encounter_for_player(&e)
        }
    });

    let all_sessions = sessions::list_sessions_for(&state.pool, &id).await?;
    let session = table
        .session_id
        .as_ref()
        .and_then(|sid| all_sessions.iter().find(|s| &s.id == sid).cloned());

    Ok(Json(TableSnapshot {
        campaign: CampaignInfo {
            id: access.campaign.id.clone(),
            name: access.campaign.name.clone(),
            owner_user_id: access.campaign.owner_user_id.clone(),
            invite_code: is_dm.then(|| access.campaign.invite_code.clone()),
        },
        role: access.role,
        user_id: user.id.clone(),
        my_character_ids: my_character_ids(&state.pool, &id, &user.id).await?,
        party: repo::member_characters(&state.pool, &id).await?,
        table,
        encounter: encounter_value,
        session,
        sessions: all_sessions,
        entities: entities::list_entities_for(&state.pool, &id, is_dm).await?,
        events: repo::visible_events(&state.pool, &id, &user.id, is_dm, None, 80).await?,
    }))
}

/// Players must not learn about monsters they cannot see from the initiative order.
fn hide_unseen_combatants(combat: &mut Combat, encounter: &Encounter) {
    let visibility = projection::player_visibility(encounter);
    let current_id = combat
        .combatants
        .get(combat.turn_index)
        .map(|c| c.id.clone());
    combat.combatants.retain(|c| {
        c.kind != "monster"
            || encounter
                .monsters
                .iter()
                .any(|m| m.id == c.ref_id && projection::is_visible(&visibility, m.x, m.y))
    });
    combat.turn_index = current_id
        .and_then(|cid| combat.combatants.iter().position(|c| c.id == cid))
        .unwrap_or(usize::MAX);
}

// ── Stream ────────────────────────────────────────────────────────────────────

async fn stream(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    campaign_access(&state.pool, &id, &user.id).await?;
    let rx = state.campaign_tx.subscribe();

    let stream = BroadcastStream::new(rx).filter_map(move |msg| match msg {
        Ok(signal) if signal.campaign_id == id => {
            Some(Ok(Event::default().event("update").data(signal.kind)))
        }
        // A lagged receiver missed signals: ask the client to refetch everything.
        Err(_) => Some(Ok(Event::default().event("update").data("resync"))),
        _ => None,
    });

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    ))
}

// ── Events ────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventQuery {
    session_id: Option<String>,
    limit: Option<i64>,
}

async fn list_events(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
    Query(query): Query<EventQuery>,
) -> Result<Json<Vec<EventRecord>>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let access = campaign_access(&state.pool, &id, &user.id).await?;
    let limit = query.limit.unwrap_or(500).clamp(1, 2000);
    Ok(Json(
        repo::visible_events(
            &state.pool,
            &id,
            &user.id,
            access.is_dm(),
            query.session_id.as_deref(),
            limit,
        )
        .await?,
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NoteInput {
    text: String,
    #[serde(default)]
    visibility: Option<String>,
}

async fn post_note(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
    Json(input): Json<NoteInput>,
) -> Result<(StatusCode, Json<EventRecord>), ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let access = campaign_access(&state.pool, &id, &user.id).await?;
    let text = input.text.trim();
    if text.is_empty() {
        return Err(ApiError::bad_request("Note text is required"));
    }
    let visibility = match (access.is_dm(), input.visibility.as_deref()) {
        (true, Some("dm")) => "dm",
        _ => "public",
    };
    let author = actor_name(&state, &access, &user).await;
    let event = repo::record_event(
        &state,
        &id,
        Some(&user.id),
        "note",
        visibility,
        json!({ "text": text, "author": author }),
    )
    .await?;
    state.notify(&id, "feed");
    Ok((StatusCode::CREATED, Json(event)))
}

async fn actor_name(state: &AppState, access: &CampaignAccess, user: &UserRow) -> String {
    if access.is_dm() {
        return "DM".to_string();
    }
    let ids = my_character_ids(&state.pool, &access.campaign.id, &user.id)
        .await
        .unwrap_or_default();
    let mut names = Vec::new();
    for id in ids {
        if let Ok(record) = repo::load_character(&state.pool, &id).await
            && let Some(name) = record.value.get("name").and_then(Value::as_str)
        {
            names.push(name.to_string());
        }
    }
    if names.is_empty() {
        user.email.clone()
    } else {
        names.join(" / ")
    }
}

// ── Rolls ─────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RollInput {
    notation: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    character_id: Option<String>,
    /// check | save | attack | damage | heal | initiative | death_save | custom
    #[serde(default)]
    roll_kind: Option<String>,
    #[serde(default)]
    advantage: Option<String>,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    secret: bool,
    /// Optional free-form context (attack target, damage type, …) shown in the feed.
    #[serde(default)]
    meta: Option<Value>,
}

async fn roll(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
    Json(input): Json<RollInput>,
) -> Result<(StatusCode, Json<EventRecord>), ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let access = campaign_access(&state.pool, &id, &user.id).await?;

    let mut character_name: Option<String> = None;
    if let Some(character_id) = &input.character_id {
        let members = repo::member_character_ids(&state.pool, &id).await?;
        if !members.contains(character_id) {
            return Err(ApiError::not_found("Character not found"));
        }
        let record = repo::load_character(&state.pool, character_id).await?;
        if !access.is_dm() && record.user_id != user.id {
            return Err(ApiError::forbidden(
                "You can only roll for your own characters",
            ));
        }
        character_name = record
            .value
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string);
    }

    let advantage = Advantage::parse(input.advantage.as_deref());
    let result = dice::roll(&input.notation, advantage).map_err(ApiError::bad_request)?;
    let roll_kind = input
        .roll_kind
        .clone()
        .unwrap_or_else(|| "custom".to_string());

    let visibility = match (input.secret, access.is_dm()) {
        (false, _) => "public",
        (true, true) => "dm",
        (true, false) => "private",
    };

    let actor = match &character_name {
        Some(name) => name.clone(),
        None => actor_name(&state, &access, &user).await,
    };

    let event = repo::record_event(
        &state,
        &id,
        Some(&user.id),
        "roll",
        visibility,
        json!({
            "label": if input.label.trim().is_empty() { input.notation.clone() } else { input.label.trim().to_string() },
            "characterId": input.character_id,
            "actor": actor,
            "rollKind": roll_kind,
            "advantage": input.advantage,
            "requestId": input.request_id,
            "meta": input.meta,
            "result": result,
        }),
    )
    .await?;

    if roll_kind == "initiative"
        && let Some(character_id) = &input.character_id
    {
        set_pc_initiative(
            &state,
            &id,
            character_id,
            character_name.as_deref().unwrap_or("?"),
            result.total,
        )
        .await?;
    }

    state.notify(&id, "feed");
    Ok((StatusCode::CREATED, Json(event)))
}

async fn set_pc_initiative(
    state: &AppState,
    campaign_id: &str,
    character_id: &str,
    name: &str,
    value: i32,
) -> Result<(), ApiError> {
    let mut table = repo::load_table_state(&state.pool, campaign_id).await?;
    let Some(combat) = &mut table.combat else {
        return Ok(());
    };
    match combat
        .combatants
        .iter_mut()
        .find(|c| c.kind == "pc" && c.ref_id == character_id)
    {
        Some(combatant) => combatant.initiative = Some(value),
        None => {
            let sprite_key = repo::load_character(&state.pool, character_id)
                .await
                .ok()
                .and_then(|r| {
                    r.value
                        .get("spriteKey")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_default();
            combat.combatants.push(Combatant {
                id: uuid::Uuid::new_v4().to_string(),
                kind: "pc".into(),
                ref_id: character_id.to_string(),
                name: name.to_string(),
                sprite_key,
                initiative: Some(value),
            });
        }
    }
    sort_initiative(combat);
    repo::save_table_state(&state.pool, campaign_id, &table).await
}

/// Highest first; combatants without a roll go last. Keeps the turn on the same combatant.
fn sort_initiative(combat: &mut Combat) {
    let current = combat
        .combatants
        .get(combat.turn_index)
        .map(|c| c.id.clone());
    combat.combatants.sort_by(|a, b| {
        b.initiative
            .unwrap_or(i32::MIN)
            .cmp(&a.initiative.unwrap_or(i32::MIN))
    });
    if let Some(current) = current {
        combat.turn_index = combat
            .combatants
            .iter()
            .position(|c| c.id == current)
            .unwrap_or(0);
    }
}

// ── Actions ───────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum Action {
    // DM
    SetActiveEncounter {
        encounter_id: Option<String>,
    },
    StartCombat,
    NextTurn,
    EndCombat,
    SetInitiative {
        combatant_id: String,
        value: i32,
    },
    RemoveCombatant {
        combatant_id: String,
    },
    Spotlight {
        title: String,
        #[serde(default)]
        body: String,
        #[serde(default)]
        kind: String,
        #[serde(default)]
        sprite_key: Option<String>,
    },
    ClearSpotlight,
    RequestRoll {
        label: String,
        #[serde(default)]
        roll_kind: String,
        #[serde(default)]
        skill: Option<String>,
        #[serde(default)]
        ability: Option<String>,
        #[serde(default)]
        dc: Option<i32>,
        #[serde(default)]
        hidden_dc: bool,
        #[serde(default)]
        character_ids: Vec<String>,
    },
    ApplyDamage {
        target_kind: String,
        target_id: String,
        amount: i32,
    },
    ToggleCondition {
        target_kind: String,
        target_id: String,
        condition: String,
    },
    Rest {
        kind: String,
    },
    AwardXp {
        amount: i32,
        character_ids: Vec<String>,
    },
    EndEncounter,
    // Players (and DM)
    MoveToken {
        token_id: String,
        x: i32,
        y: i32,
    },
    EndTurn,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ActionResponse {
    ok: bool,
    event: Option<EventRecord>,
}

async fn action(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Path(id): Path<String>,
    Json(action): Json<Action>,
) -> Result<Json<ActionResponse>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    let access = campaign_access(&state.pool, &id, &user.id).await?;
    let ctx = Ctx {
        state: &state,
        campaign_id: &id,
        user: &user,
        access: &access,
    };

    let event = match action {
        Action::MoveToken { token_id, x, y } => ctx.move_token(&token_id, x, y).await?,
        Action::EndTurn => ctx.end_turn().await?,
        dm_action => {
            access.require_dm()?;
            ctx.dm_action(dm_action).await?
        }
    };

    state.notify(&id, "table");
    Ok(Json(ActionResponse { ok: true, event }))
}

struct Ctx<'a> {
    state: &'a AppState,
    campaign_id: &'a str,
    user: &'a UserRow,
    access: &'a CampaignAccess,
}

impl Ctx<'_> {
    async fn event(
        &self,
        kind: &str,
        visibility: &str,
        payload: Value,
    ) -> Result<Option<EventRecord>, ApiError> {
        Ok(Some(
            repo::record_event(
                self.state,
                self.campaign_id,
                Some(&self.user.id),
                kind,
                visibility,
                payload,
            )
            .await?,
        ))
    }

    async fn table(&self) -> Result<TableState, ApiError> {
        repo::load_table_state(&self.state.pool, self.campaign_id).await
    }

    async fn save_table(&self, table: &TableState) -> Result<(), ApiError> {
        repo::save_table_state(&self.state.pool, self.campaign_id, table).await
    }

    async fn active_encounter(&self, table: &TableState) -> Result<Encounter, ApiError> {
        let encounter_id = table
            .active_encounter_id
            .as_ref()
            .ok_or_else(|| ApiError::bad_request("No encounter is live"))?;
        repo::load_encounter(&self.state.pool, self.campaign_id, encounter_id).await
    }

    async fn dm_action(&self, action: Action) -> Result<Option<EventRecord>, ApiError> {
        match action {
            Action::SetActiveEncounter { encounter_id } => {
                let mut table = self.table().await?;
                let name = match &encounter_id {
                    Some(eid) => Some(
                        repo::load_encounter(&self.state.pool, self.campaign_id, eid)
                            .await?
                            .name,
                    ),
                    None => None,
                };
                table.active_encounter_id = encounter_id;
                table.combat = None;
                self.save_table(&table).await?;
                match name {
                    Some(name) => {
                        self.event(
                            "scene",
                            "public",
                            json!({ "text": format!("The party arrives: {name}") }),
                        )
                        .await
                    }
                    None => Ok(None),
                }
            }
            Action::StartCombat => self.start_combat().await,
            Action::NextTurn => self.advance_turn().await,
            Action::EndCombat => {
                let mut table = self.table().await?;
                table.combat = None;
                self.save_table(&table).await?;
                self.event("combat", "public", json!({ "text": "Combat ends." }))
                    .await
            }
            Action::SetInitiative {
                combatant_id,
                value,
            } => {
                let mut table = self.table().await?;
                let combat = table
                    .combat
                    .as_mut()
                    .ok_or_else(|| ApiError::bad_request("No combat in progress"))?;
                let combatant = combat
                    .combatants
                    .iter_mut()
                    .find(|c| c.id == combatant_id)
                    .ok_or_else(|| ApiError::not_found("Combatant not found"))?;
                combatant.initiative = Some(value);
                sort_initiative(combat);
                self.save_table(&table).await?;
                Ok(None)
            }
            Action::RemoveCombatant { combatant_id } => {
                let mut table = self.table().await?;
                let combat = table
                    .combat
                    .as_mut()
                    .ok_or_else(|| ApiError::bad_request("No combat in progress"))?;
                let current = combat
                    .combatants
                    .get(combat.turn_index)
                    .map(|c| c.id.clone());
                combat.combatants.retain(|c| c.id != combatant_id);
                combat.turn_index = current
                    .and_then(|cid| combat.combatants.iter().position(|c| c.id == cid))
                    .unwrap_or(0);
                self.save_table(&table).await?;
                Ok(None)
            }
            Action::Spotlight {
                title,
                body,
                kind,
                sprite_key,
            } => {
                let mut table = self.table().await?;
                table.spotlight = Some(Spotlight {
                    title: title.clone(),
                    body: body.clone(),
                    kind,
                    sprite_key,
                });
                self.save_table(&table).await?;
                self.event(
                    "spotlight",
                    "public",
                    json!({ "title": title, "body": body }),
                )
                .await
            }
            Action::ClearSpotlight => {
                let mut table = self.table().await?;
                table.spotlight = None;
                self.save_table(&table).await?;
                Ok(None)
            }
            Action::RequestRoll {
                label,
                roll_kind,
                skill,
                ability,
                dc,
                hidden_dc,
                character_ids,
            } => {
                let request_id = uuid::Uuid::new_v4().to_string();
                self.event(
                    "roll_request",
                    "public",
                    json!({
                        "requestId": request_id,
                        "label": label,
                        "rollKind": if roll_kind.is_empty() { "check".to_string() } else { roll_kind },
                        "skill": skill,
                        "ability": ability,
                        "dc": dc,
                        "hiddenDc": hidden_dc,
                        "characterIds": character_ids,
                    }),
                )
                .await
            }
            Action::ApplyDamage {
                target_kind,
                target_id,
                amount,
            } => self.apply_damage(&target_kind, &target_id, amount).await,
            Action::ToggleCondition {
                target_kind,
                target_id,
                condition,
            } => {
                self.toggle_condition(&target_kind, &target_id, &condition)
                    .await
            }
            Action::Rest { kind } => self.rest(&kind).await,
            Action::AwardXp {
                amount,
                character_ids,
            } => {
                let names = self.award_xp(amount, &character_ids).await?;
                self.event("xp", "public", json!({ "text": format!("{} gain {amount} XP", names.join(", ")), "amount": amount })).await
            }
            Action::EndEncounter => self.end_encounter().await,
            Action::MoveToken { .. } | Action::EndTurn => {
                unreachable!("handled before the DM check")
            }
        }
    }

    async fn start_combat(&self) -> Result<Option<EventRecord>, ApiError> {
        let mut table = self.table().await?;
        let encounter = self.active_encounter(&table).await?;

        let mut combatants: Vec<Combatant> = encounter
            .monsters
            .iter()
            .filter(|m| m.hit_points_current > 0)
            .map(|m| Combatant {
                id: uuid::Uuid::new_v4().to_string(),
                kind: "monster".into(),
                ref_id: m.id.clone(),
                name: m.name.clone(),
                sprite_key: m.sprite_key.clone(),
                initiative: dice::roll("1d20", Advantage::None)
                    .ok()
                    .map(|r| r.total + m.dex_mod),
            })
            .collect();

        // PCs on the map join combat; if nobody is on the map, the whole party does.
        let mut pc_ids: Vec<String> = encounter
            .player_tokens
            .iter()
            .filter_map(|t| t.character_id.clone())
            .collect();
        if pc_ids.is_empty() {
            pc_ids = repo::member_character_ids(&self.state.pool, self.campaign_id).await?;
        }
        for character_id in pc_ids {
            if let Ok(record) = repo::load_character(&self.state.pool, &character_id).await {
                combatants.push(Combatant {
                    id: uuid::Uuid::new_v4().to_string(),
                    kind: "pc".into(),
                    ref_id: character_id,
                    name: record
                        .value
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_string(),
                    sprite_key: record
                        .value
                        .get("spriteKey")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    initiative: None,
                });
            }
        }

        let mut combat = Combat {
            round: 1,
            turn_index: 0,
            combatants,
        };
        sort_initiative(&mut combat);
        combat.turn_index = 0;
        table.combat = Some(combat);
        self.save_table(&table).await?;
        self.event(
            "combat",
            "public",
            json!({ "text": "Roll for initiative!", "initiative": true }),
        )
        .await
    }

    async fn advance_turn(&self) -> Result<Option<EventRecord>, ApiError> {
        let mut table = self.table().await?;
        let encounter = self.active_encounter(&table).await.ok();
        let combat = table
            .combat
            .as_mut()
            .ok_or_else(|| ApiError::bad_request("No combat in progress"))?;
        if combat.combatants.is_empty() {
            return Ok(None);
        }

        let is_down = |c: &Combatant| {
            c.kind == "monster"
                && encounter
                    .as_ref()
                    .and_then(|e| e.monsters.iter().find(|m| m.id == c.ref_id))
                    .is_none_or(|m| m.hit_points_current <= 0)
        };

        for _ in 0..combat.combatants.len() {
            combat.turn_index += 1;
            if combat.turn_index >= combat.combatants.len() {
                combat.turn_index = 0;
                combat.round += 1;
            }
            if !is_down(&combat.combatants[combat.turn_index]) {
                break;
            }
        }
        let current = combat.combatants[combat.turn_index].clone();
        let round = combat.round;
        self.save_table(&table).await?;
        self.event(
            "turn",
            "public",
            json!({ "text": format!("Round {round}: {}'s turn", current.name), "combatantId": current.id, "round": round }),
        )
        .await
    }

    async fn end_turn(&self) -> Result<Option<EventRecord>, ApiError> {
        if !self.access.is_dm() {
            let table = self.table().await?;
            let mine = my_character_ids(&self.state.pool, self.campaign_id, &self.user.id).await?;
            let is_my_turn = table
                .combat
                .as_ref()
                .and_then(|c| c.combatants.get(c.turn_index))
                .is_some_and(|c| c.kind == "pc" && mine.contains(&c.ref_id));
            if !is_my_turn {
                return Err(ApiError::forbidden("It is not your turn"));
            }
        }
        self.advance_turn().await
    }

    async fn move_token(
        &self,
        token_id: &str,
        x: i32,
        y: i32,
    ) -> Result<Option<EventRecord>, ApiError> {
        let table = self.table().await?;
        let mut encounter = self.active_encounter(&table).await?;
        if x < 0 || y < 0 || x >= encounter.grid_cols || y >= encounter.grid_rows {
            return Err(ApiError::bad_request("That square is off the map"));
        }
        if encounter
            .terrain
            .get(y as usize)
            .and_then(|r| r.get(x as usize))
            .map(String::as_str)
            == Some("wall")
        {
            return Err(ApiError::bad_request("You cannot move into a wall"));
        }
        let occupied = encounter
            .monsters
            .iter()
            .any(|m| m.x == x && m.y == y && m.hit_points_current > 0)
            || encounter
                .player_tokens
                .iter()
                .any(|t| t.id != token_id && t.x == x && t.y == y);
        if occupied {
            return Err(ApiError::bad_request("That square is occupied"));
        }

        let token = encounter
            .player_tokens
            .iter_mut()
            .find(|t| t.id == token_id)
            .ok_or_else(|| ApiError::not_found("Token not found"))?;

        if !self.access.is_dm() {
            let mine = my_character_ids(&self.state.pool, self.campaign_id, &self.user.id).await?;
            let character_id = token.character_id.clone().unwrap_or_default();
            if !mine.contains(&character_id) {
                return Err(ApiError::forbidden("You can only move your own token"));
            }
            if let Some(combat) = &table.combat {
                let is_my_turn = combat
                    .combatants
                    .get(combat.turn_index)
                    .is_some_and(|c| c.kind == "pc" && c.ref_id == character_id);
                if !is_my_turn {
                    return Err(ApiError::forbidden("Wait for your turn to move"));
                }
                let speed = repo::load_character(&self.state.pool, &character_id)
                    .await
                    .map(|r| repo::json_i64(&r.value, "speed"))
                    .unwrap_or(30)
                    .max(5) as i32;
                let distance = (token.x - x).abs().max((token.y - y).abs());
                if distance > speed / 5 {
                    return Err(ApiError::bad_request(format!(
                        "Too far: you can move {} squares",
                        speed / 5
                    )));
                }
            }
        }

        token.x = x;
        token.y = y;
        repo::save_encounter(&self.state.pool, &mut encounter).await?;
        Ok(None)
    }

    async fn apply_damage(
        &self,
        target_kind: &str,
        target_id: &str,
        amount: i32,
    ) -> Result<Option<EventRecord>, ApiError> {
        let verb = if amount >= 0 { "takes" } else { "regains" };
        let shown = amount.abs();
        match target_kind {
            "monster" => {
                let table = self.table().await?;
                let mut encounter = self.active_encounter(&table).await?;
                let monster = encounter
                    .monsters
                    .iter_mut()
                    .find(|m| m.id == target_id)
                    .ok_or_else(|| ApiError::not_found("Monster not found"))?;
                monster.hit_points_current =
                    (monster.hit_points_current - amount).clamp(0, monster.hit_points_max);
                let name = monster.name.clone();
                let status =
                    projection::health_status(monster.hit_points_current, monster.hit_points_max);
                let (hp, max) = (monster.hit_points_current, monster.hit_points_max);
                repo::save_encounter(&self.state.pool, &mut encounter).await?;
                let suffix = if amount >= 0 { " damage" } else { " HP" };
                self.event(
                    "damage",
                    "public",
                    json!({ "text": format!("{name} {verb} {shown}{suffix} ({status})"), "targetId": target_id, "dmDetail": format!("{hp}/{max}") }),
                )
                .await
            }
            "pc" => {
                self.require_member_character(target_id).await?;
                let mut record = repo::load_character(&self.state.pool, target_id).await?;
                let name = record
                    .value
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
                    .to_string();
                let (hp, max) = apply_hp_change(&mut record.value, amount);
                repo::save_character_value(self.state, target_id, &mut record.value).await?;
                let suffix = if amount >= 0 { " damage" } else { " HP" };
                let down = if hp == 0 {
                    " and falls unconscious!"
                } else {
                    ""
                };
                self.event(
                    "damage",
                    "public",
                    json!({ "text": format!("{name} {verb} {shown}{suffix}{down} ({hp}/{max})"), "targetId": target_id }),
                )
                .await
            }
            _ => Err(ApiError::bad_request(
                "targetKind must be 'monster' or 'pc'",
            )),
        }
    }

    async fn toggle_condition(
        &self,
        target_kind: &str,
        target_id: &str,
        condition: &str,
    ) -> Result<Option<EventRecord>, ApiError> {
        let toggle = |list: &mut Vec<String>| -> bool {
            if let Some(index) = list.iter().position(|c| c == condition) {
                list.remove(index);
                false
            } else {
                list.push(condition.to_string());
                true
            }
        };
        let (name, added) = match target_kind {
            "monster" => {
                let table = self.table().await?;
                let mut encounter = self.active_encounter(&table).await?;
                let monster = encounter
                    .monsters
                    .iter_mut()
                    .find(|m| m.id == target_id)
                    .ok_or_else(|| ApiError::not_found("Monster not found"))?;
                let added = toggle(&mut monster.conditions);
                let name = monster.name.clone();
                repo::save_encounter(&self.state.pool, &mut encounter).await?;
                (name, added)
            }
            "pc" => {
                self.require_member_character(target_id).await?;
                let mut record = repo::load_character(&self.state.pool, target_id).await?;
                let mut list = string_list(&record.value, "conditions");
                let added = toggle(&mut list);
                record.value["conditions"] = json!(list);
                let name = record
                    .value
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
                    .to_string();
                repo::save_character_value(self.state, target_id, &mut record.value).await?;
                (name, added)
            }
            _ => {
                return Err(ApiError::bad_request(
                    "targetKind must be 'monster' or 'pc'",
                ));
            }
        };
        let text = if added {
            format!("{name} is now {condition}")
        } else {
            format!("{name} is no longer {condition}")
        };
        self.event("condition", "public", json!({ "text": text }))
            .await
    }

    async fn rest(&self, kind: &str) -> Result<Option<EventRecord>, ApiError> {
        match kind {
            "long" => {
                for character_id in repo::member_character_ids(&self.state.pool, self.campaign_id).await? {
                    let mut record = repo::load_character(&self.state.pool, &character_id).await?;
                    apply_long_rest(&mut record.value);
                    repo::save_character_value(self.state, &character_id, &mut record.value).await?;
                }
                self.event("rest", "public", json!({ "text": "The party takes a long rest. HP, spell slots and hit dice are restored.", "kind": "long" })).await
            }
            "short" => {
                self.event("rest", "public", json!({ "text": "The party takes a short rest. Spend hit dice from your sheet.", "kind": "short" })).await
            }
            _ => Err(ApiError::bad_request("kind must be 'short' or 'long'")),
        }
    }

    async fn award_xp(
        &self,
        amount: i32,
        character_ids: &[String],
    ) -> Result<Vec<String>, ApiError> {
        let members = repo::member_character_ids(&self.state.pool, self.campaign_id).await?;
        let mut names = Vec::new();
        for character_id in character_ids.iter().filter(|id| members.contains(id)) {
            let mut record = repo::load_character(&self.state.pool, character_id).await?;
            let xp = repo::json_i64(&record.value, "experiencePoints") + amount as i64;
            record.value["experiencePoints"] = json!(xp);
            names.push(
                record
                    .value
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
                    .to_string(),
            );
            repo::save_character_value(self.state, character_id, &mut record.value).await?;
        }
        Ok(names)
    }

    /// Ends combat and splits XP from defeated monsters among the PCs on the map.
    async fn end_encounter(&self) -> Result<Option<EventRecord>, ApiError> {
        let mut table = self.table().await?;
        let encounter = self.active_encounter(&table).await?;
        let defeated: Vec<_> = encounter
            .monsters
            .iter()
            .filter(|m| m.hit_points_current <= 0)
            .collect();
        let total_xp: i32 = defeated.iter().map(|m| m.xp).sum();

        let mut pcs: Vec<String> = encounter
            .player_tokens
            .iter()
            .filter_map(|t| t.character_id.clone())
            .collect();
        if pcs.is_empty() {
            pcs = repo::member_character_ids(&self.state.pool, self.campaign_id).await?;
        }
        let share = if pcs.is_empty() {
            0
        } else {
            total_xp / pcs.len() as i32
        };
        if share > 0 {
            self.award_xp(share, &pcs).await?;
        }

        table.combat = None;
        self.save_table(&table).await?;
        self.event(
            "encounter_end",
            "public",
            json!({
                "text": format!("Victory! {} defeated · {} XP each", defeated.len(), share),
                "encounterId": encounter.id,
                "encounterName": encounter.name,
                "defeated": defeated.iter().map(|m| m.name.clone()).collect::<Vec<_>>(),
                "xpEach": share,
            }),
        )
        .await
    }

    async fn require_member_character(&self, character_id: &str) -> Result<(), ApiError> {
        if repo::member_character_ids(&self.state.pool, self.campaign_id)
            .await?
            .iter()
            .any(|id| id == character_id)
        {
            Ok(())
        } else {
            Err(ApiError::not_found("Character not found"))
        }
    }
}

fn string_list(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Positive `amount` is damage (temp HP absorbs first), negative is healing.
/// Returns (current, max) after the change.
pub fn apply_hp_change(character: &mut Value, amount: i32) -> (i64, i64) {
    let max = repo::json_i64(character, "hitPointsMax");
    let mut current = repo::json_i64(character, "hitPointsCurrent");
    let mut temp = repo::json_i64(character, "hitPointsTemp");
    let mut conditions = string_list(character, "conditions");

    if amount >= 0 {
        let absorbed = temp.min(amount as i64);
        temp -= absorbed;
        current = (current - (amount as i64 - absorbed)).max(0);
        if current == 0 && !conditions.iter().any(|c| c == "unconscious") {
            conditions.push("unconscious".into());
        }
    } else {
        let was_down = current == 0;
        current = (current - amount as i64).min(max);
        if current > 0 {
            conditions.retain(|c| c != "unconscious");
            if was_down {
                character["deathSaveSuccesses"] = json!(0);
                character["deathSaveFailures"] = json!(0);
            }
        }
    }

    character["hitPointsCurrent"] = json!(current);
    character["hitPointsTemp"] = json!(temp);
    character["conditions"] = json!(conditions);
    (current, max)
}

pub fn apply_long_rest(character: &mut Value) {
    let max = repo::json_i64(character, "hitPointsMax");
    let total_dice = repo::json_i64(character, "hitDiceTotal");
    let used_dice = repo::json_i64(character, "hitDiceUsed");
    character["hitPointsCurrent"] = json!(max);
    character["hitPointsTemp"] = json!(0);
    character["hitDiceUsed"] = json!((used_dice - (total_dice / 2).max(1)).max(0));
    character["deathSaveSuccesses"] = json!(0);
    character["deathSaveFailures"] = json!(0);
    let conditions: Vec<String> = string_list(character, "conditions")
        .into_iter()
        .filter(|c| c != "unconscious")
        .collect();
    character["conditions"] = json!(conditions);
    if let Some(slots) = character
        .get_mut("spellSlots")
        .and_then(Value::as_array_mut)
    {
        for slot in slots {
            slot["used"] = json!(0);
        }
    }
    if let Some(features) = character.get_mut("resources").and_then(Value::as_array_mut) {
        for resource in features {
            resource["used"] = json!(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damage_uses_temp_hp_then_knocks_out() {
        let mut c = json!({ "hitPointsMax": 10, "hitPointsCurrent": 4, "hitPointsTemp": 3 });
        assert_eq!(apply_hp_change(&mut c, 5), (2, 10));
        assert_eq!(c["hitPointsTemp"], 0);
        assert_eq!(apply_hp_change(&mut c, 9), (0, 10));
        assert_eq!(c["conditions"], json!(["unconscious"]));
        assert_eq!(apply_hp_change(&mut c, -3), (3, 10));
        assert_eq!(c["conditions"], json!([]));
        assert_eq!(apply_hp_change(&mut c, -30), (10, 10));
    }

    #[test]
    fn long_rest_restores() {
        let mut c = json!({
            "hitPointsMax": 12, "hitPointsCurrent": 1, "hitPointsTemp": 2, "hitDiceTotal": 4, "hitDiceUsed": 3,
            "spellSlots": [{ "level": 1, "max": 2, "used": 2 }], "conditions": ["unconscious", "poisoned"]
        });
        apply_long_rest(&mut c);
        assert_eq!(c["hitPointsCurrent"], 12);
        assert_eq!(c["hitDiceUsed"], 1);
        assert_eq!(c["spellSlots"][0]["used"], 0);
        assert_eq!(c["conditions"], json!(["poisoned"]));
    }
}
