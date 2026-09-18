//! Upscale Studio pipeline glue (ticket W16-02; `docs/design/AI_UPSCALING.md`,
//! `docs/design/ENHANCEMENT_WAVE_16.md` §7 path A).
//!
//! [`crate::pipeline::build_pack`] already does the part the honesty
//! contract rests on — animation-coherent upscaling, cache keying,
//! all-or-nothing refusal — and this module deliberately does not touch
//! its signature or its tests. It adds the two things a REVIEW SCREEN
//! needs on top of that: **attribution** the reviewer can show next to
//! each preview, and **palette-aware post-processing** applied to what
//! the pipeline hands back.
//!
//! ## Attribution is not folded into the cache key
//!
//! Contrast this with [`crate::pipeline::PROVENANCE_SETTING`], which
//! deliberately DOES fold into the key because the runtime is an input to
//! the pixels. A model's display name, licence string and version label
//! are not — two callers who spell the same model's name differently
//! must not get two cache entries for identical pixels, so
//! [`ModelAttribution`] travels beside a [`crate::pipeline::BuiltPack`] in
//! [`StudioPack`], never through `settings`.
//!
//! ## Post-processing runs per finished asset, not per sheet
//!
//! [`crate::pipeline::build_pack`] already does one [`crate::upscale::Upscaler::upscale`]
//! call per animation sheet and splits the result back into per-asset
//! images (`crate::pipeline::split_sheet`) — that coherence guarantee is
//! unconditional and this module does not reopen it. Post-processing runs
//! AFTER the split, once per asset, because both of its jobs are
//! per-asset facts: a tile's own original palette, and a tile's own
//! transparent pixels. Running it on the shared sheet would need the
//! per-member boundaries recomputed a second time for no benefit.

use std::collections::BTreeMap;

use crate::animation::AnimationSet;
use crate::pipeline::{self, BuiltPack, ExtractedAsset, PipelineError};
use crate::upscale::{Rgba8, UpscaleError, Upscaler};

/// Who produced a studio pack's pixels, for the review screen's status
/// line and the written pack's manifest.
///
/// Never derived from `Upscaler::model_id`/`provenance` — those exist to
/// keep the cache key honest (see the module doc) and are technical
/// identifiers (`"rf-nearest-v1"`, `"onnx/<dylib path>"`), not the
/// human-facing name/licence a reviewer needs to see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelAttribution {
    pub model_name: String,
    pub license: String,
    pub version: String,
}

impl ModelAttribution {
    /// The stub upscaler's own attribution — `NearestUpscaler` is not a
    /// model, but every [`StudioPack`] carries an attribution unconditionally
    /// (§6's "one pipeline for AI packs and artist packs": the field is
    /// never empty, it just says something different), so this is what a
    /// pack built against no real model reports.
    #[must_use]
    pub fn none() -> Self {
        Self {
            model_name: "none (exact integer scaling)".to_string(),
            license: "n/a".to_string(),
            version: "n/a".to_string(),
        }
    }
}

/// Palette-aware post-processing applied to each finished asset.
///
/// Both knobs are opt-in and independent. `edge_mask` defaults on because
/// it is cheap and can only remove halos a model painted where the
/// original tile had none; `requantize` defaults off because it is the
/// more aggressive stylistic choice criterion 2 calls "optional".
#[derive(Debug, Clone, PartialEq)]
pub struct PostProcessOptions {
    /// Snap every upscaled pixel to the nearest colour in the tile's own
    /// palette (expanded with `intermediate_shades` blended steps between
    /// every pair of the tile's original colours). Keeps a model's smooth
    /// gradient confined to colours the ORIGINAL tile's palette implies,
    /// rather than whatever the network invented.
    pub requantize: bool,
    /// How many blended steps to insert between each pair of the
    /// original tile's distinct opaque colours before requantizing. `0`
    /// requantizes to the bare original palette with no gradient at all.
    pub intermediate_shades: u8,
    /// Force every upscaled pixel whose ORIGINAL source pixel was fully
    /// transparent back to fully transparent, regardless of what the
    /// model painted there. ESRGAN-class models are trained on opaque
    /// RGB and were never shown alpha (see `crate::onnx`'s module doc);
    /// left alone they paint solid colour into a sprite's cutout, which
    /// reappears as a halo the instant it is composited over anything.
    pub edge_mask: bool,
}

impl Default for PostProcessOptions {
    fn default() -> Self {
        Self {
            requantize: false,
            intermediate_shades: 2,
            edge_mask: true,
        }
    }
}

/// Everything [`build_studio_pack`] needs beyond what
/// [`crate::pipeline::build_pack`] already takes.
#[derive(Debug, Clone, PartialEq)]
pub struct StudioOptions {
    pub post_process: PostProcessOptions,
    pub attribution: ModelAttribution,
}

/// A built pack plus the attribution a review screen shows next to it.
#[derive(Debug, Clone)]
pub struct StudioPack {
    pub built: BuiltPack,
    pub attribution: ModelAttribution,
}

/// Run the pipeline, then post-process every finished image against its
/// OWN original asset.
///
/// # Errors
/// [`PipelineError`] — everything [`crate::pipeline::build_pack`] can
/// return, plus [`PipelineError::Upscale`] if post-processing finds the
/// upscaler did not actually scale by the factor it declared (the same
/// fault [`crate::pipeline::split_sheet`] already guards against; this is
/// the belt to that braces, since post-processing runs on the already-cut
/// image rather than the shared sheet).
pub fn build_studio_pack(
    pack_id: &str,
    rom_hash: &str,
    assets: &[ExtractedAsset],
    sets: &[AnimationSet],
    upscaler: &dyn Upscaler,
    settings: &BTreeMap<String, String>,
    options: &StudioOptions,
) -> Result<StudioPack, PipelineError> {
    let mut built = pipeline::build_pack(pack_id, rom_hash, assets, sets, upscaler, settings)?;

    let by_hash: BTreeMap<&str, &ExtractedAsset> =
        assets.iter().map(|a| (a.asset_hash.as_str(), a)).collect();

    let scale = upscaler.scale();
    for (hash, image) in &mut built.images {
        let Some(asset) = by_hash.get(hash.as_str()) else {
            // Every key in `built.images` came from `assets` — pipeline's
            // own invariant. Nothing to post-process against if it did
            // not, so leave the image exactly as the pipeline produced it
            // rather than inventing an original.
            continue;
        };
        let original = Rgba8::from_indexed(
            &asset.indexed_pixels,
            &asset.palette,
            asset.width,
            asset.height,
        )
        .map_err(|source| PipelineError::Upscale {
            asset_hash: hash.clone(),
            source,
        })?;
        *image = post_process_image(image, &original, scale, &options.post_process).map_err(
            |source| PipelineError::Upscale {
                asset_hash: hash.clone(),
                source,
            },
        )?;
    }

    Ok(StudioPack {
        built,
        attribution: options.attribution.clone(),
    })
}

/// Apply [`PostProcessOptions`] to one finished (already upscaled) image.
///
/// `original` is the asset BEFORE upscaling; `scale` is the factor that
/// relates their dimensions. Every upscaled pixel maps back to exactly
/// one original pixel at `(x / scale, y / scale)` — nearest-neighbour
/// correspondence, which is the right one here regardless of what
/// interpolation the model itself used internally, because both knobs
/// this function implements are about the ORIGINAL tile's own colours and
/// transparency, not about smoothing.
///
/// # Errors
/// [`UpscaleError::ZeroScale`] for `scale == 0`.
/// [`UpscaleError::SheetTooSmall`] if `upscaled`'s dimensions are not
/// exactly `original`'s times `scale` — the same fault
/// [`crate::pipeline::split_sheet`] refuses, reported the same way so a
/// caller does not need a second error shape to handle.
pub fn post_process_image(
    upscaled: &Rgba8,
    original: &Rgba8,
    scale: u32,
    opts: &PostProcessOptions,
) -> Result<Rgba8, UpscaleError> {
    if scale == 0 {
        return Err(UpscaleError::ZeroScale);
    }
    if !opts.requantize && !opts.edge_mask {
        return Ok(upscaled.clone());
    }
    let need_w = original.width * scale;
    let need_h = original.height * scale;
    if upscaled.width != need_w || upscaled.height != need_h {
        return Err(UpscaleError::SheetTooSmall {
            sheet_width: upscaled.width,
            sheet_height: upscaled.height,
            need_x: need_w,
            need_y: need_h,
        });
    }

    let palette = expand_palette(original, opts.intermediate_shades);
    let mut pixels = upscaled.pixels.clone();

    // Both loops are ranges over `upscaled`'s own declared dimensions
    // (CLAUDE.md law 8): they terminate structurally, not by an
    // index a branch could fail to advance.
    for y in 0..upscaled.height {
        let oy = y / scale;
        for x in 0..upscaled.width {
            let ox = x / scale;
            // In bounds by construction: ox < original.width because
            // x < upscaled.width == original.width * scale.
            let Some(source) = original.pixel(ox, oy) else {
                continue;
            };
            let i = (y as usize * upscaled.width as usize + x as usize) * 4;

            if opts.edge_mask && source[3] == 0 {
                pixels[i..i + 4].copy_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            if opts.requantize {
                let current = [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]];
                let snapped = nearest_in_palette(current, &palette);
                pixels[i..i + 4].copy_from_slice(&snapped);
            }
        }
    }

    Rgba8::new(upscaled.width, upscaled.height, pixels)
}

/// Every distinct opaque colour of `original`, plus `shades` blended
/// steps between every PAIR of them.
///
/// Blends only between colours the tile actually had — never toward
/// black, white, or any invented anchor — so requantizing can only ever
/// produce a colour this specific tile's own palette implies.
fn expand_palette(original: &Rgba8, shades: u8) -> Vec<[u8; 4]> {
    let mut base: Vec<[u8; 4]> = Vec::new();
    for y in 0..original.height {
        for x in 0..original.width {
            if let Some(p) = original.pixel(x, y) {
                if p[3] != 0 && !base.contains(&p) {
                    base.push(p);
                }
            }
        }
    }

    let mut out = base.clone();
    if shades > 0 {
        let steps = u32::from(shades);
        for i in 0..base.len() {
            for j in (i + 1)..base.len() {
                for step in 1..=steps {
                    let t = step as f32 / (steps as f32 + 1.0);
                    out.push(lerp(base[i], base[j], t));
                }
            }
        }
    }
    out
}

fn lerp(a: [u8; 4], b: [u8; 4], t: f32) -> [u8; 4] {
    let mut out = [0u8; 4];
    for k in 0..4 {
        let av = f32::from(a[k]);
        let bv = f32::from(b[k]);
        out[k] = (av + (bv - av) * t).round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// Nearest colour in `palette` by squared RGB distance (alpha is not
/// compared: `post_process_image` only calls this on pixels that
/// `edge_mask` has already decided are opaque). Returns `px` unchanged if
/// `palette` is empty — a fully-transparent original tile has no opaque
/// colour to snap to, and that is `edge_mask`'s job, not this function's.
fn nearest_in_palette(px: [u8; 4], palette: &[[u8; 4]]) -> [u8; 4] {
    let Some(best) = palette.iter().copied().min_by_key(|c| {
        let dr = i32::from(c[0]) - i32::from(px[0]);
        let dg = i32::from(c[1]) - i32::from(px[1]);
        let db = i32::from(c[2]) - i32::from(px[2]);
        dr * dr + dg * dg + db * db
    }) else {
        return px;
    };
    [best[0], best[1], best[2], px[3]]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::asset_hash;
    use crate::upscale::NearestUpscaler;

    fn asset(w: u32, h: u32, indexed: Vec<u8>, palette: Vec<u8>) -> ExtractedAsset {
        ExtractedAsset {
            asset_hash: asset_hash(&indexed, &palette),
            width: w,
            height: h,
            indexed_pixels: indexed,
            palette,
        }
    }

    fn options(post_process: PostProcessOptions) -> StudioOptions {
        StudioOptions {
            post_process,
            attribution: ModelAttribution {
                model_name: "test-model".into(),
                license: "MIT".into(),
                version: "1.0".into(),
            },
        }
    }

    // -----------------------------------------------------------------
    // Attribution
    // -----------------------------------------------------------------

    #[test]
    fn attribution_travels_with_the_built_pack_untouched() {
        let up = NearestUpscaler::new(2).unwrap();
        let a = asset(1, 1, vec![0], vec![255, 0, 0, 255]);
        let opts = options(PostProcessOptions {
            requantize: false,
            intermediate_shades: 0,
            edge_mask: false,
        });
        let pack = build_studio_pack("p", "rom", &[a], &[], &up, &BTreeMap::new(), &opts).unwrap();
        assert_eq!(pack.attribution, opts.attribution);
        // The manifest itself is untouched by attribution — it is a
        // sidecar, not folded into the cache key (module doc).
        assert_eq!(pack.built.manifest.model_id, up.model_id());
    }

    #[test]
    fn none_attribution_is_never_empty() {
        let a = ModelAttribution::none();
        assert!(!a.model_name.is_empty());
        assert!(!a.license.is_empty());
    }

    // -----------------------------------------------------------------
    // Edge mask
    // -----------------------------------------------------------------

    #[test]
    fn edge_mask_keeps_transparent_pixels_transparent() {
        // 1x1 fully transparent original; NearestUpscaler carries the
        // (0-alpha) colour forward unchanged, so this proves the MASK
        // itself, not just that nothing painted anything.
        let up = NearestUpscaler::new(3).unwrap();
        let original = Rgba8::new(1, 1, vec![10, 20, 30, 0]).unwrap();
        let upscaled = up.upscale(&original).unwrap();
        let opts = PostProcessOptions {
            requantize: true, // even with requant on, transparent wins
            intermediate_shades: 2,
            edge_mask: true,
        };
        let out = post_process_image(&upscaled, &original, 3, &opts).unwrap();
        for y in 0..3 {
            for x in 0..3 {
                assert_eq!(out.pixel(x, y), Some([0, 0, 0, 0]));
            }
        }
    }

    #[test]
    fn edge_mask_off_leaves_a_models_halo_in_place() {
        // The contrast case: with the mask off, whatever colour the
        // "model" (here NearestUpscaler, standing in for one) produced
        // over a transparent source pixel survives.
        let up = NearestUpscaler::new(2).unwrap();
        let original = Rgba8::new(1, 1, vec![10, 20, 30, 0]).unwrap();
        let upscaled = up.upscale(&original).unwrap();
        let opts = PostProcessOptions {
            requantize: false,
            intermediate_shades: 0,
            edge_mask: false,
        };
        let out = post_process_image(&upscaled, &original, 2, &opts).unwrap();
        assert_eq!(out.pixel(0, 0), Some([10, 20, 30, 0]));
    }

    // -----------------------------------------------------------------
    // Requantization
    // -----------------------------------------------------------------

    #[test]
    fn requantize_snaps_to_the_original_palette_only() {
        // A 2-colour original; feed post-processing an upscaled image
        // that was corrupted to an off-palette colour and confirm it
        // gets pulled back to one of the two original colours (or a
        // blend between them), never anything else.
        let original = Rgba8::new(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
        // Pretend an upscaler produced a 2x-scaled image with an invented
        // colour nowhere near either original pixel.
        let corrupted = Rgba8::new(
            4,
            2,
            vec![
                0, 200, 0, 255, 0, 200, 0, 255, 0, 200, 0, 255, 0, 200, 0, 255, //
                0, 200, 0, 255, 0, 200, 0, 255, 0, 200, 0, 255, 0, 200, 0, 255,
            ],
        )
        .unwrap();
        let opts = PostProcessOptions {
            requantize: true,
            intermediate_shades: 0,
            edge_mask: true,
        };
        let out = post_process_image(&corrupted, &original, 2, &opts).unwrap();
        let allowed = [[255, 0, 0, 255], [0, 0, 255, 255]];
        for y in 0..out.height {
            for x in 0..out.width {
                let px = out.pixel(x, y).unwrap();
                assert!(allowed.contains(&px), "invented colour {px:?} survived");
            }
        }
    }

    #[test]
    fn intermediate_shades_add_blended_steps_between_original_colours() {
        let original = Rgba8::new(2, 1, vec![0, 0, 0, 255, 100, 100, 100, 255]).unwrap();
        let none = expand_palette(&original, 0);
        let some = expand_palette(&original, 3);
        assert_eq!(none.len(), 2);
        assert_eq!(some.len(), 2 + 3); // 2 base + 3 blended steps
                                       // Every blended shade lies strictly between the two originals on
                                       // each channel — no invented hue outside that range.
        for shade in &some[2..] {
            for k in 0..3 {
                assert!(shade[k] <= 100, "{shade:?}");
            }
        }
    }

    #[test]
    fn zero_scale_is_refused_before_any_pixel_work() {
        let img = Rgba8::new(1, 1, vec![0, 0, 0, 0]).unwrap();
        let e = post_process_image(&img, &img, 0, &PostProcessOptions::default()).unwrap_err();
        assert_eq!(e, UpscaleError::ZeroScale);
    }

    #[test]
    fn a_geometry_mismatch_is_refused() {
        let original = Rgba8::new(2, 2, vec![0; 16]).unwrap();
        let wrong = Rgba8::new(3, 3, vec![0; 36]).unwrap();
        let e =
            post_process_image(&wrong, &original, 2, &PostProcessOptions::default()).unwrap_err();
        assert!(matches!(e, UpscaleError::SheetTooSmall { .. }));
    }

    // -----------------------------------------------------------------
    // The fixture-game round trip criterion 4 names: grouping, an
    // upscale, post-processing, and attribution, end to end, against the
    // stub upscaler.
    // -----------------------------------------------------------------

    #[test]
    fn a_fixture_games_tiles_round_trip_through_the_studio() {
        use crate::animation::{group, SpriteObservation};

        // Two frames of a two-tile walk cycle, plus one static tile.
        let walk_a = asset(2, 2, vec![0, 1, 1, 0], vec![255, 0, 0, 255, 0, 255, 0, 0]);
        let walk_b = asset(2, 2, vec![1, 0, 0, 1], vec![255, 0, 0, 255, 0, 255, 0, 0]);
        let hud = asset(1, 1, vec![0], vec![0, 0, 255, 255]);

        let obs = vec![
            SpriteObservation {
                frame: 0,
                tile_id: 1,
                x: 10,
                y: 10,
                asset_hash: walk_a.asset_hash.clone(),
            },
            SpriteObservation {
                frame: 1,
                tile_id: 1,
                x: 11,
                y: 10,
                asset_hash: walk_b.asset_hash.clone(),
            },
        ];
        let sets = group(&obs);
        assert_eq!(sets.len(), 1, "the walk cycle must group into one set");

        let up = NearestUpscaler::new(2).unwrap();
        let opts = options(PostProcessOptions {
            requantize: true,
            intermediate_shades: 1,
            edge_mask: true,
        });
        let pack = build_studio_pack(
            "fixture-pack",
            "rom-fixture",
            &[walk_a.clone(), walk_b.clone(), hud.clone()],
            &sets,
            &up,
            &BTreeMap::new(),
            &opts,
        )
        .unwrap();

        assert_eq!(pack.built.manifest.entries.len(), 3);
        assert_eq!(pack.attribution.model_name, "test-model");
        for a in [&walk_a, &walk_b, &hud] {
            let img = &pack.built.images[&a.asset_hash];
            assert_eq!(img.width, a.width * 2);
            assert_eq!(img.height, a.height * 2);
        }
    }
}
