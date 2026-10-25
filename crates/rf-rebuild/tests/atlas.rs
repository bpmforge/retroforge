//! Atlas builder tests over a toy ISA (rf-rebuild cannot see a console
//! core; the real 6502 `Isa` is tested in rf-nes).
//!
//! Toy opcodes use the low nibble of the first byte, so *every* byte value
//! decodes and a random image is dense with control flow:
//! 0 NOP(1) · 1 BR rel(2) · 2 JMP abs(3) · 3 CALL abs(3) · 4 RET(1) ·
//! 5 IJMP(3) · 6 BRK(1) · 7 HALT(1) · 8 RTI(1) · 9 NOP(2) · A NOP(3) ·
//! B-F NOP(1).

use rf_core_api::coverage::{
    CoverageLog, IndirectEdge, IndirectKind, InterruptEntry, InterruptKind, WindowMap, NO_ROM,
    OPCODE, OPERAND,
};
use rf_core_api::CodeAddr;
use rf_rebuild::{
    build_atlas, entry, superset_decode, Atlas, AtlasError, DecodeMode, Decoded, EdgeKind, Flow,
    Isa, Provenance, SiteKind, Target, Why, ATLAS_FORMAT_VERSION, CACHE_SETTINGS,
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
    fn decode(&self, b: &[u8], cpu: u32, _m: DecodeMode) -> Option<Decoded> {
        let op = *b.first()? & 0x0F;
        let len = match op {
            1 | 9 => 2,
            2 | 3 | 5 | 0xA => 3,
            _ => 1,
        };
        if b.len() < len {
            return None;
        }
        let abs = || u32::from(u16::from_le_bytes([b[1], b[2]]));
        let flow = match op {
            1 => Flow::Branch {
                target: (cpu as i32 + 2 + i32::from(b[1] as i8)) as u32 & 0xFFFF,
            },
            2 => Flow::Jump { target: abs() },
            3 => Flow::Call { target: abs() },
            4 => Flow::Return,
            5 => Flow::IndirectJump,
            6 => Flow::Break,
            7 => Flow::Halt,
            8 => Flow::ReturnInterrupt,
            _ => Flow::Fall,
        };
        Some(Decoded::plain(len as u8, flow))
    }
}

/// Deterministic xorshift64*; local so no wall clock or host RNG is involved.
struct Xs(u64);
impl Xs {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Window map showing `len`-byte image linearly at `$8000` from `base`.
fn linear(base: u32) -> WindowMap {
    let mut m = WindowMap::EMPTY;
    for (p, s) in m.0.iter_mut().enumerate() {
        *s = base + (p as u32) * 0x1000;
    }
    m
}

fn at(cpu: u16, rom: u32) -> CodeAddr {
    CodeAddr {
        cpu,
        pbr: 0,
        mode: 0,
        rom: Some(rom),
    }
}

fn mark(log: &mut CoverageLog, off: usize, len: usize) {
    log.mark_prg(off, OPCODE);
    for i in 1..len {
        log.mark_prg(off + i, OPERAND);
    }
}

fn reset(log: &mut CoverageLog, rom: u32) {
    log.note_interrupt(InterruptEntry {
        kind: InterruptKind::Reset,
        target: at(0x8000 + rom as u16, rom),
    });
}

fn image(n: usize) -> Vec<u8> {
    vec![0u8; n] // all NOP
}

fn put(img: &mut [u8], off: usize, b: &[u8]) {
    img[off..off + b.len()].copy_from_slice(b);
}

fn fn_at(a: &Atlas, rom: u32) -> &rf_rebuild::Function {
    a.functions
        .iter()
        .find(|f| f.entry_rom == rom)
        .unwrap_or_else(|| panic!("no function at {rom:#x}"))
}

fn insn_roms(a: &Atlas) -> Vec<u32> {
    let mut v: Vec<u32> = a.insns().map(|i| i.rom).collect();
    v.sort_unstable();
    v
}

/// Structural invariants any atlas must satisfy, on any input.
fn check_invariants(a: &Atlas) {
    assert!(!a.stats.budget_hit, "a step budget was hit");
    assert!(a.blocks.windows(2).all(|w| w[0].start_rom < w[1].start_rom));
    assert!(a
        .functions
        .windows(2)
        .all(|w| w[0].entry_rom <= w[1].entry_rom));
    let roms = insn_roms(a);
    assert!(
        roms.windows(2).all(|w| w[0] != w[1]),
        "an instruction sits in two blocks"
    );
    assert_eq!(
        roms.len() as u32,
        a.stats.observed_insns + a.stats.inferred_insns
    );
    for f in &a.functions {
        assert!(f.blocks.windows(2).all(|w| w[0] < w[1]));
        assert!(f.blocks.iter().all(|&b| (b as usize) < a.blocks.len()));
        assert!(f.blocks.contains(
            &(a.blocks
                .iter()
                .position(|b| b.start_rom == f.entry_rom)
                .unwrap() as u32)
        ));
    }
    for b in &a.blocks {
        for e in &b.edges {
            if let Target::Rom { rom, .. } = e.to {
                assert!(rom < a.prg_len);
            }
        }
    }
}

#[test]
fn observed_and_inferred_instructions_and_edges() {
    // 8000: BR +2 -> 8004 (taken side executed, fall side never)
    // 8002: NOP(inferred)  8003: NOP(inferred, falls into 8004's leader)
    // 8004: CALL 8010      8007: RET-less tail: HALT (inferred)
    // 8010: RET
    let mut img = image(0x1000);
    put(&mut img, 0x00, &[0x01, 0x02]);
    put(&mut img, 0x04, &[0x03, 0x10, 0x80]);
    put(&mut img, 0x07, &[0x07]);
    put(&mut img, 0x10, &[0x04]);
    let mut log = CoverageLog::new(img.len());
    log.note_windows(linear(0));
    reset(&mut log, 0);
    mark(&mut log, 0x00, 2);
    mark(&mut log, 0x04, 3);
    mark(&mut log, 0x10, 1);
    let a = build_atlas(&Toy, &img, &log);
    check_invariants(&a);

    let prov = |rom| a.insns().find(|i| i.rom == rom).map(|i| i.prov);
    assert_eq!(prov(0x00), Some(Provenance::Observed));
    assert_eq!(prov(0x02), Some(Provenance::Inferred), "not-taken side");
    assert_eq!(prov(0x07), Some(Provenance::Inferred), "after the call");
    assert_eq!(prov(0x10), Some(Provenance::Observed));

    // Entries: reset target + the call target.
    let main = fn_at(&a, 0);
    assert_eq!(main.entry_kinds, entry::INTERRUPT);
    let callee = fn_at(&a, 0x10);
    assert_eq!(callee.entry_kinds, entry::CALL);
    // The callee is not inside the caller's extent (calls are not followed).
    assert!(!main.blocks.contains(&callee.blocks[0]));
    // The caller does reach the after-call instruction.
    let after = a.blocks.iter().position(|b| b.start_rom == 0x07).unwrap() as u32;
    assert!(main.blocks.contains(&after));

    // Edge provenance: taken branch observed (both ends ran), the not-taken
    // fall edge is inferred.
    let b0 = &a.blocks[0];
    let taken = b0.edges.iter().find(|e| e.kind == EdgeKind::Taken).unwrap();
    let fall = b0.edges.iter().find(|e| e.kind == EdgeKind::Fall).unwrap();
    assert_eq!(taken.prov, Provenance::Observed);
    assert_eq!(fall.prov, Provenance::Inferred);
    assert_eq!(a.stats.observed_uncovered, 0);
    assert_eq!(a.stats.observed_insns, 3);
}

#[test]
fn indirect_sites_keep_their_full_recorded_target_set() {
    let mut img = image(0x1000);
    put(&mut img, 0x00, &[0x05, 0x00, 0x03]); // IJMP ($0300)
    put(&mut img, 0x100, &[0x04]);
    put(&mut img, 0x200, &[0x04]);
    let mut log = CoverageLog::new(img.len());
    log.note_windows(linear(0));
    reset(&mut log, 0);
    mark(&mut log, 0, 3);
    mark(&mut log, 0x100, 1);
    mark(&mut log, 0x200, 1);
    for t in [0x100u32, 0x200] {
        log.note_indirect(IndirectEdge {
            site: at(0x8000, 0),
            target: at(0x8000 + t as u16, t),
            kind: IndirectKind::JmpIndirect,
        });
    }
    let a = build_atlas(&Toy, &img, &log);
    check_invariants(&a);
    let site = &a.indirect_sites[0];
    assert_eq!((site.site_rom, site.kind), (0, SiteKind::JmpIndirect));
    assert_eq!(site.targets.len(), 2);
    assert_eq!(fn_at(&a, 0x100).entry_kinds, entry::INDIRECT);
    assert_eq!(fn_at(&a, 0x200).entry_kinds, entry::INDIRECT);
}

#[test]
fn plain_rts_returns_are_not_entries_but_rts_tricks_are() {
    // 0: CALL 0x20 ; 3: HALT       0x20: RET -> returns to 3 (plain)
    // 0x10: RET used as an RTS trick landing on 0x40 (no JSR before it)
    let mut img = image(0x1000);
    put(&mut img, 0x00, &[0x03, 0x20, 0x80]);
    put(&mut img, 0x03, &[0x07]);
    put(&mut img, 0x10, &[0x04]);
    put(&mut img, 0x20, &[0x04]);
    put(&mut img, 0x40, &[0x07]);
    let mut log = CoverageLog::new(img.len());
    log.note_windows(linear(0));
    reset(&mut log, 0);
    mark(&mut log, 0, 3);
    mark(&mut log, 3, 1);
    mark(&mut log, 0x10, 1);
    mark(&mut log, 0x20, 1);
    mark(&mut log, 0x40, 1);
    log.note_indirect(IndirectEdge {
        site: at(0x8020, 0x20),
        target: at(0x8003, 3),
        kind: IndirectKind::Rts,
    });
    log.note_indirect(IndirectEdge {
        site: at(0x8010, 0x10),
        target: at(0x8040, 0x40),
        kind: IndirectKind::Rts,
    });
    let a = build_atlas(&Toy, &img, &log);
    check_invariants(&a);
    assert!(
        a.functions.iter().all(|f| f.entry_rom != 3),
        "the return site after a JSR is mid-function"
    );
    assert_eq!(fn_at(&a, 0x40).entry_kinds, entry::INDIRECT);
    // Both RTS sites keep their targets.
    assert_eq!(a.indirect_sites.len(), 2);
}

#[test]
fn executed_code_no_entry_reaches_becomes_an_orphan_function() {
    // Code at 0x300 ran (entered from RAM code, which the recorder does not
    // log as an edge) but nothing in ROM points at it.
    let mut img = image(0x1000);
    put(&mut img, 0x2FF, &[0x07]); // ends the chain from the reset code
    put(&mut img, 0x300, &[0x00, 0x04]);
    let mut log = CoverageLog::new(img.len());
    log.note_windows(linear(0));
    reset(&mut log, 0);
    mark(&mut log, 0, 1);
    mark(&mut log, 0x300, 1);
    mark(&mut log, 0x301, 1);
    let a = build_atlas(&Toy, &img, &log);
    check_invariants(&a);
    assert_eq!(a.stats.observed_uncovered, 0);
    assert_eq!(fn_at(&a, 0x300).entry_kinds, entry::ORPHAN);
    assert_eq!(a.stats.orphan_entries, 1);
}

#[test]
fn volatile_ram_runs_are_regions_not_functions() {
    let img = image(0x1000);
    let mut log = CoverageLog::new(img.len());
    log.note_windows(linear(0));
    log.mark_ram(0x0300, OPCODE);
    log.mark_ram(0x0301, OPCODE);
    log.mark_ram(0x0310, OPCODE);
    let a = build_atlas(&Toy, &img, &log);
    assert_eq!(a.volatile.len(), 2);
    assert_eq!((a.volatile[0].start, a.volatile[0].len), (0x300, 2));
    assert_eq!((a.volatile[1].start, a.volatile[1].len), (0x310, 1));
    assert!(a.functions.is_empty());
}

#[test]
fn calls_to_ram_are_unresolved_not_followed() {
    let mut img = image(0x1000);
    put(&mut img, 0, &[0x03, 0x00, 0x03]); // CALL $0300
    let mut log = CoverageLog::new(img.len());
    log.note_windows(linear(0));
    reset(&mut log, 0);
    mark(&mut log, 0, 3);
    let a = build_atlas(&Toy, &img, &log);
    check_invariants(&a);
    let call = a.blocks[0]
        .edges
        .iter()
        .find(|e| e.kind == EdgeKind::Call)
        .unwrap();
    assert_eq!(
        call.to,
        Target::Unresolved {
            cpu: 0x300,
            why: Why::NotRom
        }
    );
    // (The image's last NOP also runs off the end; that is a separate edge.)
    assert!(a.stats.unresolved_static >= 1);
}

// ---- bank context ----------------------------------------------------------

/// Two 32 KiB banks like GxROM: a CALL $9000 at the top of the bank-1 copy
/// must land in bank 1's $9000, not bank 0's.
#[test]
fn a_static_call_resolves_inside_the_banks_the_site_was_mapped_with() {
    let mut img = image(0x1_0000);
    for base in [0usize, 0x8000] {
        put(&mut img, base, &[0x03, 0x00, 0x90]); // CALL $9000
        put(&mut img, base + 0x1000, &[0x04]);
    }
    let mut log = CoverageLog::new(img.len());
    log.note_windows(linear(0));
    log.note_windows(linear(0x8000));
    // Only the bank-1 copy of the CALL executed.
    log.note_interrupt(InterruptEntry {
        kind: InterruptKind::Reset,
        target: at(0x8000, 0x8000),
    });
    mark(&mut log, 0x8000, 3);
    let a = build_atlas(&Toy, &img, &log);
    check_invariants(&a);
    let call = a
        .blocks
        .iter()
        .flat_map(|b| &b.edges)
        .find(|e| e.kind == EdgeKind::Call)
        .unwrap();
    assert_eq!(call.to.rom(), Some(0x9000));
    assert_eq!(a.stats.unresolved_static, 0);
}

#[test]
fn a_call_from_a_fixed_bank_into_a_switched_one_is_ambiguous_not_guessed() {
    // $8000-$9FFF switches between two banks; $E000-$FFFF is fixed (MMC3
    // shape). A CALL $8010 from the fixed page depends on a bank write.
    let mut img = image(0x6000);
    put(&mut img, 0x4000, &[0x03, 0x10, 0x80]); // fixed bank: CALL $8010
    let map = |sw: u32| {
        let mut m = WindowMap::EMPTY;
        m.0[0] = sw;
        m.0[1] = sw + 0x1000;
        m.0[6] = 0x4000;
        m.0[7] = 0x5000;
        m
    };
    let mut log = CoverageLog::new(img.len());
    log.note_windows(map(0));
    log.note_windows(map(0x2000));
    log.note_interrupt(InterruptEntry {
        kind: InterruptKind::Reset,
        target: at(0xE000, 0x4000),
    });
    mark(&mut log, 0x4000, 3);
    let a = build_atlas(&Toy, &img, &log);
    check_invariants(&a);
    let call = a
        .blocks
        .iter()
        .flat_map(|b| &b.edges)
        .find(|e| e.kind == EdgeKind::Call)
        .unwrap();
    assert_eq!(
        call.to,
        Target::Unresolved {
            cpu: 0x8010,
            why: Why::Ambiguous
        }
    );
    assert_eq!(a.stats.ambiguous_static, 1);
}

#[test]
fn fall_through_across_a_page_boundary_follows_the_mapping() {
    // Page 0 at ROM 0, page 1 at ROM 0x4000 (not contiguous). A 3-byte NOP
    // at $8FFE continues at $9001, which is ROM 0x4001.
    let mut img = image(0x5000);
    put(&mut img, 0xFFE, &[0x0A, 0x00, 0x00]);
    put(&mut img, 0x4001, &[0x04]);
    let mut m = WindowMap::EMPTY;
    m.0[0] = 0;
    m.0[1] = 0x4000;
    let mut log = CoverageLog::new(img.len());
    log.note_windows(m);
    log.note_interrupt(InterruptEntry {
        kind: InterruptKind::Reset,
        target: at(0x8FFE, 0xFFE),
    });
    mark(&mut log, 0xFFE, 3);
    let a = build_atlas(&Toy, &img, &log);
    check_invariants(&a);
    assert!(insn_roms(&a).contains(&0x4001));
    assert!(!insn_roms(&a).contains(&0x1001));
}

// ---- termination -------------------------------------------------------------

fn terminates_on(img: &[u8]) -> Atlas {
    let mut log = CoverageLog::new(img.len());
    log.note_windows(linear(0));
    reset(&mut log, 0);
    let a = build_atlas(&Toy, img, &log);
    check_invariants(&a);
    a
}

#[test]
fn self_looping_patterns_terminate() {
    let n = 0x8000;
    let tile = |pat: &[u8]| -> Vec<u8> { pat.iter().copied().cycle().take(n).collect() };
    for pat in [
        &[0x02u8, 0x00, 0x80][..], // JMP $8000 forever
        &[0x01, 0xFE][..],         // branch to self
        &[0x03, 0x00, 0x80][..],   // CALL self
        &[0x06][..],               // BRK storm
        &[0x07][..],               // JAM
        &[0x05, 0x00, 0x80][..],   // indirect jumps
        &[0x08][..],               // RTI storm
        &[0x01, 0x00][..],         // branch to the next instruction
    ] {
        let a = terminates_on(&tile(pat));
        assert!(a.stats.observed_insns == 0 && !a.functions.is_empty());
    }
}

#[test]
fn a_fixed_seed_random_32k_image_terminates_without_panic() {
    for seed in [1u64, 0xDEAD_BEEF, 0x0123_4567_89AB_CDEF] {
        let mut rng = Xs(seed);
        let img: Vec<u8> = (0..0x8000).map(|_| (rng.next() >> 24) as u8).collect();
        let mut log = CoverageLog::new(img.len());
        log.note_windows(linear(0));
        reset(&mut log, 0);
        // Executed bytes scattered through it, including overlapping runs
        // that contradict the decode.
        for _ in 0..200 {
            let off = (rng.next() % 0x7FF0) as usize;
            mark(&mut log, off, 1 + (rng.next() % 3) as usize);
        }
        let a = build_atlas(&Toy, &img, &log);
        check_invariants(&a);
        assert_eq!(a.stats.observed_uncovered, a.stats.undecodable_observed);
        let sup = superset_decode(&Toy, &img, &log);
        assert!(sup.candidates() > 0);
    }
}

#[test]
fn random_image_with_no_window_info_still_terminates() {
    let mut rng = Xs(77);
    let img: Vec<u8> = (0..0x8000).map(|_| (rng.next() >> 24) as u8).collect();
    let mut log = CoverageLog::new(img.len());
    reset(&mut log, 0);
    check_invariants(&build_atlas(&Toy, &img, &log));
}

#[test]
fn empty_and_tiny_images_are_fine() {
    for n in 0..4 {
        let img = vec![0x03u8; n]; // truncated CALLs
        let mut log = CoverageLog::new(n);
        log.mark_prg(0, OPCODE);
        let a = build_atlas(&Toy, &img, &log);
        assert!(!a.stats.budget_hit);
    }
}

// ---- superset ----------------------------------------------------------------

#[test]
fn superset_decodes_every_unexecuted_offset_independently() {
    let mut img = image(8);
    put(&mut img, 0, &[0x02, 0x00, 0x80]); // JMP: executed
    put(&mut img, 3, &[0x03, 0x00, 0x80]); // CALL: never ran
    let mut log = CoverageLog::new(8);
    mark(&mut log, 0, 3);
    let s = superset_decode(&Toy, &img, &log);
    assert_eq!(
        &s.len[..3],
        &[0, 0, 0],
        "executed bytes are the recording's"
    );
    assert_eq!(s.len[3], 3);
    assert_eq!(s.len[4], 1, "every offset is a candidate start");
    assert_eq!(s.len[7], 1);
    assert_eq!(s.candidates(), 5);
}

// ---- cache --------------------------------------------------------------------

#[test]
fn round_trip_and_rejection_of_foreign_or_damaged_bytes() {
    let mut img = image(0x1000);
    put(&mut img, 0, &[0x03, 0x10, 0x80, 0x04]);
    put(&mut img, 0x10, &[0x04]);
    let mut log = CoverageLog::new(img.len());
    log.note_windows(linear(0));
    reset(&mut log, 0);
    mark(&mut log, 0, 3);
    let a = build_atlas(&Toy, &img, &log);
    let bytes = a.to_bytes();
    assert_eq!(Atlas::from_bytes(&bytes).unwrap(), a);

    assert_eq!(
        Atlas::from_bytes(b"nope....").unwrap_err(),
        AtlasError::BadMagic
    );
    assert_eq!(Atlas::from_bytes(&[]).unwrap_err(), AtlasError::BadMagic);
    let mut other = bytes.clone();
    other[4..8].copy_from_slice(&(ATLAS_FORMAT_VERSION + 1).to_le_bytes());
    assert_eq!(
        Atlas::from_bytes(&other).unwrap_err(),
        AtlasError::Version(ATLAS_FORMAT_VERSION + 1)
    );
    assert!(matches!(
        Atlas::from_bytes(&bytes[..bytes.len() / 2]),
        Err(AtlasError::Decode(_))
    ));
    // Every truncation is an error, never a panic.
    for cut in 0..bytes.len().min(64) {
        let _ = Atlas::from_bytes(&bytes[..cut]);
    }
}

#[test]
fn cache_name_carries_the_format_version() {
    assert!(CACHE_SETTINGS.ends_with(&format!("-v{ATLAS_FORMAT_VERSION}")));
}

#[test]
fn the_serialised_atlas_holds_no_rom_bytes() {
    // A recognisable run of 1-byte NOPs (low nibble 0xB-0xF) as the code of
    // one function.
    let mut img = image(0x1000);
    let nops: Vec<u8> = (0..64u8)
        .map(|i| (((i % 15) + 1) << 4) | (0x0B + i % 5))
        .collect();
    put(&mut img, 0x100, &nops);
    put(&mut img, 0x100 + nops.len(), &[0x04]);
    let mut log = CoverageLog::new(img.len());
    log.note_windows(linear(0));
    reset(&mut log, 0x100);
    mark(&mut log, 0x100, 1);
    let a = build_atlas(&Toy, &img, &log);
    assert!(a.functions[0].byte_len > 60, "the signature is the code");
    let bytes = a.to_bytes();
    assert!(
        !bytes.windows(16).any(|w| w == &nops[..16]),
        "ROM bytes leaked into the cache entry"
    );
}

#[test]
fn function_byte_hash_follows_bytes_not_location() {
    // The same 4-byte function at two places hashes the same; one byte
    // different hashes differently.
    let mut img = image(0x1000);
    for (at_, tail) in [(0x100usize, 0xBu8), (0x200, 0xB), (0x300, 0xC)] {
        put(&mut img, at_, &[0x09, 0x55, tail, 0x04]);
    }
    let mut log = CoverageLog::new(img.len());
    log.note_windows(linear(0));
    for r in [0x100u32, 0x200, 0x300] {
        log.note_interrupt(InterruptEntry {
            kind: InterruptKind::Nmi,
            target: at(0x8000 + r as u16, r),
        });
        mark(&mut log, r as usize, 2);
    }
    let a = build_atlas(&Toy, &img, &log);
    check_invariants(&a);
    assert_eq!(fn_at(&a, 0x100).byte_hash, fn_at(&a, 0x200).byte_hash);
    assert_ne!(fn_at(&a, 0x100).byte_hash, fn_at(&a, 0x300).byte_hash);
    assert_eq!(fn_at(&a, 0x100).byte_len, 4);
    assert_ne!(fn_at(&a, 0x100).byte_hash, 0);
}

#[test]
fn building_twice_gives_identical_bytes() {
    let mut rng = Xs(5);
    let img: Vec<u8> = (0..0x4000).map(|_| (rng.next() >> 24) as u8).collect();
    let mut log = CoverageLog::new(img.len());
    log.note_windows(linear(0));
    reset(&mut log, 0);
    for _ in 0..50 {
        mark(&mut log, (rng.next() % 0x3FF0) as usize, 2);
    }
    let a = build_atlas(&Toy, &img, &log).to_bytes();
    let b = build_atlas(&Toy, &img, &log).to_bytes();
    assert_eq!(a, b);
}

#[test]
fn window_no_rom_pages_are_not_candidates() {
    // Sanity on the sentinel the recorder and index share.
    assert_eq!(NO_ROM, u32::MAX);
    let mut m = WindowMap::EMPTY;
    m.0[0] = 0;
    let idx = rf_rebuild::WindowIndex::new(std::slice::from_ref(&m));
    assert_eq!(idx.cpu_for_rom(5), Some(0x8005));
    assert_eq!(idx.cpu_for_rom(0x1000), None);
}
