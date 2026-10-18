//! The learned heights of one scene, laid on the scene's world (ticket
//! W34-03; `docs/design/HD2D_PLAN.md` section 2.2).
//!
//! A [`HeightCanvas`] is what the learner knew about each visited cell of a
//! scene the last time it looked: row-major, `None`-like ([`Class::Unknown`]
//! at confidence 0) where nothing was seen, so unvisited ground stays flat
//! and honest (NON_GOALS #11). It is the persisted form of "what the diorama
//! may extrude here" and is stored in its own `rf-cache` entry beside the
//! scene's picture chunk, keyed `(rom sha256, scene id)` under the tag
//! `heights-canvas-v1`. (The plan sketches a `heights` block inside the scene
//! chunk; that chunk is written wholesale and de-duplicated by CRC by the
//! stitcher, and filling it needs the shell, which is W34-04's, so the block
//! is a sibling entry here and the shell may fold it in later.)

use std::io::Read;

use rf_cache::{Cache, CacheError, CacheKey};

use super::table::{Class, Learned};
use crate::persistence::Reader;
use crate::scene_identity::SceneId;

/// Cache-key version tag.
pub const SETTINGS_HASH: &str = "heights-canvas-v1";
/// Format version inside the file.
pub const VERSION: u16 = 1;
/// Most cells one canvas may hold (a long SMW level is ~5 M 8 px cells at
/// most; 16 M is the bound a decoder will allocate for).
pub const MAX_CELLS: u64 = 16 * 1024 * 1024;

const MAGIC: &[u8; 4] = b"RFHC";
const ZSTD_LEVEL: i32 = 3;
const HEADER_LEN: usize = 4 + 2 + 8 + 4;
const FOOTER_LEN: usize = 4;
const CELL_LEN: usize = 4;

/// Learned cells over a scene's world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeightCanvas {
    /// World position of cell (0, 0)'s top-left corner.
    pub origin: (i64, i64),
    pub cols: u32,
    pub rows: u32,
    pub cell_px: u8,
    cells: Vec<Learned>,
}

impl HeightCanvas {
    /// An all-unknown canvas, or `None` when it would exceed [`MAX_CELLS`].
    #[must_use]
    pub fn new(origin: (i64, i64), cols: u32, rows: u32, cell_px: u8) -> Option<Self> {
        let n = u64::from(cols).checked_mul(u64::from(rows))?;
        (n <= MAX_CELLS).then(|| Self {
            origin,
            cols,
            rows,
            cell_px,
            cells: vec![Learned::default(); n as usize],
        })
    }

    #[must_use]
    pub fn get(&self, col: u32, row: u32) -> Option<Learned> {
        (col < self.cols && row < self.rows)
            .then(|| self.cells[row as usize * self.cols as usize + col as usize])
    }

    /// Write one cell; a cell off the canvas is ignored. A cell is only
    /// replaced by one at least as sure (a visit never forgets).
    pub fn set(&mut self, col: u32, row: u32, l: Learned) {
        if col < self.cols && row < self.rows {
            let at = row as usize * self.cols as usize + col as usize;
            if l.confidence >= self.cells[at].confidence {
                self.cells[at] = l;
            }
        }
    }

    /// Cells the renderer would draw non-flat.
    #[must_use]
    pub fn drawn_count(&self) -> usize {
        self.cells
            .iter()
            .filter(|c| c.drawn().0 != Class::Unknown)
            .count()
    }

    #[must_use]
    pub fn to_bytes(&self, scene: SceneId) -> Vec<u8> {
        let mut body = Vec::with_capacity(8 + 8 + 4 + 4 + 1 + self.cells.len() * CELL_LEN);
        body.extend_from_slice(&self.origin.0.to_le_bytes());
        body.extend_from_slice(&self.origin.1.to_le_bytes());
        body.extend_from_slice(&self.cols.to_le_bytes());
        body.extend_from_slice(&self.rows.to_le_bytes());
        body.push(self.cell_px);
        for c in &self.cells {
            let (code, arg) = match c.class {
                Class::Unknown => (0u8, 0i8),
                Class::Floor => (1, 0),
                Class::Wall => (2, 0),
                Class::Tall => (3, 0),
                Class::Platform => (4, 0),
                Class::Ramp(s) => (5, s),
                Class::Backdrop => (6, 0),
            };
            body.extend_from_slice(&[code, arg as u8, c.height_eighths, c.confidence]);
        }
        let mut out = Vec::with_capacity(HEADER_LEN + body.len() / 4 + FOOTER_LEN);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&scene.0.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&zstd::bulk::compress(&body, ZSTD_LEVEL).unwrap_or_default());
        let crc = crc32fast::hash(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        out
    }

    /// # Errors
    /// A message naming what was wrong; the caller treats it as a miss.
    pub fn from_bytes(bytes: &[u8], scene: SceneId) -> Result<Self, String> {
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
        if r.u16() != Some(VERSION) {
            return Err("unknown format version".to_string());
        }
        if r.u64() != Some(scene.0) {
            return Err("entry is for another scene".to_string());
        }
        let raw_len = r.u32().ok_or("short header")? as usize;
        let max_raw = 25 + MAX_CELLS as usize * CELL_LEN;
        if raw_len > max_raw {
            return Err(format!("claims {raw_len} body bytes (cap {max_raw})"));
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
        Self::parse(&body).ok_or_else(|| "body does not parse".to_string())
    }

    fn parse(body: &[u8]) -> Option<Self> {
        let mut r = Reader::new(body);
        let origin = (r.i64()?, r.i64()?);
        let (cols, rows, cell_px) = (r.u32()?, r.u32()?, r.u8()?);
        let n = u64::from(cols).checked_mul(u64::from(rows))?;
        if n > MAX_CELLS || n as usize * CELL_LEN != r.remaining() {
            return None;
        }
        let mut out = Self::new(origin, cols, rows, cell_px)?;
        for cell in &mut out.cells {
            let (code, arg, height, confidence) = (r.u8()?, r.u8()? as i8, r.u8()?, r.u8()?);
            let class = match code {
                0 => Class::Unknown,
                1 => Class::Floor,
                2 => Class::Wall,
                3 => Class::Tall,
                4 => Class::Platform,
                5 => Class::Ramp(arg),
                6 => Class::Backdrop,
                _ => return None,
            };
            if confidence > crate::canvas::MAX_LEVEL {
                return None;
            }
            *cell = Learned {
                class,
                height_eighths: height,
                confidence,
            };
        }
        Some(out)
    }
}

/// The cache key of a scene's height canvas.
#[must_use]
pub fn canvas_cache_key(rom_sha256: &str, scene: SceneId) -> CacheKey {
    CacheKey {
        rom_sha256: rom_sha256.to_string(),
        asset_hash: scene.to_hex(),
        producer: "rf-enhance-heights".to_string(),
        settings_hash: SETTINGS_HASH.to_string(),
    }
}

/// Persist a scene's height canvas.
///
/// # Errors
/// Propagates [`CacheError`] from `Cache::put`.
pub fn save_canvas(
    cache: &mut Cache,
    rom_sha256: &str,
    scene: SceneId,
    canvas: &HeightCanvas,
) -> Result<(), CacheError> {
    cache.put(
        &canvas_cache_key(rom_sha256, scene),
        &canvas.to_bytes(scene),
    )
}

/// Restore a scene's height canvas; anything unreadable is `Ok(None)`.
///
/// # Errors
/// Propagates [`CacheError`] from `Cache::get`.
pub fn load_canvas(
    cache: &mut Cache,
    rom_sha256: &str,
    scene: SceneId,
) -> Result<Option<HeightCanvas>, CacheError> {
    let Some(bytes) = cache.get(&canvas_cache_key(rom_sha256, scene))? else {
        return Ok(None);
    };
    match HeightCanvas::from_bytes(&bytes, scene) {
        Ok(c) => Ok(Some(c)),
        Err(why) => {
            eprintln!(
                "rf-enhance: height canvas cache: ignoring {}: {why}",
                scene.to_hex()
            );
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(label: &str) -> (Cache, std::path::PathBuf) {
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "rf-enhance-heights-{label}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        (Cache::open(&root, 64 << 20).unwrap(), root)
    }

    fn sample() -> HeightCanvas {
        let mut c = HeightCanvas::new((-32, 16), 5, 3, 8).unwrap();
        c.set(
            1,
            1,
            Learned {
                class: Class::Wall,
                height_eighths: 8,
                confidence: 3,
            },
        );
        c.set(
            4,
            2,
            Learned {
                class: Class::Ramp(-4),
                height_eighths: 8,
                confidence: 2,
            },
        );
        c
    }

    #[test]
    fn a_canvas_round_trips_through_the_cache() {
        let (mut cache, root) = cache("canvas-rt");
        let scene = SceneId(0xABCD);
        let c = sample();
        save_canvas(&mut cache, "rom", scene, &c).unwrap();
        assert_eq!(load_canvas(&mut cache, "rom", scene).unwrap(), Some(c));
        assert!(load_canvas(&mut cache, "rom", SceneId(1))
            .unwrap()
            .is_none());
        assert!(load_canvas(&mut cache, "other", scene).unwrap().is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_damaged_canvas_is_a_miss() {
        let scene = SceneId(7);
        let good = sample().to_bytes(scene);
        for cut in [0, 5, good.len() / 2, good.len() - 1] {
            assert!(HeightCanvas::from_bytes(&good[..cut], scene).is_err());
        }
        let mut flipped = good.clone();
        flipped[good.len() / 2] ^= 0x40;
        assert!(HeightCanvas::from_bytes(&flipped, scene).is_err());
        assert!(
            HeightCanvas::from_bytes(&good, SceneId(8)).is_err(),
            "other scene"
        );
    }

    #[test]
    fn a_visit_never_forgets_and_off_canvas_is_ignored() {
        let mut c = sample();
        c.set(
            1,
            1,
            Learned {
                class: Class::Floor,
                height_eighths: 0,
                confidence: 1,
            },
        );
        assert_eq!(c.get(1, 1).unwrap().class, Class::Wall);
        c.set(9, 9, Learned::default());
        assert!(c.get(9, 9).is_none());
        assert!(HeightCanvas::new((0, 0), u32::MAX, u32::MAX, 8).is_none());
    }
}
