//! Import a REAL community Mesen HD pack end to end (ticket W9-06
//! criterion 3).
//!
//! ## Why a fetched pack and not a fixture
//!
//! The criterion says "a real community pack", and it means it: proving
//! the importer against a pack we authored would prove it against its own
//! author. The fixtures in `hdpack.rs`'s unit tests are ours; this one is
//! not.
//!
//! **No licence-clear HD pack exists.** Every pack in the wild is a
//! derived work of copyrighted commercial game art, and the three
//! candidates surveyed (TasticHacks/Contra80s, AxlRocks/Megaman-Super,
//! ModernRetroDesign/ZII-mesen) all report NO LICENSE on GitHub. Brad
//! ruled 2026-08-21 that the fetch-only posture already carried by 81
//! entries in `tests/rom-manifest.toml` applies here — fetch from origin,
//! never vendor or re-host, land under gitignored `roms/`. The honest
//! caveat that ruling turned on: those 81 are hardware-conformance test
//! ROMs and this one is game art. That is a real difference, and it was
//! Brad's call rather than this ticket's.
//!
//! ## This test runs only under `--ignored`
//!
//! It needs the network, like every other artifact-backed suite here, so
//! the nine-command gate does NOT cover it. Say that out loud rather than
//! let a later pass assume otherwise — that assumption is exactly what
//! hid two real defects in the SNES hires path (W7-06, same day).
//!
//! ```text
//! cargo run -p rf-harness --bin fetch-test-roms -- mesen-hdpack-megaman-super
//! cargo test -p rf-enhance --test mesen_hdpack_real -- --ignored
//! ```
//!
//! ## What it actually pins
//!
//! Not "the pack loads". The numbers below were read off the real file
//! and every one is a claim the importer has to keep making: **7,349**
//! tile rules, **301** conditions, 89 images, and — the load-bearing one
//! — **zero missing images**. See the separator note on
//! [`image_dimensions`] for why that last assertion is the one that would
//! quietly rot into a fake PARTIAL.
//!
//! ## What the real pack found that our own fixtures could not
//!
//! All four of these were defects or gaps, and none was visible before a
//! pack from the wild was pointed at the importer:
//!
//! 1. **`<tile>` has NINE fields, not seven.** Every one of the 7,349
//!    rules here does. The published spec still documents seven; HD Pack
//!    Builder appends two of its own. `parse_hires` required equality and
//!    so rejected the entire real-world corpus at line 487. Mesen's own
//!    `HdPackLoader.cpp` reads tokens positionally and never checks that
//!    none remain — being stricter than the reference implementation is a
//!    defect in a compatibility importer, not rigour.
//! 2. **`<background>` (702 uses), `<bgm>` (17) and `<patch>` (2) were
//!    dropped in silence** by the parser's catch-all arm — the exact
//!    "applies quietly" failure criterion 2 forbids. They are now counted
//!    and reported.
//! 3. **A duplicate `<condition>` declaration** (`Wily03ScreenChecker1D`),
//!    which no hand-written fixture would have thought to contain.
//! 4. **63 commented-out `<tile>` lines** using `#`, which the parser
//!    happens to skip correctly because they do not begin with `<`.
//!
//! The result is a PARTIAL with 3,655 named gaps over 7,349 rules — not a
//! failure, but the honest report criterion 2 asks for.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rf_enhance::hdpack::{import, parse_hires, ImageInfo, Import, Unsatisfied};

/// The fetched pack, or `None` when it has not been fetched.
fn pack_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RF_MESEN_HDPACK") {
        return Some(p.into());
    }
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../roms/nes/mesen-hdpack-megaman-super");
    p.exists().then_some(p)
}

/// Width and height straight out of a PNG's IHDR.
///
/// This reads the 8-byte signature, then the IHDR chunk's two big-endian
/// u32s at offsets 16 and 20 — no decoder, because `hdpack` deliberately
/// decodes no pixels and this test has no business owning one either.
fn png_dimensions(bytes: &[u8]) -> Option<ImageInfo> {
    if bytes.len() < 24 || &bytes[0..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some(ImageInfo {
        width: w,
        height: h,
    })
}

/// Every image in the pack, keyed the way `hires.txt` spells it.
///
/// **Mesen writes `<img>` paths with BACKSLASHES** — this pack's
/// subdirectory refs are `Submerged\Foo.png`, and `parse_hires` stores
/// the raw string. Walking the checkout on a unix host yields
/// `Submerged/Foo.png`, so joining with the host separator would leave
/// all twenty subdirectory rules reporting `MissingImage` and the test
/// would "discover" a PARTIAL that is purely a separator mismatch — a
/// fake finding that looks exactly like a real one. The keys are joined
/// with `\` for that reason, and the zero-missing-images assertion below
/// is what keeps this honest.
fn image_dimensions(root: &Path) -> BTreeMap<String, ImageInfo> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    // The walk is a stack drained by `pop`, so it advances on every
    // iteration by construction (law 8) — there is no index to forget.
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == ".git") {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if path
                .extension()
                .is_none_or(|e| !e.eq_ignore_ascii_case("png"))
            {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Some(info) = png_dimensions(&bytes) else {
                continue;
            };
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let key = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("\\");
            out.insert(key, info);
        }
    }
    out
}

#[test]
#[ignore = "local: fetch-test-roms -- mesen-hdpack-megaman-super first"]
fn a_real_community_pack_imports_end_to_end() {
    let Some(dir) = pack_dir() else {
        eprintln!("SKIP: fetch-test-roms -- mesen-hdpack-megaman-super");
        return;
    };
    let text = std::fs::read_to_string(dir.join("hires.txt")).expect("hires.txt readable");
    let pack = parse_hires(&text).expect("a real v106 pack must parse");

    // Shape of the real file, read off it rather than hoped for.
    assert_eq!(pack.version, 106);
    assert_eq!(pack.scale, 1);
    assert_eq!(pack.images.len(), 89, "<img> declarations");
    // 7,412 lines in the file carry a `<tile>` tag; 63 of them are
    // commented out with a leading `#`, which is why this is not 7,412.
    // Both numbers were counted off the real file — a round number here
    // would mean nobody checked.
    assert_eq!(
        pack.tiles.len(),
        7349,
        "<tile> rules (7412 minus 63 commented out)"
    );
    // 302 `<condition>` lines, 301 distinct names: the pack declares
    // `Wily03ScreenChecker1D` TWICE. `conditions` is keyed by name, so
    // the second wins. Pinned deliberately rather than smoothed over —
    // a real pack shipping a duplicate declaration is exactly the kind
    // of thing only a pack from the wild would have told us, and the
    // spec says nothing about which one hardware-Mesen keeps.
    assert_eq!(
        pack.conditions.len(),
        301,
        "<condition> declarations (302 lines, one name declared twice)"
    );

    let images = image_dimensions(&dir);
    assert!(
        images.len() >= 89,
        "the fetched checkout must carry at least the declared images; found {}",
        images.len()
    );

    // NOTE ON WHAT IS DELIBERATELY NOT FETCHED: the pack's 708
    // `<background>` lines point at `Backdrops\\*.png`, and that
    // directory is not among the artifact's subpaths. That is not an
    // oversight — this build does not implement `<background>` at all,
    // so those images would be dead weight, and the directive is
    // REPORTED below rather than silently skipped. Implementing
    // backgrounds means adding "Backdrops" to the artifact's subpaths in
    // the same commit.
    let result = import(pack, &images);
    let Import::Partial { pack, unsatisfied } = result else {
        panic!(
            "this pack MUST report PARTIAL: it uses 17 <bgm> and 2 <patch> \
             directives this build does not implement, and 302 conditions \
             whose kinds it cannot evaluate"
        );
    };
    assert_eq!(pack.tiles.len(), 7349, "import must not drop rules");

    // THE LOAD-BEARING ASSERTION. A missing image here means either the
    // artifact's subpaths stopped covering the pack, or the `\` join in
    // `image_dimensions` regressed. Both produce a PARTIAL that looks
    // like a finding and is really a bug in this test.
    let missing: Vec<_> = unsatisfied
        .iter()
        .filter(|u| matches!(u, Unsatisfied::MissingImage { .. }))
        .collect();
    assert!(
        missing.is_empty(),
        "every one of the 89 declared images must resolve; {} did not: {:?}",
        missing.len(),
        &missing[..missing.len().min(5)]
    );

    // No rule may point outside the image it names.
    let oob: Vec<_> = unsatisfied
        .iter()
        .filter(|u| matches!(u, Unsatisfied::RegionOutOfBounds { .. }))
        .collect();
    assert!(
        oob.is_empty(),
        "{} rule(s) fall outside their image: {:?}",
        oob.len(),
        &oob[..oob.len().min(5)]
    );

    // Criterion 2, proved against a pack from the wild rather than ours:
    // the directives this build does not implement are REPORTED, with
    // counts, not dropped. Before W9-06's third pass the parser's
    // catch-all arm swallowed all nineteen of these silently.
    let mut directives: Vec<(String, usize)> = unsatisfied
        .iter()
        .filter_map(|u| match u {
            Unsatisfied::UnsupportedDirective { directive, count } => {
                Some((directive.clone(), *count))
            }
            _ => None,
        })
        .collect();
    directives.sort();
    assert_eq!(
        directives,
        vec![
            ("background".to_string(), 702),
            ("bgm".to_string(), 17),
            ("patch".to_string(), 2),
        ],
        "the real pack's unimplemented directives must be named and counted"
    );

    // Every unevaluated condition names a condition the pack actually
    // declares — a report that invented names would be worse than none.
    let names: Vec<&str> = unsatisfied
        .iter()
        .filter_map(|u| match u {
            Unsatisfied::UnevaluatedCondition { condition, .. } => Some(condition.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        !names.is_empty(),
        "this build evaluates only {:?}, and the pack uses none of them, so \
         its conditioned rules must be reported",
        rf_enhance::hdpack::EVALUATED_CONDITIONS
    );
    for name in &names {
        assert!(
            pack.conditions.contains_key(*name),
            "reported condition {name:?} is not declared by the pack"
        );
    }

    eprintln!(
        "{}",
        Import::Partial { pack, unsatisfied }
            .summary()
            .lines()
            .next()
            .unwrap_or("")
    );
}
