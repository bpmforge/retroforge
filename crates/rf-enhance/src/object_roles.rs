//! Object roles from a recipe's object table (ticket W39-02).
//!
//! A recipe's object table (`rf_rebuild::ObjectTable`: base, slot count,
//! stride, a kind offset) says where the game keeps its objects; which slot
//! is the player and what each kind byte means is recipe knowledge too. This
//! module holds that knowledge as plain data, so the layer law holds
//! (`rf-enhance` and `rf-renderer` depend on neither `rf-rebuild` nor each
//! other): the app shell fills [`ObjectRoles`] from the recipe, asks
//! [`ObjectRoles::role_of`] per sprite, and maps [`ObjectRole`] to
//! `rf_renderer::diorama_objects::ObjectClass`. No recipe means an empty
//! table and every sprite is [`ObjectRole::Unknown`].

use std::collections::BTreeMap;

/// What a recipe says an object is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ObjectRole {
    #[default]
    Unknown,
    Player,
    Enemy,
    Projectile,
    Pickup,
    Scenery,
}

/// Slot and kind knowledge from a recipe.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObjectRoles {
    /// Slots that always hold the player (e.g. slot 0).
    pub player_slots: Vec<u32>,
    /// Role of each kind byte.
    pub kinds: BTreeMap<u32, ObjectRole>,
}

impl ObjectRoles {
    /// The role of the object in `slot` whose kind byte is `kind` (`None`
    /// when the table has no kind offset or the byte was not read). A player
    /// slot wins over its kind byte.
    #[must_use]
    pub fn role_of(&self, slot: u32, kind: Option<u32>) -> ObjectRole {
        if self.player_slots.contains(&slot) {
            return ObjectRole::Player;
        }
        kind.and_then(|k| self.kinds.get(&k).copied())
            .unwrap_or_default()
    }

    /// The slot a RAM address falls in, for a table of `slots` objects of
    /// `stride` bytes from `base`; `None` outside it or for a zero stride.
    #[must_use]
    pub fn slot_of(base: u32, slots: u32, stride: u32, addr: u32) -> Option<u32> {
        let off = addr.checked_sub(base)?;
        let slot = off.checked_div(stride)?;
        (slot < slots).then_some(slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roles() -> ObjectRoles {
        ObjectRoles {
            player_slots: vec![0],
            kinds: BTreeMap::from([(0x10, ObjectRole::Enemy), (0x20, ObjectRole::Pickup)]),
        }
    }

    #[test]
    fn player_slot_beats_kind_and_unknown_is_the_default() {
        let r = roles();
        assert_eq!(r.role_of(0, Some(0x10)), ObjectRole::Player);
        assert_eq!(r.role_of(3, Some(0x10)), ObjectRole::Enemy);
        assert_eq!(r.role_of(3, Some(0x99)), ObjectRole::Unknown);
        assert_eq!(r.role_of(3, None), ObjectRole::Unknown);
        assert_eq!(
            ObjectRoles::default().role_of(0, Some(1)),
            ObjectRole::Unknown
        );
    }

    #[test]
    fn slot_of_is_bounded_and_survives_a_zero_stride() {
        assert_eq!(ObjectRoles::slot_of(0x300, 8, 16, 0x300), Some(0));
        assert_eq!(ObjectRoles::slot_of(0x300, 8, 16, 0x31f), Some(1));
        assert_eq!(ObjectRoles::slot_of(0x300, 8, 16, 0x380), None);
        assert_eq!(ObjectRoles::slot_of(0x300, 8, 16, 0x2ff), None);
        assert_eq!(ObjectRoles::slot_of(0x300, 8, 0, 0x300), None);
    }
}
