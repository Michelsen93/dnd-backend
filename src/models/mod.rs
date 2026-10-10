use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageResponse {
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserResponse {
    pub id: String,
    pub email: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthResponse {
    pub user: UserResponse,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AbilityKey {
    #[serde(rename = "str")]
    Str,
    #[serde(rename = "dex")]
    Dex,
    #[serde(rename = "con")]
    Con,
    #[serde(rename = "int")]
    Int,
    #[serde(rename = "wis")]
    Wis,
    #[serde(rename = "cha")]
    Cha,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AbilityScores {
    #[serde(rename = "str")]
    pub r#str: i32,
    #[serde(rename = "dex")]
    pub dex: i32,
    #[serde(rename = "con")]
    pub con: i32,
    #[serde(rename = "int")]
    pub int: i32,
    #[serde(rename = "wis")]
    pub wis: i32,
    #[serde(rename = "cha")]
    pub cha: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryItem {
    pub id: String,
    pub name: String,
    #[serde(default = "default_quantity")]
    pub quantity: i32,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CharacterSpell {
    pub id: String,
    pub name: String,
    pub level: i32,
    #[serde(default)]
    pub school: Option<String>,
    #[serde(default)]
    pub prepared: Option<bool>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CharacterAbility {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub level: Option<i32>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CharacterSpellSlot {
    pub level: i32,
    pub max: i32,
    #[serde(default)]
    pub used: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Character {
    pub id: String,
    pub name: String,
    pub race: String,
    pub class_id: String,
    pub level: i32,
    pub background: String,
    pub alignment: String,
    pub experience_points: i32,
    pub ability_scores: AbilityScores,
    pub proficiency_bonus_override: Option<i32>,
    pub skill_proficiencies: Vec<String>,
    pub saving_throw_proficiencies: Vec<AbilityKey>,
    pub languages: Vec<String>,
    pub tool_proficiencies: Vec<String>,
    pub hit_points_max: i32,
    pub hit_points_current: i32,
    pub hit_points_temp: i32,
    pub hit_die: i32,
    pub hit_dice_total: i32,
    pub hit_dice_used: i32,
    pub death_save_successes: i32,
    pub death_save_failures: i32,
    pub armor_class: i32,
    pub initiative: i32,
    pub speed: i32,
    #[serde(default)]
    pub equipment: Vec<String>,
    #[serde(default)]
    pub inventory: Vec<InventoryItem>,
    #[serde(default)]
    pub spells: Vec<CharacterSpell>,
    #[serde(default)]
    pub spell_slots: Vec<CharacterSpellSlot>,
    #[serde(default)]
    pub abilities: Vec<CharacterAbility>,
    pub treasure: String,
    pub features: String,
    pub ideals: String,
    pub bonds: String,
    pub flaws: String,
    pub personality_traits: String,
    pub backstory: String,
    pub sprite_key: String,
    pub avatar_url: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// Fields owned by the frontend (attacks, conditions, …) pass through untouched.
    #[serde(flatten)]
    pub extra: JsonMap,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewCharacter {
    pub name: String,
    pub race: String,
    pub class_id: String,
    pub level: i32,
    pub background: String,
    pub alignment: String,
    pub experience_points: i32,
    pub ability_scores: AbilityScores,
    pub proficiency_bonus_override: Option<i32>,
    pub skill_proficiencies: Vec<String>,
    pub saving_throw_proficiencies: Vec<AbilityKey>,
    pub languages: Vec<String>,
    pub tool_proficiencies: Vec<String>,
    pub hit_points_max: i32,
    pub hit_points_current: i32,
    pub hit_points_temp: i32,
    pub hit_die: i32,
    pub hit_dice_total: i32,
    pub hit_dice_used: i32,
    pub death_save_successes: i32,
    pub death_save_failures: i32,
    pub armor_class: i32,
    pub initiative: i32,
    pub speed: i32,
    #[serde(default)]
    pub equipment: Vec<String>,
    #[serde(default)]
    pub inventory: Vec<InventoryItem>,
    #[serde(default)]
    pub spells: Vec<CharacterSpell>,
    #[serde(default)]
    pub spell_slots: Vec<CharacterSpellSlot>,
    #[serde(default)]
    pub abilities: Vec<CharacterAbility>,
    pub treasure: String,
    pub features: String,
    pub ideals: String,
    pub bonds: String,
    pub flaws: String,
    pub personality_traits: String,
    pub backstory: String,
    pub sprite_key: String,
    pub avatar_url: Option<String>,
    #[serde(flatten)]
    pub extra: JsonMap,
}

impl NewCharacter {
    pub fn into_character(self) -> Character {
        let now = now_iso();
        Character {
            id: uuid::Uuid::new_v4().to_string(),
            name: self.name,
            race: self.race,
            class_id: self.class_id,
            level: self.level,
            background: self.background,
            alignment: self.alignment,
            experience_points: self.experience_points,
            ability_scores: self.ability_scores,
            proficiency_bonus_override: self.proficiency_bonus_override,
            skill_proficiencies: self.skill_proficiencies,
            saving_throw_proficiencies: self.saving_throw_proficiencies,
            languages: self.languages,
            tool_proficiencies: self.tool_proficiencies,
            hit_points_max: self.hit_points_max,
            hit_points_current: self.hit_points_current,
            hit_points_temp: self.hit_points_temp,
            hit_die: self.hit_die,
            hit_dice_total: self.hit_dice_total,
            hit_dice_used: self.hit_dice_used,
            death_save_successes: self.death_save_successes,
            death_save_failures: self.death_save_failures,
            armor_class: self.armor_class,
            initiative: self.initiative,
            speed: self.speed,
            equipment: self.equipment,
            inventory: self.inventory,
            spells: self.spells,
            spell_slots: self.spell_slots,
            abilities: self.abilities,
            treasure: self.treasure,
            features: self.features,
            ideals: self.ideals,
            bonds: self.bonds,
            flaws: self.flaws,
            personality_traits: self.personality_traits,
            backstory: self.backstory,
            sprite_key: self.sprite_key,
            avatar_url: self.avatar_url,
            created_at: now.clone(),
            updated_at: now,
            extra: self.extra,
        }
    }
}

fn default_quantity() -> i32 {
    1
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub id: String,
    pub character_id: String,
    pub title: String,
    pub content: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewNote {
    pub title: String,
    pub content: String,
}

impl NewNote {
    pub fn into_note(self, character_id: String) -> Note {
        let now = now_iso();
        Note {
            id: uuid::Uuid::new_v4().to_string(),
            character_id,
            title: self.title,
            content: self.content,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateNote {
    pub title: Option<String>,
    pub content: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthResponse {
    pub status: String,
}

pub fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

// ── Shared table ──────────────────────────────────────────────────────────────

pub type JsonMap = serde_json::Map<String, serde_json::Value>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BattlefieldMonster {
    pub id: String,
    pub name: String,
    pub sprite_key: String,
    pub x: i32,
    pub y: i32,
    pub hit_points_max: i32,
    pub hit_points_current: i32,
    pub armor_class: i32,
    #[serde(default)]
    pub conditions: Vec<String>,
    /// Initiative modifier (DEX mod) used when the server rolls initiative.
    #[serde(default)]
    pub dex_mod: i32,
    /// XP value awarded when defeated.
    #[serde(default)]
    pub xp: i32,
    #[serde(flatten)]
    pub extra: JsonMap,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerToken {
    pub id: String,
    #[serde(default)]
    pub character_id: Option<String>,
    pub name: String,
    pub sprite_key: String,
    pub x: i32,
    pub y: i32,
    pub vision_radius: i32,
    #[serde(flatten)]
    pub extra: JsonMap,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Encounter {
    pub id: String,
    pub campaign_id: String,
    pub name: String,
    pub grid_cols: i32,
    pub grid_rows: i32,
    pub terrain: Vec<Vec<String>>,
    pub visibility: Vec<Vec<bool>>,
    #[serde(default)]
    pub monsters: Vec<BattlefieldMonster>,
    #[serde(default)]
    pub player_tokens: Vec<PlayerToken>,
    #[serde(default = "default_true")]
    pub los_block_by_walls: bool,
    /// Bumped by the server on every write; clients send it back for optimistic concurrency.
    #[serde(default)]
    pub revision: i64,
    #[serde(default)]
    pub created_at: String,
    #[serde(flatten)]
    pub extra: JsonMap,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Combatant {
    pub id: String,
    /// "monster" or "pc"
    pub kind: String,
    /// Monster id (in the active encounter) or character id.
    pub ref_id: String,
    pub name: String,
    #[serde(default)]
    pub sprite_key: String,
    #[serde(default)]
    pub initiative: Option<i32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Combat {
    pub round: i32,
    pub turn_index: usize,
    pub combatants: Vec<Combatant>,
    /// Number of turns advanced so far. While 0, late initiative rolls re-sort and the
    /// turn stays at the top of the order (nobody has acted yet).
    #[serde(default)]
    pub turns_taken: i32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spotlight {
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub sprite_key: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableState {
    #[serde(default)]
    pub active_encounter_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub combat: Option<Combat>,
    #[serde(default)]
    pub spotlight: Option<Spotlight>,
}
