//! The per-ROM HD asset set (ticket W39-06; `docs/design/REBUILD_LAB.md`
//! section 9, "local AI HD asset pass over the tile cache").
//!
//! An offline pass (rf-ai `hd_pass`) upscales each unique tile once and
//! stores the results here; the renderer's HD tile layer reads them back
//! at draw time with a hash lookup and no model work. This module owns
//! only the data shape and its cache round trip -- it never upscales.
//!
//! The set is ONE cache entry per `(rom, model, settings)`, so loading a
//! game's HD assets is a single `Cache::get`, not one disk read per tile
//! on the frame path. Like [`crate::CanvasChunk`] it is regenerable
//! enhancement data: a decode failure is a miss, so the format carries a
//! version tag but no migration.

use std::collections::BTreeMap;
use std::io::Read;

use bincode::{Decode, Encode};

use crate::error::CacheError;
use crate::{Cache, CacheKey};

/// Format tag; bump when the wire layout changes (old entries then miss).
const FORMAT_VERSION: u32 = 1;
/// `CacheKey::asset_hash` of the whole set.
const SET_ASSET: &str = "hd-asset-set-v1";
const ZSTD_LEVEL: i32 = 3;
/// Decompression cap (same rationale as `canvas_chunk`): a bomb must fail
/// fast rather than exhaust memory.
const MAX_DECOMPRESSED_BYTES: usize = 512 * 1024 * 1024;

/// One upscaled tile: straight-alpha RGBA, `width * height * 4` bytes.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct HdTile {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl HdTile {
    /// `None` when `rgba` does not match the dimensions or is empty.
    #[must_use]
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Option<Self> {
        let ok = width > 0
            && height > 0
            && (width as usize)
                .checked_mul(height as usize)
                .and_then(|n| n.checked_mul(4))
                == Some(rgba.len());
        ok.then_some(Self {
            width,
            height,
            rgba,
        })
    }
}

/// Every HD tile built for one ROM by one model/settings pair, keyed by
/// the source tile's content hash (indexed pixels plus palette, so a
/// recoloured tile is a different key).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct HdAssetSet {
    /// Integer factor every tile was enlarged by.
    pub scale: u32,
    /// Producer identity, as in `CacheKey::producer`.
    pub model_id: String,
    /// Tile content hash (hex) -> HD tile. `BTreeMap` so serialisation is
    /// byte-stable run to run.
    pub tiles: BTreeMap<String, HdTile>,
}

impl HdAssetSet {
    /// Draw-time lookup: one hash-map probe, no allocation.
    #[must_use]
    pub fn get(&self, tile_hash: &str) -> Option<&HdTile> {
        self.tiles.get(tile_hash)
    }
}

/// The cache key of a ROM's HD asset set.
#[must_use]
pub fn hd_assets_key(rom_sha256: &str, model_id: &str, settings_hash: &str) -> CacheKey {
    CacheKey {
        rom_sha256: rom_sha256.to_string(),
        asset_hash: SET_ASSET.to_string(),
        producer: model_id.to_string(),
        settings_hash: settings_hash.to_string(),
    }
}

#[derive(Encode, Decode)]
struct Wire {
    version: u32,
    set: HdAssetSet,
}

pub(crate) fn encode(set: &HdAssetSet) -> Result<Vec<u8>, CacheError> {
    let wire = Wire {
        version: FORMAT_VERSION,
        set: set.clone(),
    };
    let raw = bincode::encode_to_vec(&wire, bincode::config::standard())
        .map_err(|e| CacheError::Encode(e.to_string()))?;
    zstd::encode_all(&raw[..], ZSTD_LEVEL).map_err(|e| CacheError::Encode(e.to_string()))
}

pub(crate) fn decode(bytes: &[u8]) -> Result<HdAssetSet, CacheError> {
    let dec = zstd::Decoder::new(bytes).map_err(|e| CacheError::Decode(e.to_string()))?;
    let mut limited = dec.take(MAX_DECOMPRESSED_BYTES as u64 + 1);
    let mut raw = Vec::new();
    limited
        .read_to_end(&mut raw)
        .map_err(|e| CacheError::Decode(e.to_string()))?;
    if raw.len() > MAX_DECOMPRESSED_BYTES {
        return Err(CacheError::Decode(
            "hd asset set exceeds the decompression cap".to_string(),
        ));
    }
    let (wire, _): (Wire, usize) = bincode::decode_from_slice(&raw, bincode::config::standard())
        .map_err(|e| CacheError::Decode(e.to_string()))?;
    if wire.version != FORMAT_VERSION {
        return Err(CacheError::Decode(format!(
            "hd asset set version {} (expected {FORMAT_VERSION})",
            wire.version
        )));
    }
    for (hash, t) in &wire.set.tiles {
        if HdTile::new(t.width, t.height, t.rgba.clone()).is_none() {
            return Err(CacheError::Decode(format!(
                "hd tile {hash}: buffer does not match {}x{}",
                t.width, t.height
            )));
        }
    }
    Ok(wire.set)
}

impl Cache {
    /// Store a ROM's HD asset set under `key` (see [`hd_assets_key`]).
    ///
    /// # Errors
    /// [`Cache::put`]'s errors, plus [`CacheError::Encode`].
    pub fn put_hd_assets(&mut self, key: &CacheKey, set: &HdAssetSet) -> Result<(), CacheError> {
        let bytes = encode(set)?;
        self.put(key, &bytes)
    }

    /// Load a ROM's HD asset set. `Ok(None)` is a miss.
    ///
    /// # Errors
    /// [`Cache::get`]'s errors, plus [`CacheError::Decode`] for a corrupt
    /// or other-version entry (callers treat that as a miss and rebuild).
    pub fn get_hd_assets(&mut self, key: &CacheKey) -> Result<Option<HdAssetSet>, CacheError> {
        match self.get(key)? {
            Some(b) => Ok(Some(decode(&b)?)),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set() -> HdAssetSet {
        let mut tiles = BTreeMap::new();
        tiles.insert(
            "aa".to_string(),
            HdTile::new(2, 2, (0..16).collect()).unwrap(),
        );
        tiles.insert(
            "bb".to_string(),
            HdTile::new(1, 1, vec![9, 8, 7, 255]).unwrap(),
        );
        HdAssetSet {
            scale: 2,
            model_id: "stub".to_string(),
            tiles,
        }
    }

    fn cache(tag: &str) -> Cache {
        let dir = std::env::temp_dir().join(format!("rf-cache-hd-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Cache::open(dir, 64 * 1024 * 1024).unwrap()
    }

    #[test]
    fn round_trips_through_the_cache() {
        let mut c = cache("rt");
        let key = hd_assets_key("rom", "stub", "s");
        assert_eq!(c.get_hd_assets(&key).unwrap(), None, "miss before put");
        c.put_hd_assets(&key, &set()).unwrap();
        assert_eq!(c.get_hd_assets(&key).unwrap(), Some(set()));
        // A different model or settings is a different entry.
        assert_eq!(
            c.get_hd_assets(&hd_assets_key("rom", "other", "s"))
                .unwrap(),
            None
        );
        assert_eq!(
            c.get_hd_assets(&hd_assets_key("rom", "stub", "t")).unwrap(),
            None
        );
    }

    #[test]
    fn encoding_is_byte_stable_and_corruption_is_an_error() {
        assert_eq!(encode(&set()).unwrap(), encode(&set()).unwrap());
        assert!(decode(&[1, 2, 3, 4]).is_err());
        let mut bad = encode(&set()).unwrap();
        let n = bad.len();
        bad.truncate(n / 2);
        assert!(decode(&bad).is_err());
    }

    #[test]
    fn hd_tile_new_rejects_mismatched_buffers() {
        assert!(HdTile::new(2, 2, vec![0; 15]).is_none());
        assert!(HdTile::new(0, 2, vec![]).is_none());
        assert!(HdTile::new(2, 2, vec![0; 16]).is_some());
    }
}
