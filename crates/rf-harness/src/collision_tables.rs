//! Driving the collision-table finder on a real cartridge (ticket W38-03).
//!
//! The finder ([`rf_rebuild::collision`]) is a [`CollisionReader`] observer;
//! this module runs frames of the data-region drivers under it, attaches the
//! player values the caller names after each frame, and loads the painted
//! height table to compare against. The watched addresses are experiment
//! parameters (a recipe `player_y` row, an address a test names), never read
//! from engine code. Only offsets, ids and counts leave a caller (law 5, D-021).

use std::collections::BTreeMap;

use rf_core_api::{EmulatorCore as _, InputFrame};
use rf_rebuild::collision::{CollisionFacts, CollisionReader};

use crate::data_regions::{NesDriver, SnesDriver};

/// What to watch each frame: little-endian byte addresses forming y and x
/// (the first is the low byte).
#[derive(Debug, Clone)]
pub struct Watch {
    pub y: Vec<u32>,
    pub x: Vec<u32>,
}

fn value(peek: impl Fn(u32) -> u8, addrs: &[u32]) -> i32 {
    addrs
        .iter()
        .enumerate()
        .fold(0i32, |acc, (i, &a)| acc | (i32::from(peek(a)) << (8 * i)))
}

/// Run `inputs` on an NES cartridge, observing from frame `from` on.
/// `pushing(frame)` says whether the pad pushed sideways on that frame;
/// `before(frame, driver)` is the test's chance to poke RAM first.
///
/// # Errors
/// A frame cannot be observed.
pub fn run_nes(
    d: &mut NesDriver,
    inputs: &[InputFrame],
    from: usize,
    watch: &Watch,
    pushing: impl Fn(usize) -> bool,
    before: impl Fn(usize, &mut NesDriver),
) -> Result<CollisionFacts, String> {
    let mut reader = CollisionReader::new(rf_rebuild::Platform::Nes);
    for (f, input) in inputs.iter().enumerate() {
        before(f, d);
        if f < from {
            d.frame(input, false)?;
            continue;
        }
        reader.set_resolver(d.resolver());
        d.frame_with(input, &mut reader)?;
        let bus = d.core.bus();
        let y = value(|a| bus.peek(a as u16), &watch.y);
        let x = value(|a| bus.peek(a as u16), &watch.x);
        reader.attach_player(y, x, pushing(f));
    }
    Ok(reader.take_facts())
}

/// Run `inputs` on a SNES cartridge, observing from frame `from` on; frames
/// where `in_play` is false are left unsampled.
///
/// # Errors
/// A frame cannot be observed.
pub fn run_snes(
    d: &mut SnesDriver,
    inputs: &[InputFrame],
    from: usize,
    watch: &Watch,
    pushing: impl Fn(usize) -> bool,
    in_play: impl Fn(&SnesDriver) -> bool,
) -> Result<CollisionFacts, String> {
    let mut reader = CollisionReader::new(rf_rebuild::Platform::Snes);
    for (f, input) in inputs.iter().enumerate() {
        if f < from {
            d.frame(input, false)?;
            continue;
        }
        reader.set_resolver(d.resolver());
        d.frame_with(input, &mut reader)?;
        // Menus and the map move the watched bytes for other reasons: leave
        // those frames unsampled.
        if in_play(d) {
            let y = value(|a| d.core.peek(a), &watch.y);
            let x = value(|a| d.core.peek(a), &watch.x);
            reader.attach_player(y, x, pushing(f));
        }
    }
    Ok(reader.take_facts())
}

/// The painted `[heights.tiles]` of a profile file, tile number to height in
/// eighths (`None`: the file does not load or has no `[heights]`).
#[must_use]
pub fn painted_heights(profile_toml: &str) -> Option<BTreeMap<u32, u32>> {
    let outcome = rf_profiles::loader::load_str(profile_toml).ok()?;
    let heights = outcome.profile.heights?;
    let map: BTreeMap<u32, u32> = heights
        .tiles
        .iter()
        .filter_map(|(k, v)| {
            Some((
                u32::from(rf_profiles::schema::Heights::parse_tile_key(k)?),
                u32::try_from(*v).ok()?,
            ))
        })
        .collect();
    (!map.is_empty()).then_some(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watched_bytes_are_little_endian() {
        let ram = [0x34u8, 0x12];
        assert_eq!(value(|a| ram[a as usize], &[0, 1]), 0x1234);
        assert_eq!(value(|a| ram[a as usize], &[1]), 0x12);
    }

    #[test]
    fn painted_heights_of_a_non_profile_is_none() {
        assert!(painted_heights("not a profile").is_none());
    }
}
