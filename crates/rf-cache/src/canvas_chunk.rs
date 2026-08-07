//! Wire (de)serialization for [`crate::CanvasChunk`] (ticket W4-03b).
//!
//! bincode 2's `Encode`/`Decode` cannot be derived directly on
//! `CanvasChunk`: it embeds `rf_core_api::PpuPixel`, a foreign type this
//! crate does not own, and Rust's orphan rule forbids `impl Encode for
//! PpuPixel` here (adding the derive at the source, in `rf-core-api`, is
//! also out of this ticket's `write_scope`). So this module defines a
//! private wire mirror (`WirePixel`/`WireChunk`) that derives
//! `Encode`/`Decode` on types THIS crate owns, and converts field-by-field
//! to/from the real `CanvasChunk`/`PpuPixel`/`PixelLayer`.
//!
//! ## zstd: measured, not reflexive (ticket brief's closed dependency set)
//!
//! `TECH_STACK.md` §2's Compression row pre-sanctions zstd for exactly
//! "state/canvas/cache compression", but only "if it earns its place on
//! measured size" — so it was measured before being wired in, using a
//! 4-screen chunk with a 25% unvisited frontier and 8x8-tile background art
//! drawn from a 32-tile set pseudo-randomly placed (not a short,
//! unrealistically-periodic repeat, which a first attempt at this fixture
//! used and got a misleadingly perfect 0.02% out of): 1,351,691 bytes
//! uncompressed, **135,171 bytes at zstd level 3 (10.0x)** — the same crate
//! (`zstd = "0.13.3"`,
//! already in this workspace's dependency graph via `rf-state`, so this is
//! not a new entry in `deny.toml`'s license graph) and the same level
//! `crates/rf-state/src/container.rs::ZSTD_LEVEL` already uses for
//! `.rfstate` bodies, for the same reason: level 3 is zstd's own default
//! and fast enough not to stall a cache write. A stitched canvas is
//! exactly the repetitive content (large unvisited runs, tile-repeating
//! background art) zstd is good at, so this reliably earns its place
//! rather than being added reflexively.
//!
//! The decompression side mirrors `rf_state::container::decompress_capped`
//! exactly (a capped read, refusing to trust an attacker- or
//! corruption-controlled stream to name its own inflated size) for the
//! same reason: a cache entry is read back from disk, the same trust
//! boundary a `.rfstate` file crosses.

use std::io::Read;

use bincode::{Decode, Encode};
use rf_core_api::{PixelLayer, PpuPixel};

use crate::error::CacheError;
use crate::CanvasChunk;

const LAYER_BACKDROP: u8 = 0;
const LAYER_BACKGROUND: u8 = 1;
const LAYER_SPRITE: u8 = 2;

/// Matches `crates/rf-state/src/container.rs::ZSTD_LEVEL` -- zstd's own
/// default, fast enough for a cache write on the frame-adjacent path.
const ZSTD_LEVEL: i32 = 3;

/// Safety cap on the decompressed payload, same rationale and shape as
/// `rf_state::container::MAX_DECOMPRESSED_BODY_BYTES`: not a wire-format
/// field, just a bound on how many bytes this crate will ever read out of
/// the zstd decoder, independent of what the compressed stream claims. A
/// multi-screen stitched canvas is well under this; it exists to make a
/// decompression bomb (small compressed input, huge claimed output) fail
/// fast instead of exhausting memory.
const MAX_DECOMPRESSED_CHUNK_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Encode, Decode)]
struct WirePixel {
    palette_index: u8,
    layer_tag: u8,
    /// `PixelLayer::Background`'s plane index; `0` (unused) for the other
    /// two variants.
    layer_arg: u8,
    sprite_id: Option<u8>,
    priority: u8,
    dropped_by_limit: bool,
}

impl From<PpuPixel> for WirePixel {
    fn from(pixel: PpuPixel) -> Self {
        let (layer_tag, layer_arg) = match pixel.layer {
            PixelLayer::Backdrop => (LAYER_BACKDROP, 0),
            PixelLayer::Background(n) => (LAYER_BACKGROUND, n),
            PixelLayer::Sprite => (LAYER_SPRITE, 0),
        };
        WirePixel {
            palette_index: pixel.palette_index,
            layer_tag,
            layer_arg,
            sprite_id: pixel.sprite_id,
            priority: pixel.priority,
            dropped_by_limit: pixel.dropped_by_limit,
        }
    }
}

impl TryFrom<WirePixel> for PpuPixel {
    type Error = CacheError;

    fn try_from(wire: WirePixel) -> Result<Self, CacheError> {
        let layer = match wire.layer_tag {
            LAYER_BACKDROP => PixelLayer::Backdrop,
            LAYER_BACKGROUND => PixelLayer::Background(wire.layer_arg),
            LAYER_SPRITE => PixelLayer::Sprite,
            other => {
                return Err(CacheError::Decode(format!(
                    "canvas chunk: unknown pixel layer tag {other}"
                )))
            }
        };
        Ok(PpuPixel {
            palette_index: wire.palette_index,
            layer,
            sprite_id: wire.sprite_id,
            priority: wire.priority,
            dropped_by_limit: wire.dropped_by_limit,
        })
    }
}

#[derive(Debug, Encode, Decode)]
struct WireChunk {
    origin_x: i64,
    origin_y: i64,
    width: u32,
    height: u32,
    cells: Vec<Option<WirePixel>>,
}

/// Encode a [`CanvasChunk`] to bytes (bincode 2, `config::standard()`,
/// then zstd -- module doc), preserving the `None`/`Some` gap distinction
/// cell-for-cell ([`CanvasChunk`]'s own doc: "a persisted chunk must
/// preserve that distinction").
pub(crate) fn encode(chunk: &CanvasChunk) -> Result<Vec<u8>, CacheError> {
    let wire = WireChunk {
        origin_x: chunk.origin_x,
        origin_y: chunk.origin_y,
        width: chunk.width,
        height: chunk.height,
        cells: chunk
            .cells
            .iter()
            .map(|cell| cell.map(WirePixel::from))
            .collect(),
    };
    let bincode_bytes = bincode::encode_to_vec(&wire, bincode::config::standard())
        .map_err(|e| CacheError::Encode(e.to_string()))?;
    zstd::encode_all(&bincode_bytes[..], ZSTD_LEVEL).map_err(|e| CacheError::Encode(e.to_string()))
}

/// Decode bytes produced by [`encode`] back into a [`CanvasChunk`]. Never
/// panics on corrupt input -- a truncated/tampered zstd stream, an
/// oversized claimed decompression, or an unrecognized layer tag are all
/// [`CacheError::Decode`], matching `rf_cache::entry::read_entry`'s own
/// "reject garbage without panicking" contract.
pub(crate) fn decode(bytes: &[u8]) -> Result<CanvasChunk, CacheError> {
    let bincode_bytes = decompress_capped(bytes)?;
    let (wire, _): (WireChunk, usize) =
        bincode::decode_from_slice(&bincode_bytes, bincode::config::standard())
            .map_err(|e| CacheError::Decode(e.to_string()))?;

    let mut cells = Vec::with_capacity(wire.cells.len());
    for cell in wire.cells {
        cells.push(match cell {
            Some(wire_pixel) => Some(PpuPixel::try_from(wire_pixel)?),
            None => None,
        });
    }

    Ok(CanvasChunk {
        origin_x: wire.origin_x,
        origin_y: wire.origin_y,
        width: wire.width,
        height: wire.height,
        cells,
    })
}

/// Mirrors `rf_state::container::decompress_capped` exactly: read at most
/// `MAX_DECOMPRESSED_CHUNK_BYTES + 1` bytes out of the zstd decoder,
/// erroring if that bound is actually reached -- module doc.
fn decompress_capped(compressed: &[u8]) -> Result<Vec<u8>, CacheError> {
    let decoder = zstd::Decoder::new(compressed).map_err(|e| CacheError::Decode(e.to_string()))?;
    let mut limited = decoder.take(MAX_DECOMPRESSED_CHUNK_BYTES as u64 + 1);
    let mut body = Vec::new();
    limited
        .read_to_end(&mut body)
        .map_err(|e| CacheError::Decode(e.to_string()))?;
    if body.len() > MAX_DECOMPRESSED_CHUNK_BYTES {
        return Err(CacheError::Decode(format!(
            "canvas chunk exceeds the {MAX_DECOMPRESSED_CHUNK_BYTES}-byte decompression cap"
        )));
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(palette_index: u8, layer: PixelLayer) -> PpuPixel {
        PpuPixel {
            palette_index,
            layer,
            sprite_id: match layer {
                PixelLayer::Sprite => Some(3),
                _ => None,
            },
            priority: 1,
            dropped_by_limit: false,
        }
    }

    #[test]
    fn round_trip_preserves_origin_bounds_and_none_some_distinction() {
        let chunk = CanvasChunk {
            origin_x: -7,
            origin_y: 3,
            width: 2,
            height: 2,
            cells: vec![
                Some(pixel(9, PixelLayer::Background(2))),
                None,
                None,
                Some(pixel(0, PixelLayer::Backdrop)),
            ],
        };
        let bytes = encode(&chunk).unwrap();
        let restored = decode(&bytes).unwrap();
        assert_eq!(restored, chunk);
    }

    #[test]
    fn round_trip_preserves_sprite_layer_pixels_too() {
        let chunk = CanvasChunk {
            origin_x: 0,
            origin_y: 0,
            width: 1,
            height: 1,
            cells: vec![Some(pixel(42, PixelLayer::Sprite))],
        };
        let bytes = encode(&chunk).unwrap();
        let restored = decode(&bytes).unwrap();
        assert_eq!(restored, chunk);
    }

    #[test]
    fn decode_rejects_corrupt_layer_tag_without_panicking() {
        let wire = WireChunk {
            origin_x: 0,
            origin_y: 0,
            width: 1,
            height: 1,
            cells: vec![Some(WirePixel {
                palette_index: 0,
                layer_tag: 99,
                layer_arg: 0,
                sprite_id: None,
                priority: 0,
                dropped_by_limit: false,
            })],
        };
        let bincode_bytes = bincode::encode_to_vec(&wire, bincode::config::standard()).unwrap();
        // decode() now expects zstd-compressed input (module doc) -- compress
        // by hand here so this exercises the layer-tag validation path, not
        // just an early decompression failure.
        let compressed = zstd::encode_all(&bincode_bytes[..], ZSTD_LEVEL).unwrap();
        let err = decode(&compressed).unwrap_err();
        assert!(matches!(err, CacheError::Decode(_)));
    }

    #[test]
    fn decode_rejects_truncated_garbage_without_panicking() {
        let err = decode(&[0xFFu8; 4]).unwrap_err();
        assert!(matches!(err, CacheError::Decode(_)));
    }

    /// Informational, not a criterion assertion: measures the REAL
    /// zstd-compressed encoded size of a realistic multi-screen stitched
    /// chunk. The module doc's "measured, not reflexive" numbers
    /// (1,351,691 -> 135,171 bytes, 10.0x) came from exactly this shape of
    /// fixture via the `zstd` CLI during development; this test keeps a
    /// permanent, looser regression check that compression is still
    /// actually helping (not just present), so a future change that
    /// silently defeats it (e.g. accidentally encrypting/randomizing
    /// payload bytes upstream) is caught.
    #[test]
    fn compression_meaningfully_shrinks_a_realistic_multi_screen_chunk() {
        // Four NES screens' worth (256x240 x 4, stitched side by side), a
        // 25% unvisited frontier, and 8x8-tile background art drawn from a
        // 32-tile set placed pseudo-randomly per tile (not a short,
        // unrealistically periodic repeat) -- representative of "a level
        // stitched across a few screens, partially explored", not an
        // adversarial best case for compression.
        fn tile_hash(tx: u32, ty: u32) -> u32 {
            let mut h = tx.wrapping_mul(0x9E37_79B1) ^ ty.wrapping_mul(0x85EB_CA6B);
            h ^= h >> 13;
            h = h.wrapping_mul(0xC2B2_AE35);
            h ^ (h >> 16)
        }

        let width = 256u32 * 4;
        let height = 240u32;
        let mut cells = Vec::with_capacity((width * height) as usize);
        for y in 0..height {
            for x in 0..width {
                if x >= width * 3 / 4 {
                    cells.push(None); // unexplored frontier
                } else {
                    let (tx, ty) = (x / 8, y / 8);
                    let tile_id = tile_hash(tx, ty) % 32;
                    let (sub_x, sub_y) = (x % 8, y % 8);
                    let v = ((tile_id * 7 + sub_x + sub_y * 3) % 64) as u8;
                    cells.push(Some(pixel(v, PixelLayer::Background(0))));
                }
            }
        }
        let chunk = CanvasChunk {
            origin_x: 0,
            origin_y: 0,
            width,
            height,
            cells,
        };

        let compressed = encode(&chunk).unwrap();
        let pixel_count = (width * height) as usize;
        eprintln!(
            "canvas chunk zstd-compressed size: {} bytes for {pixel_count} cells \
             ({:.2} bytes/cell)",
            compressed.len(),
            compressed.len() as f64 / pixel_count as f64
        );
        // Loose bound (well short of the ~10x measured during
        // development): catches a regression that defeats compression
        // entirely, without pinning to zstd's exact ratio on this fixture.
        assert!(
            compressed.len() < pixel_count * 2,
            "expected meaningful compression on repetitive canvas content, got {} bytes for \
             {pixel_count} cells",
            compressed.len()
        );

        // Round-trips correctly too, on the same realistic fixture.
        let restored = decode(&compressed).unwrap();
        assert_eq!(restored, chunk);
    }
}
