//! Object classes for the Diorama (ticket W39-02; docs/design/REBUILD_LAB.md
//! section 9, "object-aware Diorama").
//!
//! W34 stands every sprite up as a billboard with the same contact shadow,
//! because the picture alone cannot tell a hero from a bullet. When a recipe
//! binds sprites to object-table slots and kinds (W38-02), the caller knows a
//! class for each sprite and hands it in as a plain [`ObjectClass`] per
//! billboard (the conversion from `rf_rebuild::ObjectTable` happens in the
//! app shell: this crate depends on no recipe type, see `scripts/
//! validate-arch.sh`). The mesh builders then take depth and shadow from the
//! class's [`ClassStyle`] instead of one rule for all.
//!
//! No recipe means no classes: an empty slice, or [`ObjectClass::Unknown`],
//! gives [`ClassStyle::LEGACY`], whose numbers are the pre-W39 constants, so
//! the vertices are bit-identical to `build_vertices`. Everything here is
//! presentation: it reads no core memory and touches neither the Accuracy
//! buffers nor the simulation, so Original mode is untouched by construction.

/// What kind of thing a sprite is, as far as the Diorama cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ObjectClass {
    /// The recipe does not say: the pre-W39 look.
    #[default]
    Unknown,
    /// The character the player moves.
    Player,
    /// A hostile that walks or flies.
    Enemy,
    /// A shot or thrown thing in flight.
    Projectile,
    /// A coin, item or power-up waiting to be taken.
    Pickup,
    /// Fixed furniture drawn as a sprite (a sign, a torch).
    Scenery,
}

/// Depth and shadow rules of one class. Lengths are world pixels of the
/// Diorama grid (the same unit as a billboard's own size).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClassStyle {
    /// The quad's feet float this far above the ground (a hovering pickup, a
    /// bullet at chest height).
    pub lift_px: f32,
    /// The quad stands this far nearer the camera than its footprint, so
    /// the hero reads in front of what shares its row.
    pub z_bias_px: f32,
    /// Scale of the contact ellipse relative to the legacy one (1.0).
    pub shadow_scale: f32,
    /// Opacity of the contact ellipse at the feet.
    pub shadow_alpha: f32,
    /// A lifted object's shadow fades: alpha is divided by
    /// `1 + lift_px * shadow_lift_fade`.
    pub shadow_lift_fade: f32,
    /// Draw a shadow at all.
    pub casts_shadow: bool,
}

impl ClassStyle {
    /// The pre-W39 constants: 1.3 x width by 0.7 x width ellipse at alpha
    /// 0.45, on the ground, at the footprint.
    pub const LEGACY: Self = Self {
        lift_px: 0.0,
        z_bias_px: 0.0,
        shadow_scale: 1.0,
        shadow_alpha: 0.45,
        shadow_lift_fade: 0.0,
        casts_shadow: true,
    };

    /// Effective shadow opacity after the lift fade.
    #[must_use]
    pub fn effective_shadow_alpha(&self) -> f32 {
        self.shadow_alpha / (1.0 + self.lift_px.max(0.0) * self.shadow_lift_fade.max(0.0))
    }
}

impl ObjectClass {
    /// The built-in look of each class. A hero is anchored a touch toward the
    /// camera with a firm shadow; an enemy keeps the legacy look; a shot
    /// floats at body height with a small, faint shadow; a pickup bobs just
    /// off the floor; scenery is part of the set and casts none.
    #[must_use]
    pub fn style(self) -> ClassStyle {
        match self {
            Self::Unknown | Self::Enemy => ClassStyle::LEGACY,
            Self::Player => ClassStyle {
                z_bias_px: 0.5,
                shadow_alpha: 0.55,
                ..ClassStyle::LEGACY
            },
            Self::Projectile => ClassStyle {
                lift_px: 6.0,
                shadow_scale: 0.6,
                shadow_alpha: 0.35,
                shadow_lift_fade: 0.05,
                ..ClassStyle::LEGACY
            },
            Self::Pickup => ClassStyle {
                lift_px: 3.0,
                shadow_scale: 0.75,
                shadow_alpha: 0.4,
                shadow_lift_fade: 0.05,
                ..ClassStyle::LEGACY
            },
            Self::Scenery => ClassStyle {
                casts_shadow: false,
                ..ClassStyle::LEGACY
            },
        }
    }
}

/// The class of billboard `i`: [`ObjectClass::Unknown`] past the end of
/// `classes` (no recipe, or a recipe that bound fewer sprites).
#[must_use]
pub fn class_at(classes: &[ObjectClass], i: usize) -> ObjectClass {
    classes.get(i).copied().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_and_missing_classes_are_the_legacy_style() {
        assert_eq!(ObjectClass::Unknown.style(), ClassStyle::LEGACY);
        assert_eq!(class_at(&[], 3), ObjectClass::Unknown);
        assert_eq!(class_at(&[ObjectClass::Player], 1), ObjectClass::Unknown);
    }

    #[test]
    fn each_class_differs_from_legacy_except_the_enemy() {
        for c in [
            ObjectClass::Player,
            ObjectClass::Projectile,
            ObjectClass::Pickup,
            ObjectClass::Scenery,
        ] {
            assert_ne!(c.style(), ClassStyle::LEGACY, "{c:?}");
        }
        assert_eq!(ObjectClass::Enemy.style(), ClassStyle::LEGACY);
    }

    #[test]
    fn a_lifted_object_has_a_fainter_shadow() {
        let p = ObjectClass::Projectile.style();
        assert!(p.effective_shadow_alpha() < p.shadow_alpha);
        let l = ClassStyle::LEGACY;
        assert_eq!(l.effective_shadow_alpha().to_bits(), 0.45_f32.to_bits());
    }
}
