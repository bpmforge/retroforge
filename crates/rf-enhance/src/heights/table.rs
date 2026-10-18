//! What the learner knows about each tile, and how it is kept (ticket W34-03;
//! `docs/design/HD2D_PLAN.md` section 2.2).
//!
//! A [`TileHeightTable`] is per ROM: every fact is keyed by [`TileKey`] (what
//! is IN the tile, not where it sits), so a tile learned once is known in
//! every scene and survives VRAM reloads. It is a `BTreeMap`, never a hash
//! map, so iteration, equality and the cache bytes are a pure function of the
//! observations (determinism invariant).
//!
//! ## Cache entry
//!
//! One `rf-cache` entry per ROM, key `(rom sha256, "table", producer
//! "rf-enhance-heights", settings "tile-heights-v1")`:
//!
//! ```text
//! "RFTH" u16 version | u32 raw_len | zstd(body) | u32 crc32
//! body = u32 count, count x { key(8+2+1+1), evidence, learned, u16 agree, u16 contra }
//! ```
//!
//! Same discipline as `persistence.rs`: CRC32 over everything before it, every
//! count checked against a fixed bound AND the bytes that remain before
//! anything is allocated, zstd level 3. A corrupt, truncated or foreign entry
//! is a miss (the table is regenerable by play), never an error.

use std::collections::BTreeMap;
use std::io::Read;

use rf_cache::{Cache, CacheError, CacheKey};

use super::tile_grid::TileKey;
use crate::persistence::Reader;

/// Most keys one table keeps; a new key past it is not learned (bounded).
pub const MAX_KEYS: usize = 32_768;
/// Cache-key version tag: another tag is another key, so a changed format is
/// a clean miss.
pub const SETTINGS_HASH: &str = "tile-heights-v1";
/// Format version inside the file.
pub const VERSION: u16 = 1;

const MAGIC: &[u8; 4] = b"RFTH";
const ZSTD_LEVEL: i32 = 3;
const HEADER_LEN: usize = 4 + 2 + 4;
const FOOTER_LEN: usize = 4;
const MAX_RAW_BYTES: usize = 16 * 1024 * 1024;
/// Encoded bytes of one table row.
const ROW_LEN: usize = 12 + EVIDENCE_LEN + 4 + 4 + 4;
const EVIDENCE_LEN: usize = 4 * 8 + 2 + 4 * 3;

/// What a tile is, as far as play has shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Class {
    /// Not enough evidence: rendered flat.
    #[default]
    Unknown,
    /// Walkable, nothing to extrude (height 0).
    Floor,
    /// Blocks the player from the side, or is solid mass.
    Wall,
    /// Hides the player: taller than he is.
    Tall,
    /// Something to stand on (side view).
    Platform,
    /// A slope: eighths of a tile of rise per tile to the right (negative
    /// falls to the right).
    Ramp(i8),
    /// A layer that scrolls slower than the playfield: set back, never
    /// extruded.
    Backdrop,
}

impl Class {
    /// A tile the player cannot walk through (what the oracle scores).
    #[must_use]
    pub fn is_solid(self) -> bool {
        matches!(
            self,
            Class::Wall | Class::Tall | Class::Platform | Class::Ramp(_)
        )
    }

    fn code(self) -> (u8, i8) {
        match self {
            Class::Unknown => (0, 0),
            Class::Floor => (1, 0),
            Class::Wall => (2, 0),
            Class::Tall => (3, 0),
            Class::Platform => (4, 0),
            Class::Ramp(s) => (5, s),
            Class::Backdrop => (6, 0),
        }
    }

    fn from_code(c: u8, arg: i8) -> Option<Self> {
        Some(match c {
            0 => Class::Unknown,
            1 => Class::Floor,
            2 => Class::Wall,
            3 => Class::Tall,
            4 => Class::Platform,
            5 => Class::Ramp(arg),
            6 => Class::Backdrop,
            _ => return None,
        })
    }
}

/// The verdict on one tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Learned {
    pub class: Class,
    /// Extrusion in eighths of the tile's edge.
    pub height_eighths: u8,
    /// 0..=3 on the one confidence ladder (`canvas::registration`); the
    /// renderer keeps a tile flat below [`Learned::DRAW_LEVEL`].
    pub confidence: u8,
}

impl Learned {
    /// The lowest confidence at which a tile is drawn with its class.
    pub const DRAW_LEVEL: u8 = 2;

    /// The class to draw: [`Class::Unknown`] and height 0 while unsure.
    #[must_use]
    pub fn drawn(self) -> (Class, u8) {
        if self.confidence >= Self::DRAW_LEVEL {
            (self.class, self.height_eighths)
        } else {
            (Class::Unknown, 0)
        }
    }
}

/// Counts of what was seen, per key. Every field is a vote or a sighting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Evidence {
    /// Cell-frames this key was on screen (HUD and partial cells excluded).
    pub seen: u32,
    /// S1: the player stood on it (a landing counts more than a frame of
    /// standing).
    pub floor_top: u32,
    /// S2: input held toward it, the player did not move; left, right, up,
    /// down.
    pub blocked: [u32; 4],
    /// The player's body was over it: walkable air.
    pub overlapped: u32,
    /// S3: the player was drawn behind it.
    pub hides_player: u32,
    /// S6: signed votes, positive rising to the right.
    pub slope: i16,
    /// S6: votes cast, and the sum of their rise in eighths.
    pub slope_n: u32,
    pub slope_eighths: u32,
    /// S4: sightings next to a solid tile with nothing ever walking over it.
    pub mass: u32,
    /// S7: sightings inside a block of three by three or more of itself.
    /// A prior only: it lowers the bar for `mass`, never classifies alone.
    pub prior: u32,
}

/// One table row: the evidence, the verdict, and the ladder's counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Entry {
    pub evidence: Evidence,
    pub learned: Learned,
    /// Observations agreeing with `learned` since the last step up.
    pub agree: u16,
    /// Contradicting observations since the last step down.
    pub contra: u16,
}

/// Per-ROM knowledge, by tile identity.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TileHeightTable {
    pub(crate) map: BTreeMap<TileKey, Entry>,
}

impl TileHeightTable {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn get(&self, key: &TileKey) -> Option<&Entry> {
        self.map.get(key)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Rows in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&TileKey, &Entry)> {
        self.map.iter()
    }

    /// The entry for `key`, created if there is room.
    pub(crate) fn entry_mut(&mut self, key: TileKey) -> Option<&mut Entry> {
        if !self.map.contains_key(&key) && self.map.len() >= MAX_KEYS {
            return None;
        }
        Some(self.map.entry(key).or_default())
    }

    /// Tiles the renderer would draw as solid (wall, tall, platform, ramp).
    #[must_use]
    pub fn drawn_count_solid(&self) -> usize {
        self.map
            .values()
            .filter(|e| e.learned.drawn().0.is_solid())
            .count()
    }

    /// Tiles the renderer would draw non-flat.
    #[must_use]
    pub fn drawn_count(&self) -> usize {
        self.map
            .values()
            .filter(|e| e.learned.drawn().0 != Class::Unknown)
            .count()
    }

    /// Serialise (deterministic: key order).
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut body = Vec::with_capacity(4 + self.map.len() * ROW_LEN);
        body.extend_from_slice(&(self.map.len() as u32).to_le_bytes());
        for (k, e) in &self.map {
            body.extend_from_slice(&k.pattern.to_le_bytes());
            body.extend_from_slice(&k.palette.to_le_bytes());
            body.push(k.layer);
            body.push(k.cell_px);
            let ev = &e.evidence;
            for v in [
                ev.seen,
                ev.floor_top,
                ev.blocked[0],
                ev.blocked[1],
                ev.blocked[2],
                ev.blocked[3],
                ev.overlapped,
                ev.hides_player,
            ] {
                body.extend_from_slice(&v.to_le_bytes());
            }
            body.extend_from_slice(&ev.slope.to_le_bytes());
            for v in [ev.slope_n, ev.slope_eighths, ev.mass] {
                body.extend_from_slice(&v.to_le_bytes());
            }
            // `prior` rides in the spare tail so the row stays fixed-size.
            let (code, arg) = e.learned.class.code();
            body.extend_from_slice(&[
                code,
                arg as u8,
                e.learned.height_eighths,
                e.learned.confidence,
            ]);
            body.extend_from_slice(&e.agree.to_le_bytes());
            body.extend_from_slice(&e.contra.to_le_bytes());
            body.extend_from_slice(&ev.prior.to_le_bytes());
        }
        let mut out = Vec::with_capacity(HEADER_LEN + body.len() / 3 + FOOTER_LEN);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&zstd::bulk::compress(&body, ZSTD_LEVEL).unwrap_or_default());
        let crc = crc32fast::hash(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        out
    }

    /// Parse [`Self::to_bytes`].
    ///
    /// # Errors
    /// A message naming what was wrong; the caller treats it as a miss.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
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
        Self::parse_body(&body).ok_or_else(|| "body does not parse".to_string())
    }

    fn parse_body(body: &[u8]) -> Option<Self> {
        let mut r = Reader::new(body);
        let count = r.u32()? as usize;
        if count > MAX_KEYS || count.checked_mul(ROW_LEN)? != r.remaining() {
            return None;
        }
        let mut map = BTreeMap::new();
        for _ in 0..count {
            let key = TileKey {
                pattern: r.u64()?,
                palette: r.u16()?,
                layer: r.u8()?,
                cell_px: r.u8()?,
            };
            let mut ev = Evidence {
                seen: r.u32()?,
                floor_top: r.u32()?,
                blocked: [r.u32()?, r.u32()?, r.u32()?, r.u32()?],
                overlapped: r.u32()?,
                hides_player: r.u32()?,
                ..Evidence::default()
            };
            ev.slope = i16::from_le_bytes(r.take(2)?.try_into().ok()?);
            ev.slope_n = r.u32()?;
            ev.slope_eighths = r.u32()?;
            ev.mass = r.u32()?;
            let (code, arg, height, confidence) = (r.u8()?, r.u8()? as i8, r.u8()?, r.u8()?);
            let class = Class::from_code(code, arg)?;
            if confidence > crate::canvas::MAX_LEVEL {
                return None;
            }
            let (agree, contra) = (r.u16()?, r.u16()?);
            ev.prior = r.u32()?;
            // Strictly increasing keys: a duplicate or unsorted row is corrupt.
            if map
                .last_key_value()
                .is_some_and(|(last, _): (&TileKey, &Entry)| *last >= key)
            {
                return None;
            }
            map.insert(
                key,
                Entry {
                    evidence: ev,
                    learned: Learned {
                        class,
                        height_eighths: height,
                        confidence,
                    },
                    agree,
                    contra,
                },
            );
        }
        Some(Self { map })
    }
}

/// The cache key of a ROM's table.
#[must_use]
pub fn table_cache_key(rom_sha256: &str) -> CacheKey {
    CacheKey {
        rom_sha256: rom_sha256.to_string(),
        asset_hash: "table".to_string(),
        producer: "rf-enhance-heights".to_string(),
        settings_hash: SETTINGS_HASH.to_string(),
    }
}

/// Persist a ROM's table.
///
/// # Errors
/// Propagates [`CacheError`] from `Cache::put`.
pub fn save_table(
    cache: &mut Cache,
    rom_sha256: &str,
    table: &TileHeightTable,
) -> Result<(), CacheError> {
    cache.put(&table_cache_key(rom_sha256), &table.to_bytes())
}

/// Restore a ROM's table. A missing, foreign, truncated or corrupt entry is
/// `Ok(None)` with one log line: the table is regenerable by play.
///
/// # Errors
/// Propagates [`CacheError`] from `Cache::get` (an I/O or containment fault).
pub fn load_table(
    cache: &mut Cache,
    rom_sha256: &str,
) -> Result<Option<TileHeightTable>, CacheError> {
    let Some(bytes) = cache.get(&table_cache_key(rom_sha256))? else {
        return Ok(None);
    };
    match TileHeightTable::from_bytes(&bytes) {
        Ok(t) => Ok(Some(t)),
        Err(why) => {
            eprintln!("rf-enhance: tile-heights cache: ignoring entry: {why}");
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

    fn key(n: u64) -> TileKey {
        TileKey {
            pattern: n,
            palette: n as u16,
            layer: (n % 4) as u8,
            cell_px: 16,
        }
    }

    fn sample() -> TileHeightTable {
        let mut t = TileHeightTable::new();
        // Inserted out of order on purpose: the bytes must not care.
        for n in [9u64, 3, 7, 1] {
            let e = t.entry_mut(key(n)).unwrap();
            e.evidence = Evidence {
                seen: 100 + n as u32,
                floor_top: n as u32,
                blocked: [1, 2, 3, 4],
                overlapped: 5,
                hides_player: 6,
                slope: -3,
                slope_n: 4,
                slope_eighths: 9,
                mass: 11,
                prior: 12,
            };
            e.learned = Learned {
                class: if n == 7 { Class::Ramp(-5) } else { Class::Wall },
                height_eighths: 8,
                confidence: 3,
            };
            e.agree = 4;
            e.contra = 1;
        }
        t
    }

    #[test]
    fn a_table_round_trips_through_the_cache_byte_for_byte() {
        let (mut cache, root) = cache("table-rt");
        let t = sample();
        save_table(&mut cache, "romsha", &t).unwrap();
        let back = load_table(&mut cache, "romsha").unwrap().expect("a hit");
        assert_eq!(back, t);
        assert_eq!(back.to_bytes(), t.to_bytes());
        assert!(load_table(&mut cache, "another rom").unwrap().is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_bytes_do_not_depend_on_insertion_order() {
        let mut b = TileHeightTable::new();
        for n in [1u64, 7, 3, 9] {
            *b.entry_mut(key(n)).unwrap() = *sample().get(&key(n)).unwrap();
        }
        assert_eq!(b.to_bytes(), sample().to_bytes());
    }

    #[test]
    fn corrupt_truncated_or_foreign_bytes_are_misses() {
        let good = sample().to_bytes();
        for cut in [0, 3, 9, good.len() / 2, good.len() - 1] {
            assert!(
                TileHeightTable::from_bytes(&good[..cut]).is_err(),
                "cut {cut}"
            );
        }
        for at in [0, 5, good.len() / 2, good.len() - 2] {
            let mut bad = good.clone();
            bad[at] ^= 0x01;
            assert!(TileHeightTable::from_bytes(&bad).is_err(), "flip {at}");
        }
        // A corrupt entry on disk is Ok(None), not an error.
        let (mut cache, root) = cache("table-bad");
        cache
            .put(&table_cache_key("romsha"), b"not a table at all")
            .unwrap();
        assert!(load_table(&mut cache, "romsha").unwrap().is_none());
        // The retired/other version tag is another key, a clean miss.
        let mut other = table_cache_key("romsha");
        other.settings_hash = "tile-heights-v0".to_string();
        assert!(cache.get(&other).unwrap().is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_count_that_lies_is_refused_before_anything_is_allocated() {
        // Valid frame, body claims 2^31 rows but carries none.
        let body = (u32::MAX / 2).to_le_bytes().to_vec();
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&zstd::bulk::compress(&body, 3).unwrap());
        let crc = crc32fast::hash(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        assert!(TileHeightTable::from_bytes(&out).is_err());
    }

    #[test]
    fn the_table_is_bounded() {
        let mut t = TileHeightTable::new();
        for n in 0..(MAX_KEYS as u64 + 50) {
            let _ = t.entry_mut(key(n));
        }
        assert_eq!(t.len(), MAX_KEYS);
        assert!(t.entry_mut(key(0)).is_some(), "known keys still update");
    }
}
