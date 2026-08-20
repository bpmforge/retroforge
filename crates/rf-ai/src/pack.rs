//! Replacement packs and their cache keys (ticket W8-10; FR-AI-001..004,
//! `docs/design/ENHANCEMENT_RUNTIME.md` §6).
//!
//! ## One format for AI packs and artist packs
//!
//! §6 is explicit: "user-reviewable pack output (**same format as
//! hand-made replacement packs** — one pipeline for AI packs and artist
//! packs)", and FR-AI-004 repeats it. So nothing here is AI-specific.
//! A [`PackManifest`] records which assets a pack replaces and what it
//! was derived from; whether a human or a model produced the pixels is
//! one field, not a different code path.
//!
//! ## Matching is hash-based, never fuzzy
//!
//! §6: "Asset extraction is exact (indexed pixels + palette from the
//! core), so matching is **hash-based like Mesen HD packs, not fuzzy
//! image matching**." That is what makes a pack reproducible: the same
//! ROM and the same extraction produce the same asset hashes, so the
//! same pack applies, deterministically, forever.
//!
//! ## The refusal is the honesty contract in mechanical form
//!
//! A pack declares the inputs it was built from. If those do not match
//! the game actually running, it is **refused whole** — see
//! [`PackManifest::validate_against`]. A half-applied pack is worse than
//! none, because the player can no longer tell which pixels are the
//! game's and which are the pack's, which is precisely what the project's
//! honesty contract exists to prevent.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

/// Hex-encode a digest. The whole crate speaks hex strings rather than
/// byte arrays, because every one of these ends up in a manifest, a
/// filename or a diagnostic.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Hash of one extracted asset: indexed pixels plus the palette they are
/// indexed against.
///
/// **The palette is part of the identity, not decoration.** The same tile
/// bytes under two palettes are two different pictures, and a pack that
/// keyed on pixels alone would replace both with one image — the classic
/// HD-pack artefact where a recoloured enemy gets its sibling's skin.
#[must_use]
pub fn asset_hash(indexed_pixels: &[u8], palette: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b"rf-asset-v1");
    h.update((indexed_pixels.len() as u64).to_le_bytes());
    h.update(indexed_pixels);
    h.update((palette.len() as u64).to_le_bytes());
    h.update(palette);
    hex(&h.finalize())
}

/// Hash of a settings map, so a pack built with different options gets a
/// different cache key.
#[must_use]
pub fn settings_hash(settings: &BTreeMap<String, String>) -> String {
    // BTreeMap, so iteration order is the keys' order rather than
    // insertion order — two callers that set the same options in
    // different sequences must produce the SAME hash, or the cache
    // misses for no reason a user could understand.
    let mut h = Sha256::new();
    h.update(b"rf-settings-v1");
    for (k, v) in settings {
        h.update((k.len() as u64).to_le_bytes());
        h.update(k.as_bytes());
        h.update((v.len() as u64).to_le_bytes());
        h.update(v.as_bytes());
    }
    hex(&h.finalize())
}

/// FR-AI-002's lookup key: `(rom hash, asset hash, model id, settings
/// hash)`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CacheKey {
    pub rom_hash: String,
    pub asset_hash: String,
    /// The model that produced this, or the artist/tool for a hand-made
    /// pack — §6's "one pipeline" means this field is never empty, it
    /// just says something different.
    pub model_id: String,
    pub settings_hash: String,
}

impl CacheKey {
    /// A stable, filesystem-safe identifier for this key.
    ///
    /// Derived by hashing the four fields together rather than
    /// concatenating them: concatenation would produce a path long enough
    /// to hit filesystem limits, and would let two different keys collide
    /// if a field contained the separator.
    #[must_use]
    pub fn digest(&self) -> String {
        let mut h = Sha256::new();
        h.update(b"rf-cachekey-v1");
        for part in [
            &self.rom_hash,
            &self.asset_hash,
            &self.model_id,
            &self.settings_hash,
        ] {
            h.update((part.len() as u64).to_le_bytes());
            h.update(part.as_bytes());
        }
        hex(&h.finalize())
    }
}

/// One replaced asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackEntry {
    /// The asset this replaces, by hash.
    pub asset_hash: String,
    /// Which animation set it belongs to, if any (§6's grouping).
    pub animation_set: Option<String>,
    /// The replacement image's own hash, so a corrupted or swapped file
    /// is detectable.
    pub image_hash: String,
}

/// A replacement pack's manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackManifest {
    pub id: String,
    /// The ROM this pack was built against.
    pub rom_hash: String,
    /// What produced it: a model id, or an artist/tool name.
    pub model_id: String,
    pub settings_hash: String,
    pub entries: Vec<PackEntry>,
}

/// Why a pack was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackError {
    /// The pack was built for a different ROM.
    WrongRom { expected: String, found: String },
    /// The pack declares an asset the running game never produced.
    UnknownAsset { asset_hash: String },
    /// Two entries claim the same asset.
    DuplicateAsset { asset_hash: String },
    /// An entry is missing its replacement image.
    MissingImage { asset_hash: String },
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackError::WrongRom { expected, found } => write!(
                f,
                "pack was built for ROM {found}, but {expected} is running"
            ),
            PackError::UnknownAsset { asset_hash } => write!(
                f,
                "pack replaces asset {asset_hash}, which this game does not produce"
            ),
            PackError::DuplicateAsset { asset_hash } => {
                write!(f, "two entries both replace asset {asset_hash}")
            }
            PackError::MissingImage { asset_hash } => {
                write!(f, "entry for asset {asset_hash} has no replacement image")
            }
        }
    }
}

impl PackManifest {
    /// Validate against the ROM and the assets the game actually
    /// produces.
    ///
    /// **All-or-nothing.** Every problem is collected and the pack is
    /// refused whole; nothing is applied partially. A half-applied pack
    /// is worse than none, because the player can no longer tell which
    /// pixels are the game's — and returning every fault at once means a
    /// pack author fixes them in one pass rather than one per run.
    ///
    /// # Errors
    /// Returns every reason the pack cannot be applied.
    pub fn validate_against(
        &self,
        rom_hash: &str,
        known_assets: &std::collections::BTreeSet<String>,
    ) -> Result<(), Vec<PackError>> {
        let mut errors = Vec::new();

        if self.rom_hash != rom_hash {
            errors.push(PackError::WrongRom {
                expected: rom_hash.to_string(),
                found: self.rom_hash.clone(),
            });
        }

        let mut seen = std::collections::BTreeSet::new();
        for entry in &self.entries {
            if !seen.insert(entry.asset_hash.clone()) {
                errors.push(PackError::DuplicateAsset {
                    asset_hash: entry.asset_hash.clone(),
                });
            }
            if entry.image_hash.is_empty() {
                errors.push(PackError::MissingImage {
                    asset_hash: entry.asset_hash.clone(),
                });
            }
            if !known_assets.contains(&entry.asset_hash) {
                errors.push(PackError::UnknownAsset {
                    asset_hash: entry.asset_hash.clone(),
                });
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// The cache key for one of this pack's entries.
    #[must_use]
    pub fn key_for(&self, entry: &PackEntry) -> CacheKey {
        CacheKey {
            rom_hash: self.rom_hash.clone(),
            asset_hash: entry.asset_hash.clone(),
            model_id: self.model_id.clone(),
            settings_hash: self.settings_hash.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn settings(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    /// **Cache-keyed reproducibility** — criterion 1. The same inputs
    /// must always produce the same key, or a pack is not reproducible
    /// from its inputs at all.
    #[test]
    fn the_same_inputs_always_produce_the_same_key() {
        let a = asset_hash(&[1, 2, 3], &[10, 20]);
        let b = asset_hash(&[1, 2, 3], &[10, 20]);
        assert_eq!(a, b);
    }

    /// **The palette is part of an asset's identity.** The same tile
    /// bytes under two palettes are two different pictures; keying on
    /// pixels alone gives the classic HD-pack artefact where a recoloured
    /// enemy wears its sibling's skin.
    #[test]
    fn the_palette_changes_the_asset_hash() {
        assert_ne!(
            asset_hash(&[1, 2, 3], &[10, 20]),
            asset_hash(&[1, 2, 3], &[30, 40]),
            "a recolour must be a different asset"
        );
    }

    /// Length is hashed too, so two different splits of the same bytes
    /// cannot collide.
    #[test]
    fn pixels_and_palette_cannot_be_confused_for_one_another() {
        assert_ne!(
            asset_hash(&[1, 2], &[3]),
            asset_hash(&[1], &[2, 3]),
            "the boundary between pixels and palette must be part of the hash"
        );
    }

    /// **Settings order must not matter.** Two callers setting the same
    /// options in different sequences must hit the SAME cache entry, or
    /// the cache misses for a reason no user could understand.
    #[test]
    fn settings_hash_is_independent_of_insertion_order() {
        let a = settings(&[("scale", "4"), ("denoise", "on")]);
        let b = settings(&[("denoise", "on"), ("scale", "4")]);
        assert_eq!(settings_hash(&a), settings_hash(&b));
        // But a different VALUE is a different key.
        let c = settings(&[("scale", "2"), ("denoise", "on")]);
        assert_ne!(settings_hash(&a), settings_hash(&c));
    }

    /// Every field of the key participates — a pack built with a
    /// different model must not collide with one built by an artist.
    #[test]
    fn every_field_of_the_cache_key_participates() {
        let base = CacheKey {
            rom_hash: "rom".into(),
            asset_hash: "asset".into(),
            model_id: "esrgan-v1".into(),
            settings_hash: "settings".into(),
        };
        for changed in [
            CacheKey {
                rom_hash: "other".into(),
                ..base.clone()
            },
            CacheKey {
                asset_hash: "other".into(),
                ..base.clone()
            },
            CacheKey {
                model_id: "artist:jo".into(),
                ..base.clone()
            },
            CacheKey {
                settings_hash: "other".into(),
                ..base.clone()
            },
        ] {
            assert_ne!(base.digest(), changed.digest(), "{changed:?}");
        }
    }

    /// The digest is a hash rather than a concatenation: concatenating
    /// would produce paths long enough to hit filesystem limits and would
    /// let a field containing the separator forge another key.
    #[test]
    fn the_digest_is_fixed_length_regardless_of_field_length() {
        let short = CacheKey {
            rom_hash: "a".into(),
            asset_hash: "b".into(),
            model_id: "c".into(),
            settings_hash: "d".into(),
        };
        let long = CacheKey {
            rom_hash: "a".repeat(500),
            asset_hash: "b".repeat(500),
            model_id: "c".repeat(500),
            settings_hash: "d".repeat(500),
        };
        assert_eq!(short.digest().len(), 64);
        assert_eq!(long.digest().len(), 64);
    }

    fn manifest(entries: Vec<PackEntry>) -> PackManifest {
        PackManifest {
            id: "test-pack".into(),
            rom_hash: "rom-abc".into(),
            model_id: "esrgan-v1".into(),
            settings_hash: "settings-1".into(),
            entries,
        }
    }

    fn entry(asset: &str) -> PackEntry {
        PackEntry {
            asset_hash: asset.into(),
            animation_set: None,
            image_hash: format!("img-{asset}"),
        }
    }

    /// A matching pack validates.
    #[test]
    fn a_matching_pack_is_accepted() {
        let m = manifest(vec![entry("a1"), entry("a2")]);
        let known: BTreeSet<String> = ["a1", "a2", "a3"].iter().map(|s| (*s).into()).collect();
        assert!(m.validate_against("rom-abc", &known).is_ok());
    }

    /// **Criterion 3: a mismatched pack is REFUSED, not partially
    /// applied.** A half-applied pack is worse than none, because the
    /// player can no longer tell which pixels are the game's.
    #[test]
    fn a_pack_built_for_another_rom_is_refused() {
        let m = manifest(vec![entry("a1")]);
        let known: BTreeSet<String> = ["a1"].iter().map(|s| (*s).into()).collect();
        let errors = m
            .validate_against("rom-different", &known)
            .expect_err("must be refused");
        assert!(matches!(errors[0], PackError::WrongRom { .. }));
        // And the diagnostic must name both, or an author cannot act on it.
        let text = errors[0].to_string();
        assert!(
            text.contains("rom-abc") && text.contains("rom-different"),
            "{text}"
        );
    }

    #[test]
    fn an_asset_the_game_never_produces_is_refused() {
        let m = manifest(vec![entry("ghost")]);
        let known: BTreeSet<String> = ["a1"].iter().map(|s| (*s).into()).collect();
        let errors = m.validate_against("rom-abc", &known).expect_err("refused");
        assert!(errors
            .iter()
            .any(|e| matches!(e, PackError::UnknownAsset { .. })));
    }

    #[test]
    fn duplicate_and_imageless_entries_are_refused() {
        let mut e = entry("a1");
        e.image_hash = String::new();
        let m = manifest(vec![entry("a1"), entry("a1"), e]);
        let known: BTreeSet<String> = ["a1"].iter().map(|s| (*s).into()).collect();
        let errors = m.validate_against("rom-abc", &known).expect_err("refused");
        assert!(errors
            .iter()
            .any(|x| matches!(x, PackError::DuplicateAsset { .. })));
        assert!(errors
            .iter()
            .any(|x| matches!(x, PackError::MissingImage { .. })));
    }

    /// **Every fault is reported at once**, so a pack author fixes them
    /// in one pass rather than one per run.
    #[test]
    fn validation_reports_every_fault_not_just_the_first() {
        let m = manifest(vec![entry("ghost1"), entry("ghost2")]);
        let known: BTreeSet<String> = ["a1"].iter().map(|s| (*s).into()).collect();
        let errors = m
            .validate_against("wrong-rom", &known)
            .expect_err("refused");
        assert!(
            errors.len() >= 3,
            "a wrong ROM plus two unknown assets is three faults, got {errors:?}"
        );
    }

    /// §6's "one pipeline for AI packs and artist packs": nothing in the
    /// format is AI-specific — the producer is one field.
    #[test]
    fn an_artist_pack_and_an_ai_pack_use_the_same_format() {
        let known: BTreeSet<String> = ["a1"].iter().map(|s| (*s).into()).collect();
        let mut artist = manifest(vec![entry("a1")]);
        artist.model_id = "artist:jo".into();
        assert!(artist.validate_against("rom-abc", &known).is_ok());

        // ...and they key differently, so both can be cached at once.
        let ai = manifest(vec![entry("a1")]);
        assert_ne!(
            artist.key_for(&artist.entries[0]).digest(),
            ai.key_for(&ai.entries[0]).digest()
        );
    }
}
