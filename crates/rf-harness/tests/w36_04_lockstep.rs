//! Ticket W36-04: the lockstep checker on a synthetic cartridge (no ROM
//! needed). A clean run reports no divergence; a difference injected into the
//! rebuilt core only (test-side, through the CPU bus) is caught at the right
//! frame, named by field and offset, localised to an instruction, and written
//! out as a complete repro bundle.

use rf_core_api::{EmulatorCore, InputFrame, ResetKind};
use rf_harness::atlas::build_nes_atlas;
use rf_harness::lockstep::{run, Options, Side};
use rf_nes::core::NesCore;
use rf_nes::CpuBus as _;
use rf_state::Container;

/// NROM-128: `LDX #0; loop: INC $10; INX; STX $11; JMP loop`, reset -> $8000.
fn tiny_ines() -> Vec<u8> {
    let mut d = vec![0u8; 16];
    d[0..4].copy_from_slice(b"NES\x1a");
    d[4] = 1;
    d[5] = 1;
    let mut prg = vec![0xEAu8; 0x4000];
    prg[..9].copy_from_slice(&[0xA2, 0x00, 0xE6, 0x10, 0xE8, 0x86, 0x11, 0x4C, 0x02]);
    prg[9] = 0x80;
    prg[0x3FFC] = 0x00;
    prg[0x3FFD] = 0x80;
    d.extend(prg);
    d.extend(vec![0u8; 0x2000]);
    d
}

fn atlas_for(ines: &[u8]) -> rf_nes::rebuild::Atlas {
    let mut rec = NesCore::from_ines_bytes(ines).expect("loads");
    rec.set_coverage_recording(true);
    rec.reset(ResetKind::Hard);
    let mut sink = Sink;
    for _ in 0..4 {
        rec.run_frame(&InputFrame::empty(), &mut sink);
    }
    let log = rec.take_coverage().expect("armed");
    build_nes_atlas(ines, &log).expect("builds")
}

struct Sink;
impl rf_core_api::CoreSink for Sink {
    fn video_scanline(&mut self, _y: u16, _p: &[rf_core_api::PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: rf_core_api::CoreEvent) {}
}

fn inputs(n: usize) -> Vec<InputFrame> {
    (0..n)
        .map(|i| {
            let mut f = InputFrame::empty();
            f.ports[0] = (i % 7) as u16;
            f
        })
        .collect()
}

#[test]
fn identical_cores_report_no_divergence() {
    let ines = tiny_ines();
    let atlas = atlas_for(&ines);
    let r = run(&ines, &atlas, inputs(20), Options::default()).expect("runs");
    assert_eq!(r.frames_run, 20);
    assert!(r.divergence.is_none(), "{:?}", r.divergence);
    assert!(r.bundle.is_none());
    assert!(r.stats.observed > 0, "the rebuilt path really ran");
}

#[test]
fn an_injected_difference_is_caught_localised_and_bundled() {
    let ines = tiny_ines();
    let atlas = atlas_for(&ines);
    let dir = std::env::temp_dir().join(format!("rf-lockstep-bundle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut poke = |frame: u32, side: Side, core: &mut NesCore| {
        if frame == 9 && side == Side::Rebuilt {
            core.bus_mut().write(0x0300, 0x5A);
        }
    };
    let opts = Options {
        bundle_dir: Some(dir.clone()),
        input_tail: 4,
        trace_len: 16,
        before_frame: Some(&mut poke),
        ..Options::default()
    };
    let r = run(&ines, &atlas, inputs(30), opts).expect("runs");
    assert!(r.bundle_errors.is_empty(), "{:?}", r.bundle_errors);
    let d = r.divergence.expect("must diverge");
    assert_eq!(d.frame, 9);
    assert_eq!(r.frames_run, 10, "the run stopped at the first divergence");
    let wram = d.fields.iter().find(|f| f.field == "wram").expect("wram");
    assert_eq!(
        (wram.first_offset, wram.plain, wram.rebuilt),
        (0x300, 0, 0x5A)
    );
    let fine = d.fine.expect("localised to an instruction");
    assert_eq!(
        fine.instruction, 0,
        "the poke is visible after the first one"
    );
    assert!(fine.fields.iter().any(|f| f.field == "wram"));

    let bundle = r.bundle.expect("bundle written");
    for name in [
        "plain_state_before_frame.rfstate",
        "inputs.txt",
        "trace.txt",
        "diff.txt",
        "wram_plain.bin",
        "wram_rebuilt.bin",
        "README.txt",
    ] {
        assert!(bundle.files.iter().any(|f| f == name), "missing {name}");
        assert!(dir.join(name).is_file(), "{name} not on disk");
    }
    // The state is the shipped container, for this ROM, and complete.
    let bytes = std::fs::read(dir.join("plain_state_before_frame.rfstate")).unwrap();
    let (c, _) = Container::decode_default(&bytes).expect("decodes");
    let sha = rf_cart::hash::identity_nes(&ines).normalized.sha256;
    let mut hash = [0u8; 32];
    for (i, b) in hash.iter_mut().enumerate() {
        *b = u8::from_str_radix(&sha[i * 2..i * 2 + 2], 16).unwrap();
    }
    c.verify_rom(hash)
        .expect("keyed by the normalised ROM hash");
    for tag in [
        b"CPU_", b"PPU_", b"APU_", b"WRAM", b"VRAM", b"OAM_", b"CGRM", b"MAPR", b"CART",
    ] {
        assert!(
            c.chunk(*tag).is_some(),
            "chunk {}",
            String::from_utf8_lossy(tag)
        );
    }
    // Input tail: 4 frames before the diverging one, which is marked.
    let inp = std::fs::read_to_string(dir.join("inputs.txt")).unwrap();
    let lines: Vec<&str> = inp.lines().skip(1).collect();
    assert_eq!(lines.len(), 5);
    assert!(lines[4].starts_with("9 ") && lines[4].ends_with("<-- diverging"));
    // Trace: bounded, ends in the loop, with function ids on atlas hits.
    let trace = std::fs::read_to_string(dir.join("trace.txt")).unwrap();
    let n = trace.lines().count() - 1;
    assert!(n > 0 && n <= 16, "{n}");
    assert!(trace.contains("function"), "{trace}");
    let diff = std::fs::read_to_string(dir.join("diff.txt")).unwrap();
    assert!(diff.contains("wram") && diff.contains("frame 9"));
    // No ROM bytes: nothing in the bundle is as large as the PRG image.
    for f in &bundle.files {
        let len = std::fs::metadata(dir.join(f)).unwrap().len();
        assert!(len < 0x4000, "{f} is {len} bytes");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_non_cpu_poke_is_still_caught() {
    // The checker also compares what the frame put on the sinks and the
    // non-CPU memories; poking OAM through $2003/$2004 on one core only
    // changes OAM (and the cycle count, a bus write costs one).
    let ines = tiny_ines();
    let atlas = atlas_for(&ines);
    let mut poke = |frame: u32, side: Side, core: &mut NesCore| {
        if frame == 3 && side == Side::Rebuilt {
            core.bus_mut().write(0x2003, 0x10);
            core.bus_mut().write(0x2004, 0x77);
        }
    };
    let opts = Options {
        before_frame: Some(&mut poke),
        ..Options::default()
    };
    let r = run(&ines, &atlas, inputs(8), opts).expect("runs");
    let d = r.divergence.expect("caught");
    assert_eq!(d.frame, 3);
    assert!(d
        .fields
        .iter()
        .any(|f| f.field == "oam" || f.field == "cycles"));
}
