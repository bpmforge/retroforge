//! Local AI HD asset pass (ticket W39-06; `docs/design/REBUILD_LAB.md`
//! section 9).
//!
//! An OFFLINE pass over a ROM's tile and sprite cache: every unique tile
//! (keyed by [`crate::pack::asset_hash`], i.e. indexed pixels plus palette)
//! is upscaled exactly once, and the results are stored in `rf-cache` as an
//! [`HdAssetSet`]. The renderer's HD tile layer only ever looks tiles up by
//! that same hash -- no model runs during play (FR-AI-001, "zero AI on the
//! frame path").
//!
//! ## Object consistency
//!
//! When a recipe's object table says several tiles form one object, those
//! tiles are laid out on a canvas at their in-object offsets and upscaled
//! with ONE call, then cut back apart, so a model sees the whole object and
//! the seams between its tiles match. Tiles in no object are upscaled one at
//! a time. A tile that belongs to several objects keeps the HD version from
//! the object with the smallest id (objects are processed in id order), so
//! the result never depends on discovery order: the same inputs give the
//! same set.
//!
//! ## Opt-in
//!
//! Accuracy mode is the reference (CLAUDE.md law 6). [`build_cached`] does
//! nothing at all (no cache read, no model call) unless the game's toggle is
//! on.

use std::collections::{BTreeMap, BTreeSet};

use rf_cache::{hd_assets_key, Cache, CacheError, HdAssetSet, HdTile};
use sha2::{Digest, Sha256};

use crate::pipeline::ExtractedAsset;
use crate::upscale::{Rgba8, UpscaleError, Upscaler};

/// Largest object canvas edge accepted, in source pixels. Bounds the
/// allocation a malformed recipe row could demand.
pub const MAX_OBJECT_EDGE: u32 = 1024;

/// One tile placed inside an object, offset in source pixels from the
/// object's top-left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectCell {
    pub asset_hash: String,
    pub x: u32,
    pub y: u32,
}

/// Tiles a recipe's object table says belong to one object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectGroup {
    pub id: String,
    pub cells: Vec<ObjectCell>,
}

/// What a pass did, for the UI and for tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HdPassReport {
    /// Calls made to the upscaler (one per object plus one per lone tile).
    pub upscale_calls: usize,
    pub objects_composed: usize,
    pub lone_tiles: usize,
    /// Cells skipped because an earlier object already produced that tile.
    pub shared_tiles_skipped: usize,
    /// True when the set came from the cache and the model never ran.
    pub from_cache: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HdPassError {
    NoAssets,
    UnknownCell {
        object: String,
        asset_hash: String,
    },
    ObjectTooLarge {
        object: String,
        width: u32,
        height: u32,
    },
    EmptyObject {
        object: String,
    },
    Upscale {
        what: String,
        source: UpscaleError,
    },
    /// The upscaler returned an image that is not `scale` times the input.
    WrongScale {
        what: String,
    },
    Cache(CacheError),
}

impl std::fmt::Display for HdPassError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAssets => write!(f, "no tiles to upscale"),
            Self::UnknownCell { object, asset_hash } => {
                write!(f, "object {object} names unknown tile {asset_hash}")
            }
            Self::ObjectTooLarge {
                object,
                width,
                height,
            } => write!(
                f,
                "object {object} spans {width}x{height}, over the {MAX_OBJECT_EDGE} limit"
            ),
            Self::EmptyObject { object } => write!(f, "object {object} has no cells"),
            Self::Upscale { what, source } => write!(f, "upscaling {what}: {source}"),
            Self::WrongScale { what } => write!(f, "upscaler returned the wrong size for {what}"),
            Self::Cache(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for HdPassError {}

/// Hash of everything besides the ROM and the tiles that shapes the output:
/// model, its provenance, scale, and the caller's settings (keys AND values).
#[must_use]
pub fn pass_settings_hash(upscaler: &dyn Upscaler, settings: &BTreeMap<String, String>) -> String {
    let mut h = Sha256::new();
    h.update(b"rf-hd-pass-v1");
    for part in [upscaler.model_id(), &upscaler.provenance()] {
        h.update((part.len() as u64).to_le_bytes());
        h.update(part.as_bytes());
    }
    h.update(upscaler.scale().to_le_bytes());
    for (k, v) in settings {
        for part in [k, v] {
            h.update((part.len() as u64).to_le_bytes());
            h.update(part.as_bytes());
        }
    }
    h.finalize().iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn up_err(what: &str) -> impl Fn(UpscaleError) -> HdPassError + '_ {
    move |source| HdPassError::Upscale {
        what: what.to_string(),
        source,
    }
}

/// Crop `(x, y, w, h)` (already multiplied by scale) out of `img`.
fn crop(img: &Rgba8, x: u32, y: u32, w: u32, h: u32) -> Option<HdTile> {
    if x.checked_add(w)? > img.width || y.checked_add(h)? > img.height {
        return None;
    }
    let mut px = Vec::with_capacity(w as usize * h as usize * 4);
    for row in y..y + h {
        let s = (row as usize * img.width as usize + x as usize) * 4;
        px.extend_from_slice(&img.pixels[s..s + w as usize * 4]);
    }
    HdTile::new(w, h, px)
}

/// Run the pass. Pure given the upscaler: no cache, no clock.
///
/// # Errors
/// See [`HdPassError`].
pub fn build(
    assets: &[ExtractedAsset],
    objects: &[ObjectGroup],
    upscaler: &dyn Upscaler,
) -> Result<(HdAssetSet, HdPassReport), HdPassError> {
    if assets.is_empty() {
        return Err(HdPassError::NoAssets);
    }
    let scale = upscaler.scale();
    let by_hash: BTreeMap<&str, &ExtractedAsset> =
        assets.iter().map(|a| (a.asset_hash.as_str(), a)).collect();
    let mut order: Vec<&ObjectGroup> = objects.iter().collect();
    order.sort_by(|a, b| a.id.cmp(&b.id));

    let mut tiles: BTreeMap<String, HdTile> = BTreeMap::new();
    let mut report = HdPassReport::default();

    for obj in order {
        if obj.cells.is_empty() {
            return Err(HdPassError::EmptyObject {
                object: obj.id.clone(),
            });
        }
        // Resolve cells and the canvas extent first (saturating: an
        // absurd offset becomes ObjectTooLarge, never an overflow).
        let (mut cw, mut ch) = (0u32, 0u32);
        let mut resolved = Vec::with_capacity(obj.cells.len());
        for c in &obj.cells {
            let Some(a) = by_hash.get(c.asset_hash.as_str()) else {
                return Err(HdPassError::UnknownCell {
                    object: obj.id.clone(),
                    asset_hash: c.asset_hash.clone(),
                });
            };
            cw = cw.max(c.x.saturating_add(a.width));
            ch = ch.max(c.y.saturating_add(a.height));
            resolved.push((c, *a));
        }
        if cw > MAX_OBJECT_EDGE || ch > MAX_OBJECT_EDGE {
            return Err(HdPassError::ObjectTooLarge {
                object: obj.id.clone(),
                width: cw,
                height: ch,
            });
        }
        // Skip an object whose tiles are all already produced.
        let fresh = resolved
            .iter()
            .any(|(c, _)| !tiles.contains_key(&c.asset_hash));
        if !fresh {
            report.shared_tiles_skipped += resolved.len();
            continue;
        }

        let mut canvas = vec![0u8; cw as usize * ch as usize * 4];
        for (c, a) in &resolved {
            let img = Rgba8::from_indexed(&a.indexed_pixels, &a.palette, a.width, a.height)
                .map_err(up_err(&obj.id))?;
            for y in 0..a.height {
                for x in 0..a.width {
                    let s = (y as usize * a.width as usize + x as usize) * 4;
                    if img.pixels[s + 3] == 0 {
                        continue; // transparent: keep what is beneath
                    }
                    let d = ((c.y + y) as usize * cw as usize + (c.x + x) as usize) * 4;
                    canvas[d..d + 4].copy_from_slice(&img.pixels[s..s + 4]);
                }
            }
        }
        let sheet = Rgba8::new(cw, ch, canvas).map_err(up_err(&obj.id))?;
        let up = upscaler.upscale(&sheet).map_err(up_err(&obj.id))?;
        report.upscale_calls += 1;
        report.objects_composed += 1;
        if up.width != cw * scale || up.height != ch * scale {
            return Err(HdPassError::WrongScale {
                what: obj.id.clone(),
            });
        }
        for (c, a) in resolved {
            if tiles.contains_key(&c.asset_hash) {
                report.shared_tiles_skipped += 1;
                continue;
            }
            let t = crop(
                &up,
                c.x * scale,
                c.y * scale,
                a.width * scale,
                a.height * scale,
            )
            .ok_or_else(|| HdPassError::WrongScale {
                what: obj.id.clone(),
            })?;
            tiles.insert(c.asset_hash.clone(), t);
        }
    }

    // Lone tiles, in hash order, each exactly once.
    let remaining: BTreeSet<&str> = by_hash
        .keys()
        .copied()
        .filter(|h| !tiles.contains_key(*h))
        .collect();
    for hash in remaining {
        let a = by_hash[hash];
        let img = Rgba8::from_indexed(&a.indexed_pixels, &a.palette, a.width, a.height)
            .map_err(up_err(hash))?;
        let up = upscaler.upscale(&img).map_err(up_err(hash))?;
        report.upscale_calls += 1;
        report.lone_tiles += 1;
        let t = crop(&up, 0, 0, a.width * scale, a.height * scale)
            .filter(|_| up.width == a.width * scale && up.height == a.height * scale)
            .ok_or_else(|| HdPassError::WrongScale {
                what: hash.to_string(),
            })?;
        tiles.insert(hash.to_string(), t);
    }

    Ok((
        HdAssetSet {
            scale,
            model_id: upscaler.model_id().to_string(),
            tiles,
        },
        report,
    ))
}

/// The opt-in, cached entry point.
///
/// * `enabled == false` -> `Ok(None)`: no cache read, no model call.
/// * Cache hit -> the stored set; the upscaler is not called.
/// * Miss -> [`build`], then stored. A corrupt entry counts as a miss.
///
/// # Errors
/// See [`HdPassError`].
pub fn build_cached(
    enabled: bool,
    cache: &mut Cache,
    rom_sha256: &str,
    assets: &[ExtractedAsset],
    objects: &[ObjectGroup],
    upscaler: &dyn Upscaler,
    settings: &BTreeMap<String, String>,
) -> Result<Option<(HdAssetSet, HdPassReport)>, HdPassError> {
    if !enabled {
        return Ok(None);
    }
    let key = hd_assets_key(
        rom_sha256,
        upscaler.model_id(),
        &pass_settings_hash(upscaler, settings),
    );
    if let Ok(Some(set)) = cache.get_hd_assets(&key) {
        // A cached set built from a different tile list is a different
        // game state; require every requested tile be present.
        if assets.iter().all(|a| set.get(&a.asset_hash).is_some()) {
            let report = HdPassReport {
                from_cache: true,
                ..HdPassReport::default()
            };
            return Ok(Some((set, report)));
        }
    }
    let (set, report) = build(assets, objects, upscaler)?;
    cache
        .put_hd_assets(&key, &set)
        .map_err(HdPassError::Cache)?;
    Ok(Some((set, report)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::asset_hash;
    use std::cell::Cell;

    /// Deterministic stub: 2x nearest, but tints by a hash of the WHOLE
    /// input image so that context changes the output (lets tests tell
    /// "composed as an object" from "upscaled alone"). Counts calls.
    struct Stub {
        calls: Cell<usize>,
        factor: u32,
    }
    impl Stub {
        fn new() -> Self {
            Self {
                calls: Cell::new(0),
                factor: 2,
            }
        }
    }
    impl Upscaler for Stub {
        fn model_id(&self) -> &str {
            "stub-v1"
        }
        fn provenance(&self) -> String {
            "test".to_string()
        }
        fn scale(&self) -> u32 {
            self.factor
        }
        fn upscale(&self, src: &Rgba8) -> Result<Rgba8, UpscaleError> {
            self.calls.set(self.calls.get() + 1);
            let tint = src.pixels.iter().fold(0u8, |a, b| a.wrapping_add(*b)) | 1;
            let f = self.factor;
            let (w, h) = (src.width * f, src.height * f);
            let mut px = Vec::with_capacity((w * h * 4) as usize);
            for y in 0..h {
                for x in 0..w {
                    let s = ((y / f) * src.width + x / f) as usize * 4;
                    px.extend_from_slice(&[
                        src.pixels[s].wrapping_add(tint),
                        src.pixels[s + 1],
                        src.pixels[s + 2],
                        src.pixels[s + 3],
                    ]);
                }
            }
            Rgba8::new(w, h, px)
        }
    }

    fn tile(colour: [u8; 4]) -> ExtractedAsset {
        let indexed = vec![0u8; 4];
        let palette = colour.to_vec();
        ExtractedAsset {
            asset_hash: asset_hash(&indexed, &palette),
            width: 2,
            height: 2,
            indexed_pixels: indexed,
            palette,
        }
    }

    fn cell(a: &ExtractedAsset, x: u32, y: u32) -> ObjectCell {
        ObjectCell {
            asset_hash: a.asset_hash.clone(),
            x,
            y,
        }
    }

    fn cache(tag: &str) -> Cache {
        let dir = std::env::temp_dir().join(format!("rf-ai-hd-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Cache::open(dir, 64 * 1024 * 1024).unwrap()
    }

    #[test]
    fn each_unique_tile_is_upscaled_once_and_is_consistent() {
        let a = tile([10, 20, 30, 255]);
        let b = tile([40, 50, 60, 255]);
        // Duplicates in the input list are the same tile.
        let assets = [a.clone(), b.clone(), a.clone()];
        let stub = Stub::new();
        let (set, rep) = build(&assets, &[], &stub).unwrap();
        assert_eq!(stub.calls.get(), 2, "one call per unique tile");
        assert_eq!(rep.lone_tiles, 2);
        assert_eq!(set.tiles.len(), 2);
        // Same input -> same output on a second run.
        let (set2, _) = build(&assets, &[], &Stub::new()).unwrap();
        assert_eq!(set, set2);
        let t = set.get(&a.asset_hash).unwrap();
        assert_eq!((t.width, t.height), (4, 4));
    }

    #[test]
    fn same_pixels_under_a_different_palette_are_a_different_tile() {
        let a = tile([10, 20, 30, 255]);
        let b = tile([10, 20, 31, 255]);
        assert_ne!(a.asset_hash, b.asset_hash);
        let (set, _) = build(&[a.clone(), b.clone()], &[], &Stub::new()).unwrap();
        assert_ne!(set.get(&a.asset_hash), set.get(&b.asset_hash));
    }

    #[test]
    fn an_object_is_upscaled_as_one_image_so_context_reaches_every_tile() {
        let a = tile([10, 20, 30, 255]);
        let b = tile([40, 50, 60, 255]);
        let obj = ObjectGroup {
            id: "o1".into(),
            cells: vec![cell(&a, 0, 0), cell(&b, 2, 0)],
        };
        let stub = Stub::new();
        let (set, rep) = build(&[a.clone(), b.clone()], &[obj], &stub).unwrap();
        assert_eq!(stub.calls.get(), 1, "object = one model call");
        assert_eq!((rep.objects_composed, rep.lone_tiles), (1, 0));
        // The stub tints by whole-image content, so a tile upscaled with
        // its neighbour differs from the same tile upscaled alone.
        let (alone, _) = build(&[a.clone()], &[], &Stub::new()).unwrap();
        assert_ne!(set.get(&a.asset_hash), alone.get(&a.asset_hash));
        // Both tiles of the object share the one tint (seams match).
        let ta = set.get(&a.asset_hash).unwrap();
        let tb = set.get(&b.asset_hash).unwrap();
        let tint_a = ta.rgba[0].wrapping_sub(10);
        let tint_b = tb.rgba[0].wrapping_sub(40);
        assert_eq!(tint_a, tint_b);
        assert_eq!((ta.width, ta.height), (4, 4));
    }

    #[test]
    fn a_tile_in_two_objects_takes_the_lowest_id_regardless_of_input_order() {
        let a = tile([10, 20, 30, 255]);
        let b = tile([40, 50, 60, 255]);
        let c = tile([70, 80, 90, 255]);
        let o1 = ObjectGroup {
            id: "a-first".into(),
            cells: vec![cell(&a, 0, 0), cell(&b, 2, 0)],
        };
        let o2 = ObjectGroup {
            id: "b-second".into(),
            cells: vec![cell(&b, 0, 0), cell(&c, 2, 0)],
        };
        let assets = [a, b.clone(), c];
        let (s1, _) = build(&assets, &[o1.clone(), o2.clone()], &Stub::new()).unwrap();
        let (s2, _) = build(&assets, &[o2, o1], &Stub::new()).unwrap();
        assert_eq!(s1, s2);
        assert_eq!(s1.tiles.len(), 3);
    }

    #[test]
    fn bad_recipe_rows_are_refused() {
        let a = tile([1, 2, 3, 255]);
        let unknown = ObjectGroup {
            id: "o".into(),
            cells: vec![ObjectCell {
                asset_hash: "nope".into(),
                x: 0,
                y: 0,
            }],
        };
        assert!(matches!(
            build(&[a.clone()], &[unknown], &Stub::new()),
            Err(HdPassError::UnknownCell { .. })
        ));
        let huge = ObjectGroup {
            id: "o".into(),
            cells: vec![cell(&a, u32::MAX, 0)],
        };
        assert!(matches!(
            build(&[a], &[huge], &Stub::new()),
            Err(HdPassError::ObjectTooLarge { .. })
        ));
        assert_eq!(build(&[], &[], &Stub::new()), Err(HdPassError::NoAssets));
    }

    #[test]
    fn toggle_off_does_nothing_and_on_is_cached() {
        let a = tile([10, 20, 30, 255]);
        let assets = [a.clone()];
        let mut c = cache("toggle");
        let stub = Stub::new();
        let s = BTreeMap::new();

        let off = build_cached(false, &mut c, "rom", &assets, &[], &stub, &s).unwrap();
        assert!(off.is_none());
        assert_eq!(stub.calls.get(), 0, "off: model never ran");
        assert!(c.is_empty(), "off: cache untouched");

        let (first, r1) = build_cached(true, &mut c, "rom", &assets, &[], &stub, &s)
            .unwrap()
            .unwrap();
        assert!(!r1.from_cache);
        assert_eq!(stub.calls.get(), 1);

        let (second, r2) = build_cached(true, &mut c, "rom", &assets, &[], &stub, &s)
            .unwrap()
            .unwrap();
        assert!(r2.from_cache);
        assert_eq!(stub.calls.get(), 1, "second run is a pure cache hit");
        assert_eq!(first, second);

        // A different ROM does not share the entry.
        let _ = build_cached(true, &mut c, "other", &assets, &[], &stub, &s).unwrap();
        assert_eq!(stub.calls.get(), 2);
        // Different settings change the key (values count, not just keys).
        let mut s2 = BTreeMap::new();
        s2.insert("strength".to_string(), "2".to_string());
        assert_ne!(
            pass_settings_hash(&stub, &s),
            pass_settings_hash(&stub, &s2)
        );
    }
}
