//! What players are allowed to see. The DM gets raw state; players get a projection.

use serde_json::{Value, json};

use crate::models::Encounter;

fn clear_line_of_sight(encounter: &Encounter, from: (i32, i32), to: (i32, i32)) -> bool {
    let (mut x, mut y) = from;
    let (tx, ty) = to;
    let dx = (tx - x).abs();
    let dy = (ty - y).abs();
    let sx = if x < tx { 1 } else { -1 };
    let sy = if y < ty { 1 } else { -1 };
    let mut err = dx - dy;
    while !(x == tx && y == ty) {
        let e2 = 2 * err;
        if e2 > -dy {
            err -= dy;
            x += sx;
        }
        if e2 < dx {
            err += dx;
            y += sy;
        }
        if x == tx && y == ty {
            break;
        }
        if terrain_at(encounter, x, y) == Some("wall") {
            return false;
        }
    }
    true
}

fn terrain_at(encounter: &Encounter, x: i32, y: i32) -> Option<&str> {
    if x < 0 || y < 0 {
        return None;
    }
    encounter
        .terrain
        .get(y as usize)?
        .get(x as usize)
        .map(String::as_str)
}

/// Cells players can see: DM-revealed cells plus everything in a token's vision radius
/// (blocked by walls when enabled). Mirrors `features/encounter/visibility.ts`.
pub fn player_visibility(encounter: &Encounter) -> Vec<Vec<bool>> {
    (0..encounter.grid_rows)
        .map(|row| {
            (0..encounter.grid_cols)
                .map(|col| {
                    let revealed = encounter
                        .visibility
                        .get(row as usize)
                        .and_then(|r| r.get(col as usize))
                        .copied()
                        .unwrap_or(false);
                    revealed
                        || encounter.player_tokens.iter().any(|token| {
                            let dx = token.x - col;
                            let dy = token.y - row;
                            if dx * dx + dy * dy > token.vision_radius * token.vision_radius {
                                return false;
                            }
                            !encounter.los_block_by_walls
                                || clear_line_of_sight(encounter, (token.x, token.y), (col, row))
                        })
                })
                .collect()
        })
        .collect()
}

pub fn is_visible(visibility: &[Vec<bool>], x: i32, y: i32) -> bool {
    x >= 0
        && y >= 0
        && visibility
            .get(y as usize)
            .and_then(|r| r.get(x as usize))
            .copied()
            .unwrap_or(false)
}

/// A coarse health descriptor so players can judge a fight without seeing numbers.
pub fn health_status(current: i32, max: i32) -> &'static str {
    if current <= 0 {
        "down"
    } else if current >= max {
        "unhurt"
    } else if current * 2 <= max {
        "bloodied"
    } else {
        "hurt"
    }
}

const DM_ONLY_MONSTER_KEYS: &[&str] = &[
    "hitPointsCurrent",
    "hitPointsMax",
    "armorClass",
    "dexMod",
    "xp",
    "notes",
    "actions",
    "statBlockId",
];

/// Player view of an encounter: hidden monsters removed, numbers replaced by descriptors,
/// unseen terrain masked as "fog", and `visibility` replaced by the computed player view.
pub fn project_encounter_for_player(encounter: &Encounter) -> Value {
    let visibility = player_visibility(encounter);

    let terrain: Vec<Vec<String>> = encounter
        .terrain
        .iter()
        .enumerate()
        .map(|(row, cells)| {
            cells
                .iter()
                .enumerate()
                .map(|(col, tile)| {
                    if is_visible(&visibility, col as i32, row as i32) {
                        tile.clone()
                    } else {
                        "fog".to_string()
                    }
                })
                .collect()
        })
        .collect();

    let monsters: Vec<Value> = encounter
        .monsters
        .iter()
        .filter(|m| is_visible(&visibility, m.x, m.y))
        .map(|m| {
            let mut value = serde_json::to_value(m).unwrap_or(Value::Null);
            if let Value::Object(map) = &mut value {
                for key in DM_ONLY_MONSTER_KEYS {
                    map.remove(*key);
                }
                map.insert(
                    "status".into(),
                    json!(health_status(m.hit_points_current, m.hit_points_max)),
                );
            }
            value
        })
        .collect();

    let mut value = serde_json::to_value(encounter).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert("terrain".into(), json!(terrain));
        map.insert("visibility".into(), json!(visibility));
        map.insert("monsters".into(), json!(monsters));
        map.remove("dmNotes");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{BattlefieldMonster, PlayerToken};

    fn encounter() -> Encounter {
        let mut terrain = vec![vec!["grass".to_string(); 6]; 1];
        terrain[0][2] = "wall".into();
        Encounter {
            id: "e".into(),
            campaign_id: "c".into(),
            name: "Hall".into(),
            grid_cols: 6,
            grid_rows: 1,
            terrain,
            visibility: vec![vec![false; 6]],
            monsters: vec![
                BattlefieldMonster {
                    id: "near".into(),
                    name: "Goblin".into(),
                    sprite_key: "goblin".into(),
                    x: 1,
                    y: 0,
                    hit_points_max: 7,
                    hit_points_current: 3,
                    armor_class: 15,
                    conditions: vec![],
                    dex_mod: 2,
                    xp: 50,
                    extra: Default::default(),
                },
                BattlefieldMonster {
                    id: "behind-wall".into(),
                    name: "Orc".into(),
                    sprite_key: "orc".into(),
                    x: 4,
                    y: 0,
                    hit_points_max: 15,
                    hit_points_current: 15,
                    armor_class: 13,
                    conditions: vec![],
                    dex_mod: 1,
                    xp: 100,
                    extra: Default::default(),
                },
            ],
            player_tokens: vec![PlayerToken {
                id: "t".into(),
                character_id: Some("pc".into()),
                name: "Pip".into(),
                sprite_key: "rogue".into(),
                x: 0,
                y: 0,
                vision_radius: 10,
                extra: Default::default(),
            }],
            los_block_by_walls: true,
            revision: 0,
            created_at: String::new(),
            extra: Default::default(),
        }
    }

    #[test]
    fn walls_block_vision() {
        let vis = player_visibility(&encounter());
        assert_eq!(vis[0], vec![true, true, true, false, false, false]);
    }

    #[test]
    fn projection_hides_monsters_and_numbers() {
        let projected = project_encounter_for_player(&encounter());
        let monsters = projected["monsters"].as_array().unwrap();
        assert_eq!(monsters.len(), 1);
        assert_eq!(monsters[0]["id"], "near");
        assert_eq!(monsters[0]["status"], "bloodied");
        assert!(monsters[0].get("hitPointsCurrent").is_none());
        assert!(monsters[0].get("armorClass").is_none());
        assert_eq!(projected["terrain"][0][4], "fog");
    }

    #[test]
    fn health_descriptors() {
        assert_eq!(health_status(10, 10), "unhurt");
        assert_eq!(health_status(6, 10), "hurt");
        assert_eq!(health_status(5, 10), "bloodied");
        assert_eq!(health_status(0, 10), "down");
    }
}
