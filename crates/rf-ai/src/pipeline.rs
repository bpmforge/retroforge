//! The offline pack pipeline (ticket W8-10, criterion 1; FR-AI-001..004,
//! `docs/design/ENHANCEMENT_RUNTIME.md` §6).
//!
//! Extracted assets in, a validated [`crate::pack::PackManifest`] out. It
//! is the piece that ties the three already-built halves together: the
//! grouping ([`crate::animation`]), the keying and refusal
//! ([`crate::pack`]), and the model seam ([`crate::upscale`]).
//!
//! ## Animation sets are upscaled together, not frame by frame
//!
//! §6: "so a pack upscales a character **coherently instead of
//! per-frame**". That is a statement about *how the model is invoked*, and
//! honouring it is the whole reason this module exists rather than a `map`
//! over assets.
//!
//! Every member of an animation set is packed into ONE sheet
//! ([`sheet_of`]) and upscaled in a SINGLE call, then cut back apart
//! ([`split_sheet`]). The motivation is concrete: upscaling each frame of
//! an eight-frame walk cycle independently gives eight subtly different
//! characters that flicker between one another in motion — invisible in a
//! screenshot, glaring in play. One pass over one sheet gives the model
//! the same context for every frame.
//!
//! An ungrouped asset is its own one-member sheet, so there is exactly one
//! code path rather than a special case that only the rare input exercises.
//!
//! ## What "reproducible from its inputs" is allowed to mean
//!
//! Criterion 1 asks for a pack "cache-keyed so a pack is reproducible from
//! its inputs". Float inference is **not** bit-reproducible across ONNX
//! runtime versions or execution providers, so `model_id` alone does not
//! pin output pixels. Rather than quietly overclaim, the pipeline folds
//! [`crate::upscale::Upscaler::provenance`] into the settings map under
//! [`PROVENANCE_SETTING`], so a runtime change **changes the key** instead
//! of silently changing the pixels behind an unchanged one.
//!
//! What is therefore guaranteed: identical inputs and identical
//! provenance produce an identical key, and the key covers everything that
//! can change the bytes. What is NOT guaranteed: that two different
//! machines produce identical pixels from the same ONNX model. See
//! `docs/design/AI_UPSCALING.md` §4.

use std::collections::{BTreeMap, BTreeSet};

use crate::animation::AnimationSet;
use crate::pack::{settings_hash, CacheKey, PackEntry, PackManifest};
use crate::upscale::{Rgba8, UpscaleError, Upscaler};

/// Reserved settings key holding [`Upscaler::provenance`].
///
/// Reserved, and refused if a caller supplies it
/// ([`PipelineError::ReservedSetting`]): a caller who could set it by hand
/// could make two different runtimes share one cache key, which is exactly
/// the silent-substitution this key exists to prevent.
pub const PROVENANCE_SETTING: &str = "rf.provenance";

/// Hash of an upscaled RGBA image, for [`PackEntry::image_hash`].
///
/// Its own domain tag rather than reusing [`crate::pack::asset_hash`] with
/// an empty palette. `asset_hash` identifies *what the core produced* —
/// indexed pixels against the palette they are indexed by, and its doc is
/// explicit that the palette is part of that identity. An upscaled RGBA
/// image is a different kind of thing, and giving the two the same domain
/// tag would mean a future change to either could collide with the other.
#[must_use]
pub fn image_hash(img: &Rgba8) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"rf-image-v1");
    h.update(img.width.to_le_bytes());
    h.update(img.height.to_le_bytes());
    h.update((img.pixels.len() as u64).to_le_bytes());
    h.update(&img.pixels);
    h.finalize().iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// One extracted asset, exactly as the core produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedAsset {
    /// [`crate::pack::asset_hash`] of `indexed_pixels` + `palette`.
    pub asset_hash: String,
    pub width: u32,
    pub height: u32,
    /// One byte per pixel, `width * height` of them.
    pub indexed_pixels: Vec<u8>,
    /// RGBA quads.
    pub palette: Vec<u8>,
}

/// Why a pack could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineError {
    /// Nothing to build a pack from. Refused rather than emitting an empty
    /// pack, which would validate cleanly and replace nothing — a pack
    /// that claims to do something and does nothing.
    NoAssets,
    /// An animation set names an asset that was never extracted.
    UnknownSetMember { set_id: String, asset_hash: String },
    /// Two extracted assets share a hash but differ in content.
    InconsistentAsset { asset_hash: String },
    /// The caller tried to set [`PROVENANCE_SETTING`] by hand.
    ReservedSetting { key: String },
    /// The upscaler refused.
    Upscale {
        asset_hash: String,
        source: UpscaleError,
    },
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PipelineError::NoAssets => {
                write!(f, "no assets to build a pack from")
            }
            PipelineError::UnknownSetMember { set_id, asset_hash } => write!(
                f,
                "animation set {set_id} names asset {asset_hash}, which was not extracted"
            ),
            PipelineError::InconsistentAsset { asset_hash } => write!(
                f,
                "two different assets were supplied under the same hash {asset_hash}"
            ),
            PipelineError::ReservedSetting { key } => write!(
                f,
                "{key} is reserved for the upscaler's provenance and cannot be set by hand"
            ),
            PipelineError::Upscale { asset_hash, source } => {
                write!(f, "asset {asset_hash} could not be upscaled: {source}")
            }
        }
    }
}

impl std::error::Error for PipelineError {}

/// A built pack: the manifest plus the images it refers to.
///
/// The images travel with the manifest rather than being written to disk
/// here, so the pipeline stays a pure function and the caller decides
/// where a pack lives — which is also what makes it testable without a
/// filesystem.
#[derive(Debug, Clone)]
pub struct BuiltPack {
    pub manifest: PackManifest,
    /// Keyed by `asset_hash`, in the manifest's order.
    pub images: BTreeMap<String, Rgba8>,
    /// The cache key for each replaced asset.
    pub keys: BTreeMap<String, CacheKey>,
}

/// Lay a group of assets out left-to-right on one sheet.
///
/// Height is the tallest member and every member is top-left aligned;
/// unused space is fully transparent. Deliberately a simple row rather
/// than a packed atlas: the layout has to be **reconstructible exactly**
/// by [`split_sheet`] from the member sizes alone, and a bin-packer's
/// output would have to be stored alongside the sheet to be undone.
///
/// # Errors
/// [`UpscaleError`] from decoding any member.
pub fn sheet_of(members: &[&ExtractedAsset]) -> Result<Rgba8, UpscaleError> {
    let total_w: u32 = members.iter().map(|m| m.width).sum();
    let max_h: u32 = members.iter().map(|m| m.height).max().unwrap_or(0);
    if total_w == 0 || max_h == 0 {
        return Err(UpscaleError::EmptyImage {
            width: total_w,
            height: max_h,
        });
    }

    let mut sheet = vec![0u8; total_w as usize * max_h as usize * 4];
    let mut x_off = 0u32;
    for m in members {
        let img = Rgba8::from_indexed(&m.indexed_pixels, &m.palette, m.width, m.height)?;
        // Ranges throughout (CLAUDE.md law 8): both loops are bounded by
        // the member's own dimensions and advance unconditionally.
        for y in 0..img.height {
            let src = y as usize * img.width as usize * 4;
            let dst = (y as usize * total_w as usize + x_off as usize) * 4;
            let n = img.width as usize * 4;
            sheet[dst..dst + n].copy_from_slice(&img.pixels[src..src + n]);
        }
        // Advance FIRST-class: x_off moves by a non-zero width on every
        // iteration, because Rgba8::from_indexed already refused width 0.
        x_off += m.width;
    }

    Rgba8::new(total_w, max_h, sheet)
}

/// Cut an upscaled sheet back into per-asset images.
///
/// The inverse of [`sheet_of`] at `scale`: each member occupies
/// `width * scale` columns starting where its predecessors ended, and the
/// top `height * scale` rows.
///
/// # Errors
/// [`UpscaleError`] if a slice would fall outside the sheet — which means
/// the upscaler did not scale by the factor it declared.
pub fn split_sheet(
    sheet: &Rgba8,
    members: &[&ExtractedAsset],
    scale: u32,
) -> Result<Vec<Rgba8>, UpscaleError> {
    if scale == 0 {
        return Err(UpscaleError::ZeroScale);
    }
    let mut out = Vec::with_capacity(members.len());
    let mut x_off = 0u32;
    for m in members {
        let w = m.width * scale;
        let h = m.height * scale;
        if x_off + w > sheet.width || h > sheet.height {
            return Err(UpscaleError::SheetTooSmall {
                sheet_width: sheet.width,
                sheet_height: sheet.height,
                need_x: x_off + w,
                need_y: h,
            });
        }
        let mut pixels = Vec::with_capacity(w as usize * h as usize * 4);
        for y in 0..h {
            let src = (y as usize * sheet.width as usize + x_off as usize) * 4;
            pixels.extend_from_slice(&sheet.pixels[src..src + w as usize * 4]);
        }
        out.push(Rgba8::new(w, h, pixels)?);
        x_off += w;
    }
    Ok(out)
}

/// Build a replacement pack from extracted assets.
///
/// Every animation set is upscaled as one sheet; assets in no set are
/// upscaled as one-member sheets through the same path. The resulting
/// manifest is keyed per FR-AI-002 and is exactly the format a hand-made
/// pack uses (§6, FR-AI-004) — `model_id` is the only field that says an
/// AI made it.
///
/// # Errors
/// See [`PipelineError`].
pub fn build_pack(
    pack_id: &str,
    rom_hash: &str,
    assets: &[ExtractedAsset],
    sets: &[AnimationSet],
    upscaler: &dyn Upscaler,
    settings: &BTreeMap<String, String>,
) -> Result<BuiltPack, PipelineError> {
    if assets.is_empty() {
        return Err(PipelineError::NoAssets);
    }
    if let Some(key) = settings.keys().find(|k| *k == PROVENANCE_SETTING) {
        return Err(PipelineError::ReservedSetting { key: key.clone() });
    }

    // Index by hash, refusing two different assets under one hash — that
    // would mean the extractor is broken, and every downstream guarantee
    // rests on the hash being an identity.
    let mut by_hash: BTreeMap<&str, &ExtractedAsset> = BTreeMap::new();
    for a in assets {
        if let Some(prev) = by_hash.insert(&a.asset_hash, a) {
            if prev != a {
                return Err(PipelineError::InconsistentAsset {
                    asset_hash: a.asset_hash.clone(),
                });
            }
        }
    }

    // Provenance joins the settings, so the key covers the runtime as well
    // as the model. See the module doc.
    let mut effective = settings.clone();
    effective.insert(PROVENANCE_SETTING.to_string(), upscaler.provenance());
    let settings_digest = settings_hash(&effective);

    // Group the work: one job per animation set, then one per ungrouped
    // asset. BTreeSet/BTreeMap throughout so the order is the hashes'
    // order rather than discovery order — a pack that reordered itself
    // between runs would not be reproducible.
    let mut grouped: BTreeSet<&str> = BTreeSet::new();
    let mut jobs: Vec<(Option<String>, Vec<&ExtractedAsset>)> = Vec::new();

    for set in sets {
        let mut members = Vec::with_capacity(set.assets.len());
        for hash in &set.assets {
            let Some(asset) = by_hash.get(hash.as_str()) else {
                return Err(PipelineError::UnknownSetMember {
                    set_id: set.id.clone(),
                    asset_hash: hash.clone(),
                });
            };
            grouped.insert(&asset.asset_hash);
            members.push(*asset);
        }
        if !members.is_empty() {
            jobs.push((Some(set.id.clone()), members));
        }
    }

    for (hash, asset) in &by_hash {
        if !grouped.contains(hash) {
            jobs.push((None, vec![*asset]));
        }
    }

    let mut images = BTreeMap::new();
    let mut keys = BTreeMap::new();
    let mut entries = Vec::new();

    for (set_id, members) in &jobs {
        let first = members[0].asset_hash.clone();
        let sheet = sheet_of(members).map_err(|source| PipelineError::Upscale {
            asset_hash: first.clone(),
            source,
        })?;
        // THE COHERENCE GUARANTEE: one call for the whole set.
        let up = upscaler
            .upscale(&sheet)
            .map_err(|source| PipelineError::Upscale {
                asset_hash: first.clone(),
                source,
            })?;
        let parts = split_sheet(&up, members, upscaler.scale()).map_err(|source| {
            PipelineError::Upscale {
                asset_hash: first.clone(),
                source,
            }
        })?;

        for (m, img) in members.iter().zip(parts) {
            entries.push(PackEntry {
                asset_hash: m.asset_hash.clone(),
                animation_set: set_id.clone(),
                image_hash: image_hash(&img),
            });
            keys.insert(
                m.asset_hash.clone(),
                CacheKey {
                    rom_hash: rom_hash.to_string(),
                    asset_hash: m.asset_hash.clone(),
                    model_id: upscaler.model_id().to_string(),
                    settings_hash: settings_digest.clone(),
                },
            );
            images.insert(m.asset_hash.clone(), img);
        }
    }

    entries.sort_by(|a, b| a.asset_hash.cmp(&b.asset_hash));

    Ok(BuiltPack {
        manifest: PackManifest {
            id: pack_id.to_string(),
            rom_hash: rom_hash.to_string(),
            model_id: upscaler.model_id().to_string(),
            settings_hash: settings_digest,
            entries,
        },
        images,
        keys,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::asset_hash;
    use crate::upscale::NearestUpscaler;

    /// A solid `w x h` asset of one palette colour.
    fn asset(w: u32, h: u32, colour: [u8; 4]) -> ExtractedAsset {
        let indexed = vec![0u8; (w * h) as usize];
        let palette = colour.to_vec();
        ExtractedAsset {
            asset_hash: asset_hash(&indexed, &palette),
            width: w,
            height: h,
            indexed_pixels: indexed,
            palette,
        }
    }

    fn no_settings() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    #[test]
    fn empty_input_is_refused_rather_than_producing_an_empty_pack() {
        let up = NearestUpscaler::new(2).unwrap();
        let e = build_pack("p", "rom", &[], &[], &up, &no_settings()).unwrap_err();
        assert_eq!(e, PipelineError::NoAssets);
    }

    #[test]
    fn ungrouped_assets_each_become_an_entry() {
        let up = NearestUpscaler::new(2).unwrap();
        let a = asset(2, 2, [255, 0, 0, 255]);
        let b = asset(3, 1, [0, 255, 0, 255]);
        let built = build_pack(
            "p",
            "rom",
            &[a.clone(), b.clone()],
            &[],
            &up,
            &no_settings(),
        )
        .unwrap();
        assert_eq!(built.manifest.entries.len(), 2);
        assert_eq!(built.images[&a.asset_hash].width, 4);
        assert_eq!(built.images[&b.asset_hash].width, 6);
        // No set, so no set id.
        assert!(built
            .manifest
            .entries
            .iter()
            .all(|e| e.animation_set.is_none()));
    }

    #[test]
    fn an_animation_set_is_upscaled_in_one_pass_and_split_back_exactly() {
        // The §6 coherence requirement, in executable form: the members
        // come back at their own sizes, with their own colours, from a
        // single upscale call over the shared sheet.
        let up = NearestUpscaler::new(2).unwrap();
        let a = asset(2, 2, [255, 0, 0, 255]);
        let b = asset(1, 2, [0, 255, 0, 255]);
        let set = AnimationSet {
            id: "walk".into(),
            assets: [a.asset_hash.clone(), b.asset_hash.clone()]
                .into_iter()
                .collect(),
        };
        let built = build_pack(
            "p",
            "rom",
            &[a.clone(), b.clone()],
            &[set],
            &up,
            &no_settings(),
        )
        .unwrap();

        let ia = &built.images[&a.asset_hash];
        let ib = &built.images[&b.asset_hash];
        assert_eq!((ia.width, ia.height), (4, 4));
        assert_eq!((ib.width, ib.height), (2, 4));
        // Each member kept its OWN pixels — the split is aligned, not
        // smeared across the seam.
        assert_eq!(ia.pixel(0, 0), Some([255, 0, 0, 255]));
        assert_eq!(ia.pixel(3, 3), Some([255, 0, 0, 255]));
        assert_eq!(ib.pixel(0, 0), Some([0, 255, 0, 255]));
        assert_eq!(ib.pixel(1, 3), Some([0, 255, 0, 255]));
        // And both carry the set id.
        assert!(built
            .manifest
            .entries
            .iter()
            .all(|e| e.animation_set.as_deref() == Some("walk")));
    }

    #[test]
    fn a_set_member_that_was_never_extracted_is_refused() {
        let up = NearestUpscaler::new(2).unwrap();
        let a = asset(2, 2, [255, 0, 0, 255]);
        let set = AnimationSet {
            id: "walk".into(),
            assets: ["deadbeef".to_string()].into_iter().collect(),
        };
        let e = build_pack("p", "rom", &[a], &[set], &up, &no_settings()).unwrap_err();
        assert_eq!(
            e,
            PipelineError::UnknownSetMember {
                set_id: "walk".into(),
                asset_hash: "deadbeef".into()
            }
        );
    }

    #[test]
    fn provenance_is_reserved_and_cannot_be_forged_by_a_caller() {
        let up = NearestUpscaler::new(2).unwrap();
        let a = asset(2, 2, [255, 0, 0, 255]);
        let mut s = BTreeMap::new();
        s.insert(PROVENANCE_SETTING.to_string(), "lies".to_string());
        let e = build_pack("p", "rom", &[a], &[], &up, &s).unwrap_err();
        assert_eq!(
            e,
            PipelineError::ReservedSetting {
                key: PROVENANCE_SETTING.into()
            }
        );
    }

    #[test]
    fn provenance_is_folded_into_the_key() {
        // The reproducibility honesty: the key must cover the thing that
        // actually decides the pixels, not just the model name.
        let up = NearestUpscaler::new(2).unwrap();
        let a = asset(2, 2, [255, 0, 0, 255]);
        let built = build_pack("p", "rom", &[a], &[], &up, &no_settings()).unwrap();

        let mut with_provenance = BTreeMap::new();
        with_provenance.insert(PROVENANCE_SETTING.to_string(), up.provenance());
        assert_eq!(
            built.manifest.settings_hash,
            settings_hash(&with_provenance)
        );
        // And it differs from the naive hash that ignores provenance —
        // otherwise this test would pass while proving nothing.
        assert_ne!(built.manifest.settings_hash, settings_hash(&no_settings()));
    }

    #[test]
    fn the_same_inputs_produce_the_same_pack() {
        // Criterion 1, in the form it can honestly be asserted.
        let up = NearestUpscaler::new(3).unwrap();
        let a = asset(2, 2, [1, 2, 3, 255]);
        let b = asset(2, 2, [4, 5, 6, 255]);
        let mk = || {
            build_pack(
                "p",
                "rom",
                &[a.clone(), b.clone()],
                &[],
                &up,
                &no_settings(),
            )
            .unwrap()
        };
        let one = mk();
        let two = mk();
        assert_eq!(one.manifest, two.manifest);
        assert_eq!(one.keys, two.keys);
        assert_eq!(one.images, two.images);
    }

    #[test]
    fn entry_order_does_not_depend_on_input_order() {
        // A pack that reordered itself between runs would not be
        // reproducible even though every byte of content matched.
        let up = NearestUpscaler::new(2).unwrap();
        let a = asset(2, 2, [1, 2, 3, 255]);
        let b = asset(2, 2, [4, 5, 6, 255]);
        let fwd = build_pack(
            "p",
            "rom",
            &[a.clone(), b.clone()],
            &[],
            &up,
            &no_settings(),
        )
        .unwrap();
        let rev = build_pack("p", "rom", &[b, a], &[], &up, &no_settings()).unwrap();
        assert_eq!(fwd.manifest.entries, rev.manifest.entries);
    }

    #[test]
    fn a_built_pack_validates_against_the_rom_it_was_built_for() {
        // Ties the pipeline to the already-delivered criterion 3: what
        // this module emits must survive the refusal path.
        let up = NearestUpscaler::new(2).unwrap();
        let a = asset(2, 2, [255, 0, 0, 255]);
        let built = build_pack(
            "p",
            "rom",
            std::slice::from_ref(&a),
            &[],
            &up,
            &no_settings(),
        )
        .unwrap();
        let known: BTreeSet<String> = [a.asset_hash.clone()].into_iter().collect();
        assert!(built.manifest.validate_against("rom", &known).is_ok());
        // And is refused whole against a different ROM.
        assert!(built.manifest.validate_against("other", &known).is_err());
    }

    #[test]
    fn sheet_layout_round_trips_at_scale_one() {
        let a = asset(2, 3, [9, 9, 9, 255]);
        let b = asset(1, 1, [7, 7, 7, 255]);
        let members = vec![&a, &b];
        let sheet = sheet_of(&members).unwrap();
        assert_eq!((sheet.width, sheet.height), (3, 3));
        let parts = split_sheet(&sheet, &members, 1).unwrap();
        assert_eq!((parts[0].width, parts[0].height), (2, 3));
        assert_eq!((parts[1].width, parts[1].height), (1, 1));
        assert_eq!(parts[0].pixel(0, 0), Some([9, 9, 9, 255]));
        assert_eq!(parts[1].pixel(0, 0), Some([7, 7, 7, 255]));
    }

    #[test]
    fn split_refuses_a_scale_the_sheet_cannot_support() {
        // i.e. an upscaler that lied about its own scale factor.
        let a = asset(2, 2, [1, 1, 1, 255]);
        let members = vec![&a];
        let sheet = sheet_of(&members).unwrap();
        assert!(split_sheet(&sheet, &members, 4).is_err());
        assert_eq!(
            split_sheet(&sheet, &members, 0).unwrap_err(),
            UpscaleError::ZeroScale
        );
    }
}
