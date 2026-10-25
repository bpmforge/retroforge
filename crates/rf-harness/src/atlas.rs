//! Glue that pairs the generic atlas builder (`rf-rebuild`) with the NES
//! core's `Isa` and the asset cache (ticket W36-02). `rf-harness` is one of
//! the two crates allowed to see both a console core and the cache, which is
//! why this lives here rather than in `rf-rebuild`.
//!
//! SNES atlases (W36-08) go through the same cache: same key shape, same
//! format (v3 carries the 65816 decode mode on every instruction).
//!
//! The cache entry is keyed `(rom_sha256, "code-atlas", "rf-rebuild",
//! "code-atlas-v3")`: one atlas per normalised ROM hash, next to the
//! scout's canvas chunks, with the format version in the name so another
//! version is a clean miss.

use rf_cache::{Cache, CacheError, CacheKey};
use rf_core_api::coverage::CoverageLog;
use rf_nes::{NesLoadError, NesRom};
use rf_rebuild::{Atlas, CACHE_ASSET, CACHE_PRODUCER, CACHE_SETTINGS};

/// Build the atlas of an iNES image from a recording of it.
///
/// # Errors
/// Whatever [`NesRom::from_ines_bytes`] rejects.
pub fn build_nes_atlas(ines: &[u8], log: &CoverageLog) -> Result<Atlas, NesLoadError> {
    let rom = NesRom::from_ines_bytes(ines)?;
    Ok(rf_nes::rebuild::build_atlas(rom.prg_rom(), log))
}

/// Build the atlas of a SNES cartridge image (any copier header included) from
/// a recording of it (ticket W36-08). Cached under the same key shape as the
/// NES atlas: the ROM hash differs per console, so the two never collide.
///
/// # Errors
/// Whatever `rf-cart` rejects.
pub fn build_snes_atlas(raw: &[u8], log: &CoverageLog) -> Result<Atlas, rf_cart::CartError> {
    rf_snes::rebuild::build_atlas_from_image(raw, log)
}

/// The cache key of the atlas of the ROM with this normalised SHA-256 (hex).
#[must_use]
pub fn atlas_cache_key(rom_sha256: &str) -> CacheKey {
    CacheKey {
        rom_sha256: rom_sha256.to_string(),
        asset_hash: CACHE_ASSET.to_string(),
        producer: CACHE_PRODUCER.to_string(),
        settings_hash: CACHE_SETTINGS.to_string(),
    }
}

/// The cached atlas for this ROM, `None` on a miss or an entry that no
/// longer parses (another format version, or damage): both mean "rebuild".
pub fn load_cached_atlas(cache: &mut Cache, rom_sha256: &str) -> Option<Atlas> {
    let bytes = cache.get(&atlas_cache_key(rom_sha256)).ok()??;
    Atlas::from_bytes(&bytes).ok()
}

/// Store `atlas` under this ROM's key.
///
/// # Errors
/// The cache's own I/O errors.
pub fn store_atlas(cache: &mut Cache, rom_sha256: &str, atlas: &Atlas) -> Result<(), CacheError> {
    cache.put(&atlas_cache_key(rom_sha256), &atlas.to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_core_api::coverage::{WindowMap, OPCODE};

    fn tiny_ines() -> Vec<u8> {
        let mut d = vec![0u8; 16];
        d[0..4].copy_from_slice(b"NES\x1a");
        d[4] = 1;
        let mut prg = vec![0xEAu8; 0x4000]; // NOP sled
        prg[0x10] = 0x60; // RTS
        d.extend(prg);
        d
    }

    #[test]
    fn nes_atlas_round_trips_through_the_cache() {
        let ines = tiny_ines();
        let mut log = CoverageLog::new(0x4000);
        let mut m = WindowMap::EMPTY;
        for (p, s) in m.0.iter_mut().enumerate() {
            *s = (p as u32 % 4) * 0x1000;
        }
        log.note_windows(m);
        log.mark_prg(0, OPCODE);
        let atlas = build_nes_atlas(&ines, &log).expect("builds");
        assert!(atlas.stats.observed_insns >= 1);

        let dir = std::env::temp_dir().join(format!("rf-atlas-glue-{}", std::process::id()));
        let sha = "ab".repeat(32);
        {
            let mut cache = Cache::open(&dir, 1 << 20).expect("open");
            assert!(load_cached_atlas(&mut cache, &sha).is_none(), "cold miss");
            store_atlas(&mut cache, &sha, &atlas).expect("store");
        }
        let mut cache = Cache::open(&dir, 1 << 20).expect("reopen");
        assert_eq!(load_cached_atlas(&mut cache, &sha), Some(atlas));
        assert!(load_cached_atlas(&mut cache, &"cd".repeat(32)).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snes_atlas_builds_with_and_without_a_copier_header_and_round_trips() {
        let mut rom = vec![0u8; 0x1_0000];
        let base = 0x7FC0;
        rom[base + 0x15] = 0x20; // LoROM
        rom[base + 0x17] = 6;
        let sum: u16 = 0xBEEF;
        rom[base + 0x1C..base + 0x1E].copy_from_slice(&(sum ^ 0xFFFF).to_le_bytes());
        rom[base + 0x1E..base + 0x20].copy_from_slice(&sum.to_le_bytes());
        rom[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes());
        rom[..4].copy_from_slice(&[0xA9, 0x01, 0xEA, 0x60]);
        let mut log = CoverageLog::with_volatile_len(rom.len(), 128 * 1024);
        log.mark_prg(
            0,
            rf_core_api::coverage::OPCODE | rf_core_api::coverage::mode_bit(0),
        );
        log.mark_prg(1, rf_core_api::coverage::OPERAND);
        let atlas = build_snes_atlas(&rom, &log).expect("builds");
        assert_eq!(atlas.mode_count, 5);
        assert!(atlas.insns().any(|i| i.rom == 2 && i.mode == 0));

        // The same cartridge behind a 512-byte copier header: same offsets.
        let mut headered = vec![0u8; 512];
        headered.extend_from_slice(&rom);
        assert_eq!(build_snes_atlas(&headered, &log).expect("builds"), atlas);

        let dir = std::env::temp_dir().join(format!("rf-atlas-snes-{}", std::process::id()));
        let sha = "ef".repeat(32);
        {
            let mut cache = Cache::open(&dir, 1 << 20).expect("open");
            store_atlas(&mut cache, &sha, &atlas).expect("store");
        }
        let mut cache = Cache::open(&dir, 1 << 20).expect("reopen");
        assert_eq!(load_cached_atlas(&mut cache, &sha), Some(atlas));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
