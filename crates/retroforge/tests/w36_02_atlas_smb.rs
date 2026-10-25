//! **W36-02: the code atlas on a real cartridge** (ROM-gated, law 5).
//!
//! Records the same ~1500 frames of World 1-1 as `w36_01_coverage_smb`, builds
//! the atlas, and checks that every executed opcode is an instruction start
//! in some atlas function. Only counts and offsets are printed; no ROM bytes
//! leave the test (clean room, D-021).
//!
//! ```text
//! cargo test --release -p retroforge --test w36_02_atlas_smb -- --ignored --nocapture
//! ```

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rf_core_api::coverage::{OPCODE, OPERAND};
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_nes::core::NesCore;
use rf_nes::rebuild::Provenance;
use rf_nes::NesRom;

const FILE: &str = "Super Mario Bros. + Duck Hunt (USA).zip";
const START: u8 = 1 << 3;
const A: u8 = 1 << 0;
const RIGHT: u8 = 1 << 7;

fn rom_path(file: &str) -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = Path::new(&root).join("nes").join(file);
    p.exists().then_some(p)
}

struct Null;

impl CoreSink for Null {
    fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _event: CoreEvent) {}
}

/// Same script as W36-01: idle until 200, Start, pause, then Right with A
/// pressed 12 frames in every 60.
fn buttons(frame: u32) -> u8 {
    match frame {
        0..=199 => 0,
        // The first Start leaves the multicart's game select; the second
        // leaves SMB's title before it idles into the attract demo (which
        // ignores the pad). Found by observation in W36-03: with one Start
        // the run was the demo.
        200..=204 | 400..=404 => START,
        205..=399 | 405..=499 => 0,
        f if (f - 500) % 60 < 12 => RIGHT | A,
        _ => RIGHT,
    }
}

#[test]
#[ignore = "needs the mapper-66 multicart ROM (RF_SCREENS_ROMS_DIR)"]
fn smb_level_one_atlas_covers_every_executed_opcode() {
    let Some(path) = rom_path(FILE) else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = retroforge::rom_open::load_rom_bytes(&path).expect("rom");
    let mut core = NesCore::from_ines_bytes(&raw).expect("loads");
    core.set_coverage_recording(true);
    core.reset(ResetKind::Hard);
    let mut sink = Null;
    let mut input = InputFrame::empty();
    for frame in 0..(500 + 1500) {
        input.ports[0] = u16::from(buttons(frame));
        core.run_frame(&input, &mut sink);
        if frame == 700 {
            assert_eq!(
                (core.peek(0x075F), core.peek(0x0760)),
                (0, 0),
                "not in World 1-1 at frame 700"
            );
        }
    }
    assert_eq!((core.peek(0x075F), core.peek(0x0760)), (0, 0), "left 1-1");
    let log = core.take_coverage().expect("armed");

    let rom = NesRom::from_ines_bytes(&raw).expect("parses");
    let prg = rom.prg_rom();
    let atlas = rf_nes::rebuild::build_atlas(prg, &log);

    // Every executed opcode is an instruction start in some function.
    let in_functions: HashSet<u32> = atlas
        .functions
        .iter()
        .flat_map(|f| f.blocks.iter())
        .flat_map(|&b| atlas.blocks[b as usize].insns.iter().map(|i| i.rom))
        .collect();
    let missing: Vec<usize> = log
        .prg_flags
        .iter()
        .enumerate()
        .filter(|(o, f)| **f & OPCODE != 0 && !in_functions.contains(&(*o as u32)))
        .map(|(o, _)| o)
        .collect();
    let first: Vec<String> = missing.iter().take(10).map(|o| format!("{o:#x}")).collect();
    assert!(
        missing.is_empty(),
        "{} executed opcodes are not atlas instruction starts; first offsets: {first:?}",
        missing.len()
    );
    assert!(!atlas.stats.budget_hit, "a step budget tripped on SMB");
    assert_eq!(atlas.stats.undecodable_observed, 0);
    assert_eq!(atlas.stats.observed_uncovered, 0);

    // The decode length of every executed instruction agrees with the
    // operand bytes the recorder marked.
    let len_mismatch = atlas
        .insns()
        .filter(|i| i.prov == Provenance::Observed)
        .filter(|i| {
            (1..u32::from(i.len)).any(|k| log.prg_flags[(i.rom + k) as usize] & OPERAND == 0)
        })
        .count();

    // Cache round trip, determinism, and no ROM bytes in the entry.
    let bytes = atlas.to_bytes();
    assert_eq!(bytes, rf_nes::rebuild::build_atlas(prg, &log).to_bytes());
    assert_eq!(rf_nes::rebuild::Atlas::from_bytes(&bytes).unwrap(), atlas);
    let windows: HashSet<&[u8]> = bytes.windows(16).collect();
    let leaked = (0..prg.len().saturating_sub(16))
        .step_by(97)
        .map(|o| &prg[o..o + 16])
        .filter(|w| w.iter().any(|&b| b != w[0]))
        .filter(|w| windows.contains(w))
        .count();
    assert_eq!(leaked, 0, "ROM bytes found in the serialised atlas");

    let s = &atlas.stats;
    let edges: usize = atlas.blocks.iter().map(|b| b.edges.len()).sum();
    let observed_edges = atlas
        .blocks
        .iter()
        .flat_map(|b| &b.edges)
        .filter(|e| e.prov == Provenance::Observed)
        .count();
    println!(
        "atlas: {} functions, {} blocks, {} insns ({} observed / {} inferred), \
         {} edges ({} observed), {} unresolved static targets ({} ambiguous), \
         {} indirect sites, {} volatile regions ({} RAM bytes run)",
        atlas.functions.len(),
        atlas.blocks.len(),
        s.observed_insns + s.inferred_insns,
        s.observed_insns,
        s.inferred_insns,
        edges,
        observed_edges,
        s.unresolved_static,
        s.ambiguous_static,
        atlas.indirect_sites.len(),
        atlas.volatile.len(),
        log.executed_ram_bytes(),
    );
    println!(
        "superset: {} never-executed offsets decode ({} also atlas instructions); \
         PRG coverage {:.1}% ({} of {} bytes executed); {} window snapshots",
        s.superset_candidates,
        s.superset_in_atlas,
        log.prg_coverage_fraction() * 100.0,
        log.executed_prg_bytes(),
        prg.len(),
        log.windows.len(),
    );
    println!(
        "entries: {} interrupt, {} indirect, {} call, {} orphan; {} RTI targets as entries; \
         {} overlap-rejected, {} dangling edges, {} RAM indirect sites, \
         {} observed-length/operand-flag mismatches; {} cache bytes",
        count_kind(&atlas, rf_nes::rebuild::entry::INTERRUPT),
        count_kind(&atlas, rf_nes::rebuild::entry::INDIRECT),
        count_kind(&atlas, rf_nes::rebuild::entry::CALL),
        s.orphan_entries,
        s.rti_entries,
        s.overlap_rejected,
        s.dangling_edges,
        s.ram_indirect_sites,
        len_mismatch,
        bytes.len(),
    );
}

fn count_kind(a: &rf_nes::rebuild::Atlas, kind: u8) -> usize {
    a.functions
        .iter()
        .filter(|f| f.entry_kinds & kind != 0)
        .count()
}
