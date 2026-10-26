//! **Amdahl bench for the rebuild JIT ceiling** (ticket W36-12;
//! `docs/design/REBUILD_LAB.md` section 4, "Known tension").
//!
//! The NES core ticks the PPU and APU on **every bus cycle** (`NesBus::read`
//! / `write`), so a compiled tier that must issue the identical cycle stream
//! can only remove opcode decode and dispatch (and, optimistically, the ALU
//! and flag work the interpreter does generically). This bench measures how
//! much time that is, before any IR exists.
//!
//! - **(a) real frame**: the CPU stepping the real bus for a window of
//!   frames, draining video and audio the way a host does
//!   (`machine_frame.rs` shape).
//! - **(b) flat replay**: the *same instruction stream* re-run against a
//!   flat bus with no devices. A recording pass logs every bus operation
//!   (address, value, direction) and the NMI/IRQ line level after it; the
//!   replay bus hands the recorded values back and re-plays the line levels,
//!   so `CountingBus`'s interrupt polling sees the same inputs. The setup
//!   asserts the replayed `Cpu` equals the recorded end state and that every
//!   address and direction matched, which is the proof that (b) executed the
//!   identical stream rather than a similar one.
//! - **(c) bus and device cost** = (a) - (b). The share a JIT could remove is
//!   at most (b)/(a): both arms pay the `dyn CpuBus` calls and the interrupt
//!   sampling wrapper, and the replay still runs the interpreter's own
//!   decode, dispatch and ALU, so (b) is an upper bound on what compiled
//!   code can take out.
//!
//! Workloads: the synthetic machine_frame loop and our own homebrew
//! RF-Scroller (both in the tree, always run if built), plus a cartridge
//! named by `RF_AMDAHL_ROM` (a raw iNES file; the numbers in the design doc
//! were taken with Super Mario Bros. extracted from the multicart). The
//! scripted-input window is the same in all three arms.
//!
//! ```text
//! cargo bench -p rf-nes --bench amdahl
//! RF_AMDAHL_ROM=/path/to/game.nes cargo bench -p rf-nes --bench amdahl
//! ```

use std::path::Path;
use std::time::{Duration, Instant};

use criterion::{criterion_group, criterion_main, Criterion};
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_nes::core::NesCore;
use rf_nes::{Cpu, CpuBus, NesBus};

struct NullSink;

impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _ev: CoreEvent) {}
}

/// One bus operation as the CPU saw it, plus the interrupt lines after it.
#[derive(Clone, Copy)]
struct Ev {
    addr: u16,
    val: u8,
    /// bit 0 write, bit 1 NMI line, bit 2 IRQ line (after the op).
    bits: u8,
}

const WRITE: u8 = 1;
const NMI: u8 = 2;
const IRQ: u8 = 4;

/// Wraps the real bus and logs what the CPU saw. Everything else delegates.
struct Recorder<'a> {
    bus: &'a mut NesBus,
    log: &'a mut Vec<Ev>,
}

impl Recorder<'_> {
    fn lines(&self) -> u8 {
        (if self.bus.nmi_line() { NMI } else { 0 }) | (if self.bus.irq_line() { IRQ } else { 0 })
    }
}

impl CpuBus for Recorder<'_> {
    fn read(&mut self, addr: u16) -> u8 {
        let val = self.bus.read(addr);
        let bits = self.lines();
        self.log.push(Ev { addr, val, bits });
        val
    }
    fn write(&mut self, addr: u16, val: u8) {
        self.bus.write(addr, val);
        let bits = self.lines() | WRITE;
        self.log.push(Ev { addr, val, bits });
    }
    fn nmi_line(&self) -> bool {
        self.bus.nmi_line()
    }
    fn nmi_edge_pending(&self) -> bool {
        self.bus.nmi_edge_pending()
    }
    fn irq_line(&self) -> bool {
        self.bus.irq_line()
    }
}

/// The flat bus: no devices, values and line levels from the recording.
/// `CHECK` additionally asserts address and direction on every op (setup
/// only; the timed arm is unchecked).
struct Replay<'a, const CHECK: bool> {
    ev: &'a [Ev],
    next: usize,
    nmi: bool,
    irq: bool,
}

impl<'a, const CHECK: bool> Replay<'a, CHECK> {
    fn new(ev: &'a [Ev], nmi: bool, irq: bool) -> Self {
        Self {
            ev,
            next: 0,
            nmi,
            irq,
        }
    }
    #[inline(always)]
    fn take(&mut self, addr: u16, write: bool) -> Ev {
        let e = self.ev[self.next];
        self.next += 1;
        if CHECK {
            assert_eq!(e.addr, addr, "op {}: address", self.next - 1);
            assert_eq!(
                e.bits & WRITE != 0,
                write,
                "op {}: direction",
                self.next - 1
            );
        }
        self.nmi = e.bits & NMI != 0;
        self.irq = e.bits & IRQ != 0;
        e
    }
}

impl<const CHECK: bool> CpuBus for Replay<'_, CHECK> {
    #[inline(always)]
    fn read(&mut self, addr: u16) -> u8 {
        self.take(addr, false).val
    }
    #[inline(always)]
    fn write(&mut self, addr: u16, val: u8) {
        let e = self.take(addr, true);
        if CHECK {
            assert_eq!(e.val, val, "op {}: written value", self.next - 1);
        }
    }
    #[inline(always)]
    fn nmi_line(&self) -> bool {
        self.nmi
    }
    #[inline(always)]
    fn irq_line(&self) -> bool {
        self.irq
    }
}

/// A workload: an iNES image, how many frames to run before the window, the
/// window length, and the scripted controller byte per frame.
struct Workload {
    name: &'static str,
    rom: Vec<u8>,
    lead_in: u32,
    window: u32,
    script: fn(u32) -> u8,
}

fn synthetic_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 16 + 0x4000 + 0x2000];
    rom[0..4].copy_from_slice(b"NES\x1a");
    rom[4] = 1;
    rom[5] = 1;
    let prg = &mut rom[16..16 + 0x4000];
    let code: &[u8] = &[
        0xA9, 0x1E, 0x8D, 0x01, 0x20, // rendering on
        0xA9, 0x9F, 0x8D, 0x00, 0x40, // pulse 1 on
        0xA9, 0x40, 0x8D, 0x02, 0x40, //
        0xA9, 0x08, 0x8D, 0x03, 0x40, //
        0xA9, 0x01, 0x8D, 0x15, 0x40, //
        0xAD, 0x00, 0x00, 0x69, 0x01, 0x8D, 0x00, 0x00, // loop at $8018
        0xEE, 0x01, 0x00, 0x4C, 0x18, 0x80,
    ];
    prg[..code.len()].copy_from_slice(code);
    prg[0x3FFC] = 0x00;
    prg[0x3FFD] = 0x80;
    rom
}

fn no_input(_frame: u32) -> u8 {
    0
}

/// RF-Scroller: hold Right (bit 7).
fn hold_right(_frame: u32) -> u8 {
    0x80
}

/// SMB through the multicart menu: two Starts, then Right with a jump in
/// every 60 frames (the W36-03 drive script one).
fn smb_script(frame: u32) -> u8 {
    const START: u8 = 1 << 3;
    match frame {
        0..=199 => 0,
        200..=204 | 400..=404 => START,
        205..=399 | 405..=499 => 0,
        f if (f - 500) % 60 < 12 => 0x80 | 1,
        _ => 0x80,
    }
}

fn workloads() -> Vec<Workload> {
    let mut w = vec![Workload {
        name: "synthetic",
        rom: synthetic_rom(),
        lead_in: 2,
        window: 30,
        script: no_input,
    }];
    let scroller = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/nes/rf-scroller/build/rf-scroller.nes");
    if let Ok(rom) = std::fs::read(scroller) {
        w.push(Workload {
            name: "rf_scroller",
            rom,
            lead_in: 120,
            window: 60,
            script: hold_right,
        });
    }
    if let Some(path) = std::env::var_os("RF_AMDAHL_ROM") {
        let rom = std::fs::read(&path).expect("RF_AMDAHL_ROM is readable");
        w.push(Workload {
            name: "smb",
            rom,
            lead_in: 650,
            window: 60,
            script: smb_script,
        });
    }
    w
}

fn input_for(script: fn(u32) -> u8, frame: u32) -> InputFrame {
    let mut i = InputFrame::empty();
    i.ports[0] = u16::from(script(frame));
    i
}

/// A core advanced through the lead-in with the scripted input.
fn warmed_core(w: &Workload) -> NesCore {
    let mut core = NesCore::from_ines_bytes(&w.rom).expect("rom loads");
    core.reset(ResetKind::Hard);
    for f in 0..w.lead_in {
        core.run_frame(&input_for(w.script, f), &mut NullSink);
    }
    core
}

/// One frame of real stepping, drains inside the timed region.
#[inline(never)]
fn real_frame(cpu: &mut Cpu, bus: &mut NesBus, buttons: u8) -> u32 {
    bus.set_controller_buttons(0, buttons);
    let start = bus.frame_count();
    let mut steps = 0;
    while bus.frame_count() == start {
        cpu.step(bus);
        bus.drain_video(&mut NullSink);
        bus.drain_audio(&mut NullSink);
        steps += 1;
    }
    steps
}

/// The recorded window: start state, bus operations, end state.
struct Trace {
    start: Cpu,
    nmi0: bool,
    irq0: bool,
    ev: Vec<Ev>,
    steps: u32,
    end: Cpu,
}

fn record(w: &Workload) -> Trace {
    let mut core = warmed_core(w);
    let (cpu, bus) = core.parts_mut();
    let start = *cpu;
    let (nmi0, irq0) = (bus.nmi_line(), bus.irq_line());
    let mut ev = Vec::new();
    let mut steps = 0u32;
    for f in 0..w.window {
        bus.set_controller_buttons(0, w.script(w.lead_in + f));
        let first = bus.frame_count();
        while bus.frame_count() == first {
            cpu.step(&mut Recorder { bus, log: &mut ev });
            bus.drain_video(&mut NullSink);
            bus.drain_audio(&mut NullSink);
            steps += 1;
        }
    }
    let end = *cpu;
    Trace {
        start,
        nmi0,
        irq0,
        ev,
        steps,
        end,
    }
}

impl Workload {
    fn script(&self, frame: u32) -> u8 {
        (self.script)(frame)
    }
}

/// Prove the replay executes the recorded stream (module doc).
fn verify(t: &Trace) {
    let mut cpu = t.start;
    let mut bus = Replay::<true>::new(&t.ev, t.nmi0, t.irq0);
    for _ in 0..t.steps {
        cpu.step(&mut bus);
    }
    assert_eq!(bus.next, t.ev.len(), "replay consumed the whole trace");
    assert_eq!(cpu, t.end, "replay ended in the recorded Cpu state");
}

#[inline(never)]
fn replay(t: &Trace) -> Cpu {
    let mut cpu = t.start;
    let mut bus = Replay::<false>::new(&t.ev, t.nmi0, t.irq0);
    for _ in 0..t.steps {
        cpu.step(&mut bus);
    }
    cpu
}

fn bench_amdahl(c: &mut Criterion) {
    for w in workloads() {
        let trace = record(&w);
        verify(&trace);
        eprintln!(
            "amdahl[{}]: {} frames, {} instructions, {} bus cycles ({:.1} cycles/instr)",
            w.name,
            w.window,
            trace.steps,
            trace.ev.len(),
            trace.ev.len() as f64 / f64::from(trace.steps)
        );

        let mut g = c.benchmark_group(format!("amdahl_{}", w.name));
        g.sample_size(10);
        g.warm_up_time(Duration::from_secs(1));
        g.measurement_time(Duration::from_secs(8));

        // (a) one iteration = the whole window on the real bus.
        g.bench_function("a_real_window", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let mut core = warmed_core(&w);
                    let (cpu, bus) = core.parts_mut();
                    let t0 = Instant::now();
                    for f in 0..w.window {
                        std::hint::black_box(real_frame(cpu, bus, w.script(w.lead_in + f)));
                    }
                    total += t0.elapsed();
                }
                total
            });
        });
        // (b) the same instruction stream on the flat bus.
        g.bench_function("b_flat_replay", |b| {
            b.iter(|| std::hint::black_box(replay(&trace)));
        });
        g.finish();
    }
}

criterion_group!(benches, bench_amdahl);
criterion_main!(benches);
