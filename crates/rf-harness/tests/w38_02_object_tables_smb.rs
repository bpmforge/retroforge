//! **W38-02: the object-table finder on Super Mario Bros.** (ROM-gated, law 5).
//!
//! Plays the first stretch of 1-1 (plain frames, no instrumentation), samples
//! RAM, OAM and scroll after every frame, and asks the finder for the object
//! tables. The acceptance number is the share of live slot-frames whose x and
//! y match an OAM entry exactly. Only offsets, strides and counts are printed
//! (D-021).
//!
//! ```text
//! cargo test --release -p rf-harness --test w38_02_object_tables_smb -- --ignored --nocapture
//! ```

use std::io::Read as _;
use std::path::PathBuf;
use std::time::Instant;

use rf_core_api::{EmulatorCore, InputFrame, ResetKind};
use rf_harness::object_tables::{nes_cam, nes_prepare, nes_step_frame, Console, Recorder};
use rf_nes::core::NesCore;
use rf_nes::CpuBus as _;
use rf_rebuild::objtable::{grade_of, infer, to_row};

const FILE: &str = "Super Mario Bros. + Duck Hunt (USA).zip";
const START: u8 = 1 << 3;
const A: u8 = 1 << 0;
const RIGHT: u8 = 1 << 7;
/// 500 frames to get through both title screens plus the intro, then play.
const LEAD_IN: u32 = 500;
const PLAY: u32 = 1200;
/// Last frame sampled.
const DEATH: u32 = 1700;
const FRAMES: u32 = LEAD_IN + PLAY;

fn rom_path() -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = std::path::Path::new(&root).join("nes").join(FILE);
    p.exists().then_some(p)
}

/// The single NES image inside the zip (content-sniffed, like the app).
fn load_rom(path: &std::path::Path) -> Vec<u8> {
    let bytes = std::fs::read(path).expect("reads the zip");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("zip");
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).expect("entry");
        if !entry.is_file() {
            continue;
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).expect("reads");
        if matches!(
            rf_cart::Cartridge::load(&buf),
            Ok(rf_cart::Cartridge::Nes { .. })
        ) {
            return buf;
        }
    }
    panic!("no NES image in the zip");
}

/// Two Starts (game select, then SMB's title), then Right with A pressed 12
/// frames in every 60. Same as `w36_03_rebuild_smb`'s script one.
fn script_one(frame: u32) -> u8 {
    // Same menus; long hops (A held 34 frames in 60) so Mario clears the
    // pipes and stairs of 1-1 and keeps meeting Goombas and Koopas.
    match frame {
        0..=199 => 0,
        200..=204 | 400..=404 => START,
        205..=399 | 405..=499 => 0,
        f if (f - 500) % 60 < 34 => RIGHT | A,
        _ => RIGHT,
    }
}

#[test]
#[ignore = "needs Super Mario Bros. + Duck Hunt (USA) (RF_SCREENS_ROMS_DIR)"]
fn smb_object_table_matches_oam() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = load_rom(&path);
    let mut core = NesCore::from_ines_bytes(&raw).expect("loads");
    core.reset(ResetKind::Hard);
    nes_prepare(&mut core);
    let cam_next = std::env::var("CAM_NEXT").is_ok();
    let mut rec = Recorder::new(Console::Nes, cam_next);
    let t = Instant::now();
    for f in 0..DEATH.min(FRAMES) {
        let mut input = InputFrame::empty();
        input.ports[0] = u16::from(script_one(f));
        // Test-side: keep the invincibility timer up so the run goes on past
        // the first Goomba (the stretch studied is play, not the death).
        if f >= LEAD_IN {
            core.bus_mut().write(0x079F, 0x20);
        }
        let cap = nes_step_frame(&mut core, &input);
        if (LEAD_IN - 60..DEATH).contains(&f) {
            rec.push(cap, nes_cam(&core));
        }
    }
    let trace = rec.trace;
    println!("sampled {} frames in {:?}", trace.frames.len(), t.elapsed());
    if std::env::var("OBJDUMP").is_ok() {
        for (k, fr) in trace
            .frames
            .iter()
            .enumerate()
            .skip(700)
            .step_by(40)
            .take(8)
        {
            println!(
                "f{k} cam {} {} x86 {:#x} pg6d {:#x} rel3ad {:#x} sl71c {:#x} sp {:?}",
                fr.cam_x,
                fr.cam_y,
                fr.ram[0x86],
                fr.ram[0x6d],
                fr.ram[0x3ad],
                fr.ram[0x71c],
                fr.sprites
                    .iter()
                    .take(4)
                    .map(|s| (s.x, s.y))
                    .collect::<Vec<_>>()
            );
        }
    }
    let t = Instant::now();
    let found = infer(&trace);
    println!("inferred {} tables in {:?}", found.len(), t.elapsed());
    for f in &found {
        let row = to_row(f, grade_of(f), "; SMB 1-1");
        println!(
            "TABLE base {:#x} slots {} stride {} x_lo {:?} x_hi {:?} y {:?} y_hi {:?} kind {:?} state {:?} z {:.0}",
            row.base, row.slots, row.stride, row.x_lo, row.x_hi, row.y, row.y_hi, row.kind, row.state, f.z
        );
        let b = &f.binding;
        println!(
            "  lag {} dx {} dy {} expected {} matched {} rate {:.4} frames {} clean {} from_state {} grade {:?}",
            b.lag, b.dx, b.dy, b.expected, b.matched, b.rate(), b.frames_checked, b.frames_clean, b.live_from_state, row.grade
        );
        for s in &b.slots {
            println!(
                "  slot {} oam {:?} bound {} live {}",
                s.slot, s.oam_index, s.bound_frames, s.live_frames
            );
        }
    }
    let best = found.first().expect("a table");
    assert!(best.table.slots >= 3, "{:?}", best.table);
    assert!(best.binding.expected >= 500, "{:?}", best.binding);
    assert!(best.binding.rate() >= 0.9, "rate {}", best.binding.rate());
}
