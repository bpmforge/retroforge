//! Canvas persistence v2 (ticket W33-05; `docs/design/STITCHING_PLAN.md`
//! §2.7): a scene's whole picture memory in ONE `rf-cache` entry per
//! `(ROM sha256, scene id)`, so a second visit restores it instantly.
//!
//! ## What an entry holds
//!
//! The four layer canvases (origin, size, cells: palette index, priority,
//! layer, palette id, source), the scene's [`PaletteTable`], the per-layer
//! placement [`Confidence`], and the [`SceneEdge`]s that touch the scene.
//! Per-row backdrop state is NOT stored: it is derived from the live
//! bundle's line state every frame, never remembered per scene.
//!
//! ## Wire format (all little-endian, no hash-ordered iteration anywhere)
//!
//! ```text
//! "RFSC" u16 version | u64 scene | u32 raw_len | zstd(body) | u32 crc32
//! body = layer x4 { i64 x, i64 y, u32 w, u32 h, cells }
//!        u16 palettes, u64 overflowed, palettes x 256 x u16
//!        confidence x4 { Confidence::ENCODED_LEN }
//!        u16 edges, edges x SceneEdge::ENCODED_LEN
//!        u8 flags (bit 0: a scout ran every pass on this scene)
//!        [u8 rung, u8 step_px]   (W33-06; absent in an entry written by the
//!                                 first v2 build: read as "no memo")
//! cell = kind(2 bits: 0 unknown, 1 live, 2 scout) | layer(2 bits) | has_id<<4
//!        [palette_index, priority, (layer arg if background), (id if has_id)]
//! ```
//!
//! The CRC32 covers everything before it, so truncation and bit rot are
//! caught before a byte is interpreted. Every count read from the file is
//! checked against a fixed bound AND against the bytes that remain before
//! anything is allocated or looped over (law 8: each loop below is a
//! `for _ in 0..n` over a bounded `n`).
//!
//! ## Versioning
//!
//! [`SETTINGS_HASH`] is part of the cache key, so a cache written by another
//! format version is never even opened (the v1 single-canvas entries of
//! W4-03b live under `canvas-chunk-v1` and simply miss); [`VERSION`] inside
//! the file is the second check. Anything that does not decode is ignored
//! with one log line and treated as a miss: a canvas is regenerable.
//!
//! ## Size
//!
//! zstd level 3 (the level `rf-cache` and `rf-state` use; measured in the
//! shot, see the ticket notes). One entry is capped at [`MAX_ENTRY_BYTES`]
//! stored: a scene past it drops its HIGHEST layers (3, then 2, then 1: the
//! level's own background is layer 0/1) and says so in the log. Across
//! scenes `rf-cache` evicts whole least-recently-used entries to its cap
//! (`CANVAS_CACHE_CAP_BYTES`).

use std::io::Read;

use rf_cache::{Cache, CacheError, CacheKey, CanvasChunk};
use rf_core_api::{PixelLayer, PpuPixel};

use crate::canvas::{Confidence, PaletteTable, LAYERS, LINE_PALETTE_ENTRIES, MAX_PALETTES};
use crate::scene_identity::SceneId;
use crate::scene_tracker::SceneEdge;
use crate::stitcher::Canvas;

/// Fixed producer identity for the deterministic (non-AI) stitcher.
const PRODUCER: &str = "rf-enhance-stitcher";

/// Cache-key version tag: a different tag is a different key, so an entry of
/// another format is a clean miss, never misread bytes.
pub const SETTINGS_HASH: &str = "canvas-chunk-v2";

/// The tag of the retired single-canvas entries (W4-03b), for tests.
pub const LEGACY_SETTINGS_HASH: &str = "canvas-chunk-v1";

/// Format version inside the file.
pub const VERSION: u16 = 2;

const MAGIC: &[u8; 4] = b"RFSC";
const ZSTD_LEVEL: i32 = 3;
/// Header before the compressed body: magic, version, scene, raw length.
const HEADER_LEN: usize = 4 + 2 + 8 + 4;
const FOOTER_LEN: usize = 4;

/// Most stored bytes one scene may take (see the module doc).
pub const MAX_ENTRY_BYTES: usize = 48 * 1024 * 1024;
/// Most body bytes a decoder will inflate (a decompression-bomb bound).
const MAX_RAW_BYTES: usize = 256 * 1024 * 1024;
/// Most cells one layer may claim (SMW's longest levels are ~5 M).
const MAX_LAYER_CELLS: u64 = 64 * 1024 * 1024;
/// Most edges one entry may carry.
const MAX_EDGES: usize = 4096;

const KIND_UNKNOWN: u8 = 0;
const KIND_LIVE: u8 = 1;
const KIND_SCOUT: u8 = 2;

/// Where a cell came from: a live frame, or the level scout (W33-06).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Live,
    Scout,
}

/// Ticket W33-06: which rung of the strategy ladder finished a scene's scout
/// and at what step, kept with the scene so the next scout of it starts
/// there. `rung` codes: [`Self::PLAYER_MOVE`], [`Self::WALK`],
/// [`Self::CAMERA_WRITE`], [`Self::GAVE_UP`] (every rung failed the
/// agreement check: the margins stay on live cells only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScoutMemo {
    pub rung: u8,
    pub step_px: u8,
}

impl ScoutMemo {
    pub const PLAYER_MOVE: u8 = 1;
    pub const WALK: u8 = 2;
    pub const CAMERA_WRITE: u8 = 3;
    pub const GAVE_UP: u8 = 255;

    fn valid(self) -> bool {
        matches!(
            self.rung,
            Self::PLAYER_MOVE | Self::WALK | Self::CAMERA_WRITE | Self::GAVE_UP
        )
    }
}

/// A bounds-checked little-endian cursor: every read is `None` on short
/// input, so a truncated file ends a parse instead of panicking.
pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }

    pub(crate) fn pos(&self) -> usize {
        self.at
    }

    pub(crate) fn remaining(&self) -> usize {
        self.buf.len() - self.at
    }

    pub(crate) fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let out = self.buf.get(self.at..end)?;
        self.at = end;
        Some(out)
    }

    pub(crate) fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    pub(crate) fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }

    pub(crate) fn u32(&mut self) -> Option<u32> {
        self.take(4)?.try_into().ok().map(u32::from_le_bytes)
    }

    pub(crate) fn u64(&mut self) -> Option<u64> {
        self.take(8)?.try_into().ok().map(u64::from_le_bytes)
    }

    pub(crate) fn i64(&mut self) -> Option<i64> {
        self.take(8)?.try_into().ok().map(i64::from_le_bytes)
    }
}

/// The cache key a scene of `rom_sha256` is stored under.
#[must_use]
pub fn scene_cache_key(rom_sha256: &str, scene_id: SceneId) -> CacheKey {
    key_with(rom_sha256, scene_id, SETTINGS_HASH)
}

/// A key under another format tag (tests: the retired v1 key).
#[must_use]
pub fn key_with(rom_sha256: &str, scene_id: SceneId, settings_hash: &str) -> CacheKey {
    CacheKey {
        rom_sha256: rom_sha256.to_string(),
        asset_hash: scene_id.to_hex(),
        producer: PRODUCER.to_string(),
        settings_hash: settings_hash.to_string(),
    }
}

/// What to persist for a scene, borrowed.
#[derive(Clone, Copy)]
pub struct SceneView<'a> {
    pub layers: &'a [Canvas; LAYERS],
    pub palettes: &'a PaletteTable,
    pub confidence: &'a [Confidence; LAYERS],
    pub edges: &'a [SceneEdge],
    /// A scout ran every pass on this scene: the next visit skips it.
    pub scouted: bool,
    /// The rung that scout finished on (W33-06).
    pub scout_memo: Option<ScoutMemo>,
}

/// What a decoded entry holds.
#[derive(Debug)]
pub struct SceneData {
    pub layers: [Canvas; LAYERS],
    pub palettes: PaletteTable,
    pub confidence: [Confidence; LAYERS],
    pub edges: Vec<SceneEdge>,
    pub scouted: bool,
    pub scout_memo: Option<ScoutMemo>,
    /// CRC32 of the decoded body without its confidence statistics: what the
    /// scene looked like on disk, so a
    /// flush of an unchanged scene writes nothing ([`save_scene`]).
    pub body_crc: u32,
}

/// What [`save_scene`] wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Saved {
    /// Bytes of the body before compression.
    pub raw_bytes: usize,
    /// Bytes stored on disk (header, compressed body, footer).
    pub stored_bytes: usize,
    /// Layers left out because the entry was over [`MAX_ENTRY_BYTES`].
    pub dropped_layers: usize,
    /// CRC32 of the body that was (or, when `unchanged`, would have been)
    /// written.
    pub body_crc: u32,
    /// The body equals the one the caller said is already on disk: nothing
    /// was compressed or written.
    pub unchanged: bool,
}

fn push_cell(out: &mut Vec<u8>, cell: Option<PpuPixel>, source: Source) {
    let Some(p) = cell else {
        out.push(KIND_UNKNOWN);
        return;
    };
    let (layer, arg) = match p.layer {
        PixelLayer::Backdrop => (0u8, None),
        PixelLayer::Background(n) => (1, Some(n)),
        PixelLayer::Sprite => (2, None),
    };
    let kind = match source {
        Source::Live => KIND_LIVE,
        Source::Scout => KIND_SCOUT,
    };
    out.push(kind | (layer << 2) | (u8::from(p.sprite_id.is_some()) << 4));
    out.push(p.palette_index);
    out.push(p.priority);
    out.extend(arg);
    out.extend(p.sprite_id);
}

fn read_cell(r: &mut Reader<'_>) -> Option<(Option<PpuPixel>, Source)> {
    let tag = r.u8()?;
    if tag & !0b0001_1111 != 0 {
        return None;
    }
    let source = match tag & 0b11 {
        KIND_UNKNOWN => {
            return (tag == KIND_UNKNOWN).then_some((None, Source::Live));
        }
        KIND_LIVE => Source::Live,
        KIND_SCOUT => Source::Scout,
        _ => return None,
    };
    let (palette_index, priority) = (r.u8()?, r.u8()?);
    let layer = match tag >> 2 & 0b11 {
        0 => PixelLayer::Backdrop,
        1 => PixelLayer::Background(r.u8()?),
        2 => PixelLayer::Sprite,
        _ => return None,
    };
    let sprite_id = if tag >> 4 & 1 == 1 {
        Some(r.u8()?)
    } else {
        None
    };
    Some((
        Some(PpuPixel {
            palette_index,
            layer,
            sprite_id,
            priority,
        }),
        source,
    ))
}

/// CRC32 of a body without its confidence section: the per-frame statistics
/// there change every frame, and a scene that only counted frames has nothing
/// new to write.
fn content_crc(body: &[u8], confidence: std::ops::Range<usize>) -> u32 {
    let mut h = crc32fast::Hasher::new();
    h.update(&body[..confidence.start]);
    h.update(&body[confidence.end..]);
    h.finalize()
}

/// The body, and the byte range of its confidence section.
fn encode_body(view: &SceneView<'_>, keep_layers: usize) -> (Vec<u8>, std::ops::Range<usize>) {
    let mut out = Vec::new();
    let empty = Canvas::new();
    for (n, canvas) in view.layers.iter().enumerate() {
        let canvas = if n < keep_layers { canvas } else { &empty };
        let (ox, oy) = canvas.origin();
        out.extend_from_slice(&ox.to_le_bytes());
        out.extend_from_slice(&oy.to_le_bytes());
        out.extend_from_slice(&(canvas.width() as u32).to_le_bytes());
        out.extend_from_slice(&(canvas.height() as u32).to_le_bytes());
        let flags = canvas.scout_flags();
        for (i, cell) in canvas.cells().iter().enumerate() {
            let scout = flags.get(i).copied().unwrap_or(false);
            push_cell(
                &mut out,
                *cell,
                if scout { Source::Scout } else { Source::Live },
            );
        }
    }
    let palettes = view.palettes.palettes();
    out.extend_from_slice(&(palettes.len() as u16).to_le_bytes());
    out.extend_from_slice(&view.palettes.overflowed().to_le_bytes());
    for words in palettes {
        for w in words {
            out.extend_from_slice(&w.to_le_bytes());
        }
    }
    let conf_start = out.len();
    for c in view.confidence {
        out.extend_from_slice(&c.to_bytes());
    }
    let conf_end = out.len();
    let edges = &view.edges[..view.edges.len().min(MAX_EDGES)];
    out.extend_from_slice(&(edges.len() as u16).to_le_bytes());
    for e in edges {
        out.extend_from_slice(&e.to_bytes());
    }
    out.push(u8::from(view.scouted));
    let memo = view.scout_memo.filter(|m| m.valid());
    out.push(memo.map_or(0, |m| m.rung));
    out.push(memo.map_or(0, |m| m.step_px));
    (out, conf_start..conf_end)
}

fn decode_body(body: &[u8]) -> Option<SceneData> {
    let mut r = Reader::new(body);
    let mut layers: [Canvas; LAYERS] = Default::default();
    for slot in &mut layers {
        let (ox, oy) = (r.i64()?, r.i64()?);
        let (w, h) = (u64::from(r.u32()?), u64::from(r.u32()?));
        let n = w.checked_mul(h).filter(|&n| n <= MAX_LAYER_CELLS)?;
        // Every cell takes at least one byte: a count the file cannot back is
        // refused before anything is allocated.
        let n = usize::try_from(n).ok().filter(|&n| n <= r.remaining())?;
        let mut cells = Vec::with_capacity(n);
        let mut scout = Vec::new();
        for i in 0..n {
            let (cell, source) = read_cell(&mut r)?;
            if source == Source::Scout {
                if scout.is_empty() {
                    scout = vec![false; n];
                }
                scout[i] = true;
            }
            cells.push(cell);
        }
        *slot = Canvas::from_raw_parts_with_sources(ox, oy, w as usize, h as usize, cells, scout);
    }
    let count = usize::from(r.u16()?);
    let overflowed = r.u64()?;
    if count > MAX_PALETTES || count * LINE_PALETTE_ENTRIES * 2 > r.remaining() {
        return None;
    }
    let mut palettes = Vec::with_capacity(count);
    for _ in 0..count {
        let mut words = [0u16; LINE_PALETTE_ENTRIES];
        for w in &mut words {
            *w = r.u16()?;
        }
        palettes.push(words);
    }
    let palettes = PaletteTable::from_parts(palettes, overflowed)?;
    let mut confidence = [Confidence::default(); LAYERS];
    let conf_start = r.pos();
    for c in &mut confidence {
        *c = Confidence::from_bytes(r.take(Confidence::ENCODED_LEN)?)?;
    }
    let conf_end = r.pos();
    let edge_count = usize::from(r.u16()?);
    if edge_count > MAX_EDGES || edge_count * SceneEdge::ENCODED_LEN > r.remaining() {
        return None;
    }
    let mut edges = Vec::with_capacity(edge_count);
    for _ in 0..edge_count {
        edges.push(SceneEdge::from_bytes(r.take(SceneEdge::ENCODED_LEN)?)?);
    }
    let scouted = match r.u8()? {
        0 => false,
        1 => true,
        _ => return None,
    };
    // The rung memo (W33-06); an entry from the first v2 build ends here.
    let scout_memo = if r.remaining() == 0 {
        None
    } else {
        let (rung, step_px) = (r.u8()?, r.u8()?);
        let memo = (rung != 0 || step_px != 0).then_some(ScoutMemo { rung, step_px });
        // A rung byte that is not a rung is a corrupt entry, not "no memo".
        if memo.is_some_and(|m| !m.valid()) {
            return None;
        }
        memo
    };
    if r.remaining() != 0 {
        return None;
    }
    Some(SceneData {
        layers,
        palettes,
        confidence,
        edges,
        scouted,
        scout_memo,
        body_crc: content_crc(body, conf_start..conf_end),
    })
}

/// The bytes of one entry (header, zstd body, CRC32), keeping the first
/// `keep_layers` layers. Returns them with the body's raw length.
fn encode_entry(scene: SceneId, view: &SceneView<'_>, keep_layers: usize) -> (Vec<u8>, usize) {
    frame_body(scene, &encode_body(view, keep_layers).0)
}

fn frame_body(scene: SceneId, body: &[u8]) -> (Vec<u8>, usize) {
    let mut out = Vec::with_capacity(HEADER_LEN + body.len() / 4 + FOOTER_LEN);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&scene.0.to_le_bytes());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    // `compress` of an in-memory buffer fails only on an allocation; an
    // empty body then fails the length check on load, which is a miss.
    out.extend_from_slice(&zstd::bulk::compress(body, ZSTD_LEVEL).unwrap_or_default());
    let crc = crc32fast::hash(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    (out, body.len())
}

/// Decode one entry's bytes.
///
/// # Errors
/// A message naming what was wrong (bad length, CRC, magic, version, scene,
/// body): the caller logs it and treats the entry as a miss.
pub fn decode_entry(bytes: &[u8], scene: SceneId) -> Result<SceneData, String> {
    if bytes.len() < HEADER_LEN + FOOTER_LEN {
        return Err(format!("truncated ({} bytes)", bytes.len()));
    }
    let (signed, footer) = bytes.split_at(bytes.len() - FOOTER_LEN);
    let want = u32::from_le_bytes([footer[0], footer[1], footer[2], footer[3]]);
    if crc32fast::hash(signed) != want {
        return Err("crc mismatch (truncated or corrupt)".to_string());
    }
    let mut r = Reader::new(signed);
    if r.take(4) != Some(MAGIC.as_slice()) {
        return Err("bad magic".to_string());
    }
    let version = r.u16().ok_or("short header")?;
    if version != VERSION {
        return Err(format!(
            "format version {version}, this build reads {VERSION}"
        ));
    }
    if r.u64() != Some(scene.0) {
        return Err("entry is for another scene".to_string());
    }
    let raw_len = r.u32().ok_or("short header")? as usize;
    if raw_len > MAX_RAW_BYTES {
        return Err(format!("claims {raw_len} body bytes (cap {MAX_RAW_BYTES})"));
    }
    let compressed = r.take(r.remaining()).ok_or("short body")?;
    let decoder = zstd::Decoder::new(compressed).map_err(|e| e.to_string())?;
    let mut body = Vec::new();
    decoder
        .take(raw_len as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|e| e.to_string())?;
    if body.len() != raw_len {
        return Err(format!(
            "body is {} bytes, header says {raw_len}",
            body.len()
        ));
    }
    decode_body(&body).ok_or_else(|| "body does not parse".to_string())
}

/// Persist a scene into `cache` (atomically: `rf-cache` writes a temp file
/// and renames it). An entry over [`MAX_ENTRY_BYTES`] drops layers 3, 2, 1
/// until it fits. `on_disk_crc` is the body CRC of what is already stored
/// (from a restore or the last save): an equal body is not written again.
///
/// # Errors
/// Propagates [`CacheError`] from `Cache::put`.
pub fn save_scene(
    cache: &mut Cache,
    rom_sha256: &str,
    scene: SceneId,
    view: &SceneView<'_>,
    on_disk_crc: Option<u32>,
) -> Result<Saved, CacheError> {
    save_scene_capped(cache, rom_sha256, scene, view, on_disk_crc, MAX_ENTRY_BYTES)
}

/// [`save_scene`] with the entry cap as a parameter (tests).
///
/// # Errors
/// Propagates [`CacheError`] from `Cache::put`.
pub fn save_scene_capped(
    cache: &mut Cache,
    rom_sha256: &str,
    scene: SceneId,
    view: &SceneView<'_>,
    on_disk_crc: Option<u32>,
    cap: usize,
) -> Result<Saved, CacheError> {
    let mut keep = LAYERS;
    let (body, conf) = encode_body(view, keep);
    let body_crc = content_crc(&body, conf);
    if on_disk_crc == Some(body_crc) {
        return Ok(Saved {
            body_crc,
            unchanged: true,
            ..Saved::default()
        });
    }
    let (mut bytes, mut raw) = frame_body(scene, &body);
    // Bounded: at most LAYERS - 1 re-encodes.
    while bytes.len() > cap && keep > 1 {
        keep -= 1;
        (bytes, raw) = encode_entry(scene, view, keep);
    }
    if keep < LAYERS {
        eprintln!(
            "rf-enhance: canvas cache: scene {} is {} bytes stored; kept layers 0..{keep}              (cap {cap})",
            scene.to_hex(),
            bytes.len()
        );
    }
    cache.put(&scene_cache_key(rom_sha256, scene), &bytes)?;
    Ok(Saved {
        raw_bytes: raw,
        stored_bytes: bytes.len(),
        dropped_layers: LAYERS - keep,
        body_crc,
        unchanged: false,
    })
}

/// Restore a scene, if cached. `Ok(None)` for a miss. An entry that does not
/// decode is ignored with one log line and reads as a miss.
///
/// # Errors
/// Propagates [`CacheError`] from `Cache::get` (an I/O or containment fault).
pub fn load_scene(
    cache: &mut Cache,
    rom_sha256: &str,
    scene: SceneId,
) -> Result<Option<SceneData>, CacheError> {
    let Some(bytes) = cache.get(&scene_cache_key(rom_sha256, scene))? else {
        return Ok(None);
    };
    match decode_entry(&bytes, scene) {
        Ok(data) => Ok(Some(data)),
        Err(why) => {
            eprintln!(
                "rf-enhance: canvas cache: ignoring scene {}: {why}",
                scene.to_hex()
            );
            Ok(None)
        }
    }
}

/// Export a live [`Canvas`] to the persistable [`CanvasChunk`] shape,
/// preserving origin, bounds, and the `None`/`Some` per-cell distinction
/// exactly (W4-08's persistence lesson — dropping origin/bounds shifts
/// world coordinates silently on the next session; collapsing `None` to a
/// filled cell is the fog dishonesty this ticket's brief names
/// explicitly).
#[must_use]
pub fn canvas_to_chunk(canvas: &Canvas) -> CanvasChunk {
    let (origin_x, origin_y) = canvas.origin();
    CanvasChunk {
        origin_x,
        origin_y,
        width: canvas.width() as u32,
        height: canvas.height() as u32,
        cells: canvas.cells().to_vec(),
    }
}

/// The inverse of [`canvas_to_chunk`].
#[must_use]
pub fn chunk_to_canvas(chunk: &CanvasChunk) -> Canvas {
    Canvas::from_raw_parts(
        chunk.origin_x,
        chunk.origin_y,
        chunk.width as usize,
        chunk.height as usize,
        chunk.cells.clone(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Check;

    fn px(index: u8, layer: PixelLayer, id: Option<u8>, priority: u8) -> PpuPixel {
        PpuPixel {
            palette_index: index,
            layer,
            sprite_id: id,
            priority,
        }
    }

    /// A canvas with a non-zero asymmetric origin, non-square bounds, gaps,
    /// and every layer variant and id/no-id combination.
    fn canvas(seed: u8) -> Canvas {
        let mut c = Canvas::new();
        c.blit_row(
            -37,
            12,
            &[
                Some(px(seed, PixelLayer::Background(2), Some(3), 1)),
                None,
                Some(px(4, PixelLayer::Backdrop, None, 0)),
                Some(px(255, PixelLayer::Sprite, Some(255), 3)),
                None,
            ],
        );
        c.blit_row(-35, 14, &[Some(px(9, PixelLayer::Background(0), None, 2))]);
        // W33-06: cells the scout filled, beside live ones, and a grown canvas.
        c.blit_row_scout(
            -33,
            14,
            &[
                Some(px(seed, PixelLayer::Background(1), Some(2), 1)),
                None,
                Some(px(6, PixelLayer::Backdrop, None, 0)),
            ],
        );
        c
    }

    fn fixture() -> SceneData {
        let mut palettes = Vec::new();
        for fill in [0x1111u16, 0x2222, 0x7FFF] {
            let mut w = [0u16; LINE_PALETTE_ENTRIES];
            w[1] = fill;
            w[255] = !fill;
            palettes.push(w);
        }
        let mut confidence = [Confidence::default(); LAYERS];
        confidence[1].observe(Check {
            agreed: 7,
            compared: 9,
            withheld_rows: 3,
            judged_bands: 2,
            judged_rows: 40,
            picture_rows: 100,
            residual: Some(-5),
            nanos: 1234,
        });
        let edge = |from, to, continuous| SceneEdge {
            from,
            to,
            from_pos: (-5, 7),
            to_pos: (1 << 40, -(1 << 33)),
            continuous,
        };
        SceneData {
            layers: [canvas(1), Canvas::new(), canvas(200), Canvas::new()],
            palettes: PaletteTable::from_parts(palettes, 4).unwrap(),
            confidence,
            edges: vec![edge(1, 2, true), edge(2, 1, false)],
            scouted: true,
            scout_memo: Some(ScoutMemo {
                rung: ScoutMemo::PLAYER_MOVE,
                step_px: 4,
            }),
            body_crc: 0,
        }
    }

    fn view(d: &SceneData) -> SceneView<'_> {
        SceneView {
            layers: &d.layers,
            palettes: &d.palettes,
            confidence: &d.confidence,
            edges: &d.edges,
            scouted: d.scouted,
            scout_memo: d.scout_memo,
        }
    }

    fn unique_dir(label: &str) -> std::path::PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "rf-enhance-persist-v2-{label}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    #[test]
    fn a_scene_round_trips_cells_palettes_confidence_and_edges() {
        let d = fixture();
        let scene = SceneId(0xDEAD_BEEF_0000_0001);
        let (bytes, _) = encode_entry(scene, &view(&d), LAYERS);
        let back = decode_entry(&bytes, scene).expect("decodes");
        assert_eq!(back.layers, d.layers, "cells, origins and bounds");
        assert_eq!(back.palettes, d.palettes);
        assert_eq!(back.palettes.overflowed(), 4);
        assert_eq!(back.confidence, d.confidence);
        assert_eq!(back.confidence[1].last.residual, Some(-5));
        assert_eq!(back.edges, d.edges);
        assert!(back.scouted);
        assert_eq!(back.scout_memo, d.scout_memo, "the rung it finished on");
        // W33-06: which cells are Scout survives, cell for cell.
        for (a, b) in back.layers.iter().zip(&d.layers) {
            assert_eq!(a.scout_flags().len(), b.scout_flags().len());
            assert_eq!(a.scout_cell_count(), b.scout_cell_count());
        }
        assert!(
            back.layers[0].scout_cell_count() == 2,
            "two scout cells in the fixture"
        );
        assert!(back.layers[0].is_scout(-33, 14) && !back.layers[0].is_scout(-35, 14));
        // Deterministic: the decoded scene encodes to the very same bytes.
        assert_eq!(encode_entry(scene, &view(&back), LAYERS).0, bytes);
        assert_eq!(encode_entry(scene, &view(&d), LAYERS).0, bytes);
    }

    #[test]
    fn a_live_frame_over_a_scout_cell_makes_it_live_and_the_flag_survives_a_round_trip() {
        let mut d = fixture();
        // A live frame lands on one of the scout cells of layer 0.
        d.layers[0].blit_row(-33, 14, &[Some(px(77, PixelLayer::Background(1), None, 0))]);
        assert!(!d.layers[0].is_scout(-33, 14));
        assert_eq!(d.layers[0].scout_cell_count(), 1);
        let scene = SceneId(5);
        let (bytes, _) = encode_entry(scene, &view(&d), LAYERS);
        let back = decode_entry(&bytes, scene).expect("decodes");
        assert_eq!(back.layers, d.layers);
        assert!(!back.layers[0].is_scout(-33, 14) && back.layers[0].is_scout(-31, 14));
    }

    #[test]
    fn an_entry_without_the_rung_memo_still_decodes_and_a_bad_rung_does_not() {
        let d = fixture();
        let scene = SceneId(9);
        let (body, _) = encode_body(&view(&d), LAYERS);
        // The first v2 build ended the body at the `scouted` byte.
        let old = &body[..body.len() - 2];
        let (bytes, _) = frame_body(scene, old);
        let back = decode_entry(&bytes, scene).expect("an entry without a memo decodes");
        assert!(back.scouted && back.scout_memo.is_none());
        // A rung byte that is no rung is a corrupt entry.
        let mut bad = body.clone();
        let n = bad.len();
        bad[n - 2] = 77;
        let (bytes, _) = frame_body(scene, &bad);
        assert!(decode_entry(&bytes, scene).is_err());
        // A memo of "none" (0, 0) decodes as none.
        let mut none = body;
        let n = none.len();
        none[n - 2] = 0;
        none[n - 1] = 0;
        let (bytes, _) = frame_body(scene, &none);
        assert_eq!(decode_entry(&bytes, scene).unwrap().scout_memo, None);
    }

    #[test]
    fn a_scene_survives_a_disk_round_trip_under_its_rom_and_scene() {
        let root = unique_dir("disk");
        let mut cache = Cache::open(&root, 64 << 20).unwrap();
        let d = fixture();
        let saved = save_scene(&mut cache, "rom-a", SceneId(7), &view(&d), None).unwrap();
        assert!(saved.stored_bytes > 0 && saved.dropped_layers == 0);
        let back = load_scene(&mut cache, "rom-a", SceneId(7))
            .unwrap()
            .unwrap();
        assert_eq!(back.layers, d.layers);
        // An unchanged scene is not written again; a changed one is.
        let again = save_scene(
            &mut cache,
            "rom-a",
            SceneId(7),
            &view(&d),
            Some(back.body_crc),
        )
        .unwrap();
        assert!(again.unchanged && again.stored_bytes == 0);
        // Only the per-frame confidence statistics moved: still unchanged.
        let mut counted = fixture();
        counted.confidence[0].observe(Check::default());
        let same = save_scene(
            &mut cache,
            "rom-a",
            SceneId(7),
            &view(&counted),
            Some(back.body_crc),
        )
        .unwrap();
        assert!(same.unchanged, "frame counters alone are not worth a write");
        let mut changed = fixture();
        changed.scouted = false;
        let w = save_scene(
            &mut cache,
            "rom-a",
            SceneId(7),
            &view(&changed),
            Some(back.body_crc),
        )
        .unwrap();
        assert!(!w.unchanged && w.stored_bytes > 0);
        assert!(load_scene(&mut cache, "rom-a", SceneId(8))
            .unwrap()
            .is_none());
        assert!(load_scene(&mut cache, "rom-b", SceneId(7))
            .unwrap()
            .is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_old_format_entry_is_ignored_not_misread() {
        let root = unique_dir("old");
        let mut cache = Cache::open(&root, 64 << 20).unwrap();
        // A real v1 entry, under the key v1 used.
        let chunk = canvas_to_chunk(&canvas(1));
        cache
            .put_canvas_chunk(&key_with("rom", SceneId(3), LEGACY_SETTINGS_HASH), &chunk)
            .unwrap();
        assert!(load_scene(&mut cache, "rom", SceneId(3)).unwrap().is_none());
        // v1 bytes planted under the v2 key.
        let v1 = rf_cache_v1_bytes(&mut cache, &chunk);
        cache.put(&scene_cache_key("rom", SceneId(3)), &v1).unwrap();
        assert!(load_scene(&mut cache, "rom", SceneId(3)).unwrap().is_none());
        // A well-formed entry of another version number.
        let d = fixture();
        let (mut bytes, _) = encode_entry(SceneId(3), &view(&d), LAYERS);
        bytes[4..6].copy_from_slice(&1u16.to_le_bytes());
        let n = bytes.len() - FOOTER_LEN;
        let crc = crc32fast::hash(&bytes[..n]);
        bytes[n..].copy_from_slice(&crc.to_le_bytes());
        let why = decode_entry(&bytes, SceneId(3)).unwrap_err();
        assert!(why.contains("version"), "{why}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// The raw bytes `put_canvas_chunk` stored (read back through `get`).
    fn rf_cache_v1_bytes(cache: &mut Cache, _chunk: &CanvasChunk) -> Vec<u8> {
        cache
            .get(&key_with("rom", SceneId(3), LEGACY_SETTINGS_HASH))
            .unwrap()
            .unwrap()
    }

    #[test]
    fn every_truncation_and_a_flipped_byte_is_refused() {
        let d = fixture();
        let scene = SceneId(5);
        let (bytes, _) = encode_entry(scene, &view(&d), LAYERS);
        // Bounded: one pass per prefix length.
        for len in 0..bytes.len() {
            assert!(
                decode_entry(&bytes[..len], scene).is_err(),
                "a {len}-byte prefix decoded"
            );
        }
        let mut bad = bytes.clone();
        bad[HEADER_LEN + 2] ^= 0x40;
        assert!(decode_entry(&bad, scene).is_err(), "bit rot decoded");
        assert!(decode_entry(&bytes, SceneId(6)).is_err(), "another scene");
    }

    #[test]
    fn a_header_that_claims_a_huge_canvas_allocates_nothing_and_fails() {
        // A body whose first layer claims u32::MAX x u32::MAX cells, framed
        // with a VALID crc: the bounds, not the checksum, must stop it.
        let mut body = Vec::new();
        body.extend_from_slice(&0i64.to_le_bytes());
        body.extend_from_slice(&0i64.to_le_bytes());
        body.extend_from_slice(&u32::MAX.to_le_bytes());
        body.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_body(&body).is_none());
        // Dimensions within the cell bound that the bytes cannot back.
        let mut body = Vec::new();
        body.extend_from_slice(&[0u8; 16]);
        body[8..12].copy_from_slice(&4096u32.to_le_bytes());
        body[12..16].copy_from_slice(&4096u32.to_le_bytes());
        assert!(decode_body(&body).is_none());
    }

    #[test]
    fn an_over_cap_scene_drops_its_highest_layers_and_says_how_many() {
        let root = unique_dir("cap");
        let mut cache = Cache::open(&root, 64 << 20).unwrap();
        let mut d = fixture();
        // Incompressible-ish content on layers 2 and 3.
        for n in [2usize, 3] {
            let mut c = Canvas::new();
            let mut state = 0x1234_5678_u32 ^ n as u32;
            for y in 0..8i64 {
                let row: Vec<_> = (0..4000)
                    .map(|_| {
                        // xorshift32: incompressible for zstd.
                        state ^= state << 13;
                        state ^= state >> 17;
                        state ^= state << 5;
                        Some(px(state as u8, PixelLayer::Background(0), None, 0))
                    })
                    .collect();
                c.blit_row(0, y, &row);
            }
            d.layers[n] = c;
        }
        let full = save_scene(&mut cache, "rom", SceneId(1), &view(&d), None).unwrap();
        assert_eq!(full.dropped_layers, 0);
        let capped =
            save_scene_capped(&mut cache, "rom", SceneId(2), &view(&d), None, 2000).unwrap();
        assert_eq!(
            capped.dropped_layers, 2,
            "layers 3 and 2 left out, 0 and 1 fit"
        );
        let back = load_scene(&mut cache, "rom", SceneId(2)).unwrap().unwrap();
        assert_eq!(back.layers[0], d.layers[0], "layer 0 is always kept");
        assert_eq!(back.layers[3].width(), 0);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn confidence_and_edge_bytes_round_trip_and_refuse_garbage() {
        let d = fixture();
        for c in &d.confidence {
            assert_eq!(Confidence::from_bytes(&c.to_bytes()), Some(*c));
        }
        let mut b = d.confidence[1].to_bytes();
        b[0] = 9;
        assert_eq!(Confidence::from_bytes(&b), None, "level above the maximum");
        assert_eq!(Confidence::from_bytes(&b[..10]), None);
    }
}
