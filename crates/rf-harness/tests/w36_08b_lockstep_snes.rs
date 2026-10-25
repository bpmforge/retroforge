//! Ticket W36-08b: the SNES lockstep checker on a synthetic LoROM (no ROM
//! needed). A clean run reports no divergence; a difference injected into the
//! rebuilt core only is caught at the right frame, named by field and offset,
//! localised to an instruction when it is a CPU/WRAM one, and written out as
//! a complete repro bundle holding a shipped SNES `.rfstate`.

use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_harness::atlas::build_snes_atlas;
use rf_harness::lockstep::Options;
use rf_harness::lockstep::Side;
use rf_harness::lockstep_snes::run;
use rf_snes::core::SnesCore;
use rf_state::Container;

/// 64 KiB LoROM. Native mode, NMI + auto-joypad on, then per frame:
/// `JSL $018000` (INC $30 ; RTL), copy the pad word `$4218` to `$60`, `WAI`.
fn rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x1_0000];
    let base = 0x7FC0;
    rom[base + 0x15] = 0x20;
    rom[base + 0x17] = 6;
    let sum: u16 = 0xBEEF;
    rom[base + 0x1C..base + 0x1E].copy_from_slice(&(sum ^ 0xFFFF).to_le_bytes());
    rom[base + 0x1E..base + 0x20].copy_from_slice(&sum.to_le_bytes());
    rom[0x7FEA..0x7FEC].copy_from_slice(&0x8100u16.to_le_bytes());
    rom[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes());
    let code: &[u8] = &[
        0x78, 0x18, 0xFB, // SEI ; CLC ; XCE
        0xC2, 0x30, // REP #$30
        0xA2, 0xFF, 0x1F, 0x9A, // LDX #$1FFF ; TXS
        0xE2, 0x20, // SEP #$20
        0xA9, 0x81, 0x8D, 0x00, 0x42, // LDA #$81 ; STA $4200 (NMI + auto-joypad)
        0xC2, 0x30, // REP #$30
        // main:
        0x22, 0x00, 0x80, 0x01, // JSL $018000
        0xAD, 0x18, 0x42, // LDA $4218
        0x8D, 0x60, 0x00, // STA $0060
        0xCB, // WAI
        0x4C, 0x12, 0x80, // JMP main ($8012)
    ];
    rom[..code.len()].copy_from_slice(code);
    rom[0x0100..0x0104].copy_from_slice(&[0xEE, 0x20, 0x00, 0x40]); // NMI: INC $20 ; RTI
    rom[0x8000..0x8004].copy_from_slice(&[0xEE, 0x30, 0x00, 0x6B]); // INC $30 ; RTL
    rom
}

struct Sink;
impl CoreSink for Sink {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

fn inputs(n: usize) -> Vec<InputFrame> {
    (0..n)
        .map(|i| {
            let mut f = InputFrame::empty();
            f.ports[0] = (i % 7) as u16 | 0x8000;
            f
        })
        .collect()
}

fn atlas_for(raw: &[u8]) -> rf_rebuild::Atlas {
    let mut rec = SnesCore::load(raw).expect("loads");
    rec.set_coverage_recording(true);
    rec.reset(ResetKind::Hard);
    for input in inputs(6) {
        rec.system_mut().bus.joypads.ports[0] = input.ports[0];
        rec.run_frame(&input, &mut Sink);
    }
    let log = rec.take_coverage().expect("armed");
    build_snes_atlas(raw, &log).expect("builds")
}

#[test]
fn identical_cores_report_no_divergence() {
    let raw = rom();
    let atlas = atlas_for(&raw);
    let r = run(&raw, &atlas, inputs(20), Options::default()).expect("runs");
    assert!(r.divergence.is_none(), "{:?}", r.divergence);
    assert_eq!(r.frames_run, 20);
    assert!(r.stats.observed > 50, "{:?}", r.stats);
    assert!(r.stats.interrupt_entries >= 19, "{:?}", r.stats);
}

#[test]
fn a_wram_poke_on_the_rebuilt_core_is_caught_at_its_frame_and_instruction() {
    let raw = rom();
    let atlas = atlas_for(&raw);
    let dir = std::env::temp_dir().join(format!("rf-lockstep-snes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut poke = |frame: u32, side: Side, core: &mut SnesCore| {
        if frame == 9 && side == Side::Rebuilt {
            core.system_mut().bus.wram[0x5000] = 0x5A;
        }
    };
    let opts = Options {
        bundle_dir: Some(dir.clone()),
        input_tail: 4,
        before_frame: Some(&mut poke),
        ..Options::default()
    };
    let r = run(&raw, &atlas, inputs(20), opts).expect("runs");
    let d = r.divergence.expect("caught");
    assert_eq!(d.frame, 9);
    assert_eq!(r.frames_run, 10);
    let wram = d.fields.iter().find(|f| f.field == "wram").expect("wram");
    assert_eq!(
        (wram.first_offset, wram.plain, wram.rebuilt),
        (0x5000, 0, 0x5A)
    );
    let fine = d.fine.expect("localised to an instruction");
    assert_eq!(
        fine.instruction, 0,
        "the poke is visible after the first one"
    );
    assert_eq!(fine.pc_before >> 16, 0, "24-bit PBR:PC, bank 0 here");

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
    assert!(r.bundle_errors.is_empty(), "{:?}", r.bundle_errors);
    // The state is the shipped SNES container, for this ROM, and complete.
    let bytes = std::fs::read(dir.join("plain_state_before_frame.rfstate")).unwrap();
    let (c, _) = Container::decode_default(&bytes).expect("decodes");
    assert_eq!(c.header.console, 1, "SNES");
    let sha = rf_cart::hash::identity_snes(&raw).normalized.sha256;
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
    let inp = std::fs::read_to_string(dir.join("inputs.txt")).unwrap();
    let lines: Vec<&str> = inp.lines().skip(1).collect();
    assert_eq!(lines.len(), 5);
    assert!(lines[4].starts_with("9 ") && lines[4].ends_with("<-- diverging"));
    let trace = std::fs::read_to_string(dir.join("trace.txt")).unwrap();
    assert!(trace.contains("atlas(") && trace.contains(" m"), "{trace}");
    assert!(trace.lines().count() > 1);
    let diff = std::fs::read_to_string(dir.join("diff.txt")).unwrap();
    assert!(diff.contains("wram") && diff.contains("frame 9"));
    for f in &bundle.files {
        let len = std::fs::metadata(dir.join(f)).unwrap().len();
        assert!(
            len < 0x2_1000,
            "{f} is {len} bytes (no ROM bytes: smaller than WRAM + slack)"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Each non-CPU memory the checker claims to compare, poked on the rebuilt
/// core only, is caught at frame level under its own field name.
#[test]
fn a_difference_in_any_compared_memory_is_caught_and_named() {
    let raw = rom();
    let atlas = atlas_for(&raw);
    type Poke = fn(&mut SnesCore);
    let cases: [(&str, Poke); 5] = [
        ("vram", |c| c.system_mut().bus.ppu.vram[0x2468] ^= 1),
        ("oam", |c| c.system_mut().bus.ppu.oam[0x30] ^= 1),
        ("cgram", |c| c.system_mut().bus.ppu.cgram[7] ^= 1),
        ("apu_state", |c| c.system_mut().bus.apu.aram[0x1234] ^= 1),
        ("bus_state", |c| c.system_mut().bus.vmain ^= 1),
    ];
    for (field, poke_it) in cases {
        let mut poke = |frame: u32, side: Side, core: &mut SnesCore| {
            if frame == 4 && side == Side::Rebuilt {
                poke_it(core);
            }
        };
        let opts = Options {
            before_frame: Some(&mut poke),
            ..Options::default()
        };
        let r = run(&raw, &atlas, inputs(8), opts).expect("runs");
        let d = r
            .divergence
            .unwrap_or_else(|| panic!("{field}: not caught"));
        assert_eq!(d.frame, 4, "{field}");
        assert!(
            d.fields.iter().any(|f| f.field == field),
            "{field}: {:?}",
            d.fields
        );
    }
}

#[test]
fn an_atlas_for_another_cartridge_is_refused() {
    let raw = rom();
    let atlas = atlas_for(&raw);
    let mut other = raw.clone();
    other.truncate(0x8000);
    other[0x7FD7] = 5;
    assert!(run(&other, &atlas, inputs(2), Options::default()).is_err());
}
