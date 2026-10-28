//! **W36-08b: Super Mario World, Yoshi's Island 1, rebuilt versus plain in
//! lockstep** (ROM-gated, law 5).
//!
//! 1. Record the W36-08 session (menus to the level, the W33-07 play script,
//!    then jump-and-run to 1,500 level frames) on a plain core and build the
//!    atlas from the recording.
//! 2. Lockstep the rebuilt core against a plain one over **the recorded
//!    script**: every frame, CPU registers, work RAM, VRAM, OAM, CGRAM, SRAM,
//!    the CPU-bus/PPU/APU state and the video and audio hashes must agree.
//! 3. The same over a **second, different** script (same menus, other play:
//!    different jumps, runs, a crouch, a pause): code and widths the atlas
//!    never saw are the interpreter's, counted, and still bit-identical.
//!
//! "In real play" is checked with the profile's own RAM facts on a plain core
//! (game mode `$7E0100 == $14`, camera `$7E001A` and player X `$7E0094`
//! moving) for both scripts. Only counts are printed; no ROM bytes leave the
//! test (clean room, D-021).
//!
//! ```text
//! cargo test --release -p retroforge --test w36_08b_lockstep_smw -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};

use rf_core_api::{
    CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, RebuildStats, ResetKind,
};
use rf_harness::lockstep::Options;
use rf_input::SnesButton;
use rf_snes::core::SnesCore;

/// Only for the shared script constants.
#[allow(dead_code, unused_imports)]
#[path = "w33_07_common/mod.rs"]
mod discovery;

const FILE: &str = "Super Mario World (USA).zip";
const GAME_MODE: u32 = 0x7E_0100;
const IN_LEVEL: u8 = 0x14;
const CAMERA_X: u32 = 0x7E_001A;
const PLAYER_X: u32 = 0x7E_0094;
const PLAY_FRAMES: usize = 1500;

/// Another way through the same level: other jump lengths, a run with Y held,
/// a crouch, a reversal, a pause.
const SECOND_PLAY: &str = "R+B30 R10 w10 L8 R+B20 R+Y40 R30 B12 R20 D6 R+B14 R22 B8 w30 \
S6 w40 S6 w20 R+B+Y50 R40 w6 L6 R+B24 R30";
const SECOND_MORE: &str = "R+B+Y28 R16 L10 R+B18 R30 B10 w4 R24 D4 R+B22 R28";

fn rom_path() -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = Path::new(&root).join("snes").join(FILE);
    p.exists().then_some(p)
}

/// `wN` idle N frames, `<buttons>N` hold N frames (`A B X Y S E U D L R`
/// joined by `+`), as `Rig::script` reads them.
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
                    "A" => SnesButton::A.bit(),
                    "B" => SnesButton::B.bit(),
                    "X" => SnesButton::X.bit(),
                    "Y" => SnesButton::Y.bit(),
                    "S" => SnesButton::Start.bit(),
                    "E" => SnesButton::Select.bit(),
                    "U" => SnesButton::Up.bit(),
                    "D" => SnesButton::Down.bit(),
                    "L" => SnesButton::Left.bit(),
                    "R" => SnesButton::Right.bit(),
                    other => panic!("button {other}"),
                };
                m | (1 << bit)
            });
            (mask, n.parse().expect("N"))
        };
        out.extend(std::iter::repeat_n(mask, n));
    }
    out
}

/// Menus to the level, then `play`, then `more` until the level has seen
/// `PLAY_FRAMES` frames. Returns the pad words and the level start frame.
fn session(play: &str, more: &str) -> (Vec<u16>, usize) {
    let to_level = discovery::smw_level_pad();
    let level_start = to_level.len();
    let mut pad = to_level;
    pad.extend(frames_of(play));
    let more = frames_of(more);
    while pad.len() < level_start + PLAY_FRAMES {
        pad.extend(&more);
    }
    (pad, level_start)
}

fn inputs(pad: &[u16]) -> Vec<InputFrame> {
    pad.iter()
        .map(|&mask| {
            let mut f = InputFrame::empty();
            f.ports[0] = mask;
            f
        })
        .collect()
}

struct Null;
impl CoreSink for Null {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

fn word(core: &SnesCore, addr: u32) -> u16 {
    u16::from(core.peek(addr)) | (u16::from(core.peek(addr + 1)) << 8)
}

/// Run `pad` on a plain core (optionally recording coverage) and report how
/// much of it was real play. Returns the coverage log when recording.
fn play(
    raw: &[u8],
    pad: &[u16],
    level_start: usize,
    record: bool,
) -> (Option<rf_core_api::CoverageLog>, (usize, u16, u16)) {
    let mut core = SnesCore::load(raw).expect("loads");
    core.reset(ResetKind::Hard);
    core.set_coverage_recording(record);
    let (mut in_level, mut cam_min, mut cam_max) = (0usize, u16::MAX, 0u16);
    for (frame, &mask) in pad.iter().enumerate() {
        core.system_mut().bus.joypads.ports[0] = mask;
        core.run_frame(&InputFrame::empty(), &mut Null);
        if frame + 1 == level_start {
            assert_eq!(
                core.peek(GAME_MODE),
                IN_LEVEL,
                "not in the level at its start"
            );
        }
        if frame >= level_start && core.peek(GAME_MODE) == IN_LEVEL {
            in_level += 1;
            let cam = word(&core, CAMERA_X);
            (cam_min, cam_max) = (cam_min.min(cam), cam_max.max(cam));
        }
    }
    let px = word(&core, PLAYER_X);
    (
        core.take_coverage(),
        (in_level, cam_max.saturating_sub(cam_min), px),
    )
}

fn report(label: &str, frames: u32, s: &RebuildStats) {
    println!(
        "{label}: {frames} frames, zero divergence. {} instructions: {} observed / {} inferred \
         dispatch; fallback {} ({:.3}%): no node {}, width mismatch {}, work RAM {}, other {}; \
         {} interrupt entries; {} indirect transfers checked, {} dispatch misses, {} distinct",
        s.instructions(),
        s.observed,
        s.inferred,
        s.fallback,
        s.fallback_rate() * 100.0,
        s.fallback_no_node,
        s.fallback_width,
        s.fallback_ram,
        s.fallback_other,
        s.interrupt_entries,
        s.indirect_checked,
        s.dispatch_misses,
        s.delta_len,
    );
}

#[test]
#[ignore = "needs Super Mario World (USA) (RF_SCREENS_ROMS_DIR); minutes in release"]
fn smw_yoshis_island_one_rebuilt_equals_plain_over_two_scripts() {
    let Some(path) = rom_path() else {
        println!("SKIP: {FILE} not found");
        return;
    };
    let raw = retroforge::rom_open::load_rom_bytes(&path).expect("rom");
    let (pad_a, start_a) = session(discovery::SMW_PLAY, "R+B14 R22");
    let (pad_b, start_b) = session(SECOND_PLAY, SECOND_MORE);
    assert_ne!(pad_a, pad_b, "the second script must differ");

    // ---- record script A, build the atlas -----------------------------------
    let (log, (in_level, cam_span, px)) = play(&raw, &pad_a, start_a, true);
    let log = log.expect("armed");
    println!(
        "script A: {} frames, {in_level} in the level, camera span {cam_span}, player x {px}; \
         recorded {} instructions",
        pad_a.len(),
        log.instructions_recorded
    );
    assert!(in_level * 10 >= PLAY_FRAMES * 6 && cam_span >= 200);
    let atlas = rf_harness::atlas::build_snes_atlas(&raw, &log).expect("atlas");
    println!(
        "atlas: {} functions, {} insns ({} observed / {} inferred)",
        atlas.functions.len(),
        atlas.stats.observed_insns + atlas.stats.inferred_insns,
        atlas.stats.observed_insns,
        atlas.stats.inferred_insns
    );

    // ---- script B is real play too ------------------------------------------
    let (_, (in_level_b, cam_span_b, px_b)) = play(&raw, &pad_b, start_b, false);
    println!(
        "script B: {} frames, {in_level_b} in the level, camera span {cam_span_b}, player x {px_b}",
        pad_b.len()
    );
    assert!(in_level_b * 10 >= PLAY_FRAMES * 6 && cam_span_b >= 200);

    // ---- lockstep over both -------------------------------------------------
    let dir = std::env::temp_dir().join(format!("rf-lockstep-smw-{}", std::process::id()));
    for (label, pad) in [("A (recorded)", &pad_a), ("B (different)", &pad_b)] {
        let opts = Options::<SnesCore> {
            bundle_dir: Some(dir.join(&label[..1])),
            ..Options::default()
        };
        let r = rf_harness::lockstep_snes::run(&raw, &atlas, inputs(pad), opts).expect("runs");
        if let Some(d) = &r.divergence {
            panic!(
                "script {label}: diverged at frame {}: {:?}; fine {:?}; bundle {:?}",
                d.frame, d.fields, d.fine, r.bundle
            );
        }
        assert_eq!(r.frames_run as usize, pad.len());
        report(&format!("script {label}"), r.frames_run, &r.stats);
        assert!(r.stats.instructions() > 1_000_000);
        assert!(r.stats.observed > 0);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
