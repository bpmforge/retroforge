//! The profile's scene key on the core thread (ticket W33-09).
//!
//! `[[scene.level_id]]` rows name RAM bytes that say which level the game is
//! in. [`SceneKeySpec`] is the profile's table cut down to what the core
//! thread needs each frame: read the bytes, say whether the game is in a
//! level at all, hand both to the scene tracker
//! ([`rf_enhance::scene_tracker::SceneHints`]).

use rf_profiles::schema::Scene;

/// One row: where, how wide, and what it reads when no level is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneKeyRow {
    pub addr: u32,
    pub len: u8,
    pub idle: u32,
    /// Whether reading something other than `idle` says a level is running
    /// (`[[scene.level_id]].decides`).
    pub decides: bool,
}

/// A profile's `[scene]` table, ready to read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SceneKeySpec {
    pub rows: Vec<SceneKeyRow>,
    /// `sections = "axis"` (see `SceneHints::axis_sections`); `None` leaves
    /// the tracker's default (on without rows, off with them).
    pub axis_sections: Option<bool>,
}

impl SceneKeySpec {
    #[must_use]
    pub fn from_profile(scene: &Scene) -> Self {
        Self {
            rows: scene
                .level_id
                .iter()
                .map(|r| SceneKeyRow {
                    addr: r.addr,
                    len: r.len.unwrap_or(1).clamp(1, 4),
                    idle: r.idle.unwrap_or(0),
                    decides: r.decides.unwrap_or(true),
                })
                .collect(),
            axis_sections: scene.sections.as_deref().map(|s| s == "axis"),
        }
    }

    /// The key's bytes (every row, little-endian), or `None` when there are no
    /// rows or every row reads its `idle` value: the game is not in a level.
    /// `peek` reads one byte of the console's bus.
    #[must_use]
    pub fn read(&self, peek: impl Fn(u32) -> u8) -> Option<Vec<u8>> {
        let mut bytes = Vec::with_capacity(self.rows.len() * 2);
        let mut in_level = false;
        for row in &self.rows {
            let mut value = 0u32;
            for i in 0..u32::from(row.len) {
                let b = peek(row.addr + i);
                bytes.push(b);
                value |= u32::from(b) << (8 * i);
            }
            in_level |= row.decides && value != row.idle;
        }
        in_level.then_some(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> SceneKeySpec {
        SceneKeySpec {
            rows: vec![
                SceneKeyRow {
                    addr: 0x61,
                    len: 2,
                    idle: 0,
                    decides: true,
                },
                SceneKeyRow {
                    addr: 0x70,
                    len: 1,
                    idle: 0xFF,
                    decides: true,
                },
            ],
            axis_sections: None,
        }
    }

    #[test]
    fn every_row_at_its_idle_value_is_no_level() {
        let ram = |a: u32| if a == 0x70 { 0xFF } else { 0 };
        assert_eq!(spec().read(ram), None);
    }

    #[test]
    fn one_row_off_idle_is_a_level_and_the_key_is_every_rows_bytes_little_endian() {
        let ram = |a: u32| match a {
            0x61 => 0x92,
            0x62 => 0xBC,
            0x70 => 0xFF,
            _ => 0,
        };
        assert_eq!(spec().read(ram), Some(vec![0x92, 0xBC, 0xFF]));
    }

    #[test]
    fn a_row_that_does_not_decide_joins_the_key_but_cannot_start_a_level() {
        let mut spec = spec();
        spec.rows[0].decides = false;
        // The word holds stale values on the map: no level while the deciding
        // row idles ...
        let stale = |a: u32| if a == 0x70 { 0xFF } else { 0x77 };
        assert_eq!(spec.read(stale), None);
        // ... and its bytes are part of the key once the other row says so.
        let ram = |a: u32| match a {
            0x61 => 0x92,
            0x62 => 0xBC,
            0x70 => 0x05,
            _ => 0,
        };
        assert_eq!(spec.read(ram), Some(vec![0x92, 0xBC, 0x05]));
    }

    #[test]
    fn a_spec_with_no_rows_never_names_a_level() {
        assert_eq!(SceneKeySpec::default().read(|_| 7), None);
    }

    #[test]
    fn the_profile_table_becomes_a_spec() {
        let text = r#"
            [meta]
            profile_version = "0.1"
            title = "t"
            console = "nes"
            region = "ntsc"

            [[identity]]
            sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd"

            [scene]
            sections = "axis"

            [[scene.level_id]]
            addr = 0x2A
            idle = 0
            source = "observed: test"
        "#;
        let profile = rf_profiles::load_str(text).expect("loads").profile;
        let spec = SceneKeySpec::from_profile(profile.scene.as_ref().expect("scene"));
        assert_eq!(
            spec,
            SceneKeySpec {
                rows: vec![SceneKeyRow {
                    addr: 0x2A,
                    len: 1,
                    idle: 0,
                    decides: true
                }],
                axis_sections: Some(true),
            }
        );
    }
}
