//! **W37-04: what the shadow core costs** (ROM-gated, law 5).
//!
//! Times 600 frames of real play on one NES and one SNES cartridge with Rebuild
//! on (the vectors-only atlas installed), with and without the shadow check
//! (`retroforge::shadow_check::ShadowCheck`: a plain core stepped beside the
//! player's, a snapshot of it per frame, and the lockstep comparison), the
//! same driver the core thread uses. Both sides draw into the shell's own
//! `FrameBuffer`, as the app does; the shadow draws into nothing. The runs are
//! interleaved (off, on, off, on, ...) so machine load hits both alike, and
//! the shadow must report zero differences, which makes this a clean-play
//! check on real code too. Only timings and counts are printed; no ROM bytes
//! leave the test (clean room, D-021).
//!
//! ```text
//! cargo test --release -p retroforge --test w37_04_shadow_cost -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use retroforge::shadow_check::{ShadowCheck, ShadowStatus};
use retroforge::stepper::EmuStepper;
use rf_core_api::{CoverageLog, InputFrame};
use rf_input::SnesButton;

const NES_FILE: &str = "Super Mario Bros. + Duck Hunt (USA).zip";
const SNES_FILE: &str = "Super Mario World (USA).zip";
const WARM: u32 = 60;
const FRAMES: u32 = 600;
const ROUNDS: usize = 4;

fn rom_path(console: &str, file: &str) -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = Path::new(&root).join(console).join(file);
    p.exists().then_some(p)
}

/// Start pulses to leave the menus, then a run to the right with jumps. The
/// frame numbers are the session's (warm-up included), so both configurations
/// see the same input.
fn pad(snes: bool, frame: u32) -> u16 {
    let (start, right, a) = if snes {
        (
            1u16 << SnesButton::Start.bit(),
            1u16 << SnesButton::Right.bit(),
            1u16 << SnesButton::A.bit(),
        )
    } else {
        (1 << 3, 1 << 7, 1 << 0)
    };
    match frame {
        0..=99 => 0,
        100..=104 | 200..=204 | 300..=304 => start,
        105..=199 | 205..=299 | 305..=399 => 0,
        f if (f - 400) % 60 < 12 => right | a,
        _ => right,
    }
}

fn session(raw: &[u8], snes: bool, shadow: bool, root: &Path) -> (f64, Option<ShadowStatus>) {
    let mut st = EmuStepper::open(raw).expect("opens");
    let atlas = retroforge::rebuild::build_for(raw, &CoverageLog::new(0)).expect("atlas");
    st.set_rebuild_atlas(Some(Box::new(atlas)))
        .expect("installs");
    st.resume();
    let mut sink = rf_renderer::FrameBuffer::new();
    let mut check = shadow.then(|| ShadowCheck::start(root.to_path_buf(), "cost-run", Vec::new()));
    let mut elapsed = 0.0;
    for frame in 0..WARM + FRAMES {
        let mut input = InputFrame::empty();
        input.ports[0] = pad(snes, frame);
        let t = Instant::now();
        if let Some(c) = check.as_mut() {
            c.before_frame(&mut st, raw, input);
        }
        let ran = st.latch_and_advance_frame(input, &mut sink) > 0;
        if let Some(c) = check.as_mut() {
            // No checkpoint inside the timing: it is a disk write every 600 frames.
            c.after_frame(&mut st, raw, input, ran, false);
        }
        if frame >= WARM {
            elapsed += t.elapsed().as_secs_f64();
        }
    }
    (elapsed, check.as_ref().map(ShadowCheck::status))
}

fn measure(label: &str, raw: &[u8], snes: bool) {
    let root = std::env::temp_dir().join(format!("rf-shadow-cost-{}", std::process::id()));
    let (mut off, mut on) = (Vec::new(), Vec::new());
    for round in 0..ROUNDS {
        let (t_off, _) = session(raw, snes, false, &root);
        let (t_on, status) = session(raw, snes, true, &root);
        let Some(ShadowStatus::Checking {
            frames,
            divergences,
        }) = status
        else {
            panic!("{label}: the shadow was not checking: {status:?}");
        };
        assert_eq!(
            divergences, 0,
            "{label}: the live shadow found a difference"
        );
        assert_eq!(
            frames,
            u64::from(WARM + FRAMES),
            "{label}: every frame compared"
        );
        println!(
            "{label} round {round}: off {:.2} ms/frame, on {:.2} ms/frame",
            t_off * 1000.0 / f64::from(FRAMES),
            t_on * 1000.0 / f64::from(FRAMES)
        );
        off.push(t_off);
        on.push(t_on);
    }
    let _ = std::fs::remove_dir_all(&root);
    let best = |v: &[f64]| v.iter().copied().fold(f64::MAX, f64::min) * 1000.0 / f64::from(FRAMES);
    let (b_off, b_on) = (best(&off), best(&on));
    println!(
        "{label}: best of {ROUNDS}: off {b_off:.2} ms/frame, on {b_on:.2} ms/frame, \
         extra {:.2} ms/frame ({:.2}x); real time is 16.64 ms/frame",
        b_on - b_off,
        b_on / b_off
    );
}

#[test]
#[ignore = "needs Super Mario Bros. + Duck Hunt and Super Mario World (RF_SCREENS_ROMS_DIR)"]
fn the_shadow_core_cost_on_a_real_nes_and_a_real_snes_cartridge() {
    for (label, console, file, snes) in [
        ("NES SMB multicart", "nes", NES_FILE, false),
        ("SNES Super Mario World", "snes", SNES_FILE, true),
    ] {
        let Some(path) = rom_path(console, file) else {
            println!("SKIP: {file} not found");
            continue;
        };
        let raw = retroforge::rom_open::load_rom_bytes(&path).expect("rom");
        measure(label, &raw, snes);
    }
}
