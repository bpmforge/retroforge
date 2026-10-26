//! **W36-12: the Rust dev emitter compiles** (ignored: needs `rustc` and the
//! built RF-Scroller fixture).
//!
//! Records RF-Scroller (our own homebrew, `fixtures/nes/rf-scroller`), builds
//! its atlas, lowers every function to IR and emits Rust, then compiles the
//! result with `rustc --crate-type lib --edition 2021` into `target/`. The
//! emitted source is only ever written under `target/`: emitter output for a
//! commercial game is never committed (D-021, law 5), and this test does not
//! touch one.
//!
//! ```text
//! cargo test -p rf-harness --test w36_12_emit -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::process::Command;

use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_harness::atlas::build_nes_atlas;
use rf_nes::core::NesCore;
use rf_nes::rebuild::emit::emit_module;

struct NullSink;

impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

fn out_dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/w36-12-emit");
    std::fs::create_dir_all(&d).expect("creates the output directory under target/");
    d
}

#[test]
#[ignore = "needs rustc and fixtures/nes/rf-scroller/build/rf-scroller.nes"]
fn rf_scroller_emitted_rust_compiles() {
    let rom = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/nes/rf-scroller/build/rf-scroller.nes");
    let Ok(raw) = std::fs::read(&rom) else {
        println!("SKIP: {} is not built", rom.display());
        return;
    };

    let mut core = NesCore::from_ines_bytes(&raw).expect("loads");
    core.set_coverage_recording(true);
    core.reset(ResetKind::Hard);
    for f in 0..600u32 {
        let mut input = InputFrame::empty();
        input.ports[0] = if f > 60 { 0x80 } else { 0 }; // hold Right
        core.run_frame(&input, &mut NullSink);
    }
    let log = core.take_coverage().expect("armed");
    let atlas = build_nes_atlas(&raw, &log).expect("atlas");
    let prg = rf_nes::NesRom::from_ines_bytes(&raw).expect("rom");
    let prg = prg.prg_rom();

    let fns: Vec<_> = atlas.functions.iter().collect();
    let src = emit_module(&atlas, &fns, prg);
    let dir = out_dir();
    let rs = dir.join("rf_scroller_emitted.rs");
    std::fs::write(&rs, &src).expect("writes the emitted source");

    let out = Command::new("rustc")
        .args([
            "--crate-type",
            "lib",
            "--edition",
            "2021",
            "--emit",
            "metadata,link",
        ])
        .arg("--crate-name")
        .arg("rf_scroller_emitted")
        .arg("--out-dir")
        .arg(&dir)
        .arg(&rs)
        .output()
        .expect("rustc runs");
    println!(
        "emitted {} functions, {} lines, {} bytes -> {}",
        fns.len(),
        src.lines().count(),
        src.len(),
        rs.display()
    );
    assert!(
        out.status.success(),
        "rustc failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(src.contains("// cycle 1: opcode fetch"));
    assert!(src.contains("DUMMY"), "dummy cycles are marked");
}
