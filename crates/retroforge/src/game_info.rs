//! Game info: a profile's items (lives, health, coins …) as the player
//! reads them (ticket W27-05; design https://claude.ai/artifact/6NGLfLgWmt8FGfvNsutDLB).
//!
//! Pure: the app hands in the profile's `[[memory_map]]` rows, the bytes
//! the core read for them this frame (`FrameMsg::items`, same order), and
//! the labels the player pinned; out come the rows the Game info section
//! lists and the chips drawn over the game. Reading memory never changes
//! play, so this works in every mode, Accuracy included.

use rf_profiles::schema::MemoryMapEntry;

/// One item as shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The profile row's `label`, the key pins are saved under.
    pub label: String,
    /// What the player reads: "Lives", "Coins".
    pub name: String,
    /// The value as the game shows it.
    pub value: String,
    /// `(value, max)` when the profile names this item's maximum.
    pub bar: Option<(i64, i64)>,
    /// Seen behaving in play on this dump (the census's note), or only
    /// cited from a community map.
    pub checked: bool,
}

/// The rows a profile has that Game info can show: values of up to eight
/// bytes. Wider rows (a level's tile map) are not a number to read.
#[must_use]
pub fn showable(rows: &[MemoryMapEntry]) -> Vec<MemoryMapEntry> {
    rows.iter()
        .filter(|r| (1..=8).contains(&r.len))
        .cloned()
        .collect()
}

/// The words for a label: a few the games use, else the label itself
/// with its underscores as spaces.
#[must_use]
pub fn name_for(label: &str) -> String {
    let known = match label {
        "lives" => "Lives",
        "lives_p2" => "Lives (P2)",
        "health" => "Health",
        "health_max" => "Max health",
        "energy" => "Energy",
        "coins" => "Coins",
        "rupees" => "Rupees",
        "bombs" => "Bombs",
        "keys" => "Keys",
        "missiles" => "Missiles",
        "score" => "Score",
        "timer" => "Time",
        "world" => "World",
        "level" => "Level",
        "stage" => "Stage",
        "area" => "Area",
        "power_up" => "Power-up",
        "weapon" => "Weapon",
        "continues" => "Continues",
        "player_x" => "Player X",
        "player_y" => "Player Y",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_owned();
    }
    let mut out = label.replace('_', " ");
    if let Some(first) = out.get(..1) {
        out = first.to_uppercase() + &out[1..];
    }
    out
}

/// Every showable item with this frame's value (rows and `values` in the
/// same order; a missing value reads as nothing yet).
#[must_use]
pub fn items(rows: &[MemoryMapEntry], values: &[Vec<u8>]) -> Vec<Item> {
    rows.iter()
        .enumerate()
        .map(|(i, row)| {
            let bytes = values.get(i).map_or(&[][..], Vec::as_slice);
            let bar = row.max_label.as_ref().and_then(|max| {
                let at = rows.iter().position(|r| &r.label == max)?;
                let max_bytes = values.get(at)?;
                Some((row.number(bytes), rows[at].number(max_bytes)))
            });
            Item {
                label: row.label.clone(),
                name: name_for(&row.label),
                value: if bytes.is_empty() {
                    "\u{2014}".to_owned()
                } else {
                    row.display(bytes)
                },
                bar,
                checked: row
                    .notes
                    .as_deref()
                    .is_some_and(|n| n.contains("atched in play on this dump")),
            }
        })
        .collect()
}

/// The pinned items, in the order pinned, at most four (the design's
/// corner holds four chips).
#[must_use]
pub fn chips(all: &[Item], pinned: &[String]) -> Vec<Item> {
    pinned
        .iter()
        .filter_map(|label| all.iter().find(|i| &i.label == label).cloned())
        .take(4)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(label: &str, addr: u32, len: u32) -> MemoryMapEntry {
        MemoryMapEntry {
            addr,
            len,
            ty: "u8".into(),
            label: label.into(),
            notes: Some("Watched in play on this dump: 2-4.".into()),
            source: Some("test".into()),
            show_add: 0,
            show: None,
            names: Vec::new(),
            max_label: None,
        }
    }

    #[test]
    fn items_read_like_the_game() {
        let mut lives = row("lives", 0x75A, 1);
        lives.show_add = 1;
        let mut hp = row("health", 0x10, 1);
        hp.max_label = Some("health_max".into());
        let max = row("health_max", 0x11, 1);
        let map = row("screen_tile_mapping", 0x6530, 0x2C0);
        let rows = showable(&[lives, hp, max, map]);
        assert_eq!(rows.len(), 3, "a tile map is not a value");
        let all = items(&rows, &[vec![2], vec![3], vec![5]]);
        assert_eq!(
            (all[0].name.as_str(), all[0].value.as_str()),
            ("Lives", "3")
        );
        assert!(all[0].checked);
        assert_eq!(all[1].bar, Some((3, 5)));
        assert_eq!(name_for("energy_tanks"), "Energy tanks");
    }

    #[test]
    fn chips_follow_the_pins_in_order_up_to_four() {
        let rows: Vec<MemoryMapEntry> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|l| row(l, 0, 1))
            .collect();
        let all = items(&rows, &[vec![1], vec![2], vec![3], vec![4], vec![5]]);
        let pins: Vec<String> = ["e", "a", "zz", "b", "c", "d"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let labels: Vec<String> = chips(&all, &pins).into_iter().map(|i| i.label).collect();
        assert_eq!(labels, ["e", "a", "b", "c"]);
    }
}
