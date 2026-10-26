//! Ticket W36-09: the understand pass end to end on a small synthetic NES
//! cartridge. Run it, profile every function by observation, and check the
//! rules namer finds the reset, NMI, pad-reading and OAM-DMA routines from
//! what they do to the hardware alone.

use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_nes::core::NesCore;
use rf_nes::rebuild::{build_atlas, Atlas};
use rf_rebuild::naming::propose_names;
use rf_rebuild::profile::observe;
use rf_rebuild::Platform;

/// Reset: NMI on, then forever `JSR pads; JSR oam; JMP loop`.
/// `pads` strobes `$4016` and reads it eight times; `oam` triggers OAM DMA
/// from page 2; the NMI handler is `INC $10; RTI`.
fn rom() -> Vec<u8> {
    let mut d = vec![0u8; 16];
    d[0..4].copy_from_slice(b"NES\x1a");
    d[4] = 1;
    d[5] = 1;
    let mut prg = vec![0xEAu8; 0x4000];
    let mut put = |at: usize, code: &[u8]| prg[at..at + code.len()].copy_from_slice(code);
    put(
        0x0000,
        &[
            0x78, 0xA9, 0x80, 0x8D, 0x00, 0x20, // SEI; LDA #$80; STA $2000
            0x20, 0x00, 0x81, // loop: JSR $8100
            0x20, 0x20, 0x81, // JSR $8120
            0x4C, 0x06, 0x80, // JMP loop
        ],
    );
    put(
        0x0100,
        &[
            0xA9, 0x01, 0x8D, 0x16, 0x40, 0xA9, 0x00, 0x8D, 0x16, 0x40, // strobe
            0xA2, 0x08, 0xAD, 0x16, 0x40, 0xCA, 0xD0, 0xFA, 0x60, // 8 reads
        ],
    );
    put(0x0120, &[0xA9, 0x02, 0x8D, 0x14, 0x40, 0x60]);
    put(0x0140, &[0xE6, 0x10, 0x40]);
    prg[0x3FFA..0x3FFC].copy_from_slice(&0x8140u16.to_le_bytes());
    prg[0x3FFC..0x3FFE].copy_from_slice(&0x8000u16.to_le_bytes());
    d.extend(prg);
    d.extend(vec![0u8; 0x2000]);
    d
}

struct Quiet;
impl CoreSink for Quiet {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

fn atlas_of(raw: &[u8]) -> Atlas {
    let mut rec = NesCore::from_ines_bytes(raw).expect("loads");
    rec.set_coverage_recording(true);
    rec.reset(ResetKind::Hard);
    for _ in 0..4 {
        rec.run_frame(&InputFrame::empty(), &mut Quiet);
    }
    let log = rec.take_coverage().expect("armed");
    let prg = rf_nes::NesRom::from_ines_bytes(raw).expect("rom");
    build_atlas(prg.prg_rom(), &log)
}

#[test]
fn the_rules_name_the_routines_from_their_behaviour() {
    let raw = rom();
    let atlas = atlas_of(&raw);
    let mut core = NesCore::from_ines_bytes(&raw).expect("loads");
    core.reset(ResetKind::Hard);
    let inputs = vec![InputFrame::empty(); 6];
    let u = observe(&mut core, Platform::Nes, &atlas, inputs, |c, _, i| {
        c.bus_mut().set_controller_buttons(0, i.ports[0] as u8);
    })
    .expect("observes");
    assert_eq!(u.frames, 6);
    assert_eq!(u.vectors.nmi, vec![0x8140]);
    let names = propose_names(&u, &atlas);
    let by_entry = |cpu: u32| {
        let f = atlas
            .functions
            .iter()
            .position(|f| f.entry_cpu == cpu)
            .expect("function") as u32;
        names
            .iter()
            .find(|p| p.function == f)
            .map(|p| (p.name.as_str(), p.confidence))
    };
    assert_eq!(by_entry(0x8000), Some(("reset", 99)));
    assert_eq!(by_entry(0x8140), Some(("nmi", 99)));
    assert_eq!(by_entry(0x8100), Some(("read_controllers", 92)));
    assert_eq!(by_entry(0x8120), Some(("oam_dma", 97)));

    // Behaviour facts the namer used.
    let pads = atlas
        .functions
        .iter()
        .position(|f| f.entry_cpu == 0x8100)
        .unwrap();
    let p = &u.functions[pads];
    assert_eq!(p.reg(0x4016).reads, 8 * p.calls);
    assert_eq!(p.reg(0x4016).writes, 2 * p.calls);
    assert!(p.callers.contains_key(&0), "called from the reset function");
    let nmi = atlas
        .functions
        .iter()
        .position(|f| f.entry_cpu == 0x8140)
        .unwrap();
    assert!(
        (5..=6).contains(&u.functions[nmi].nmi_entries),
        "about one NMI per frame (the first frame ends before NMI is on)"
    );
    assert_eq!(u.functions[nmi].nmi_insns, u.functions[nmi].insns);
    assert!(u.functions[nmi].ram_write.contains(0x10));
    assert!(p.main_insns > 0 && p.nmi_insns == 0);
}
