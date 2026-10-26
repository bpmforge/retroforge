//! Recipe matching over a toy ISA (ticket W36-05): shape hashes survive a move
//! that re-points absolute operands, an opcode change does not match, ties go
//! to the nearest address hint, and the v2 atlas keeps its hashes through the
//! cache round trip. The real 6502 mask rule is tested in rf-nes.
//!
//! Toy opcodes (low nibble): 3 CALL abs(3) · 4 RET(1) · A load abs(3, falls).

use rf_core_api::coverage::{
    CoverageLog, InterruptEntry, InterruptKind, WindowMap, OPCODE, OPERAND,
};
use rf_core_api::CodeAddr;
use rf_rebuild::{
    build_atlas, entry, match_recipe, resolve_patches, Atlas, AtlasError, Confidence, DecodeMode,
    Decoded, EntryKind, Flow, Grade, HookPoints, Isa, PatchRefusal, PatchRow, Recipe,
    RecipeFunction, UnmatchedReason, ATLAS_FORMAT_VERSION,
};

struct Toy;

impl Isa for Toy {
    fn max_len(&self) -> usize {
        3
    }
    fn addr_mask(&self) -> u32 {
        0xFFFF
    }
    fn masked_operand(&self, insn: &[u8], _m: DecodeMode) -> std::ops::Range<usize> {
        if insn.len() == 3 {
            1..3
        } else {
            0..0
        }
    }
    fn decode(&self, b: &[u8], _cpu: u32, _m: DecodeMode) -> Option<Decoded> {
        let op = *b.first()? & 0x0F;
        let len = if matches!(op, 3 | 0xA) { 3 } else { 1 };
        if b.len() < len {
            return None;
        }
        let flow = match op {
            3 => Flow::Call {
                target: u32::from(u16::from_le_bytes([b[1], b[2]])),
            },
            4 => Flow::Return,
            _ => Flow::Fall,
        };
        Some(Decoded::plain(len as u8, flow))
    }
}

/// `entry`: `LDA entry; JSR entry; RET` with both operands pointing at the
/// function's own CPU address, followed by `tail`.
fn put_fn(img: &mut [u8], rom: usize, first_op: u8) {
    let cpu = 0x8000u16 + rom as u16;
    let [lo, hi] = cpu.to_le_bytes();
    img[rom..rom + 7].copy_from_slice(&[first_op, lo, hi, 0x03, lo, hi, 0x04]);
}

fn atlas_of(img: &[u8], entries: &[usize]) -> Atlas {
    let tagged: Vec<(usize, InterruptKind)> =
        entries.iter().map(|&e| (e, InterruptKind::Nmi)).collect();
    atlas_with(img, &tagged)
}

/// Like [`atlas_of`], with the interrupt each entry was reached by.
fn atlas_with(img: &[u8], entries: &[(usize, InterruptKind)]) -> Atlas {
    let mut log = CoverageLog::new(img.len());
    let mut m = WindowMap::EMPTY;
    for (p, s) in m.0.iter_mut().enumerate() {
        *s = (p as u32) * 0x1000;
    }
    log.note_windows(m);
    for &(rom, kind) in entries {
        log.note_interrupt(InterruptEntry {
            kind,
            target: CodeAddr {
                cpu: 0x8000 + rom as u16,
                pbr: 0,
                mode: 0,
                rom: Some(rom as u32),
            },
        });
        for (off, len) in [(0, 3), (3, 3), (6, 1)] {
            log.mark_prg(rom + off, OPCODE);
            for i in 1..len {
                log.mark_prg(rom + off + i, OPERAND);
            }
        }
    }
    build_atlas(&Toy, img, &log)
}

fn func_at(a: &Atlas, rom: u32) -> &rf_rebuild::Function {
    a.functions
        .iter()
        .find(|f| f.entry_rom == rom)
        .expect("function")
}

fn row(name: &str, f: &rf_rebuild::Function, exact: bool, hint: Option<u32>) -> RecipeFunction {
    RecipeFunction {
        grade: rf_rebuild::Grade::Observed,
        entry: None,
        insn_count: None,
        mode: None,
        confidence: None,
        name: name.to_string(),
        shape_hash: f.shape_hash,
        exact_hash: exact.then_some(f.byte_hash),
        address_hint: hint,
        hooks: HookPoints {
            entry: true,
            exit: true,
        },
        source: "test".to_string(),
    }
}

fn recipe(rows: Vec<RecipeFunction>) -> Recipe {
    Recipe {
        id: "toy".to_string(),
        engine: "toy".to_string(),
        functions: rows,
        ram: vec![],
        patches: vec![],
        ..Recipe::default()
    }
}

#[test]
fn a_moved_function_with_repointed_operands_matches_by_shape() {
    // The original, observed on one dump.
    let mut a_img = vec![0u8; 0x1000];
    put_fn(&mut a_img, 0x100, 0x0A);
    let a = atlas_of(&a_img, &[0x100]);
    let original = func_at(&a, 0x100).clone();
    assert_eq!(original.byte_len, 7);

    // The same function elsewhere with its operands re-pointed at itself.
    let mut b_img = vec![0u8; 0x1000];
    put_fn(&mut b_img, 0x300, 0x0A);
    let b = atlas_of(&b_img, &[0x300]);
    let moved = func_at(&b, 0x300);
    assert_ne!(original.byte_hash, moved.byte_hash, "the bytes differ");
    assert_eq!(original.shape_hash, moved.shape_hash, "the shape does not");

    let r = recipe(vec![row("tick", &original, true, Some(0x8100))]);
    let report = match_recipe(&b, &r);
    assert!(report.unmatched.is_empty());
    assert_eq!(report.function_of("tick"), Some(0));
    assert_eq!(report.matched[0].confidence, Confidence::Shape);
    // And in the atlas it was observed on, the exact hash raises confidence.
    assert_eq!(
        match_recipe(&a, &r).matched[0].confidence,
        Confidence::Exact
    );
}

#[test]
fn a_changed_opcode_does_not_match() {
    let mut a_img = vec![0u8; 0x1000];
    put_fn(&mut a_img, 0x100, 0x0A);
    let a = atlas_of(&a_img, &[0x100]);
    // Same length and flow, different opcode byte (0x1A is also a toy load).
    let mut c_img = vec![0u8; 0x1000];
    put_fn(&mut c_img, 0x100, 0x1A);
    let c = atlas_of(&c_img, &[0x100]);
    assert_ne!(func_at(&a, 0x100).shape_hash, func_at(&c, 0x100).shape_hash);

    let r = recipe(vec![row("tick", func_at(&a, 0x100), true, None)]);
    let report = match_recipe(&c, &r);
    assert!(report.matched.is_empty());
    assert_eq!(report.unmatched, ["tick"]);
}

#[test]
fn several_candidates_resolve_to_the_nearest_hint_then_the_exact_hash() {
    let mut img = vec![0u8; 0x1000];
    put_fn(&mut img, 0x100, 0x0A);
    put_fn(&mut img, 0x300, 0x0A);
    let a = atlas_of(&img, &[0x100, 0x300]);
    assert_eq!(a.functions.len(), 2);
    let (f100, f300) = (func_at(&a, 0x100).clone(), func_at(&a, 0x300).clone());
    assert_eq!(f100.shape_hash, f300.shape_hash);
    let idx = |rom: u32| a.functions.iter().position(|f| f.entry_rom == rom).unwrap() as u32;

    let pick = |hint: Option<u32>, exact_of: Option<&rf_rebuild::Function>| {
        let mut r = row("tick", &f100, false, hint);
        r.exact_hash = exact_of.map(|f| f.byte_hash);
        match_recipe(&a, &recipe(vec![r]))
    };
    let near_300 = pick(Some(0x82F0), None);
    assert_eq!(near_300.function_of("tick"), Some(idx(0x300)));
    assert_eq!(near_300.matched[0].candidates, 2);
    assert_eq!(
        pick(Some(0x8110), None).function_of("tick"),
        Some(idx(0x100))
    );
    assert_eq!(
        pick(None, None).function_of("tick"),
        Some(idx(0x100)),
        "no hint: the lowest entry, deterministically"
    );
    // The exact hash outranks a hint that points the other way.
    let exact = pick(Some(0x8100), Some(&f300));
    assert_eq!(exact.function_of("tick"), Some(idx(0x300)));
    assert_eq!(exact.matched[0].confidence, Confidence::Exact);
}

#[test]
fn the_v3_atlas_round_trips_its_hashes_and_a_v2_cache_entry_is_a_miss() {
    let mut img = vec![0u8; 0x1000];
    put_fn(&mut img, 0x100, 0x0A);
    let a = atlas_of(&img, &[0x100]);
    assert_ne!(func_at(&a, 0x100).shape_hash, 0);
    let bytes = a.to_bytes();
    assert_eq!(Atlas::from_bytes(&bytes), Ok(a.clone()));

    assert_eq!(ATLAS_FORMAT_VERSION, 5);
    assert_eq!(rf_rebuild::CACHE_SETTINGS, "code-atlas-v5");
    let mut v2 = bytes;
    v2[4..8].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(Atlas::from_bytes(&v2), Err(AtlasError::Version(2)));
}

fn patch(name: &str, function: &str, offset: u32, value: u8) -> PatchRow {
    PatchRow {
        grade: rf_rebuild::Grade::Observed,
        name: name.to_string(),
        mod_name: "m".to_string(),
        function: function.to_string(),
        offset,
        value,
        source: "test".to_string(),
    }
}

fn patched_recipe(f: &rf_rebuild::Function, exact: bool, rows: Vec<PatchRow>) -> Recipe {
    Recipe {
        patches: rows,
        ..recipe(vec![row("tick", f, exact, Some(f.entry_cpu))])
    }
}

#[test]
fn patch_rows_resolve_to_prg_offsets_in_the_stream_order() {
    let mut img = vec![0u8; 0x1000];
    put_fn(&mut img, 0x100, 0x0A);
    let a = atlas_of(&img, &[0x100]);
    let f = func_at(&a, 0x100).clone();
    // Stream bytes: 0..3 load, 3..6 call, 6 return.
    let r = patched_recipe(
        &f,
        true,
        vec![
            patch("first", "tick", 0, 0x1A),
            patch("ret", "tick", 6, 0x04),
        ],
    );
    let out = resolve_patches(&a, &r, "m").expect("resolves");
    let got: Vec<(&str, u32, u8)> = out
        .iter()
        .map(|p| (p.name.as_str(), p.prg_offset, p.value))
        .collect();
    assert_eq!(got, [("first", 0x100, 0x1A), ("ret", 0x106, 0x04)]);
}

#[test]
fn a_mod_is_refused_whole_for_every_way_a_row_can_fail() {
    let mut img = vec![0u8; 0x1000];
    put_fn(&mut img, 0x100, 0x0A);
    let a = atlas_of(&img, &[0x100]);
    let f = func_at(&a, 0x100).clone();
    let ok = patch("ok", "tick", 1, 9);
    let refuse = |r: &Recipe, atlas: &Atlas| resolve_patches(atlas, r, "m").unwrap_err();

    // No such mod.
    let r = patched_recipe(&f, true, vec![ok.clone()]);
    assert_eq!(
        resolve_patches(&a, &r, "other"),
        Err(PatchRefusal::NoSuchMod("other".into()))
    );
    // A row naming a function the recipe does not list.
    let r = patched_recipe(&f, true, vec![ok.clone(), patch("bad", "nope", 0, 1)]);
    assert!(matches!(
        refuse(&r, &a),
        PatchRefusal::UnknownFunction { .. }
    ));
    // Past the end of the function (7 bytes).
    let r = patched_recipe(&f, true, vec![ok.clone(), patch("far", "tick", 7, 1)]);
    assert!(matches!(
        refuse(&r, &a),
        PatchRefusal::OutOfFunction { len: 7, .. }
    ));
    // Two rows on one byte.
    let r = patched_recipe(&f, true, vec![ok.clone(), patch("dup", "tick", 1, 2)]);
    assert!(matches!(refuse(&r, &a), PatchRefusal::Overlap { .. }));
    // No exact hash in the recipe: shape alone does not vouch for the byte.
    let r = patched_recipe(&f, false, vec![ok.clone()]);
    assert!(matches!(refuse(&r, &a), PatchRefusal::NotExact { .. }));

    // A moved copy matches on shape only (the exact hash differs): refused.
    let mut b_img = vec![0u8; 0x1000];
    put_fn(&mut b_img, 0x300, 0x0A);
    let b = atlas_of(&b_img, &[0x300]);
    let r = patched_recipe(&f, true, vec![ok.clone()]);
    assert!(matches!(refuse(&r, &b), PatchRefusal::NotExact { .. }));
    // A different function altogether: unmatched.
    let mut c_img = vec![0u8; 0x1000];
    put_fn(&mut c_img, 0x100, 0x1A);
    let c = atlas_of(&c_img, &[0x100]);
    assert!(matches!(refuse(&r, &c), PatchRefusal::Unmatched { .. }));
    // Every refusal reads as a sentence naming the row.
    assert!(refuse(&r, &c).to_string().contains("`ok`"));
}

#[test]
fn identical_copies_are_ambiguous_not_guessed() {
    // Two functions with the very same bytes (operands point at one place).
    let mut img = vec![0u8; 0x1000];
    for rom in [0x100usize, 0x300] {
        img[rom..rom + 7].copy_from_slice(&[0x0A, 0x00, 0x90, 0x03, 0x00, 0x90, 0x04]);
    }
    let a = atlas_of(&img, &[0x100, 0x300]);
    let (f1, f2) = (func_at(&a, 0x100).clone(), func_at(&a, 0x300).clone());
    assert_eq!(f1.byte_hash, f2.byte_hash);
    let r = patched_recipe(&f1, true, vec![patch("p", "tick", 0, 1)]);
    assert_eq!(
        resolve_patches(&a, &r, "m"),
        Err(PatchRefusal::Ambiguous {
            row: "p".into(),
            function: "tick".into(),
            copies: 2
        })
    );
}

#[test]
fn an_entry_mode_picks_between_functions_of_one_shape() {
    // The same bytes entered in two decode modes are two atlas functions with
    // one shape hash (the 65816 case, W36-10).
    let mut img = vec![0u8; 0x1000];
    put_fn(&mut img, 0x100, 0x0A);
    let mut a = atlas_of(&img, &[0x100]);
    let mut twin = func_at(&a, 0x100).clone();
    twin.entry_mode = 3;
    a.functions.push(twin.clone());
    let base = func_at(&a, 0x100).clone();
    assert_eq!(base.shape_hash, twin.shape_hash);

    let with_mode = |mode: Option<u8>| {
        let mut r = row("f", &base, false, None);
        r.mode = mode;
        match_recipe(&a, &recipe(vec![r]))
    };
    // No mode: any candidate; the lowest entry (then lowest mode order) wins.
    assert_eq!(with_mode(None).matched.len(), 1);
    assert_eq!(with_mode(None).matched[0].candidates, 2);
    let m0 = with_mode(Some(base.entry_mode));
    let m3 = with_mode(Some(3));
    assert_eq!(
        a.functions[m0.matched[0].function as usize].entry_mode,
        base.entry_mode
    );
    assert_eq!(a.functions[m3.matched[0].function as usize].entry_mode, 3);
    assert_ne!(m0.matched[0].function, m3.matched[0].function);
    // A mode no function was entered in: unmatched, whatever the shape says.
    assert_eq!(with_mode(Some(4)).unmatched, ["f"]);
}

#[test]
fn rows_with_one_shape_match_only_at_their_own_address() {
    // Two rows nothing but an address tells apart (a pair of identical
    // interrupt stubs): where one moved, it must not be handed to the other.
    let mut obs = vec![0u8; 0x1000];
    put_fn(&mut obs, 0x100, 0x0A);
    let f = func_at(&atlas_of(&obs, &[0x100]), 0x100).clone();
    let a = row("a", &f, true, Some(0x8100));
    let b = row("b", &f, true, Some(0x8300));
    let r = recipe(vec![a, b]);

    // Same ROM: `a` is where it was observed; `b`'s twin is not at 0x8300.
    let same = match_recipe(&atlas_of(&obs, &[0x100]), &r);
    assert_eq!(same.matched.len(), 1);
    assert_eq!(same.matched[0].name, "a");
    assert_eq!(same.unmatched, ["b"]);

    // The function moved: neither row is at its hint, so neither guesses.
    let mut moved = vec![0u8; 0x1000];
    put_fn(&mut moved, 0x500, 0x0A);
    let m = match_recipe(&atlas_of(&moved, &[0x500]), &r);
    assert!(m.matched.is_empty(), "{m:?}");

    // A lone row still follows a move (the W36-05 behaviour).
    let lone = recipe(vec![row("a", &f, false, Some(0x8100))]);
    let m = match_recipe(&atlas_of(&moved, &[0x500]), &lone);
    assert_eq!(m.matched.len(), 1);
}

// ---- recipe format v1 (ticket W38-00) -----------------------------------

const TWIN_IMG: usize = 0x1000;

/// Two byte-identical stubs, `first` reached by an NMI and `second` by an
/// IRQ.
fn twin_atlas(first: usize, second: usize) -> Atlas {
    let mut img = vec![0u8; TWIN_IMG];
    put_fn(&mut img, first, 0x0A);
    put_fn(&mut img, second, 0x0A);
    atlas_with(
        &img,
        &[(first, InterruptKind::Nmi), (second, InterruptKind::Irq)],
    )
}

fn twin_rows(a: &Atlas, with_entry: bool) -> Recipe {
    let (nmi, irq) = (func_at(a, 0x100), func_at(a, 0x300));
    let mut rn = row("nmi", nmi, true, Some(nmi.entry_cpu));
    let mut ri = row("irq", irq, true, Some(irq.entry_cpu));
    if with_entry {
        rn.entry = Some(EntryKind::Nmi);
        ri.entry = Some(EntryKind::Irq);
    }
    recipe(vec![rn, ri])
}

#[test]
fn the_atlas_tells_an_nmi_target_from_an_irq_one() {
    let a = twin_atlas(0x100, 0x300);
    let (nmi, irq) = (func_at(&a, 0x100), func_at(&a, 0x300));
    // (The toy stub also JSRs itself, so CALL rides along.)
    let intr = entry::INTERRUPT | entry::CALL;
    assert_eq!(nmi.entry_kinds, intr | entry::NMI);
    assert_eq!(irq.entry_kinds, intr | entry::IRQ);
    assert_eq!(
        EntryKind::sole_interrupt(nmi.entry_kinds),
        Some(EntryKind::Nmi)
    );
    assert_eq!(
        EntryKind::sole_interrupt(entry::NMI | entry::IRQ),
        None,
        "one handler behind two vectors names no single kind"
    );
}

#[test]
fn twin_stubs_a_hack_moved_resolve_by_entry_kind_not_by_address() {
    // As observed: NMI stub at $8100, IRQ stub at $8300.
    let original = twin_atlas(0x100, 0x300);
    // The hack swapped them: the NMI vector now reaches $8300.
    let hacked = twin_atlas(0x300, 0x100);
    let id =
        |a: &Atlas, rom: u32| a.functions.iter().position(|f| f.entry_rom == rom).unwrap() as u32;

    // Without entry kinds the rows are pinned to their hints, so each takes
    // the function at its old address: the NMI row lands on the IRQ stub.
    let legacy = match_recipe(&hacked, &twin_rows(&original, false));
    assert_eq!(legacy.function_of("nmi"), Some(id(&hacked, 0x100)));

    // With them, each follows the vector that reaches it.
    let v1 = match_recipe(&hacked, &twin_rows(&original, true));
    assert_eq!(v1.function_of("nmi"), Some(id(&hacked, 0x300)));
    assert_eq!(v1.function_of("irq"), Some(id(&hacked, 0x100)));
    // And on the dump they were observed on, nothing changes.
    let same = match_recipe(&original, &twin_rows(&original, true));
    assert_eq!(same.function_of("nmi"), Some(id(&original, 0x100)));
    assert_eq!(same.function_of("irq"), Some(id(&original, 0x300)));
}

#[test]
fn a_row_with_an_entry_kind_no_function_has_is_unmatched() {
    let original = twin_atlas(0x100, 0x300);
    let mut img = vec![0u8; TWIN_IMG];
    put_fn(&mut img, 0x300, 0x0A);
    // Only an IRQ stub survives: the NMI row must not take it.
    let hacked = atlas_with(&img, &[(0x300, InterruptKind::Irq)]);
    let report = match_recipe(&hacked, &twin_rows(&original, true));
    assert_eq!(report.unmatched, ["nmi"]);
    assert!(report.function_of("irq").is_some());
}

#[test]
fn a_candidate_row_never_drives_a_patch_or_a_writing_hook() {
    let mut img = vec![0u8; TWIN_IMG];
    put_fn(&mut img, 0x100, 0x0A);
    let a = atlas_of(&img, &[0x100]);
    let f = func_at(&a, 0x100);
    let mut p = patch("lead", "tick", 2, 0x77);
    p.mod_name = "m".to_string();

    // Observed and confirmed rows are applied.
    for g in [Grade::Observed, Grade::Confirmed] {
        let mut r = patched_recipe(
            f,
            true,
            vec![PatchRow {
                grade: g,
                ..p.clone()
            }],
        );
        r.functions[0].grade = g;
        assert!(resolve_patches(&a, &r, "m").is_ok(), "{g:?}");
    }
    // A candidate patch row, or a patch on a candidate function, is refused.
    let mut cand_patch = patched_recipe(f, true, vec![p.clone()]);
    cand_patch.patches[0].grade = Grade::Candidate;
    assert!(matches!(
        resolve_patches(&a, &cand_patch, "m"),
        Err(PatchRefusal::Candidate { .. })
    ));
    let mut cand_fn = patched_recipe(f, true, vec![p]);
    cand_fn.functions[0].grade = Grade::Candidate;
    assert!(matches!(
        resolve_patches(&a, &cand_fn, "m"),
        Err(PatchRefusal::Candidate { .. })
    ));

    // A candidate function row still matches (labels, read-only hooks) but a
    // write-capable host gets nothing for it.
    let report = match_recipe(&a, &cand_fn);
    assert!(report.function_of("tick").is_some());
    assert_eq!(report.function_of_writable("tick"), None);
    let ok = match_recipe(&a, &patched_recipe(f, true, vec![]));
    assert!(ok.function_of_writable("tick").is_some());
}

#[test]
fn an_unmatched_row_is_removed_not_reached_or_changed() {
    let mut orig_img = vec![0u8; TWIN_IMG];
    put_fn(&mut orig_img, 0x100, 0x0A);
    let original = atlas_with(&orig_img, &[(0x100, InterruptKind::Nmi)]);
    let mut r = row("nmi", func_at(&original, 0x100), true, Some(0x8100));
    r.insn_count = Some(func_at(&original, 0x100).insn_count);
    assert_eq!(r.insn_count, Some(3));
    let why = |img: &[u8], ran: &[usize], row: &RecipeFunction| {
        let run = |o: usize| ran.iter().any(|&b| (b..b + 7).contains(&o));
        rf_rebuild::classify_unmatched(&Toy, img, &run, row)
    };

    // Removed: nothing of that shape anywhere.
    assert_eq!(
        why(&vec![0u8; TWIN_IMG], &[], &r),
        Some(UnmatchedReason::Removed)
    );
    // Not reached: the code is there, moved, and the drive never ran it.
    let mut moved = vec![0u8; TWIN_IMG];
    put_fn(&mut moved, 0x500, 0x0A);
    assert_eq!(why(&moved, &[], &r), Some(UnmatchedReason::NotReached));
    // Changed: it ran, but not the way the row insists on (an IRQ stub now).
    let mut irq = r.clone();
    irq.entry = Some(EntryKind::Irq);
    let ran = atlas_with(&moved, &[(0x500, InterruptKind::Nmi)]);
    assert_eq!(
        match_recipe(&ran, &recipe(vec![irq.clone()])).unmatched,
        ["nmi"]
    );
    assert_eq!(why(&moved, &[0x500], &irq), Some(UnmatchedReason::Changed));
    // No instruction count, no scan.
    r.insn_count = None;
    assert_eq!(why(&moved, &[], &r), None);
}
