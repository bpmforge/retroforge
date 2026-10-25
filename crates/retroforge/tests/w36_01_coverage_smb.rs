//! **W36-01: the coverage recorder on a real cartridge** (ROM-gated, law 5).
//!
//! Records ~1500 frames of the first level of the mapper-66 multicart
//! (Start at frame ~200 leaves the game-select screen, then Right held with
//! occasional A) and cross-checks the recorder against an independent
//! oracle: the same run driven one instruction at a time, noting `pc`
//! before every step. Only counts are printed; no ROM bytes leave the test
//! (clean room, D-021).
//!
//! ```text
//! cargo test --release -p retroforge --test w36_01_coverage_smb -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};

use rf_core_api::coverage::OPCODE;
use rf_core_api::{
    CoreEvent, CoreSink, EmulatorCore, IndirectKind, InterruptKind, PpuPixel, ResetKind, Step,
};
use rf_nes::core::NesCore;

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

/// Buttons held on `frame`: idle until 200, Start for 5, a short pause,
/// then Right with A pressed 12 frames in every 60.
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
fn smb_level_one_coverage_matches_an_independent_pc_oracle() {
    let Some(path) = rom_path(FILE) else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = retroforge::rom_open::load_rom_bytes(&path).expect("rom");
    let mut core = NesCore::from_ines_bytes(&raw).expect("loads");
    core.set_coverage_recording(true);
    core.reset(ResetKind::Hard);

    let prg_len = core.bus().prg_rom_len();
    let mut fetched = vec![false; prg_len];
    let mut oracle_instructions = 0u64;
    let mut sink = Null;
    let mut start_x = 0u16;
    for frame in 0..(500 + 1500) {
        core.bus_mut().set_controller_buttons(0, buttons(frame));
        loop {
            let pc = core.cpu().pc;
            let (nmi, irq) = core.cpu().pending_interrupt();
            if !(nmi || irq) {
                oracle_instructions += 1;
                if let Some(o) = core.bus().prg_rom_offset(pc) {
                    fetched[o] = true;
                }
            }
            if core.step(Step::Instruction, &mut sink).frame_complete {
                break;
            }
        }
        // Proof the run is in World 1-1, from the observed RAM facts in
        // profiles/nes/super-mario-bros/profile.toml ($075F world, $0760
        // level, both zero-indexed; $0086 Mario's on-screen x, 0x28 at the
        // start of play; $006D its page, so page*256+x is the level x).
        if frame == 700 {
            assert_eq!(
                (core.peek(0x075F), core.peek(0x0760)),
                (0, 0),
                "not in World 1-1 at frame 700"
            );
            assert_eq!(core.peek(0x0770), 1, "not in play at frame 700 (demo?)");
            start_x = world_x(&core);
        }
    }
    let end_x = world_x(&core);
    println!("Mario world x: frame 700 {start_x}, end {end_x}");
    assert_eq!(
        (core.peek(0x075F), core.peek(0x0760)),
        (0, 0),
        "left World 1-1 during the run"
    );
    assert!(end_x > start_x, "Mario never moved right in 1-1");

    let log = core.take_coverage().expect("armed");
    let opcode_flagged = log
        .prg_flags
        .iter()
        .enumerate()
        .filter(|(_, f)| **f & OPCODE != 0);
    let mut flagged = 0usize;
    for (o, _) in opcode_flagged {
        flagged += 1;
        assert!(
            fetched[o],
            "PRG offset {o:#x} flagged OPCODE but never fetched"
        );
    }
    let fetched_n = fetched.iter().filter(|&&b| b).count();
    println!(
        "coverage: executed {} of {} PRG bytes ({:.1}%), {} opcode / {} operand, \
         {} instructions, {} indirect edges, {} interrupt entries, {} RAM bytes run; \
         oracle fetched {} distinct opcode offsets",
        log.executed_prg_bytes(),
        prg_len,
        log.prg_coverage_fraction() * 100.0,
        log.opcode_prg_bytes(),
        log.prg_flags
            .iter()
            .filter(|f| **f & rf_core_api::coverage::OPERAND != 0)
            .count(),
        log.instructions_recorded,
        log.indirect.len(),
        log.interrupts.len(),
        log.executed_ram_bytes(),
        fetched_n,
    );
    assert_eq!(
        flagged, fetched_n,
        "recorder and oracle disagree on opcode bytes"
    );
    assert_eq!(log.instructions_recorded, oracle_instructions);
    assert!(log.executed_prg_bytes() > 0);
    assert!(log.interrupts.iter().any(|i| i.kind == InterruptKind::Nmi));
    assert!(log
        .interrupts
        .iter()
        .any(|i| i.kind == InterruptKind::Reset));
    assert!(log
        .indirect
        .iter()
        .any(|e| matches!(e.kind, IndirectKind::JmpIndirect | IndirectKind::Rts)));
}

/// Mario's x in the level: page ($006D) times 256 plus on-screen x ($0086),
/// both from profiles/nes/super-mario-bros/profile.toml.
fn world_x(core: &NesCore) -> u16 {
    u16::from(core.peek(0x006D)) * 256 + u16::from(core.peek(0x0086))
}
