//! Upscale Studio: capture-while-playing, review, and pack write-out
//! (ticket W16-02; `docs/design/AI_UPSCALING.md`;
//! `docs/design/ENHANCEMENT_WAVE_16.md` §7 path A).
//!
//! ## Why this bridges two crates that do not know about each other
//!
//! `rf_ai` is the upscale pipeline: animation-coherent upscaling
//! (`rf_ai::animation`, `rf_ai::pipeline`), a SHA-256-keyed cache/pack
//! format (`rf_ai::pack`) and this ticket's post-processing/attribution
//! glue (`rf_ai::studio`). It knows nothing about the NES, CHR memory, or
//! Mesen's `hires.txt` identity — that is deliberate (its own module
//! docs: "no model, no runtime, and no network").
//!
//! `rf_enhance::hdpack` knows the Mesen `TileData`/palette identity and
//! how to lay tiles onto a tileset (`PackBuilder`) and serialize the
//! result (`write_hires`), but decodes no pixels at all (its own module
//! doc: "PNG is deliberately out of band").
//!
//! Neither may depend on the other for this ticket's purposes, so the
//! shell — which already depends on both, and which is where
//! `crate::stepper::hd_placements` already does exactly this kind of
//! console-specific-to-console-agnostic conversion — is where a captured
//! tile becomes an `rf_ai::pipeline::ExtractedAsset` for the pipeline,
//! and where the finished pixels are laid back onto a Mesen-compatible
//! tileset + `hires.txt`, with a sidecar `manifest.toml` carrying the
//! attribution `hires.txt` itself has no field for.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

use rf_ai::animation::{group, SpriteObservation};
use rf_ai::pack::asset_hash;
use rf_ai::pipeline::ExtractedAsset;
use rf_ai::studio::{self, ModelAttribution, PostProcessOptions, StudioOptions, StudioPack};
use rf_ai::upscale::{NearestUpscaler, Rgba8};

use rf_enhance::hdpack::{PackBuilder, TileData, TileObservation};

use crate::stepper::StudioTileCapture;

/// One tile captured while playing, deduplicated by asset hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedTile {
    pub asset: ExtractedAsset,
    /// The Mesen-format tile identity this same tile is keyed by —
    /// [`write_pack`]'s own `PackBuilder` call is keyed on this, not on
    /// `asset.asset_hash` (`rf_ai::pack`'s SHA-256 identity means nothing
    /// to Mesen or to `rf_enhance::hdpack`'s loader).
    pub mesen_tile: TileData,
    /// The raw NES palette bytes matching `mesen_tile`'s identity — see
    /// `crate::stepper::StudioTileCapture::mesen_palette`'s own doc for
    /// why this is a different thing from `asset.palette`.
    pub mesen_palette: [u8; 4],
    pub layer: rf_enhance::hd_render::Layer,
}

/// What the reviewer decided about one captured tile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Write the model's (or stub's) output for this tile.
    Approved,
    /// Drop this tile from the written pack entirely.
    Rejected,
    /// Skip the model for this tile and use a user-picked PNG instead.
    /// The PNG must be exactly `8 * scale` square — [`write_pack`]
    /// refuses rather than silently resizing or cropping it.
    Replaced(PathBuf),
}

/// `Approved` is the default: a review screen that opens with everything
/// off would ask the user to turn on every single tile by hand before a
/// first pack could ever be written, for what is normally the common
/// case (the run looked fine, a handful of tiles need a closer look).
impl Default for Decision {
    fn default() -> Self {
        Decision::Approved
    }
}

/// Cap on tracked sprite-position observations.
///
/// `rf_ai::animation::group`'s own algorithm only ever links CONSECUTIVE
/// frame numbers (its module doc: "Only CONSECUTIVE frames: a gap means
/// the entity may have despawned"), so an observation older than every
/// frame still inside this window can never link to anything captured
/// after it — dropping it loses no reachable grouping, only stale
/// history a longer session would otherwise accumulate forever. At a
/// generous 64 sprites/frame (the NES's own OAM ceiling) this is several
/// hundred frames of history, comfortably more than any walk cycle.
const MAX_TRACKED_OBSERVATIONS: usize = 20_000;

/// Tiles captured while playing, deduplicated, with the sprite position
/// history [`rf_ai::animation::group`] needs.
#[derive(Debug, Clone, Default)]
pub struct CaptureSession {
    tiles: BTreeMap<String, CapturedTile>,
    observations: VecDeque<SpriteObservation>,
    frame: u64,
}

impl CaptureSession {
    /// Record one frame's tiles.
    ///
    /// Cheap to call for a tile already known: the dedup key is the
    /// asset hash, but every SPRITE occurrence still feeds the animation
    /// grouper (bounded — see [`MAX_TRACKED_OBSERVATIONS`]), which needs
    /// every frame a sprite appeared on and where, not just its first
    /// sighting.
    ///
    /// **Only sprites feed the grouper.** `rf_ai::animation`'s own module
    /// doc scopes it to sprites ("cluster extracted SPRITES by OAM tile
    /// id + adjacency-in-time"). Background tiles sit edge-to-edge on the
    /// nametable at 8px spacing, so at that scale
    /// `rf_ai::animation::POSITION_TOLERANCE` (8px) would transitively
    /// link the ENTIRE visible screen into one "animation" — not a
    /// coherence bug to work around, a question this grouper was never
    /// meant to answer for background art. A background tile that never
    /// joins a set is not lost: `rf_ai::pipeline::build_pack` upscales
    /// every asset outside a set as its own one-member sheet, the same
    /// path a grouped member takes.
    pub fn observe(&mut self, captures: &[StudioTileCapture]) {
        for c in captures {
            let hash = asset_hash(&c.indexed_pixels, &c.palette_rgba);
            if matches!(c.layer, rf_enhance::hd_render::Layer::Sprite) {
                self.observations.push_back(SpriteObservation {
                    frame: self.frame,
                    // `rf_ai::animation::group` does not read `tile_id`
                    // (its own module doc: the position+adjacency test
                    // alone decides grouping) — 0 is not a lossy
                    // placeholder, it is simply unused.
                    tile_id: 0,
                    x: c.x,
                    y: c.y,
                    asset_hash: hash.clone(),
                });
                while self.observations.len() > MAX_TRACKED_OBSERVATIONS {
                    self.observations.pop_front();
                }
            }
            self.tiles
                .entry(hash.clone())
                .or_insert_with(|| CapturedTile {
                    asset: ExtractedAsset {
                        asset_hash: hash,
                        width: 8,
                        height: 8,
                        indexed_pixels: c.indexed_pixels.to_vec(),
                        palette: c.palette_rgba.to_vec(),
                    },
                    mesen_tile: c.tile.clone(),
                    mesen_palette: c.mesen_palette,
                    layer: c.layer,
                });
        }
        // Unconditional advance (CLAUDE.md law 8): every call to
        // `observe` is one frame, whether or not it carried any tiles.
        self.frame += 1;
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    pub fn clear(&mut self) {
        self.tiles.clear();
        self.observations.clear();
        self.frame = 0;
    }

    #[must_use]
    pub fn tiles(&self) -> &BTreeMap<String, CapturedTile> {
        &self.tiles
    }

    /// A snapshot of the sprite-position history, for [`run`] to group
    /// off the UI thread.
    ///
    /// Deliberately NOT a `Vec<AnimationSet>` computed here: grouping is
    /// `rf_ai::animation::group`'s O(sprites-per-frame²) work per
    /// consecutive frame pair, and `run` is what a caller spawns onto a
    /// worker thread (its own doc) — doing the grouping here, before that
    /// spawn, would put exactly the blocking work FRONTEND_UI's "the UI
    /// never blocks" principle exists to keep off this thread back onto
    /// it.
    #[must_use]
    pub fn observations(&self) -> Vec<SpriteObservation> {
        self.observations.iter().cloned().collect()
    }
}

/// Which upscaler [`run`] uses.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelChoice {
    /// The exact-integer stub — always available, and what the kittest
    /// scenario and every `rf_ai` pipeline test run against. Never a
    /// placeholder pretending to be a model: its status line always says
    /// "none", never a model name.
    Stub { scale: u32 },
    /// A local ONNX model (`docs/design/AI_UPSCALING.md`), compiled in
    /// only under the `ai-onnx` feature — see `Cargo.toml`'s own comment
    /// on why a default build carries neither `ort` nor this variant.
    #[cfg(feature = "ai-onnx")]
    Onnx {
        dylib_path: PathBuf,
        model_path: PathBuf,
        model_id: String,
        license: String,
        version: String,
        scale: u32,
    },
}

/// What [`run`] produced, for the studio window's status line.
pub struct RunOutcome {
    pub pack: Result<StudioPack, String>,
    /// Names the model, the provider, and the licence — the status line
    /// criterion 3 asks for, built here rather than re-derived by the UI
    /// so there is exactly one place that knows what each `ModelChoice`
    /// means in words.
    pub status_line: String,
}

/// Run the studio pipeline over `assets`, grouped by `observations`, with
/// `choice`.
///
/// Intended to run OFF the UI thread — the caller spawns it
/// (`std::thread::spawn`), same pattern as `crate::app`'s library-scan
/// worker: FRONTEND_UI's "the UI never blocks" principle applies to the
/// studio's Run button exactly as it does to a rescan. Grouping
/// (`rf_ai::animation::group`) runs INSIDE this call rather than before
/// it for exactly that reason — see
/// [`CaptureSession::observations`]'s doc.
///
/// `run` always builds its own empty settings map for
/// `rf_ai::pipeline::build_pack` — there is no caller-supplied settings
/// path yet, so there is nothing a caller's own `rf_ai::pipeline::PROVENANCE_SETTING`
/// could collide with today.
#[must_use]
pub fn run(
    pack_id: &str,
    rom_hash: &str,
    assets: &[ExtractedAsset],
    observations: &[SpriteObservation],
    choice: &ModelChoice,
    post_process: PostProcessOptions,
) -> RunOutcome {
    let sets = group(observations);
    match choice {
        ModelChoice::Stub { scale } => {
            let up = match NearestUpscaler::new(*scale) {
                Ok(u) => u,
                Err(e) => {
                    return RunOutcome {
                        pack: Err(e.to_string()),
                        status_line: "stub upscaler — invalid scale".to_string(),
                    }
                }
            };
            let options = StudioOptions {
                post_process,
                attribution: ModelAttribution::none(),
            };
            let pack = studio::build_studio_pack(
                pack_id,
                rom_hash,
                assets,
                &sets,
                &up,
                &BTreeMap::new(),
                &options,
            )
            .map_err(|e| e.to_string());
            RunOutcome {
                pack,
                status_line: format!(
                    "model: none (exact integer x{scale}) \u{b7} provider: CPU \u{b7} licence: n/a"
                ),
            }
        }
        #[cfg(feature = "ai-onnx")]
        ModelChoice::Onnx {
            dylib_path,
            model_path,
            model_id,
            license,
            version,
            scale,
        } => run_onnx(
            pack_id,
            rom_hash,
            assets,
            &sets,
            dylib_path,
            model_path,
            model_id,
            license,
            version,
            *scale,
            post_process,
        ),
    }
}

#[cfg(feature = "ai-onnx")]
#[allow(clippy::too_many_arguments)]
fn run_onnx(
    pack_id: &str,
    rom_hash: &str,
    assets: &[ExtractedAsset],
    sets: &[rf_ai::animation::AnimationSet],
    dylib_path: &Path,
    model_path: &Path,
    model_id: &str,
    license: &str,
    version: &str,
    scale: u32,
    post_process: PostProcessOptions,
) -> RunOutcome {
    use rf_ai::onnx::OnnxUpscaler;

    let (providers, provider_label) = onnx_providers();
    let up = match OnnxUpscaler::load_with_providers(
        dylib_path, model_path, model_id, scale, &providers,
    ) {
        Ok(u) => u,
        Err(e) => {
            return RunOutcome {
                pack: Err(e.to_string()),
                status_line: format!(
                    "model: {model_id} \u{b7} provider: {provider_label} \u{b7} FAILED TO LOAD: {e}"
                ),
            }
        }
    };
    let options = StudioOptions {
        post_process,
        attribution: ModelAttribution {
            model_name: model_id.to_string(),
            license: license.to_string(),
            version: version.to_string(),
        },
    };
    let pack = studio::build_studio_pack(
        pack_id,
        rom_hash,
        assets,
        sets,
        &up,
        &BTreeMap::new(),
        &options,
    )
    .map_err(|e| e.to_string());
    RunOutcome {
        pack,
        status_line: format!(
            "model: {model_id} \u{b7} provider: {provider_label} \u{b7} licence: {license}"
        ),
    }
}

/// Best-effort CoreML EP registration, CPU otherwise
/// (`docs/design/ENHANCEMENT_WAVE_16.md` §7). Only compiled with the
/// `ai-onnx-coreml` feature on top of `ai-onnx` — see `Cargo.toml`'s own
/// comment for why `ort::ep::CoreML` does not exist without it.
#[cfg(feature = "ai-onnx-coreml")]
fn onnx_providers() -> (
    Vec<rf_ai::onnx::ort::ep::ExecutionProviderDispatch>,
    &'static str,
) {
    (
        vec![rf_ai::onnx::ort::ep::CoreML::default().build()],
        "CoreML EP (falls back to CPU EP for unsupported ops)",
    )
}

#[cfg(all(feature = "ai-onnx", not(feature = "ai-onnx-coreml")))]
fn onnx_providers() -> (
    Vec<rf_ai::onnx::ort::ep::ExecutionProviderDispatch>,
    &'static str,
) {
    (Vec::new(), "CPU EP")
}

/// Write a [`StudioPack`] to `dir` as a Mesen-compatible tileset PNG +
/// `hires.txt`, plus a sidecar `manifest.toml` carrying the attribution
/// `hires.txt` has no field for.
///
/// `captured` supplies the Mesen tile identity for each asset hash the
/// pipeline processed — [`CaptureSession::tiles`] is the caller's usual
/// source. `decisions` narrows what gets written: a `Rejected` tile is
/// skipped entirely, and a `Replaced` tile's PNG bytes are used in place
/// of whatever the pipeline produced for it (refused if it is not
/// exactly `8 * scale` square). Every entry not otherwise decided is
/// treated as [`Decision::Approved`] — see that variant's own doc for why
/// the default leans permissive.
///
/// Returns the number of tiles written.
///
/// # Errors
/// A message naming what went wrong. Best-effort per file is
/// deliberately NOT the policy here (contrast
/// `rf_enhance::hdpack::Import`'s partial-load stance): a pack
/// half-written to disk with no record of which half is exactly the
/// "half-applied is worse than none" failure `rf_ai::pack`'s own module
/// doc warns about, so the first fault aborts the whole write before any
/// file lands.
pub fn write_pack(
    dir: &Path,
    pack: &StudioPack,
    captured: &BTreeMap<String, CapturedTile>,
    decisions: &BTreeMap<String, Decision>,
) -> Result<usize, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;

    // Every captured tile is 8x8 (`CaptureSession::observe`), so an
    // upscaled image's width divided by 8 IS the upscaler's own scale
    // factor — no separate parameter needed.
    let scale = pack
        .built
        .images
        .values()
        .next()
        .map_or(1, |img| (img.width / 8).max(1));
    let tile_px = 8 * scale;

    let mut mesen_observations: Vec<(String, TileObservation)> = Vec::new();
    let mut final_images: BTreeMap<String, Rgba8> = BTreeMap::new();

    for entry in &pack.built.manifest.entries {
        let decision = decisions
            .get(&entry.asset_hash)
            .cloned()
            .unwrap_or_default();
        if matches!(decision, Decision::Rejected) {
            continue;
        }
        let Some(tile) = captured.get(&entry.asset_hash) else {
            // A Mesen pack is keyed on CHR identity, not the pipeline's
            // own asset hash — an entry with no captured identity cannot
            // be placed, and dropping it silently would be exactly the
            // half-applied pack this function's own doc refuses to ship.
            return Err(format!(
                "asset {} has no captured Mesen tile identity",
                entry.asset_hash
            ));
        };

        let image = match decision {
            Decision::Replaced(png_path) => {
                let decoded = image::open(&png_path)
                    .map_err(|e| format!("opening replacement {}: {e}", png_path.display()))?
                    .to_rgba8();
                if decoded.width() != tile_px || decoded.height() != tile_px {
                    return Err(format!(
                        "replacement {} is {}x{}, but this pack needs {tile_px}x{tile_px} (8x8 at x{scale})",
                        png_path.display(),
                        decoded.width(),
                        decoded.height()
                    ));
                }
                Rgba8::new(decoded.width(), decoded.height(), decoded.into_raw())
                    .map_err(|e| e.to_string())?
            }
            // `Rejected` cannot reach here (filtered above); handled
            // together with `Approved` because both use the pipeline's
            // own output rather than a replacement file.
            Decision::Approved | Decision::Rejected => {
                let Some(img) = pack.built.images.get(&entry.asset_hash) else {
                    continue;
                };
                img.clone()
            }
        };

        mesen_observations.push((
            entry.asset_hash.clone(),
            TileObservation {
                tile: tile.mesen_tile.clone(),
                palette: tile.mesen_palette,
            },
        ));
        final_images.insert(entry.asset_hash.clone(), image);
    }

    if mesen_observations.is_empty() {
        return Err("nothing approved to write".to_string());
    }

    // Slot layout: `PackBuilder` assigns slots by `(TileData, palette)`'s
    // OWN sorted order, not insertion order (its doc: "so the same
    // recording always produces the same tileset layout") — this reverse
    // lookup lets a finished slot be matched back to the asset it holds,
    // using the same key `PackBuilder` itself sorts by.
    const IMAGE_NAME: &str = "upscaled.png";
    let columns = (mesen_observations.len() as f64).sqrt().ceil().max(1.0) as u32;
    let mut builder = PackBuilder::new(IMAGE_NAME, 8, columns, scale);
    let mut key_to_asset: BTreeMap<(TileData, [u8; 4]), String> = BTreeMap::new();
    for (hash, obs) in &mesen_observations {
        builder.observe(obs);
        key_to_asset.insert((obs.tile.clone(), obs.palette), hash.clone());
    }
    let hd_pack = builder.build();

    // One tileset image at `hd_pack`'s own declared layout.
    let rows = (hd_pack.tiles.len() as u32).div_ceil(columns).max(1);
    let sheet_w = columns * tile_px;
    let sheet_h = rows * tile_px;
    let mut sheet = vec![0u8; sheet_w as usize * sheet_h as usize * 4];
    for rule in &hd_pack.tiles {
        let key = (rule.key.tile.clone(), rule.key.palette);
        let (Some(hash),) = (key_to_asset.get(&key),) else {
            continue;
        };
        let Some(image) = final_images.get(hash) else {
            continue;
        };
        for y in 0..tile_px {
            let src = y as usize * tile_px as usize * 4;
            let dst = ((rule.y + y) as usize * sheet_w as usize + rule.x as usize) * 4;
            let n = tile_px as usize * 4;
            sheet[dst..dst + n].copy_from_slice(&image.pixels[src..src + n]);
        }
    }

    let hires_text = rf_enhance::hdpack::write_hires(&hd_pack);
    std::fs::write(dir.join("hires.txt"), hires_text)
        .map_err(|e| format!("writing hires.txt: {e}"))?;
    let png = rf_renderer::png::encode_rgba(&sheet, sheet_w, sheet_h);
    std::fs::write(dir.join(IMAGE_NAME), png).map_err(|e| format!("writing {IMAGE_NAME}: {e}"))?;

    let written_hashes: std::collections::BTreeSet<&str> = mesen_observations
        .iter()
        .map(|(hash, _)| hash.as_str())
        .collect();
    let manifest = PackManifestFile {
        id: pack.built.manifest.id.clone(),
        rom_hash: pack.built.manifest.rom_hash.clone(),
        model_id: pack.built.manifest.model_id.clone(),
        settings_hash: pack.built.manifest.settings_hash.clone(),
        attribution: AttributionFile {
            model_name: pack.attribution.model_name.clone(),
            license: pack.attribution.license.clone(),
            version: pack.attribution.version.clone(),
        },
        entries: pack
            .built
            .manifest
            .entries
            .iter()
            .filter(|e| written_hashes.contains(e.asset_hash.as_str()))
            .map(|e| PackEntryFile {
                asset_hash: e.asset_hash.clone(),
                animation_set: e.animation_set.clone(),
                image_hash: e.image_hash.clone(),
            })
            .collect(),
    };
    let toml_text =
        toml::to_string_pretty(&manifest).map_err(|e| format!("serializing manifest: {e}"))?;
    std::fs::write(dir.join("manifest.toml"), toml_text)
        .map_err(|e| format!("writing manifest.toml: {e}"))?;

    Ok(hd_pack.tiles.len())
}

/// Attribution + provenance `hires.txt` has no field for
/// (`rf_enhance::hdpack`'s own module doc: it is a compatibility surface,
/// not RetroForge's own evolvable format).
///
/// A separate `serde`-derived struct rather than adding `Serialize` to
/// `rf_ai::pack::PackManifest` itself: `rf-ai` deliberately carries no
/// `serde`/`toml` dependency (its own `Cargo.toml` doc: minimal deps so
/// `cargo test --workspace` needs nothing external), and this crate
/// already depends on both for `crate::settings`/`crate::library`.
#[derive(Debug, serde::Serialize)]
struct PackManifestFile {
    id: String,
    rom_hash: String,
    model_id: String,
    settings_hash: String,
    attribution: AttributionFile,
    entries: Vec<PackEntryFile>,
}

#[derive(Debug, serde::Serialize)]
struct AttributionFile {
    model_name: String,
    license: String,
    version: String,
}

#[derive(Debug, serde::Serialize)]
struct PackEntryFile {
    asset_hash: String,
    animation_set: Option<String>,
    image_hash: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(
        x: i32,
        y: i32,
        tile_index: u32,
        colour: [u8; 4],
        layer: rf_enhance::hd_render::Layer,
    ) -> StudioTileCapture {
        let mut indexed_pixels = [0u8; 64];
        // A solid tile: every pixel is palette index 1, so it is opaque
        // and distinguishable from an all-zero (backdrop) tile.
        indexed_pixels.fill(1);
        let mut palette_rgba = [0u8; 16];
        palette_rgba[4..8].copy_from_slice(&colour);
        StudioTileCapture {
            tile: TileData::ChrRom(tile_index),
            mesen_palette: [0x0F, 0x00, 0x10, 0x20],
            layer,
            x,
            y,
            indexed_pixels,
            palette_rgba,
        }
    }

    fn bg(x: i32, y: i32, tile_index: u32, colour: [u8; 4]) -> StudioTileCapture {
        capture(
            x,
            y,
            tile_index,
            colour,
            rf_enhance::hd_render::Layer::Background,
        )
    }

    fn sprite(x: i32, y: i32, tile_index: u32, colour: [u8; 4]) -> StudioTileCapture {
        capture(
            x,
            y,
            tile_index,
            colour,
            rf_enhance::hd_render::Layer::Sprite,
        )
    }

    // -----------------------------------------------------------------
    // CaptureSession
    // -----------------------------------------------------------------

    #[test]
    fn capture_session_deduplicates_by_asset_hash() {
        let mut session = CaptureSession::default();
        session.observe(&[bg(10, 10, 1, [255, 0, 0, 255])]);
        session.observe(&[bg(20, 20, 1, [255, 0, 0, 255])]); // same tile, elsewhere
        assert_eq!(session.len(), 1);
    }

    #[test]
    fn capture_session_keeps_distinct_tiles_distinct() {
        let mut session = CaptureSession::default();
        session.observe(&[
            bg(10, 10, 1, [255, 0, 0, 255]),
            bg(20, 10, 2, [0, 255, 0, 255]),
        ]);
        assert_eq!(session.len(), 2);
    }

    #[test]
    fn only_sprites_feed_the_animation_grouper() {
        // Background tiles sit at 8px spacing, so if they fed the
        // grouper too, `POSITION_TOLERANCE` (8px) would transitively
        // link the whole screen into one set. Confirmed here: four
        // "walking" BACKGROUND tiles at consecutive positions/frames
        // produce NO observations at all.
        let mut session = CaptureSession::default();
        for f in 0..4u8 {
            let mut tile = bg(100 + i32::from(f), 50, u32::from(f), [255, 0, 0, 255]);
            tile.indexed_pixels[0] = f;
            session.observe(&[tile]);
        }
        assert!(session.observations().is_empty());
    }

    #[test]
    fn capture_session_groups_a_sprite_walk_cycle_into_one_animation_set() {
        let mut session = CaptureSession::default();
        // Same screen position across consecutive frames, different
        // pixel content each frame (so each is a distinct asset hash) —
        // exactly `rf_ai::animation::group`'s walk-cycle case.
        for f in 0..4u8 {
            let mut tile = sprite(100, 50, u32::from(f), [255, 0, 0, 255]);
            tile.indexed_pixels[0] = f; // perturb so the hash differs per frame
            session.observe(&[tile]);
        }
        let sets = group(&session.observations());
        assert_eq!(
            sets.len(),
            1,
            "a sprite walk cycle must group into one set: {sets:?}"
        );
        assert_eq!(sets[0].assets.len(), 4);
    }

    #[test]
    fn observations_are_capped_rather_than_growing_without_bound() {
        let mut session = CaptureSession::default();
        // Push far more sprite observations than the cap, one distinct
        // asset per call so nothing is deduplicated away.
        for f in 0..(MAX_TRACKED_OBSERVATIONS + 500) {
            let mut tile = sprite(0, 0, 1, [1, 2, 3, 255]);
            tile.indexed_pixels[0] = (f % 251) as u8;
            tile.indexed_pixels[1] = (f / 251) as u8;
            session.observe(&[tile]);
        }
        assert!(session.observations().len() <= MAX_TRACKED_OBSERVATIONS);
    }

    #[test]
    fn clear_resets_everything() {
        let mut session = CaptureSession::default();
        session.observe(&[sprite(0, 0, 1, [1, 2, 3, 255])]);
        assert!(!session.is_empty());
        session.clear();
        assert!(session.is_empty());
        assert!(session.observations().is_empty());
    }

    // -----------------------------------------------------------------
    // run (stub upscaler)
    // -----------------------------------------------------------------

    #[test]
    fn run_with_the_stub_upscales_by_the_requested_factor() {
        let mut session = CaptureSession::default();
        session.observe(&[bg(0, 0, 1, [255, 0, 0, 255])]);
        let assets: Vec<ExtractedAsset> =
            session.tiles().values().map(|t| t.asset.clone()).collect();
        let outcome = run(
            "test-pack",
            "rom-hash",
            &assets,
            &session.observations(),
            &ModelChoice::Stub { scale: 3 },
            PostProcessOptions::default(),
        );
        let pack = outcome
            .pack
            .expect("stub upscaler cannot fail on valid input");
        let image = &pack.built.images[&assets[0].asset_hash];
        assert_eq!((image.width, image.height), (24, 24));
        assert!(outcome.status_line.to_lowercase().contains("cpu"));
        // Attribution never claims a model when none ran.
        assert_eq!(
            pack.attribution.model_name,
            ModelAttribution::none().model_name
        );
    }

    // -----------------------------------------------------------------
    // write_pack — Mesen-compatible round trip
    // -----------------------------------------------------------------

    fn session_with_two_tiles() -> CaptureSession {
        let mut session = CaptureSession::default();
        session.observe(&[bg(0, 0, 1, [255, 0, 0, 255]), bg(8, 0, 2, [0, 255, 0, 255])]);
        session
    }

    fn run_stub(session: &CaptureSession, scale: u32) -> StudioPack {
        let assets: Vec<ExtractedAsset> =
            session.tiles().values().map(|t| t.asset.clone()).collect();
        run(
            "pack-id",
            "rom-hash",
            &assets,
            &session.observations(),
            &ModelChoice::Stub { scale },
            PostProcessOptions::default(),
        )
        .pack
        .unwrap()
    }

    #[test]
    fn write_pack_produces_a_hires_txt_its_own_loader_accepts() {
        let session = session_with_two_tiles();
        let pack = run_stub(&session, 2);
        let dir = std::env::temp_dir().join(format!(
            "rf-upscale-studio-test-{}-mesen-roundtrip",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let written = write_pack(&dir, &pack, session.tiles(), &BTreeMap::new()).unwrap();
        assert_eq!(written, 2);
        assert!(dir.join("hires.txt").exists());
        assert!(dir.join("upscaled.png").exists());
        assert!(dir.join("manifest.toml").exists());

        // The Mesen round trip criterion 2 asks for: `hires.txt` parses
        // with `rf_enhance::hdpack`'s OWN loader, and every captured
        // tile is actually findable through the same lookup the renderer
        // uses — a file that merely parses but matches nothing would
        // pass a weaker assertion.
        let text = std::fs::read_to_string(dir.join("hires.txt")).unwrap();
        let loaded = rf_enhance::hdpack::parse_hires(&text).unwrap();
        assert_eq!(loaded.tiles.len(), 2);
        for tile in session.tiles().values() {
            assert!(
                loaded
                    .lookup(&tile.mesen_tile, &tile.mesen_palette)
                    .is_some(),
                "lost tile {:?}",
                tile.mesen_tile
            );
        }

        let manifest_text = std::fs::read_to_string(dir.join("manifest.toml")).unwrap();
        assert!(manifest_text.contains(&pack.attribution.model_name));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_rejected_tile_is_excluded_from_the_written_pack() {
        let session = session_with_two_tiles();
        let pack = run_stub(&session, 2);
        let dir = std::env::temp_dir().join(format!(
            "rf-upscale-studio-test-{}-rejects-one",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let rejected_hash = session.tiles().keys().next().unwrap().clone();
        let mut decisions = BTreeMap::new();
        decisions.insert(rejected_hash.clone(), Decision::Rejected);
        let written = write_pack(&dir, &pack, session.tiles(), &decisions).unwrap();
        assert_eq!(written, 1);

        let text = std::fs::read_to_string(dir.join("hires.txt")).unwrap();
        let loaded = rf_enhance::hdpack::parse_hires(&text).unwrap();
        let rejected_tile = &session.tiles()[&rejected_hash];
        assert!(loaded
            .lookup(&rejected_tile.mesen_tile, &rejected_tile.mesen_palette)
            .is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_replacement_png_of_the_wrong_size_is_refused() {
        let session = session_with_two_tiles();
        let pack = run_stub(&session, 2); // needs 16x16 replacements
        let dir = std::env::temp_dir().join(format!(
            "rf-upscale-studio-test-{}-bad-replacement",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let wrong_size_png = std::env::temp_dir().join(format!(
            "rf-upscale-studio-test-{}-wrong-size.png",
            std::process::id()
        ));
        let img = image::RgbaImage::new(8, 8); // needs 16x16 at scale 2
        img.save(&wrong_size_png).unwrap();

        let hash = session.tiles().keys().next().unwrap().clone();
        let mut decisions = BTreeMap::new();
        decisions.insert(hash, Decision::Replaced(wrong_size_png.clone()));

        let err = write_pack(&dir, &pack, session.tiles(), &decisions).unwrap_err();
        assert!(err.contains("16x16"), "{err}");

        std::fs::remove_file(&wrong_size_png).ok();
        std::fs::remove_dir_all(&dir).ok();
    }
}
