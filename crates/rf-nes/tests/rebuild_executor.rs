//! **The block executor changes nothing it does not announce** (ticket
//! W36-03; `docs/design/REBUILD_LAB.md` section 4).
//!
//! Synthetic cartridges: record a run, build the atlas, then run the same
//! cartridge with the executor off and on and compare everything that is
//! observable every frame. The executor is the interpreter with a map, so
//! bit identity is the whole contract; the counters are the other half.

use rf_core_api::CpuRegs;
use rf_core_api::{
    CoreEvent, CoreSink, EmulatorCore, InputFrame, Mos6502Regs, PpuPixel, RebuildStats, ResetKind,
};
use rf_nes::core::NesCore;
use rf_nes::rebuild::{build_atlas, Atlas};
use rf_nes::NesRom;

struct Hasher {
    h: u64,
}

impl Hasher {
    fn new() -> Self {
        Self {
            h: 0xcbf2_9ce4_8422_2325,
        }
    }
    fn feed(&mut self, b: u8) {
        self.h ^= u64::from(b);
        self.h = self.h.wrapping_mul(0x0100_0000_01b3);
    }
}

impl CoreSink for Hasher {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        self.feed(y as u8);
        for p in pixels {
            self.feed(p.palette_index);
        }
    }
    fn audio(&mut self, samples: &[i16]) {
        for s in samples {
            self.feed(*s as u8);
        }
    }
    fn event(&mut self, _event: CoreEvent) {}
}

fn ines(mapper: u8, prg: &[u8]) -> Vec<u8> {
    assert_eq!(prg.len() % 0x4000, 0);
    let mut d = vec![0u8; 16];
    d[0..4].copy_from_slice(b"NES\x1a");
    d[4] = (prg.len() / 0x4000) as u8;
    d[5] = 1;
    d[6] = (mapper & 0x0F) << 4;
    d[7] = mapper & 0xF0;
    d.extend_from_slice(prg);
    d.extend(vec![0u8; 0x2000]);
    d
}

fn put(prg: &mut [u8], at: usize, code: &[u8]) {
    prg[at..at + code.len()].copy_from_slice(code);
}

/// 16 KiB NROM: RAM routine via JSR/RTS, an RTS-trick jump, `JMP (ind)`
/// alternating between two targets, NMI with RTI.
fn nrom_ines() -> Vec<u8> {
    let mut prg = vec![0u8; 0x4000];
    put(
        &mut prg,
        0x00,
        &[
            0xA9, 0xE8, // 00 LDA #$E8   (INX)
            0x8D, 0x00, 0x03, // 02 STA $0300
            0xA9, 0x60, // 05 LDA #$60   (RTS)
            0x8D, 0x01, 0x03, // 07 STA $0301
            0xA9, 0x80, // 0A LDA #$80   (NMI on)
            0x8D, 0x00, 0x20, // 0C STA $2000
            0x20, 0x00, 0x03, // 0F JSR $0300  (runs from RAM)
            0xA9, 0x80, // 12 LDA #$80
            0x48, // 14 PHA
            0xA9, 0x1F, // 15 LDA #$1F
            0x48, // 17 PHA
            0x60, // 18 RTS -> $8020
        ],
    );
    put(
        &mut prg,
        0x20,
        &[
            0xE6, 0x10, // 20 INC $10
            0xA5, 0x10, // 22 LDA $10
            0x29, 0x01, // 24 AND #$01
            0xAA, // 26 TAX
            0xBD, 0x60, 0x80, // 27 LDA $8060,X
            0x8D, 0x10, 0x03, // 2A STA $0310
            0xA9, 0x80, // 2D LDA #$80
            0x8D, 0x11, 0x03, // 2F STA $0311
            0x6C, 0x10, 0x03, // 32 JMP ($0310)
        ],
    );
    put(&mut prg, 0x40, &[0xE8, 0xE8, 0x4C, 0x0F, 0x80]); // INX INX JMP $800F
    put(&mut prg, 0x50, &[0xCA, 0x4C, 0x0F, 0x80]); // DEX JMP $800F
    put(&mut prg, 0x60, &[0x40, 0x50]);
    put(&mut prg, 0x100, &[0xE6, 0x00, 0x40]); // NMI: INC $00; RTI
    put(&mut prg, 0x3FFA, &[0x00, 0x81, 0x00, 0x80, 0x00, 0x82]);
    ines(0, &prg)
}

/// Mapper 66 (GxROM), two 32 KiB PRG banks. Bank 0 selects bank 1 and jumps
/// to `$8200`; bank 1's code there selects bank 0 and jumps to `$8300`. The
/// register write targets `$9000`, which reads `$FF` in both banks, so bus
/// conflicts leave the value alone.
fn gxrom_ines() -> Vec<u8> {
    let mut prg = vec![0xFFu8; 0x10000];
    for bank in 0..2usize {
        let b = bank * 0x8000;
        put(&mut prg, b + 0x100, &[0xE6, 0x00, 0x40]); // NMI: INC $00; RTI
        put(&mut prg, b + 0x7FFA, &[0x00, 0x81, 0x00, 0x80, 0x00, 0x82]);
    }
    put(
        &mut prg,
        0x0000,
        &[
            0xA9, 0x80, 0x8D, 0x00, 0x20, // 00 NMI on
            0xA9, 0x10, 0x8D, 0x00, 0x90, // 05 select PRG bank 1
            0x4C, 0x00, 0x82, // 0A JMP $8200
        ],
    );
    put(
        &mut prg,
        0x0300,
        &[0xE6, 0x20, 0x4C, 0x05, 0x80], // INC $20; JMP $8005
    );
    // The instruction after each register write is fetched from the *new*
    // bank, so each bank carries the continuation the other one needs.
    put(&mut prg, 0x8000 + 0x0A, &[0x4C, 0x00, 0x82]); // bank 1: JMP $8200
    put(&mut prg, 0x0207, &[0x4C, 0x00, 0x83]); // bank 0: JMP $8300
    put(
        &mut prg,
        0x8200,
        &[
            0xE6, 0x21, // INC $21
            0xA9, 0x00, 0x8D, 0x00, 0x90, // select PRG bank 0
            0x4C, 0x00, 0x83, // JMP $8300
        ],
    );
    ines(66, &prg)
}

/// Everything observable after a frame.
#[derive(Debug, PartialEq, Eq)]
struct Snap {
    regs: Mos6502Regs,
    wram: u64,
    video_audio: u64,
    vram_oam_pal: u64,
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h = Hasher::new();
    bytes.iter().for_each(|&b| h.feed(b));
    h.h
}

fn snap(core: &NesCore, sink: &Hasher) -> Snap {
    let v = core.state_view();
    let CpuRegs::Mos6502(regs) = v.cpu_regs else {
        panic!("6502");
    };
    let mut other = v.vram.to_vec();
    other.extend_from_slice(v.oam);
    other.extend_from_slice(v.cgram);
    Snap {
        regs,
        wram: fnv(v.wram),
        video_audio: sink.h,
        vram_oam_pal: fnv(&other),
    }
}

fn run(core: &mut NesCore, frames: usize) -> Vec<Snap> {
    let mut sink = Hasher::new();
    (0..frames)
        .map(|_| {
            core.run_frame(&InputFrame::empty(), &mut sink);
            snap(core, &sink)
        })
        .collect()
}

fn fresh(ines: &[u8]) -> NesCore {
    let mut core = NesCore::from_ines_bytes(ines).expect("loads");
    core.reset(ResetKind::Hard);
    core
}

fn record_atlas(ines: &[u8], frames: usize) -> Atlas {
    let mut core = NesCore::from_ines_bytes(ines).expect("loads");
    core.set_coverage_recording(true);
    core.reset(ResetKind::Hard);
    run(&mut core, frames);
    let log = core.take_coverage().expect("armed");
    let rom = NesRom::from_ines_bytes(ines).expect("parses");
    build_atlas(rom.prg_rom(), &log)
}

fn with_rebuild(ines: &[u8], atlas: Atlas) -> NesCore {
    let mut core = fresh(ines);
    core.set_rebuild(Some(atlas)).expect("installs");
    core
}

const FRAMES: usize = 40;

#[test]
fn nrom_program_is_bit_identical_with_rebuild_on() {
    let image = nrom_ines();
    let atlas = record_atlas(&image, FRAMES);
    let off = run(&mut fresh(&image), FRAMES);
    let mut core = with_rebuild(&image, atlas);
    let on = run(&mut core, FRAMES);
    assert_eq!(off, on);

    let s = core.rebuild_stats().expect("installed");
    assert!(s.observed > 100, "{s:?}");
    assert!(s.fallback > 0, "the RAM routine runs through the fallback");
    assert!(s.interrupt_entries >= 30, "an NMI every frame");
    assert!(s.indirect_checked > 0);
    assert_eq!(s.dispatch_misses, 0, "same program, same targets: {s:?}");
    assert!(core.rebuild_deltas().is_empty());
    assert!(s.fallback_rate() < 0.5);
}

#[test]
fn unrecorded_indirect_target_is_counted_and_changes_nothing() {
    let image = nrom_ines();
    let mut atlas = record_atlas(&image, FRAMES);
    // Forget one of the two JMP ($0310) targets: $8050 (ROM offset 0x50).
    let site = atlas
        .indirect_sites
        .iter_mut()
        .find(|s| s.site_rom == 0x32)
        .expect("the JMP indirect site");
    let before = site.targets.len();
    site.targets.retain(|t| t.rom() != Some(0x50));
    assert_eq!(site.targets.len(), before - 1);

    let off = run(&mut fresh(&image), FRAMES);
    let mut core = with_rebuild(&image, atlas);
    let on = run(&mut core, FRAMES);
    assert_eq!(off, on, "a miss must never change behaviour");

    let s = core.rebuild_stats().unwrap();
    assert!(s.dispatch_misses >= 10, "every second loop misses: {s:?}");
    let deltas = core.rebuild_deltas();
    assert_eq!(deltas.len(), 1, "deduplicated: {deltas:?}");
    assert_eq!(deltas[0].site.rom, Some(0x32));
    assert_eq!(deltas[0].target.rom, Some(0x50));
    assert_eq!(s.delta_len, 1);
}

#[test]
fn rebuild_armed_with_the_coverage_recorder_is_still_identical() {
    let image = nrom_ines();
    let atlas = record_atlas(&image, FRAMES);
    let off = run(&mut fresh(&image), FRAMES);

    let mut core = with_rebuild(&image, atlas);
    core.set_coverage_recording(true);
    let both = run(&mut core, FRAMES);
    assert_eq!(off, both);
    let log = core.take_coverage().expect("recorder ran alongside");
    assert!(log.instructions_recorded > 100);
    assert!(core.rebuild_stats().unwrap().observed > 100);

    // Disarming one leaves the other working.
    core.set_coverage_recording(false);
    let before = core.rebuild_stats().unwrap().observed;
    run(&mut core, 2);
    assert!(core.rebuild_stats().unwrap().observed > before);
}

#[test]
fn bank_switching_cart_is_identical_and_never_falls_back() {
    let image = gxrom_ines();
    let atlas = record_atlas(&image, FRAMES);
    let off = run(&mut fresh(&image), FRAMES);
    let mut core = with_rebuild(&image, atlas);
    let on = run(&mut core, FRAMES);
    assert_eq!(off, on);
    assert!(
        core.peek(0x20) > 0 && core.peek(0x21) > 0,
        "both banks ran: {} {}",
        core.peek(0x20),
        core.peek(0x21)
    );

    let s = core.rebuild_stats().unwrap();
    assert_eq!(s.fallback, 0, "every executed byte is in the atlas: {s:?}");
    assert_eq!(s.dispatch_misses, 0);
    assert!(s.observed > 100);
}

#[test]
fn an_atlas_for_another_cartridge_is_refused_and_load_drops_it() {
    let nrom = nrom_ines();
    let gx = gxrom_ines();
    let mut core = fresh(&gx);
    let err = core.set_rebuild(Some(record_atlas(&nrom, 4)));
    assert!(err.is_err(), "16 KiB atlas into a 64 KiB cart");
    assert!(core.rebuild_stats().is_none());

    core.set_rebuild(Some(record_atlas(&gx, 4))).unwrap();
    assert!(core.rebuild_stats().is_some());
    core.set_rebuild(None).unwrap();
    assert!(core.rebuild_stats().is_none());

    core.set_rebuild(Some(record_atlas(&gx, 4))).unwrap();
    core.load(rf_core_api::CartImage {
        rom: &gx,
        sram: None,
    })
    .unwrap();
    assert!(core.rebuild_stats().is_none(), "a new cartridge drops it");
}

#[test]
fn the_trait_entry_point_downcasts_the_atlas_and_rejects_strangers() {
    let image = nrom_ines();
    let mut core = fresh(&image);
    let dynamic: &mut dyn EmulatorCore = &mut core;
    assert!(dynamic.set_rebuild_atlas(Some(Box::new(5u32))).is_err());
    dynamic
        .set_rebuild_atlas(Some(Box::new(record_atlas(&image, 4))))
        .unwrap();
    assert_eq!(
        dynamic.rebuild_stats().map(|s: RebuildStats| s.observed),
        Some(0)
    );
    dynamic.set_rebuild_atlas(None).unwrap();
    assert!(dynamic.rebuild_stats().is_none());
}
