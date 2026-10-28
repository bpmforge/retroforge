//! **W38-02: the object-table finder on Super Mario World** (ROM-gated, law 5).
//!
//! Menus to Yoshi's Island 1 and play (the W38-01 script, plain frames), RAM,
//! OAM and BG1 scroll sampled after each frame. Only offsets, strides and
//! counts are printed (D-021).
//!
//! ```text
//! cargo test --release -p rf-harness --test w38_02_object_tables_smw -- --ignored --nocapture
//! ```

use std::io::Read as _;
use std::path::PathBuf;
use std::time::Instant;

use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_harness::object_tables::{snes_cam, snes_step_frame, Console, Recorder};
use rf_input::SnesButton;
use rf_rebuild::objtable::{grade_of, infer, to_row};
use rf_snes::core::SnesCore;

const FILE: &str = "Super Mario World (USA).zip";

/// Menus to Yoshi's Island 1 and play: the token scripts of
/// `crates/retroforge/tests/w33_07_common/mod.rs`, as `w36_08_atlas_smw`
/// plays them. `wN` idles N frames, `<buttons>N` holds them
/// (`A B X Y S E U D L R`).
const TO_LEVEL: &str = "w200 S6 w108 A6 w108 S6 w108 A6 w108 S6 w108 A6 w108 S6 w108 A6 w108 S6 w108 A6 w108 w300 A8 w408 A8 w200 R40 w20 L40 w20 R40 w20 L40 w20 R40 w20 L40 w20 R60 w20 R200 w300 L8 w158 A8 w158";
const PLAY: &str = "L2 w3 B8 w52 B8 w52 B8 w52 B8 w52 R+B14 R22 R+B14 R22 S6 w100 S6 w60 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22 R+B14 R22";

fn rom_path() -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = std::path::Path::new(&root).join("snes").join(FILE);
    p.exists().then_some(p)
}

fn load_rom(path: &std::path::Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).ok()?;
        if !entry.is_file() {
            continue;
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).ok()?;
        if matches!(
            rf_cart::Cartridge::load(&buf),
            Ok(rf_cart::Cartridge::Snes { .. })
        ) {
            return Some(buf);
        }
    }
    None
}

fn frames_of(text: &str) -> Vec<u16> {
    let mut out = Vec::new();
    for tok in text.split_whitespace() {
        let (mask, n) = if let Some(n) = tok.strip_prefix('w') {
            (0u16, n.parse::<usize>().expect("wN"))
        } else {
            let split = tok.find(|c: char| c.is_ascii_digit()).expect("button+N");
            let (b, n) = tok.split_at(split);
            let mask = b.split('+').fold(0u16, |m, c| {
                let bit = match c {
                    "A" => SnesButton::A,
                    "B" => SnesButton::B,
                    "X" => SnesButton::X,
                    "Y" => SnesButton::Y,
                    "S" => SnesButton::Start,
                    "E" => SnesButton::Select,
                    "U" => SnesButton::Up,
                    "D" => SnesButton::Down,
                    "L" => SnesButton::Left,
                    "R" => SnesButton::Right,
                    other => panic!("button {other}"),
                }
                .bit();
                m | (1 << bit)
            });
            (mask, n.parse().expect("N"))
        };
        out.extend(std::iter::repeat_n(mask, n));
    }
    out
}

/// Menus to the level, then play until the level has seen 1,500 frames.
fn session() -> Vec<u16> {
    let mut pad = frames_of(TO_LEVEL);
    let start = pad.len();
    pad.extend(frames_of(PLAY));
    let more = frames_of("R+B14 R22");
    while pad.len() < start + 1500 {
        pad.extend(&more);
    }
    pad
}

struct Null;
impl CoreSink for Null {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

#[test]
#[ignore = "needs Super Mario World (USA) (RF_SCREENS_ROMS_DIR)"]
fn smw_object_table_matches_oam() {
    let Some(raw) = rom_path().and_then(|p| load_rom(&p)) else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let mut core = SnesCore::load(&raw).expect("loads");
    core.reset(ResetKind::Hard);
    let pad = session();
    let lead = frames_of(TO_LEVEL).len() - 60;
    let mut rec = Recorder::new(Console::Snes, std::env::var("CAM_NEXT").is_ok());
    let t = Instant::now();
    for (f, &mask) in pad.iter().enumerate() {
        let mut input = InputFrame::empty();
        input.ports[0] = mask;
        if f >= lead {
            let cap = snes_step_frame(&mut core, &input);
            rec.push(cap, snes_cam(&core));
        } else {
            core.system_mut().bus.joypads.ports[0] = mask;
            core.run_frame(&input, &mut Null);
        }
    }
    let trace = rec.trace;
    if let Ok(p) = std::env::var("RF_OBJ_TRACE_OUT") {
        rf_harness::object_tables::save_trace(&trace, Console::Snes, std::path::Path::new(&p))
            .expect("saves");
    }
    println!("sampled {} frames in {:?}", trace.frames.len(), t.elapsed());
    let t = Instant::now();
    let found = infer(&trace);
    println!("inferred {} tables in {:?}", found.len(), t.elapsed());
    for f in &found {
        let row = to_row(f, grade_of(f), "; SMW YI1");
        println!(
            "TABLE base {:#x} slots {} stride {} x_lo {:?} x_hi {:?} y {:?} y_hi {:?} kind {:?} state {:?} z {:.0}",
            row.base, row.slots, row.stride, row.x_lo, row.x_hi, row.y, row.y_hi, row.kind, row.state, f.z
        );
        let b = &f.binding;
        println!(
            "  lag {} dx {} dy {} expected {} matched {} rate {:.4} frames {} clean {} from_state {} grade {:?}",
            b.lag, b.dx, b.dy, b.expected, b.matched, b.rate(), b.frames_checked, b.frames_clean, b.live_from_state, row.grade
        );
        println!(
            "  misses: undrawn {} off_set {} excluded {} (blank frames {}) non_drawn {:?}",
            b.undrawn, b.off_set, b.excluded, b.blank_frames, b.non_drawn
        );
        for s in &b.slots {
            println!(
                "  slot {} oam {:?} bound {} live {} matched {} control {} back {} offsets {}",
                s.slot,
                s.oam_index,
                s.bound_frames,
                s.live_frames,
                s.matched_frames,
                s.control_frames,
                s.back,
                s.offsets.len()
            );
        }
        if std::env::var("MISSES").is_ok() {
            println!("  missed {:?}", &b.missed[..b.missed.len().min(60)]);
        }
    }
    let best = found.first().expect("a table");
    // W38-02b: exact match in 98% or more of the live slot-frames, Observed.
    // Slots 8..12 of the game's twelve are never live in this level (the
    // finder does not claim a slot it has no evidence for).
    assert!(best.table.slots >= 8, "{:?}", best.table);
    assert!(best.binding.expected >= 500, "{:?}", best.binding);
    assert!(best.binding.rate() >= 0.98, "rate {}", best.binding.rate());
    assert_eq!(grade_of(best), rf_rebuild::recipe::Grade::Observed);
}
