//! Soft-patching glue (ticket W32-01, `docs/design/GAME_PATCHING.md` level
//! 1): find the patch files that belong to a ROM, apply the ones the user
//! turned on **in memory**, and describe the resulting session.
//!
//! The parsers live in [`rf_enhance::patch`] (pure data). This module owns
//! the filesystem and the policy:
//!
//! * Nothing applies by default -- a patch runs only if its key is in the
//!   game's persisted `patches` list (`GameSettings::patches_enabled`).
//! * The ROM file is never written, and neither is the patch.
//! * A patched session is NOT the reference (law 6): [`SoftPatchSession::is_reference`]
//!   is false, and the status badge says "Patched: ...".
//! * A recipe MOD (ticket W36-06, `rf_rebuild::PatchRow`s) is the same kind of
//!   thing: bytes of the in-memory image, off by default per game, never
//!   written to disk, and the session it makes is not the cartridge either.
//! * A patched session has its own identity (`session_sha256`) so its save
//!   states never mix with the unpatched game's; `base_sha256` is kept for
//!   settings and profile matching, which still name the base ROM.
use std::path::{Path, PathBuf};

use rf_enhance::patch::bso::{BsoSettings, WidescreenConvention};
use rf_enhance::patch::discover::{classify_beside, classify_ext, is_bso_for, order_key};
use rf_enhance::patch::{apply_patch, PatchKind, MAX_OUTPUT};

/// Sub-directory of the app's config dir holding per-ROM patch folders:
/// `<config>/retroforge/patches/<rom sha256>/`.
pub const PATCHES_DIR: &str = "patches";

/// Largest `.bso` read (they are a few dozen bytes).
const MAX_BSO_BYTES: u64 = 4096;

/// One patch file offered to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The persisted identity: the file name beside the ROM, or
    /// `patches/<name>` for one from the config dir.
    pub key: String,
    pub path: PathBuf,
    pub kind: PatchKind,
}

/// Everything found for one ROM.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovery {
    /// In application order.
    pub patches: Vec<Candidate>,
    /// `Game.bso` beside the ROM, if any.
    pub bso: Option<PathBuf>,
}

fn file_len_ok(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() <= MAX_OUTPUT as u64)
}

/// Look for patches for the ROM at `rom_path`: beside it (`Game.bps`,
/// `Game.ips1`, ...) and in `<config>/retroforge/patches/<base_sha256>/`.
/// Never fails: an unreadable directory simply offers nothing.
#[must_use]
pub fn discover(rom_path: &Path, config_root: Option<&Path>, base_sha256: &str) -> Discovery {
    let mut found: Vec<((u8, u32, PatchKind, String), Candidate)> = Vec::new();
    let mut bso = None;
    if let (Some(dir), Some(stem)) = (
        rom_path.parent(),
        rom_path.file_stem().and_then(|s| s.to_str()),
    ) {
        // `read_dir` is an iterator: it ends when the directory does.
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_bso_for(stem, &name) && file_len_ok(&entry.path()) {
                bso = Some(entry.path());
            } else if let Some(c) = classify_beside(stem, &name) {
                if file_len_ok(&entry.path()) {
                    let (i, k, n) = order_key(&c, &name);
                    found.push((
                        (0, i, k, n),
                        Candidate {
                            key: name,
                            path: entry.path(),
                            kind: c.kind,
                        },
                    ));
                }
            }
        }
    }
    if let Some(root) = config_root {
        let dir = root
            .join(crate::bindings_store::APP_DIR)
            .join(PATCHES_DIR)
            .join(base_sha256);
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(ext) = Path::new(&name).extension().and_then(|e| e.to_str()) else {
                continue;
            };
            if let Some(c) = classify_ext(ext) {
                if file_len_ok(&entry.path()) {
                    let (i, k, n) = order_key(&c, &name);
                    found.push((
                        (1, i, k, n),
                        Candidate {
                            key: format!("{PATCHES_DIR}/{name}"),
                            path: entry.path(),
                            kind: c.kind,
                        },
                    ));
                }
            }
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    Discovery {
        patches: found.into_iter().map(|(_, c)| c).collect(),
        bso,
    }
}

/// The result of applying the enabled patches to a ROM image.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    /// The image to boot (the base image, untouched, when nothing applied).
    pub image: Vec<u8>,
    /// Keys of the patches that applied, in order.
    pub applied: Vec<String>,
    /// One plain-words line per patch that was enabled but failed.
    pub failures: Vec<String>,
    /// Header notes ("applied to the image without its copier header").
    pub notes: Vec<String>,
    pub widescreen: WidescreenConvention,
}

/// Apply every candidate whose key is in `enabled`, in order, to `base`.
/// A patch that fails is skipped and reported; the rest still apply.
#[must_use]
pub fn apply_enabled(base: Vec<u8>, discovery: &Discovery, enabled: &[String]) -> Outcome {
    let mut out = Outcome {
        image: base,
        ..Outcome::default()
    };
    for c in discovery
        .patches
        .iter()
        .filter(|c| enabled.contains(&c.key))
    {
        let bytes = match std::fs::read(&c.path) {
            Ok(b) => b,
            Err(e) => {
                out.failures
                    .push(format!("{}: could not read ({e})", c.key));
                continue;
            }
        };
        match apply_patch(&out.image, &bytes) {
            Ok((image, used)) => {
                out.image = image;
                out.applied.push(c.key.clone());
                if !used.note().is_empty() {
                    out.notes.push(format!("{}: {}", c.key, used.note()));
                }
            }
            Err(e) => out.failures.push(format!("{}: {e}", c.key)),
        }
    }
    if !out.applied.is_empty() {
        if let Some(settings) = discovery.bso.as_deref().and_then(read_bso) {
            out.widescreen = settings.convention();
        }
    }
    out
}

fn read_bso(path: &Path) -> Option<BsoSettings> {
    use std::io::Read;
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_BSO_BYTES)
        .read_to_string(&mut text)
        .ok()?;
    BsoSettings::parse(&text).ok()
}

/// The result of applying recipe mods to a ROM image.
#[derive(Debug, Clone, Default)]
pub struct ModOutcome {
    /// The image to boot (the base image, untouched, when nothing applied).
    pub image: Vec<u8>,
    /// Names of the mods that applied.
    pub applied: Vec<String>,
    /// One plain-words line per mod that was enabled but refused.
    pub failures: Vec<String>,
}

/// Apply the enabled recipe mods to `base`, an iNES image, **in memory**.
///
/// `atlas` must be the code atlas of this same unpatched image (the shell
/// finds it in the per-ROM cache): rows are resolved against it, because a
/// patched immediate changes a function's hashes. A mod whose rows do not all
/// resolve to an exact, unambiguous function is refused whole and reported;
/// the others still apply. With no atlas every enabled mod is refused with the
/// reason, never applied on a guess.
#[must_use]
pub fn apply_mods(
    base: Vec<u8>,
    atlas: Option<&rf_rebuild::Atlas>,
    recipe: &rf_rebuild::Recipe,
    enabled: &[String],
) -> ModOutcome {
    let mut out = ModOutcome {
        image: base,
        ..ModOutcome::default()
    };
    for name in recipe.mods() {
        if !enabled.iter().any(|e| e == name) {
            continue;
        }
        let Some(atlas) = atlas else {
            out.failures.push(format!(
                "Mod `{name}` not applied: this ROM has no code atlas yet \
                 (open the Coverage tab, record some play, turn Rebuild on once)"
            ));
            continue;
        };
        let resolved = match rf_rebuild::resolve_patches(atlas, recipe, name) {
            Ok(r) => r,
            Err(e) => {
                out.failures.push(format!("Mod `{name}` not applied: {e}"));
                continue;
            }
        };
        let edits: Vec<(u32, u8)> = resolved.iter().map(|p| (p.prg_offset, p.value)).collect();
        match rf_enhance::patch::prg::apply_prg_bytes(&out.image, &edits) {
            Ok(image) => {
                out.image = image;
                out.applied.push(name.to_owned());
            }
            Err(e) => out.failures.push(format!("Mod `{name}` not applied: {e}")),
        }
    }
    out
}

/// What the running session knows about its patches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SoftPatchSession {
    /// Hash of the unpatched ROM: settings and profile matching use it.
    pub base_sha256: String,
    /// Hash of the image actually running: save states key on it. Equal to
    /// `base_sha256` when nothing is applied.
    pub session_sha256: String,
    /// Keys of the applied patches, in order.
    pub applied: Vec<String>,
    /// Names of the recipe mods applied (ticket W36-06).
    pub mods: Vec<String>,
    /// What a `.bso` beside the ROM declared (W32-03 acts on it).
    pub widescreen_convention: WidescreenConvention,
}

impl SoftPatchSession {
    /// An unpatched session.
    #[must_use]
    pub fn unpatched(sha256: &str) -> Self {
        Self {
            base_sha256: sha256.to_owned(),
            session_sha256: sha256.to_owned(),
            ..Self::default()
        }
    }

    /// Law 6: a patched session is not the cartridge.
    #[must_use]
    pub fn is_reference(&self) -> bool {
        self.applied.is_empty() && self.mods.is_empty()
    }

    /// The badge suffix, e.g. `Patched: Game.bps, Game.ips1` or
    /// `Patched: Game.bps, mod live-enemies`.
    #[must_use]
    pub fn badge(&self) -> Option<String> {
        let parts: Vec<String> = self
            .applied
            .iter()
            .cloned()
            .chain(self.mods.iter().map(|m| format!("mod {m}")))
            .collect();
        (!parts.is_empty()).then(|| format!("Patched: {}", parts.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_enhance::patch::bso::WidescreenConvention;

    fn temp_dir(label: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("rf-w32-01-{label}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// IPS patch writing `data` at `off`.
    fn ips(off: usize, data: &[u8]) -> Vec<u8> {
        let mut v = b"PATCH".to_vec();
        v.extend_from_slice(&[(off >> 16) as u8, (off >> 8) as u8, off as u8]);
        v.extend_from_slice(&(data.len() as u16).to_be_bytes());
        v.extend_from_slice(data);
        v.extend_from_slice(b"EOF");
        v
    }

    fn rom() -> Vec<u8> {
        (0..2048u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
            .collect()
    }

    fn setup() -> (PathBuf, PathBuf) {
        let dir = temp_dir("disc");
        let rom_path = dir.join("Game.sfc");
        std::fs::write(&rom_path, rom()).unwrap();
        (dir, rom_path)
    }

    #[test]
    fn discovery_orders_stacks_and_ignores_partial_names() {
        let (dir, rom_path) = setup();
        for n in [
            "Game.ips2",
            "Game.ips",
            "Game.bps",
            "Game.ips1",
            "Game2.ips",
            "Game.ips.bak",
            "Other.bps",
            "Game.bso",
        ] {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        let cfg = temp_dir("cfg");
        let hash = "ab".repeat(32);
        let pdir = cfg.join("retroforge").join(PATCHES_DIR).join(&hash);
        std::fs::create_dir_all(&pdir).unwrap();
        std::fs::write(pdir.join("fan-translation.ups"), b"x").unwrap();
        std::fs::write(pdir.join("notes.txt"), b"x").unwrap();
        let d = discover(&rom_path, Some(&cfg), &hash);
        let keys: Vec<&str> = d.patches.iter().map(|c| c.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "Game.bps",
                "Game.ips",
                "Game.ips1",
                "Game.ips2",
                "patches/fan-translation.ups"
            ]
        );
        assert_eq!(d.bso, Some(dir.join("Game.bso")));
        // No ROM-adjacent patches and no config dir: nothing offered.
        let (_, lone) = setup();
        assert!(discover(&lone, None, &hash).patches.is_empty());
    }

    #[test]
    fn nothing_applies_by_default_and_enabled_ones_stack_in_order() {
        let (dir, rom_path) = setup();
        std::fs::write(dir.join("Game.ips"), ips(10, b"AAAA")).unwrap();
        std::fs::write(dir.join("Game.ips1"), ips(12, b"BB")).unwrap();
        let d = discover(&rom_path, None, "h");
        let base = rom();
        let none = apply_enabled(base.clone(), &d, &[]);
        assert_eq!(none.image, base);
        assert!(none.applied.is_empty());
        let both = apply_enabled(base.clone(), &d, &["Game.ips1".into(), "Game.ips".into()]);
        assert_eq!(both.applied, ["Game.ips", "Game.ips1"]);
        assert_eq!(&both.image[10..14], b"AABB");
        assert_eq!(both.image.len(), base.len());
        // The ROM file itself is untouched.
        assert_eq!(std::fs::read(&rom_path).unwrap(), base);
    }

    #[test]
    fn a_failing_patch_is_reported_and_skipped() {
        let (dir, rom_path) = setup();
        std::fs::write(dir.join("Game.bps"), b"BPS1 truncated").unwrap();
        std::fs::write(dir.join("Game.ips"), ips(0, b"ok")).unwrap();
        let d = discover(&rom_path, None, "h");
        let o = apply_enabled(rom(), &d, &["Game.bps".into(), "Game.ips".into()]);
        assert_eq!(o.applied, ["Game.ips"]);
        assert_eq!(o.failures.len(), 1);
        assert!(o.failures[0].starts_with("Game.bps"));
    }

    #[test]
    fn bso_sets_the_convention_only_when_its_patch_is_enabled() {
        let (dir, rom_path) = setup();
        std::fs::write(dir.join("Game.ips"), ips(0, b"w")).unwrap();
        std::fs::write(dir.join("Game.bso"), "w1s1W48S2i0o1p1b1B1c1").unwrap();
        let d = discover(&rom_path, None, "h");
        assert_eq!(
            apply_enabled(rom(), &d, &[]).widescreen,
            WidescreenConvention::None
        );
        assert_eq!(
            apply_enabled(rom(), &d, &["Game.ips".into()]).widescreen,
            WidescreenConvention::BsnesHd { extra_per_side: 48 }
        );
    }

    #[test]
    fn patched_identity_is_separate_and_not_the_reference() {
        let base = crate::hash::sha256_hex(&rom());
        let u = SoftPatchSession::unpatched(&base);
        assert!(u.is_reference());
        assert_eq!(u.badge(), None);
        assert_eq!(u.session_sha256, u.base_sha256);
        let (dir, rom_path) = setup();
        std::fs::write(dir.join("Game.ips"), ips(5, b"zz")).unwrap();
        let d = discover(&rom_path, None, &base);
        let o = apply_enabled(rom(), &d, &["Game.ips".into()]);
        let s = SoftPatchSession {
            base_sha256: base.clone(),
            session_sha256: crate::hash::sha256_hex(&o.image),
            applied: o.applied,
            mods: Vec::new(),
            widescreen_convention: o.widescreen,
        };
        assert_ne!(s.session_sha256, s.base_sha256);
        assert!(!s.is_reference());
        assert_eq!(s.badge().as_deref(), Some("Patched: Game.ips"));
    }

    /// A 16 KiB NOP sled with a return at $10, and the recipe + atlas a
    /// recording of its first 17 bytes gives.
    fn mod_fixture() -> (Vec<u8>, rf_rebuild::Atlas, rf_rebuild::Recipe) {
        use rf_core_api::coverage::{CoverageLog, WindowMap, OPCODE};
        let mut ines = vec![0u8; 16];
        ines[0..4].copy_from_slice(b"NES\x1a");
        ines[4] = 1;
        let mut prg = vec![0xEAu8; 0x4000];
        prg[0x10] = 0x60;
        ines.extend(&prg);
        let mut log = CoverageLog::new(0x4000);
        let mut m = WindowMap::EMPTY;
        for (p, s) in m.0.iter_mut().enumerate() {
            *s = (p as u32 % 4) * 0x1000;
        }
        log.note_windows(m);
        for i in 0..=0x10 {
            log.mark_prg(i, OPCODE);
        }
        let atlas = rf_nes::rebuild::build_atlas(&prg, &log);
        let f = atlas
            .functions
            .iter()
            .find(|f| f.entry_rom == 0)
            .expect("function at 0");
        let recipe = rf_rebuild::Recipe {
            id: "t".into(),
            engine: "t".into(),
            functions: vec![rf_rebuild::RecipeFunction {
                grade: rf_rebuild::Grade::Observed,
                entry: None,
                insn_count: None,
                mode: None,
                confidence: None,
                name: "sled".into(),
                shape_hash: f.shape_hash,
                exact_hash: Some(f.byte_hash),
                address_hint: None,
                hooks: rf_rebuild::HookPoints::default(),
                source: "test".into(),
            }],
            ram: vec![],
            patches: vec![rf_rebuild::PatchRow {
                grade: rf_rebuild::Grade::Observed,
                name: "p".into(),
                mod_name: "m".into(),
                function: "sled".into(),
                offset: 3,
                value: 0xAA,
                source: "test".into(),
            }],
            ..Default::default()
        };
        (ines, atlas, recipe)
    }

    #[test]
    fn a_mod_applies_in_memory_only_when_enabled_and_exact() {
        let (ines, atlas, recipe) = mod_fixture();
        let on = vec!["m".to_string()];
        // Off by default: the image is the input, untouched.
        let none = apply_mods(ines.clone(), Some(&atlas), &recipe, &[]);
        assert_eq!((none.image == ines, none.applied.len()), (true, 0));
        // On: one byte of the copy changes, at the PRG offset.
        let r = apply_mods(ines.clone(), Some(&atlas), &recipe, &on);
        assert_eq!(r.applied, ["m"]);
        assert!(r.failures.is_empty());
        let diff: Vec<usize> = (0..ines.len()).filter(|&i| ines[i] != r.image[i]).collect();
        assert_eq!(diff, [16 + 3]);
        assert_eq!(r.image[16 + 3], 0xAA);
        // No atlas: refused with the reason, nothing applied.
        let r = apply_mods(ines.clone(), None, &recipe, &on);
        assert!(r.applied.is_empty() && r.image == ines);
        assert!(r.failures[0].contains("no code atlas"), "{:?}", r.failures);
        // A function that is not the one the row was written for.
        let mut bad = recipe.clone();
        bad.functions[0].exact_hash = Some(1);
        let r = apply_mods(ines.clone(), Some(&atlas), &bad, &on);
        assert!(r.applied.is_empty() && r.image == ines);
        assert!(r.failures[0].contains("not applied"), "{:?}", r.failures);
    }

    #[test]
    fn a_mod_session_is_not_the_reference_and_names_the_mod() {
        let mut s = SoftPatchSession::unpatched("a");
        assert!(s.is_reference());
        s.mods.push("live-enemies".into());
        s.session_sha256 = "b".into();
        assert!(!s.is_reference());
        assert_eq!(s.badge().as_deref(), Some("Patched: mod live-enemies"));
        s.applied.push("Game.ips".into());
        assert_eq!(
            s.badge().as_deref(),
            Some("Patched: Game.ips, mod live-enemies")
        );
    }
}
