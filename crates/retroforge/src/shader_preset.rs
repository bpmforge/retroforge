//! Shader presets: named, saved bundles of shader stages and their
//! parameter values (ticket W39-07; `docs/design/RENDERER.md` §4,
//! `docs/design/UX_WAVE_20.md` §4).
//!
//! A preset is **presentation only**, exactly like a single shader: it
//! post-processes the picture the player sees and never the
//! accuracy-exact buffers, so it behaves identically with Rebuild on or
//! off (Rebuild changes what the core renders; a preset changes how that
//! finished frame is drawn). Nothing here touches a core.
//!
//! A preset is saved the same way the plain shader choice is:
//! `VideoSettings::shader` holds a string, and a preset is the string
//! `preset:<id>`. That keeps the per-game / per-console / global layering
//! (W21-04) working unchanged, and a settings file naming a preset this
//! build lacks loads as "unknown" and shows the plain picture, as an
//! unknown shader id already does.
//!
//! First-party presets are built from existing shaders only; no new WGSL
//! was needed (a CRT, an LCD grid and the xBR-class scaler cover them).
//! Player presets live in `ShaderSettings::presets`.

use std::collections::BTreeMap;

use rf_renderer::{ChainStage, ShaderKind};
use serde::{Deserialize, Serialize};

use crate::shader_select::{kind_from_id, stage, ShaderSettings};

/// Prefix that marks a saved `VideoSettings::shader` string as a preset.
pub const PRESET_PREFIX: &str = "preset:";

/// Pre-upscale applied to every stage but the last in a multi-stage
/// preset, so a scaler hands the next stage a larger picture to work on.
pub const INTERMEDIATE_SCALE: u32 = 2;

/// One stage of a preset: a shader (by manifest id) and the parameter
/// values it should run with (by parameter name; missing means default).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PresetStage {
    pub shader: String,
    pub params: BTreeMap<String, f32>,
}

impl PresetStage {
    /// The shader this stage names, if this build has it.
    #[must_use]
    pub fn kind(&self) -> Option<ShaderKind> {
        kind_from_id(&self.shader)
    }

    /// Parameter values in manifest order, each clamped to the manifest
    /// range, defaulted when missing or not finite.
    #[must_use]
    pub fn values(&self, kind: ShaderKind) -> Vec<f32> {
        kind.manifest()
            .params
            .iter()
            .map(|p| {
                self.params
                    .get(p.name)
                    .copied()
                    .filter(|v| v.is_finite())
                    .map_or(p.default, |v| v.clamp(p.min, p.max))
            })
            .collect()
    }

    fn of(kind: ShaderKind, values: &[(&str, f32)]) -> Self {
        PresetStage {
            shader: kind.manifest().id.to_string(),
            params: values.iter().map(|(n, v)| ((*n).to_string(), *v)).collect(),
        }
    }
}

/// A named chain of stages.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ShaderPreset {
    /// Stable id; the saved selection is `preset:<id>`.
    pub id: String,
    /// What the player sees in the picker.
    pub name: String,
    pub stages: Vec<PresetStage>,
}

/// The first-party presets, in picker order. The values are looks, not
/// measurements: each is a starting point inside the manifest ranges.
#[must_use]
pub fn first_party() -> Vec<ShaderPreset> {
    let mk = |id: &str, name: &str, stages: Vec<PresetStage>| ShaderPreset {
        id: id.to_string(),
        name: name.to_string(),
        stages,
    };
    vec![
        mk(
            "consumer-crt",
            "Consumer CRT",
            vec![PresetStage::of(
                ShaderKind::Crt,
                &[
                    ("beam_sigma", 0.35),
                    ("mask_strength", 0.35),
                    ("gamma", 2.2),
                ],
            )],
        ),
        mk(
            "pvm",
            "PVM",
            vec![PresetStage::of(
                ShaderKind::Crt,
                &[("beam_sigma", 0.2), ("mask_strength", 0.15), ("gamma", 2.4)],
            )],
        ),
        mk(
            "sharp-lcd",
            "Sharp LCD",
            vec![PresetStage::of(
                ShaderKind::LcdGrid,
                &[
                    ("grid_strength", 0.35),
                    ("gap", 0.08),
                    ("subpixel_strength", 0.2),
                ],
            )],
        ),
        mk(
            "smooth-upscaler",
            "Smooth upscaler",
            vec![
                PresetStage::of(ShaderKind::Xbr, &[("threshold", 0.4), ("strength", 1.0)]),
                PresetStage::of(ShaderKind::SharpBilinear, &[("sharpness", 0.6)]),
            ],
        ),
        mk(
            "smooth-crt",
            "Smoothed CRT",
            vec![
                PresetStage::of(ShaderKind::Xbr, &[("threshold", 0.4), ("strength", 0.8)]),
                PresetStage::of(
                    ShaderKind::Crt,
                    &[("beam_sigma", 0.3), ("mask_strength", 0.25), ("gamma", 2.2)],
                ),
            ],
        ),
    ]
}

/// What a saved `VideoSettings::shader` string means.
#[derive(Debug, Clone, PartialEq)]
pub enum Selection {
    /// Nothing saved: the plain picture.
    None,
    /// A single shader, with its per-shader tuned values.
    Kind(ShaderKind),
    /// A preset (first-party or the player's).
    Preset(ShaderPreset),
    /// Names a shader or preset this build lacks: plain picture, and the
    /// UI says so.
    Unknown,
}

/// `preset:<id>` for `id`.
#[must_use]
pub fn saved_id(id: &str) -> String {
    format!("{PRESET_PREFIX}{id}")
}

/// Find a preset by id: player presets first would let a player shadow a
/// first-party id, so first-party wins and ids of saved presets are
/// generated to avoid them (see [`ShaderSettings::save_preset`]).
#[must_use]
pub fn find(id: &str, settings: &ShaderSettings) -> Option<ShaderPreset> {
    first_party()
        .into_iter()
        .find(|p| p.id == id)
        .or_else(|| settings.presets.get(id).cloned())
}

/// Resolve a saved string. An unknown preset id (or a preset whose every
/// stage names an unknown shader) is [`Selection::Unknown`], never a panic.
#[must_use]
pub fn resolve(saved: Option<&str>, settings: &ShaderSettings) -> Selection {
    let Some(s) = saved else {
        return Selection::None;
    };
    if let Some(id) = s.strip_prefix(PRESET_PREFIX) {
        return match find(id, settings) {
            Some(p) if p.stages.iter().any(|st| st.kind().is_some()) => Selection::Preset(p),
            _ => Selection::Unknown,
        };
    }
    kind_from_id(s).map_or(Selection::Unknown, Selection::Kind)
}

/// The GPU stages for a selection over a `w`x`h` source shown at `n`x.
/// The last stage outputs `w*n` x `h*n`; earlier stages output
/// [`INTERMEDIATE_SCALE`]x. Stages naming shaders this build lacks are
/// skipped. `None` for [`Selection::None`] / [`Selection::Unknown`].
#[must_use]
pub fn chain_stages(
    selection: &Selection,
    settings: &ShaderSettings,
    w: u32,
    h: u32,
    n: u32,
) -> Option<Vec<ChainStage>> {
    let planned: Vec<(ShaderKind, Vec<f32>)> = match selection {
        Selection::None | Selection::Unknown => return None,
        Selection::Kind(k) => vec![(*k, settings.values(*k))],
        Selection::Preset(p) => p
            .stages
            .iter()
            .filter_map(|s| s.kind().map(|k| (k, s.values(k))))
            .collect(),
    };
    let last = planned.len().checked_sub(1)?;
    Some(
        planned
            .into_iter()
            .enumerate()
            .map(|(i, (k, v))| {
                let m = if i == last { n } else { INTERMEDIATE_SCALE };
                stage(k, &v).with_out_size(w * m, h * m)
            })
            .collect(),
    )
}

/// Turn a player's name into a preset id that cannot collide with a
/// first-party one or an existing player preset.
#[must_use]
pub fn fresh_id(name: &str, settings: &ShaderSettings) -> String {
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let base = format!("user-{}", if slug.is_empty() { "preset" } else { &slug });
    let mut id = base.clone();
    // Terminates: each pass appends a larger suffix and the set of taken
    // ids is finite.
    let mut n = 2u32;
    while find(&id, settings).is_some() {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}

impl ShaderSettings {
    /// Save the stages `selection` currently shows, under `name`, as a
    /// player preset; returns its id. `None`/`Unknown` save nothing.
    pub fn save_preset(&mut self, name: &str, selection: &Selection) -> Option<String> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        let stages = match selection {
            Selection::None | Selection::Unknown => return None,
            Selection::Kind(k) => vec![PresetStage {
                shader: k.manifest().id.to_string(),
                params: k
                    .manifest()
                    .params
                    .iter()
                    .zip(self.values(*k))
                    .map(|(p, v)| (p.name.to_string(), v))
                    .collect(),
            }],
            Selection::Preset(p) => p.stages.clone(),
        };
        let id = fresh_id(name, self);
        self.presets.insert(
            id.clone(),
            ShaderPreset {
                id: id.clone(),
                name: name.chars().take(40).collect(),
                stages,
            },
        );
        Some(id)
    }

    /// Delete a player preset (first-party ones cannot be deleted).
    pub fn delete_preset(&mut self, id: &str) -> bool {
        self.presets.remove(id).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_party_presets_are_unique_and_in_manifest_range() {
        let mut ids = std::collections::BTreeSet::new();
        for p in first_party() {
            assert!(ids.insert(p.id.clone()), "duplicate id {}", p.id);
            assert!(!p.stages.is_empty());
            for st in &p.stages {
                let kind = st.kind().expect("first-party preset names a real shader");
                for (v, d) in st.values(kind).iter().zip(kind.manifest().params) {
                    assert!(
                        (d.min..=d.max).contains(v),
                        "{}: {} out of range",
                        p.id,
                        d.name
                    );
                }
                // Authored values must already be in range: clamping must
                // be a no-op for them.
                for (name, v) in &st.params {
                    let d = kind.manifest().params.iter().find(|d| d.name == name);
                    let d = d.unwrap_or_else(|| panic!("{}: unknown param {name}", p.id));
                    assert!((d.min..=d.max).contains(v), "{}: {name}={v}", p.id);
                }
            }
        }
    }

    #[test]
    fn values_clamp_default_and_ignore_nan() {
        let st = PresetStage {
            shader: "scanlines".into(),
            params: BTreeMap::from([("intensity".into(), 9.0), ("period".into(), f32::NAN)]),
        };
        let v = st.values(ShaderKind::Scanlines);
        assert_eq!(v[0], 1.0);
        assert_eq!(v[1], ShaderKind::Scanlines.manifest().params[1].default);
    }

    #[test]
    fn unknown_ids_fall_back_instead_of_failing() {
        let s = ShaderSettings::default();
        assert_eq!(resolve(None, &s), Selection::None);
        assert_eq!(resolve(Some("preset:no-such"), &s), Selection::Unknown);
        assert_eq!(resolve(Some("crt-aperture"), &s), Selection::Unknown);
        assert!(matches!(
            resolve(Some("crt"), &s),
            Selection::Kind(ShaderKind::Crt)
        ));
        assert!(matches!(
            resolve(Some("preset:pvm"), &s),
            Selection::Preset(_)
        ));
        assert!(chain_stages(&Selection::Unknown, &s, 256, 240, 3).is_none());
    }

    #[test]
    fn a_preset_of_only_unknown_shaders_is_unknown() {
        let mut s = ShaderSettings::default();
        s.presets.insert(
            "user-x".into(),
            ShaderPreset {
                id: "user-x".into(),
                name: "x".into(),
                stages: vec![PresetStage {
                    shader: "from-the-future".into(),
                    params: BTreeMap::new(),
                }],
            },
        );
        assert_eq!(resolve(Some("preset:user-x"), &s), Selection::Unknown);
    }

    #[test]
    fn chain_sizes_last_stage_to_n_and_earlier_to_intermediate() {
        let s = ShaderSettings::default();
        let sel = resolve(Some("preset:smooth-crt"), &s);
        let stages = chain_stages(&sel, &s, 256, 240, 3).expect("stages");
        assert_eq!(stages.len(), 2);
        assert_eq!(stages[0].kind, ShaderKind::Xbr);
        assert_eq!(stages[1].kind, ShaderKind::Crt);
        let single = chain_stages(&resolve(Some("preset:pvm"), &s), &s, 256, 240, 3).unwrap();
        assert_eq!(single.len(), 1);
    }

    #[test]
    fn save_round_trips_through_toml_and_never_shadows_first_party() {
        let mut s = ShaderSettings::default();
        s.set(ShaderKind::Crt, "beam_sigma", 0.5);
        let sel = resolve(Some("crt"), &s);
        let id = s.save_preset("PVM", &sel).expect("saved");
        assert_ne!(id, "pvm", "player ids are namespaced");
        let id2 = s.save_preset("PVM", &sel).expect("saved again");
        assert_ne!(id, id2, "duplicate names get distinct ids");
        assert_eq!(s.save_preset("   ", &sel), None);
        assert_eq!(s.save_preset("n", &Selection::None), None);

        let text = toml::to_string(&s).expect("encode");
        let back: ShaderSettings = toml::from_str(&text).expect("decode");
        assert_eq!(back, s);
        let Selection::Preset(p) = resolve(Some(&saved_id(&id)), &back) else {
            panic!("saved preset resolves");
        };
        assert_eq!(p.stages[0].values(ShaderKind::Crt)[0], 0.5);
        assert!(back.clone().delete_preset(&id));
    }

    #[test]
    fn old_settings_without_presets_still_load() {
        let s: ShaderSettings = toml::from_str("[params.crt]\ngamma = 2.0\n").expect("decode");
        assert!(s.presets.is_empty());
    }

    /// Synthetic 256x240 test card (no game data): colour bars, a grey
    /// ramp, hard diagonals and a 1-px checker, so scalers, masks and
    /// scanlines each have something to show.
    fn test_card() -> Vec<u8> {
        let (w, h) = (256usize, 240usize);
        let bars = [
            [235, 235, 235],
            [235, 235, 16],
            [16, 235, 235],
            [16, 235, 16],
            [235, 16, 235],
            [235, 16, 16],
            [16, 16, 235],
            [16, 16, 16],
        ];
        let mut px = vec![255u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let c: [u8; 3] = if y < 100 {
                    bars[x * 8 / w].map(|v| v as u8)
                } else if y < 130 {
                    let g = (x * 255 / (w - 1)) as u8;
                    [g, g, g]
                } else if y < 190 {
                    let on = ((x as i32 - 128).abs() as usize + y) % 16 < 3 || (x + y) % 24 < 2;
                    if on {
                        [250, 250, 250]
                    } else {
                        [20, 24, 60]
                    }
                } else if (x + y) % 2 == 0 {
                    [230, 200, 40]
                } else {
                    [30, 30, 30]
                };
                let i = (y * w + x) * 4;
                px[i..i + 3].copy_from_slice(&c);
            }
        }
        px
    }

    /// By-eye acceptance (W39-07): render every preset and the plain
    /// frame over the test card at 3x and write PNGs to
    /// `RF_PRESET_PNG_DIR` (skipped when unset or no GPU). Output is
    /// never committed.
    #[test]
    fn render_every_preset_over_the_test_card() {
        let Ok(dir) = std::env::var("RF_PRESET_PNG_DIR") else {
            return;
        };
        let Ok(gpu) = rf_renderer::gpu::GpuContext::request_headless() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        std::fs::create_dir_all(&dir).expect("mkdir");
        let chain = rf_renderer::ShaderChain::new(&gpu);
        let s = ShaderSettings::default();
        let card = test_card();
        let write = |name: &str, rgba: &[u8], w: u32, h: u32| {
            std::fs::write(
                std::path::Path::new(&dir).join(format!("{name}.png")),
                rf_renderer::png::encode_rgba(rgba, w, h),
            )
            .expect("write png");
        };
        write("00-plain", &card, 256, 240);
        for p in first_party() {
            let sel = resolve(Some(&saved_id(&p.id)), &s);
            let stages = chain_stages(&sel, &s, 256, 240, 3).expect("stages");
            let out = chain
                .render(&gpu, &card, 256, 240, &stages)
                .expect("render");
            assert_eq!(out.len(), 768 * 720 * 4);
            assert_ne!(out, card, "{} must change the picture", p.id);
            write(&p.id, &out, 768, 720);
        }
    }
}
