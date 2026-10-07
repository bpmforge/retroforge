//! Settings › Video's shader choice, as data (ticket W20-02;
//! `docs/design/UX_WAVE_20.md` §4, `docs/design/RENDERER.md` §4).
//!
//! `rf_renderer::ShaderChain` and its six first-party shaders shipped with
//! W3-02/W3-02a, each with a provenance manifest and a list of tunable
//! parameters "for a UI-generated control" (`ShaderParamDescriptor`). The
//! app never constructed the chain: Settings showed a free-text field
//! whose hint said shader names "arrive with W3-02a", long after they had.
//!
//! This module is the GPU-free half: which shaders exist, how a saved
//! choice maps back to one, how saved parameter values become a
//! [`ChainStage`] (clamped to the manifest's own range, defaulted when
//! missing), and how much to upscale before shading. The GPU pass itself
//! runs in `crate::app`.
//!
//! A shader is **presentation**, like MetalFX: it post-processes the
//! picture the player sees and nothing else. It does not light the
//! honesty badge, and it never touches the accuracy-exact buffers that
//! compare mode and screenshots are built from (RENDERER.md §4/§6).

use std::collections::BTreeMap;

use rf_renderer::{ChainStage, ShaderKind};
use serde::{Deserialize, Serialize};

/// Every first-party shader, in the order the picker lists them.
pub const KINDS: [ShaderKind; 6] = [
    ShaderKind::Nearest,
    ShaderKind::SharpBilinear,
    ShaderKind::Scanlines,
    ShaderKind::Crt,
    ShaderKind::LcdGrid,
    ShaderKind::Xbr,
];

/// The largest pre-shading upscale. 4× a 256×240 frame is 1024×960 —
/// enough for a scanline or mask pattern to read on a large window,
/// without a per-frame readback that costs more than the frame itself.
pub const MAX_OUTPUT_SCALE: u32 = 4;

/// Ticket W21-04: the name a player sees first — what the shader looks
/// like, not what it is ("CRT TV", not "CRT-class"). The manifest's
/// `display_name` stays beside it, smaller, for people who know it.
#[must_use]
pub const fn player_name(kind: ShaderKind) -> &'static str {
    match kind {
        ShaderKind::Nearest => "Pixel perfect",
        ShaderKind::SharpBilinear => "Smooth pixels",
        ShaderKind::Scanlines => "Scanlines",
        ShaderKind::Crt => "CRT TV",
        ShaderKind::LcdGrid => "Handheld LCD",
        ShaderKind::Xbr => "Smoothed edges",
    }
}

/// The shader a saved id names (its manifest `id`), if any.
#[must_use]
pub fn kind_from_id(id: &str) -> Option<ShaderKind> {
    KINDS.into_iter().find(|k| k.manifest().id == id)
}

/// Per-shader parameter values, keyed by manifest id then parameter name.
/// A section of its own (`[shaders]`) rather than a field on
/// `VideoSettings`, which is `Eq` and cannot hold floats.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ShaderSettings {
    pub params: BTreeMap<String, BTreeMap<String, f32>>,
}

impl ShaderSettings {
    /// `kind`'s parameter values in manifest order — the saved value
    /// clamped to the descriptor's range, or its default when unsaved or
    /// not finite.
    #[must_use]
    pub fn values(&self, kind: ShaderKind) -> Vec<f32> {
        let manifest = kind.manifest();
        let saved = self.params.get(manifest.id);
        manifest
            .params
            .iter()
            .map(|p| {
                saved
                    .and_then(|m| m.get(p.name))
                    .copied()
                    .filter(|v| v.is_finite())
                    .map_or(p.default, |v| v.clamp(p.min, p.max))
            })
            .collect()
    }

    /// Store one parameter value.
    pub fn set(&mut self, kind: ShaderKind, name: &str, value: f32) {
        self.params
            .entry(kind.manifest().id.to_string())
            .or_default()
            .insert(name.to_string(), value);
    }

    /// Forget `kind`'s saved values (back to the manifest defaults).
    pub fn reset(&mut self, kind: ShaderKind) {
        self.params.remove(kind.manifest().id);
    }
}

/// The chain stage for `kind` with `values` in manifest order (missing
/// trailing values take the manifest default).
#[must_use]
pub fn stage(kind: ShaderKind, values: &[f32]) -> ChainStage {
    let params = kind.manifest().params;
    let v = |i: usize| values.get(i).copied().unwrap_or_else(|| params[i].default);
    match kind {
        ShaderKind::Nearest => ChainStage::nearest(),
        ShaderKind::SharpBilinear => ChainStage::sharp_bilinear(v(0)),
        ShaderKind::Scanlines => ChainStage::scanlines(v(0), v(1)),
        ShaderKind::Crt => ChainStage::crt(v(0), v(1), v(2)),
        ShaderKind::LcdGrid => ChainStage::lcd_grid(v(0), v(1), v(2)),
        ShaderKind::Xbr => ChainStage::xbr(v(0), v(1)),
    }
}

/// How many times to upscale a `frame_lines`-tall frame before shading,
/// so the pattern a shader draws is at roughly the size it will be shown
/// in a `play_height`-tall area: the nearest whole multiple, at least 1,
/// at most [`MAX_OUTPUT_SCALE`].
#[must_use]
pub fn output_scale(play_height: f32, frame_lines: usize) -> u32 {
    if frame_lines == 0 || !play_height.is_finite() || play_height <= 0.0 {
        return 1;
    }
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let k = (play_height / frame_lines as f32).round() as u32;
    k.clamp(1, MAX_OUTPUT_SCALE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_round_trips_through_its_manifest_id() {
        for kind in KINDS {
            assert_eq!(kind_from_id(kind.manifest().id), Some(kind));
        }
        assert_eq!(
            kind_from_id("crt-aperture"),
            None,
            "an unknown saved name is no shader"
        );
    }

    #[test]
    fn values_default_clamp_and_ignore_nan() {
        let mut s = ShaderSettings::default();
        let defaults: Vec<f32> = ShaderKind::Scanlines
            .manifest()
            .params
            .iter()
            .map(|p| p.default)
            .collect();
        assert_eq!(s.values(ShaderKind::Scanlines), defaults);
        s.set(ShaderKind::Scanlines, "intensity", 9.0);
        s.set(ShaderKind::Scanlines, "period", f32::NAN);
        let v = s.values(ShaderKind::Scanlines);
        assert_eq!(v[0], 1.0, "clamped to the manifest max");
        assert_eq!(v[1], defaults[1], "NaN falls back to the default");
        s.reset(ShaderKind::Scanlines);
        assert_eq!(s.values(ShaderKind::Scanlines), defaults);
    }

    #[test]
    fn stages_carry_their_kind() {
        let s = ShaderSettings::default();
        for kind in KINDS {
            assert_eq!(stage(kind, &s.values(kind)).kind, kind);
        }
    }

    #[test]
    fn output_scale_tracks_the_play_area_within_bounds() {
        assert_eq!(output_scale(720.0, 240), 3);
        assert_eq!(output_scale(100.0, 240), 1);
        assert_eq!(output_scale(5000.0, 240), MAX_OUTPUT_SCALE);
        assert_eq!(output_scale(f32::NAN, 240), 1);
        assert_eq!(output_scale(720.0, 0), 1);
    }

    #[test]
    fn every_shader_has_a_distinct_player_name() {
        let names: std::collections::BTreeSet<_> = KINDS.iter().map(|k| player_name(*k)).collect();
        assert_eq!(names.len(), KINDS.len());
    }
}
