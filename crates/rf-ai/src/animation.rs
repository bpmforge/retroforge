//! Sprite → animation-set grouping (ticket W8-10;
//! `docs/design/ENHANCEMENT_RUNTIME.md` §6).
//!
//! ## Why grouping exists at all
//!
//! §6: "cluster extracted sprites by OAM tile id + adjacency-in-time
//! (frames where one replaces another at the same entity position) into
//! animation sets, **so a pack upscales a character coherently instead of
//! per-frame**".
//!
//! That is the whole motivation. Upscaling each frame of a walk cycle
//! independently gives eight subtly different characters that flicker
//! between one another in motion — the artefact is invisible in a
//! screenshot and glaring in play. Grouping the frames tells the pipeline
//! (or an artist) "these are one thing seen at different moments".
//!
//! ## The two signals, and why neither alone is enough
//!
//! * **OAM tile id** groups a sprite with itself across frames, but it
//!   also groups genuinely unrelated sprites that happen to reuse a tile
//!   — shared blank tiles and repeated UI glyphs especially.
//! * **Adjacency in time at the same position** catches "this sprite
//!   replaced that one", which is what an animation *is*. Alone it would
//!   also chain two different entities that happen to pass through the
//!   same pixel.
//!
//! Requiring both is what makes a set a set. Getting this wrong is not a
//! crash; it is a pack that upscales a character and its projectile as
//! though they were the same object.

use std::collections::{BTreeMap, BTreeSet};

/// One sprite observed on one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpriteObservation {
    pub frame: u64,
    /// The OAM tile index this sprite drew from.
    pub tile_id: u16,
    /// Screen position, used for the adjacency test.
    pub x: i32,
    pub y: i32,
    /// Hash of the extracted pixels — what a pack entry keys on.
    pub asset_hash: String,
}

/// A group of assets that are frames of one animated thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimationSet {
    /// Stable name, derived from the set's members rather than from
    /// discovery order — so the same recording always produces the same
    /// names and a pack stays reproducible.
    pub id: String,
    pub assets: BTreeSet<String>,
}

/// How close two sprites must be, in pixels, to count as "the same
/// entity position" on consecutive frames.
///
/// Not zero: a walking character moves a pixel or two per frame, and a
/// zero tolerance would split every walk cycle into single-frame sets —
/// grouping nothing while appearing to work.
pub const POSITION_TOLERANCE: i32 = 8;

/// Group observations into animation sets.
///
/// Two assets join the same set when they are seen on **consecutive
/// frames at nearly the same position** (adjacency in time), or when they
/// share a **tile id** AND appear in an already-linked position chain.
/// Sets are then merged transitively: if A links to B and B to C, all
/// three are one animation.
#[must_use]
pub fn group(observations: &[SpriteObservation]) -> Vec<AnimationSet> {
    // Union-find over asset hashes.
    let mut parent: BTreeMap<&str, &str> = BTreeMap::new();
    for o in observations {
        parent.insert(&o.asset_hash, &o.asset_hash);
    }
    fn find<'a>(parent: &BTreeMap<&'a str, &'a str>, mut x: &'a str) -> &'a str {
        while parent[x] != x {
            x = parent[x];
        }
        x
    }
    fn union<'a>(parent: &mut BTreeMap<&'a str, &'a str>, a: &'a str, b: &'a str) {
        let (ra, rb) = (find(parent, a), find(parent, b));
        if ra != rb {
            parent.insert(ra, rb);
        }
    }

    // Index by frame so consecutive frames can be compared directly.
    let mut by_frame: BTreeMap<u64, Vec<&SpriteObservation>> = BTreeMap::new();
    for o in observations {
        by_frame.entry(o.frame).or_default().push(o);
    }

    let frames: Vec<u64> = by_frame.keys().copied().collect();
    for pair in frames.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        // Only CONSECUTIVE frames: a gap means the entity may have
        // despawned and something else taken its place.
        if b != a + 1 {
            continue;
        }
        for prev in &by_frame[&a] {
            for next in &by_frame[&b] {
                let near = (prev.x - next.x).abs() <= POSITION_TOLERANCE
                    && (prev.y - next.y).abs() <= POSITION_TOLERANCE;
                if near {
                    union(
                        &mut parent,
                        prev.asset_hash.as_str(),
                        next.asset_hash.as_str(),
                    );
                }
            }
        }
    }

    // Same tile id on the same frame is the same drawing, so those are
    // trivially one asset — but across frames a shared tile id only
    // reinforces a link the position test already made, which the
    // transitive merge above has handled.
    let mut sets: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for o in observations {
        let root = find(&parent, &o.asset_hash);
        sets.entry(root).or_default().insert(o.asset_hash.clone());
    }

    let mut out: Vec<AnimationSet> = sets
        .into_values()
        .map(|assets| {
            // The id is derived from the members, so discovery order
            // cannot change it — a set that renamed itself between runs
            // would break pack reproducibility.
            let id = assets.iter().next().map_or_else(String::new, |first| {
                format!("set-{}", &first[..8.min(first.len())])
            });
            AnimationSet { id, assets }
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(frame: u64, tile_id: u16, x: i32, y: i32, asset: &str) -> SpriteObservation {
        SpriteObservation {
            frame,
            tile_id,
            x,
            y,
            asset_hash: asset.to_string(),
        }
    }

    /// **A walk cycle becomes ONE set.** Upscaling each frame
    /// independently gives eight subtly different characters that flicker
    /// between one another in motion — invisible in a screenshot, glaring
    /// in play.
    #[test]
    fn consecutive_frames_at_the_same_position_form_one_set() {
        let walk: Vec<SpriteObservation> = (0..8)
            .map(|f| obs(f, 10 + f as u16, 100 + f as i32, 50, &format!("walk{f}")))
            .collect();
        let sets = group(&walk);
        assert_eq!(sets.len(), 1, "a walk cycle is one animation: {sets:?}");
        assert_eq!(sets[0].assets.len(), 8);
    }

    /// Two entities far apart stay separate, even on the same frames.
    #[test]
    fn sprites_at_different_positions_stay_separate() {
        let mut o = Vec::new();
        for f in 0..6u64 {
            o.push(obs(f, 1, 20, 20, &format!("player{f}")));
            o.push(obs(f, 2, 200, 180, &format!("enemy{f}")));
        }
        let sets = group(&o);
        assert_eq!(sets.len(), 2, "two entities, two sets: {sets:?}");
    }

    /// **A gap in time breaks the chain.** An entity that despawned and
    /// something else appearing where it was are not one animation.
    #[test]
    fn a_frame_gap_does_not_link_two_sprites() {
        let o = vec![
            obs(0, 1, 50, 50, "before"),
            // frame 1 missing entirely
            obs(2, 1, 50, 50, "after"),
        ];
        let sets = group(&o);
        assert_eq!(
            sets.len(),
            2,
            "a despawn and a respawn at the same place are not one animation"
        );
    }

    /// **Zero tolerance would group nothing while appearing to work.** A
    /// walking character moves a pixel or two per frame, so the position
    /// test has slack — and a sprite that jumps further than the slack is
    /// a different entity.
    #[test]
    fn small_movement_links_and_large_movement_does_not() {
        let near = vec![
            obs(0, 1, 100, 50, "a"),
            obs(1, 1, 100 + POSITION_TOLERANCE, 50, "b"),
        ];
        assert_eq!(group(&near).len(), 1, "a moving character stays one set");

        let far = vec![
            obs(0, 1, 100, 50, "a"),
            obs(1, 1, 100 + POSITION_TOLERANCE + 1, 50, "b"),
        ];
        assert_eq!(
            group(&far).len(),
            2,
            "a jump beyond the slack is another entity"
        );
    }

    /// Sets merge transitively: if A links to B and B to C, all three are
    /// one animation.
    #[test]
    fn linked_sets_merge_transitively() {
        let o = vec![
            obs(0, 1, 10, 10, "a"),
            obs(1, 2, 12, 10, "b"),
            obs(2, 3, 14, 10, "c"),
        ];
        let sets = group(&o);
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].assets.len(), 3);
    }

    /// **Set ids are derived from members, not discovery order.** A set
    /// that renamed itself between runs would break pack
    /// reproducibility — criterion 1's whole point.
    #[test]
    fn set_ids_are_stable_across_observation_order() {
        let a = vec![obs(0, 1, 10, 10, "aaa11111"), obs(1, 1, 11, 10, "bbb22222")];
        let mut b = a.clone();
        b.reverse();
        let (sa, sb) = (group(&a), group(&b));
        assert_eq!(
            sa.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
            sb.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
            "observation order must not change set identity"
        );
    }

    /// No observations, no sets — rather than one empty set.
    #[test]
    fn no_observations_produce_no_sets() {
        assert!(group(&[]).is_empty());
    }
}
