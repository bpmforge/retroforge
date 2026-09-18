//! Library thumbnails: save-state screenshot / first-frame capture / user
//! art folder, in priority order, per ROM hash (ticket W15-05;
//! `docs/design/UX_WAVE_15.md` §4, `plan.json` W15-05).
//!
//! ## Why this module has no `egui` in it
//!
//! Everything here is pure data and filesystem/cache lookups: which
//! source wins ([`thumbnail_source`]), how a title is normalized for
//! art-folder matching ([`normalize_title`]), whether a frame is worth
//! capturing at all ([`frame_is_non_uniform`]), and the cache round-trip
//! for the one source that must persist across sessions
//! ([`first_frame_key`]/[`put_first_frame`]/[`get_first_frame`]). Decoding
//! the winning bytes into an `egui::ColorImage`/`TextureHandle` is
//! `crate::app`'s job (it already owns the `egui::Context` this needs) —
//! keeping the decode there is also what makes every function in this
//! file testable with no `egui_kittest` harness at all.
//!
//! ## Priority order (§4, acceptance 1)
//!
//! 1. Most recent save-state screenshot (`crate::state_slots` already
//!    writes one beside every occupied slot).
//! 2. A one-time first-frame capture on first successful boot — the ONLY
//!    source this module persists itself, via `rf-cache`, because it is
//!    the only one with nowhere else to live: a save-state screenshot
//!    lives beside its `.rfstate`, and user art lives in the user's own
//!    folder.
//! 3. A match from the user's art folder (Settings › Paths), by
//!    normalized title.
//!
//! ## Storage: the existing size-capped LRU, not a second cache
//!
//! The first-frame capture is stored via `rf_cache::Cache::put`/`get`
//! (ticket W4-08), keyed by a [`rf_cache::CacheKey`] whose `rom_sha256` is
//! the game's normalized hash and whose `producer`/`asset_hash` name this
//! use case (`FIRST_FRAME_PRODUCER`/`FIRST_FRAME_ASSET`) — `Cache::get`/
//! `put` stay payload-agnostic by design (`rf-cache`'s own module doc), so
//! this is a caller of the generic store, not a second cache
//! implementation.

use std::path::{Path, PathBuf};

use rf_cache::{Cache, CacheKey};

/// Which source produced a thumbnail (acceptance 1's priority order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailSource {
    /// The most recent occupied save-state slot's screenshot.
    SaveState,
    /// The cached one-time first-frame capture.
    FirstFrame,
    /// A file in the user's configured art folder, matched by normalized
    /// title.
    UserArt,
    /// Ticket W15-09, ruling D-011: fetched from libretro-thumbnails, the
    /// last resort — only reachable when Settings' "Fetch box art from
    /// the internet" toggle is on (`crate::art`). Distinct from the other
    /// three so callers can draw the network indicator (`UX_WAVE_15.md`
    /// §4: "nothing on screen should imply 'local' when it wasn't").
    Fetched,
}

/// Priority order, as a pure function of what is AVAILABLE (acceptance
/// 1): the caller does every filesystem/cache lookup and passes in only
/// whether each source has something, which is what makes this testable
/// without touching a disk.
#[must_use]
pub fn thumbnail_source(
    has_save_state_screenshot: bool,
    has_first_frame_capture: bool,
    has_user_art: bool,
) -> Option<ThumbnailSource> {
    thumbnail_source_with_fetch(
        has_save_state_screenshot,
        has_first_frame_capture,
        has_user_art,
        false,
    )
}

/// The same priority order as [`thumbnail_source`], with the fourth
/// source (ticket W15-09, `ThumbnailSource::Fetched`) appended at the
/// bottom of the ladder — save-state screenshot, first-frame, user art
/// folder, then fetched art (`UX_WAVE_15.md` §4, plan.json W15-09
/// acceptance 5). Kept as a separate function rather than adding a
/// parameter to [`thumbnail_source`] would: every existing call site
/// (and every existing test) that only knows about the first three
/// sources stays correct with no `has_fetched_art: false` boilerplate to
/// thread through.
#[must_use]
pub fn thumbnail_source_with_fetch(
    has_save_state_screenshot: bool,
    has_first_frame_capture: bool,
    has_user_art: bool,
    has_fetched_art: bool,
) -> Option<ThumbnailSource> {
    if has_save_state_screenshot {
        Some(ThumbnailSource::SaveState)
    } else if has_first_frame_capture {
        Some(ThumbnailSource::FirstFrame)
    } else if has_user_art {
        Some(ThumbnailSource::UserArt)
    } else if has_fetched_art {
        Some(ThumbnailSource::Fetched)
    } else {
        None
    }
}

/// Normalize a title for art-folder matching (§4.3, "the DuckStation
/// pattern"): lowercase, strip anything in `(...)`/`[...]` (region/version
/// tags such as `(USA)`, `(Rev 1)`, `[!]`), drop punctuation, collapse
/// whitespace.
#[must_use]
pub fn normalize_title(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut depth: i32 = 0;
    for c in title.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            _ if depth > 0 => {}
            c if c.is_alphanumeric() => {
                out.extend(c.to_lowercase());
            }
            _ => out.push(' '),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Find a file in `art_folder` whose normalized stem matches `title`.
///
/// `.png` only (module doc, acceptance 1's "`.jpg` only if a decoder
/// exists, else `.png`"): `retroforge`'s `image` dependency has only the
/// `png` feature enabled (`Cargo.toml`'s own comment on why — no other
/// codec is a considered dependency yet), so there is no JPEG decoder in
/// this build to use one with.
#[must_use]
pub fn find_user_art(art_folder: &Path, title: &str) -> Option<PathBuf> {
    let target = normalize_title(title);
    if target.is_empty() {
        return None;
    }
    let entries = std::fs::read_dir(art_folder).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let is_png = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("png"));
        if !is_png {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if normalize_title(stem) == target {
            return Some(path);
        }
    }
    None
}

/// Whether `rgba` is worth capturing as a first-frame thumbnail: not a
/// single flat colour (acceptance 1: "a frame where the core has rendered
/// something, not frame 0's black").
///
/// A full per-pixel scan rather than a handful of samples: a frame is at
/// most a few hundred KB, this runs once per frame only until a hash gets
/// its one capture, and a sampled check would risk calling a frame
/// "rendered" from a lucky sample inside a black letterbox border.
#[must_use]
pub fn frame_is_non_uniform(rgba: &[u8]) -> bool {
    let Some(first) = rgba.get(0..4) else {
        return false;
    };
    rgba.chunks_exact(4).any(|px| px != first)
}

/// `rf_cache::CacheKey`'s free-text fields for the first-frame capture use
/// case (module doc): fixed values so every call site keys the same way.
const FIRST_FRAME_ASSET: &str = "thumbnail";
const FIRST_FRAME_PRODUCER: &str = "first-frame-capture";
/// Bumped if the stored payload's shape ever changes (currently: raw PNG
/// bytes), so an old build's cache entries are never handed to a decoder
/// that no longer agrees with them.
const FIRST_FRAME_SETTINGS: &str = "v1";

fn first_frame_key(rom_sha256: &str) -> CacheKey {
    CacheKey {
        rom_sha256: rom_sha256.to_string(),
        asset_hash: FIRST_FRAME_ASSET.to_string(),
        producer: FIRST_FRAME_PRODUCER.to_string(),
        settings_hash: FIRST_FRAME_SETTINGS.to_string(),
    }
}

/// Whether a first-frame capture is already cached for `rom_sha256` — the
/// "one-time" half of acceptance 1: a capture already on disk is never
/// retaken.
#[must_use]
pub fn has_first_frame(cache: &Cache, rom_sha256: &str) -> bool {
    cache.contains(&first_frame_key(rom_sha256))
}

/// Persist a first-frame capture's PNG bytes.
///
/// # Errors
/// Propagates [`Cache::put`]'s errors (disk full, containment violation).
pub fn put_first_frame(
    cache: &mut Cache,
    rom_sha256: &str,
    png_bytes: &[u8],
) -> Result<(), rf_cache::CacheError> {
    cache.put(&first_frame_key(rom_sha256), png_bytes)
}

/// Read back a first-frame capture's PNG bytes, or `None` on a miss.
///
/// # Errors
/// Propagates [`Cache::get`]'s errors (a genuinely corrupt/tampered
/// entry) — distinct from `Ok(None)`, an ordinary cache miss.
pub fn get_first_frame(
    cache: &mut Cache,
    rom_sha256: &str,
) -> Result<Option<Vec<u8>>, rf_cache::CacheError> {
    cache.get(&first_frame_key(rom_sha256))
}

/// Where the thumbnail cache lives: under Settings › Paths' configured
/// cache directory (or the config root's default `cache` folder), in its
/// own `thumbnails` subdirectory so it never shares entries with (or
/// evicts against) the stitched-canvas cache's own root
/// (`crate::core_thread::canvas_cache_root`).
#[must_use]
pub fn cache_root(
    config_root: Option<&Path>,
    paths: &crate::settings::PathSettings,
) -> Option<PathBuf> {
    let base = match &paths.cache_dir {
        Some(dir) => dir.clone(),
        None => config_root?.join("cache"),
    };
    Some(base.join("thumbnails"))
}

/// Open the thumbnail cache, respecting Settings › Paths' configured cap
/// (acceptance 1: "respecting the existing size-capped LRU"). `None` when
/// there is nowhere to root it (no config directory on this platform) or
/// `Cache::open` itself fails — persistence here is strictly additive,
/// same stance as `CanvasAccumulator`'s own cache: a missing cache means
/// first-frame capture degrades to "never captured this session", never a
/// reason to fail loading a ROM.
#[must_use]
pub fn open_cache(
    config_root: Option<&Path>,
    paths: &crate::settings::PathSettings,
) -> Option<Cache> {
    let root = cache_root(config_root, paths)?;
    let cap_bytes = paths.cache_cap_mb.saturating_mul(1024 * 1024);
    Cache::open(root, cap_bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- thumbnail_source: acceptance 1's priority order -------------

    #[test]
    fn save_state_wins_over_everything() {
        assert_eq!(
            thumbnail_source(true, true, true),
            Some(ThumbnailSource::SaveState)
        );
        assert_eq!(
            thumbnail_source(true, false, true),
            Some(ThumbnailSource::SaveState)
        );
    }

    #[test]
    fn first_frame_wins_when_no_save_state() {
        assert_eq!(
            thumbnail_source(false, true, true),
            Some(ThumbnailSource::FirstFrame)
        );
        assert_eq!(
            thumbnail_source(false, true, false),
            Some(ThumbnailSource::FirstFrame)
        );
    }

    #[test]
    fn user_art_is_the_last_resort() {
        assert_eq!(
            thumbnail_source(false, false, true),
            Some(ThumbnailSource::UserArt)
        );
    }

    #[test]
    fn none_available_is_none() {
        assert_eq!(thumbnail_source(false, false, false), None);
    }

    // ---- thumbnail_source_with_fetch: acceptance 5's fourth source ----

    #[test]
    fn fetched_art_is_the_last_resort_of_all_four() {
        assert_eq!(
            thumbnail_source_with_fetch(false, false, false, true),
            Some(ThumbnailSource::Fetched)
        );
    }

    #[test]
    fn fetched_art_never_beats_any_local_source() {
        assert_eq!(
            thumbnail_source_with_fetch(true, false, false, true),
            Some(ThumbnailSource::SaveState)
        );
        assert_eq!(
            thumbnail_source_with_fetch(false, true, false, true),
            Some(ThumbnailSource::FirstFrame)
        );
        assert_eq!(
            thumbnail_source_with_fetch(false, false, true, true),
            Some(ThumbnailSource::UserArt)
        );
    }

    #[test]
    fn no_source_at_all_including_fetch_is_none() {
        assert_eq!(
            thumbnail_source_with_fetch(false, false, false, false),
            None
        );
    }

    // ---- normalize_title -----------------------------------------------

    #[test]
    fn normalize_title_lowercases_and_strips_region_tags() {
        assert_eq!(
            normalize_title("Super Mario Bros. 3 (USA) (Rev 1)"),
            "super mario bros 3"
        );
    }

    #[test]
    fn normalize_title_strips_bracketed_tags_too() {
        assert_eq!(normalize_title("Contra [!]"), "contra");
    }

    #[test]
    fn normalize_title_collapses_punctuation_and_whitespace() {
        assert_eq!(
            normalize_title("Kirby's   Adventure!!"),
            "kirby s adventure"
        );
    }

    #[test]
    fn normalize_title_ignores_an_unmatched_closing_bracket() {
        // Depth must not go negative and start eating ordinary text after
        // a stray ')' with no opener.
        assert_eq!(normalize_title("Foo) Bar"), "foo bar");
    }

    #[test]
    fn normalize_title_is_stable_for_matching_a_stem_against_itself() {
        let a = normalize_title("Mega Man 2 (USA)");
        let b = normalize_title("mega_man_2");
        assert_eq!(a, b);
    }

    // ---- frame_is_non_uniform ------------------------------------------

    #[test]
    fn a_solid_black_frame_is_uniform() {
        let rgba = vec![0u8; 4 * 100];
        assert!(!frame_is_non_uniform(&rgba));
    }

    #[test]
    fn a_frame_with_any_differing_pixel_is_non_uniform() {
        let mut rgba = vec![0u8; 4 * 100];
        rgba[4 * 50] = 255; // one pixel's red channel differs
        assert!(frame_is_non_uniform(&rgba));
    }

    #[test]
    fn an_empty_buffer_is_not_non_uniform() {
        assert!(!frame_is_non_uniform(&[]));
    }

    // ---- find_user_art ---------------------------------------------------

    fn unique_dir(label: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rf-thumbnail-test-{label}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn find_user_art_matches_by_normalized_title_ignoring_case_and_tags() {
        let dir = unique_dir("art-match");
        std::fs::write(dir.join("super_mario_bros_3.png"), b"fake-png").unwrap();
        let found = find_user_art(&dir, "Super Mario Bros. 3 (USA)");
        assert_eq!(found, Some(dir.join("super_mario_bros_3.png")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn find_user_art_ignores_non_png_files() {
        let dir = unique_dir("art-nonpng");
        std::fs::write(dir.join("contra.jpg"), b"fake-jpg").unwrap();
        assert_eq!(find_user_art(&dir, "Contra"), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn find_user_art_returns_none_when_nothing_matches() {
        let dir = unique_dir("art-none");
        std::fs::write(dir.join("unrelated.png"), b"fake-png").unwrap();
        assert_eq!(find_user_art(&dir, "Contra"), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- cache round-trip (acceptance 1) --------------------------------

    #[test]
    fn first_frame_round_trips_through_the_cache() {
        let dir = unique_dir("cache-roundtrip");
        let mut cache = Cache::open(&dir, 1_000_000).unwrap();
        assert!(!has_first_frame(&cache, "rom-abc"));
        put_first_frame(&mut cache, "rom-abc", b"fake-png-bytes").unwrap();
        assert!(has_first_frame(&cache, "rom-abc"));
        assert_eq!(
            get_first_frame(&mut cache, "rom-abc").unwrap(),
            Some(b"fake-png-bytes".to_vec())
        );
        // A different hash is unaffected.
        assert!(!has_first_frame(&cache, "rom-xyz"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn first_frame_cache_survives_reopen() {
        let dir = unique_dir("cache-persist");
        {
            let mut cache = Cache::open(&dir, 1_000_000).unwrap();
            put_first_frame(&mut cache, "rom-persist", b"bytes").unwrap();
        }
        let mut reopened = Cache::open(&dir, 1_000_000).unwrap();
        assert_eq!(
            get_first_frame(&mut reopened, "rom-persist").unwrap(),
            Some(b"bytes".to_vec())
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- cache_root / open_cache -----------------------------------------

    #[test]
    fn cache_root_prefers_the_configured_cache_dir_over_the_config_root() {
        let paths = crate::settings::PathSettings {
            cache_dir: Some(PathBuf::from("/configured/cache")),
            ..Default::default()
        };
        assert_eq!(
            cache_root(Some(Path::new("/config/root")), &paths),
            Some(PathBuf::from("/configured/cache/thumbnails"))
        );
    }

    #[test]
    fn cache_root_falls_back_to_the_config_root_when_unset() {
        let paths = crate::settings::PathSettings::default();
        assert_eq!(
            cache_root(Some(Path::new("/config/root")), &paths),
            Some(PathBuf::from("/config/root/cache/thumbnails"))
        );
    }

    #[test]
    fn cache_root_is_none_with_no_config_root_and_no_configured_dir() {
        let paths = crate::settings::PathSettings::default();
        assert_eq!(cache_root(None, &paths), None);
    }
}
