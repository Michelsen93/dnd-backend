//! Dice notation parser and roller. Mirrors `../dnd/src/utils/dice.ts`.
//!
//! Grammar: terms joined by `+`/`-`. A term is a constant (`3`) or dice (`2d6`, `d20`,
//! `2d20kh1`, `2d20kl1`, `4d6kh3`). Advantage/disadvantage is applied to the first d20 term.

use rand::Rng;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Advantage {
    None,
    Advantage,
    Disadvantage,
}

impl Advantage {
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("adv") | Some("advantage") => Self::Advantage,
            Some("dis") | Some("disadvantage") => Self::Disadvantage,
            _ => Self::None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Keep {
    All,
    High(u32),
    Low(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Term {
    Dice {
        sign: i32,
        count: u32,
        sides: u32,
        keep: Keep,
    },
    Constant(i32),
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiceGroup {
    pub notation: String,
    pub sides: u32,
    pub sign: i32,
    pub rolls: Vec<u32>,
    /// Indexes into `rolls` that count toward the total.
    pub kept: Vec<usize>,
    pub subtotal: i32,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RollResult {
    pub notation: String,
    pub groups: Vec<DiceGroup>,
    pub modifier: i32,
    pub total: i32,
    /// The kept face of the first d20 group, if any (for crit/fumble detection).
    pub natural: Option<u32>,
    pub crit: bool,
    pub fumble: bool,
}

const MAX_DICE: u32 = 100;
const MAX_SIDES: u32 = 1000;

fn parse(notation: &str) -> Result<Vec<Term>, String> {
    let compact: String = notation
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase();
    if compact.is_empty() {
        return Err("Empty dice notation".into());
    }
    let mut terms = Vec::new();
    let mut sign = 1;
    let mut current = String::new();
    let flush = |current: &mut String, sign: i32, terms: &mut Vec<Term>| -> Result<(), String> {
        if current.is_empty() {
            return Err(format!("Unsupported dice notation: {notation}"));
        }
        terms.push(
            parse_term(current, sign)
                .ok_or_else(|| format!("Unsupported dice notation: {notation}"))?,
        );
        current.clear();
        Ok(())
    };
    for (index, ch) in compact.chars().enumerate() {
        match ch {
            '+' | '-' => {
                if index == 0 {
                    sign = if ch == '-' { -1 } else { 1 };
                    continue;
                }
                flush(&mut current, sign, &mut terms)?;
                sign = if ch == '-' { -1 } else { 1 };
            }
            _ => current.push(ch),
        }
    }
    flush(&mut current, sign, &mut terms)?;
    Ok(terms)
}

fn parse_term(raw: &str, sign: i32) -> Option<Term> {
    if let Ok(value) = raw.parse::<i32>() {
        return Some(Term::Constant(sign * value));
    }
    let (count_raw, rest) = raw.split_once('d')?;
    let count = if count_raw.is_empty() {
        1
    } else {
        count_raw.parse::<u32>().ok()?
    };
    let (sides_raw, keep) = if let Some((sides, n)) = rest.split_once("kh") {
        (sides, Keep::High(n.parse().ok()?))
    } else if let Some((sides, n)) = rest.split_once("kl") {
        (sides, Keep::Low(n.parse().ok()?))
    } else {
        (rest, Keep::All)
    };
    let sides = sides_raw.parse::<u32>().ok()?;
    if count == 0 || count > MAX_DICE || !(2..=MAX_SIDES).contains(&sides) {
        return None;
    }
    match keep {
        Keep::High(n) | Keep::Low(n) if n == 0 || n > count => None,
        _ => Some(Term::Dice {
            sign,
            count,
            sides,
            keep,
        }),
    }
}

/// Validate notation without rolling.
pub fn validate(notation: &str) -> Result<(), String> {
    parse(notation).map(|_| ())
}

pub fn roll(notation: &str, advantage: Advantage) -> Result<RollResult, String> {
    let mut rng = rand::thread_rng();
    roll_with(notation, advantage, |sides| rng.gen_range(1..=sides))
}

pub fn roll_with(
    notation: &str,
    advantage: Advantage,
    mut die: impl FnMut(u32) -> u32,
) -> Result<RollResult, String> {
    let mut terms = parse(notation)?;

    if advantage != Advantage::None
        && let Some(Term::Dice { count, keep, .. }) = terms.iter_mut().find(|t| {
            matches!(
                t,
                Term::Dice {
                    sides: 20,
                    count: 1,
                    keep: Keep::All,
                    ..
                }
            )
        })
    {
        *count = 2;
        *keep = if advantage == Advantage::Advantage {
            Keep::High(1)
        } else {
            Keep::Low(1)
        };
    }

    let mut groups = Vec::new();
    let mut modifier = 0;
    for term in &terms {
        match term {
            Term::Constant(value) => modifier += value,
            Term::Dice {
                sign,
                count,
                sides,
                keep,
            } => {
                let rolls: Vec<u32> = (0..*count).map(|_| die(*sides)).collect();
                let mut order: Vec<usize> = (0..rolls.len()).collect();
                order.sort_by_key(|&i| rolls[i]);
                let mut kept: Vec<usize> = match keep {
                    Keep::All => (0..rolls.len()).collect(),
                    Keep::High(n) => order.iter().rev().take(*n as usize).copied().collect(),
                    Keep::Low(n) => order.iter().take(*n as usize).copied().collect(),
                };
                kept.sort_unstable();
                let subtotal = sign * kept.iter().map(|&i| rolls[i] as i32).sum::<i32>();
                let keep_suffix = match keep {
                    Keep::All => String::new(),
                    Keep::High(n) => format!("kh{n}"),
                    Keep::Low(n) => format!("kl{n}"),
                };
                groups.push(DiceGroup {
                    notation: format!(
                        "{}{}d{}{}",
                        if *sign < 0 { "-" } else { "" },
                        count,
                        sides,
                        keep_suffix
                    ),
                    sides: *sides,
                    sign: *sign,
                    rolls,
                    kept,
                    subtotal,
                });
            }
        }
    }

    let total = groups.iter().map(|g| g.subtotal).sum::<i32>() + modifier;
    let natural = groups
        .iter()
        .find(|g| g.sides == 20 && g.kept.len() == 1)
        .map(|g| g.rolls[g.kept[0]]);

    Ok(RollResult {
        notation: notation.trim().to_string(),
        groups,
        modifier,
        total,
        natural,
        crit: natural == Some(20),
        fumble: natural == Some(1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed(values: Vec<u32>) -> impl FnMut(u32) -> u32 {
        let mut iter = values.into_iter();
        move |_| iter.next().expect("enough dice")
    }

    #[test]
    fn rolls_simple_notation() {
        let r = roll_with("2d6+3", Advantage::None, fixed(vec![4, 5])).unwrap();
        assert_eq!(r.total, 12);
        assert_eq!(r.modifier, 3);
        assert_eq!(r.natural, None);
    }

    #[test]
    fn multiple_terms_and_negative() {
        let r = roll_with("1d8+2d6-1", Advantage::None, fixed(vec![8, 1, 2])).unwrap();
        assert_eq!(r.total, 10);
        assert_eq!(r.groups.len(), 2);
    }

    #[test]
    fn advantage_keeps_highest_d20() {
        let r = roll_with("1d20+5", Advantage::Advantage, fixed(vec![3, 20])).unwrap();
        assert_eq!(r.total, 25);
        assert!(r.crit);
        let r = roll_with("d20+5", Advantage::Disadvantage, fixed(vec![1, 20])).unwrap();
        assert_eq!(r.total, 6);
        assert!(r.fumble);
    }

    #[test]
    fn keep_highest_for_ability_scores() {
        let r = roll_with("4d6kh3", Advantage::None, fixed(vec![1, 6, 4, 3])).unwrap();
        assert_eq!(r.total, 13);
    }

    #[test]
    fn rejects_garbage() {
        assert!(validate("").is_err());
        assert!(validate("2x6").is_err());
        assert!(validate("1d1").is_err());
        assert!(validate("1000d6").is_err());
        assert!(validate("2d6+").is_err());
        assert!(validate("3d6kh4").is_err());
        assert!(validate("-1d4+2").is_ok());
    }
}
