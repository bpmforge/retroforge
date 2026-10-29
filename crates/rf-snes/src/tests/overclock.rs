//! Ticket W30-08 part A: the SNES CPU and SA-1 clock multipliers, and the
//! idle observer (`frame_idle`), on synthetic 65816 programs.
//!
//! Every program is a few bytes of hand-assembled code on a bare LoROM
//! image, so each number asserted here can be derived from the opcodes
//! rather than recorded from a run.

use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, PpuPixel, StateError, StateWriter, Step};

use crate::core::SnesCore;
use crate::state::StateRegion;

/// A bare LoROM image: 32 KiB, header at `$7FC0`, `chipset`/`map` as given,
/// reset vector `$8000`, native-mode NMI vector `$8100` (handler: `INC $20;
/// RTI`).
fn image(map: u8, chipset: u8, program: &[u8]) -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    let base = 0x7FC0;
    rom[base + 0x15] = map;
    rom[base + 0x16] = chipset;
    rom[base + 0x17] = 6;
    rom[base + 0x18] = 3;
    let checksum: u16 = 0xBEEF;
    rom[base + 0x1C..base + 0x1E].copy_from_slice(&(checksum ^ 0xFFFF).to_le_bytes());
    rom[base + 0x1E..base + 0x20].copy_from_slice(&checksum.to_le_bytes());
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    // Native-mode NMI vector ($FFEA) and emulation-mode ($FFFA) both point
    // at the handler.
    for v in [0x7FEA, 0x7FFA] {
        rom[v] = 0x00;
        rom[v + 1] = 0x81;
    }
    rom[0x100..0x103].copy_from_slice(&[0xE6, 0x20, 0x40]); // INC $20 ; RTI
    rom[..program.len()].copy_from_slice(program);
    rom
}

/// Plain LoROM cart.
fn plain(program: &[u8]) -> Vec<u8> {
    image(0x20, 0x00, program)
}

/// `n` iterations of a write-carrying work loop per NMI period, then
/// (optionally) `WAI`:
///
/// ```text
/// CLC / XCE / REP #$10 / LDA #$80 / STA $4200     ; native, 16-bit X, NMI on
/// main: LDX #n
/// w:    STX $12 ; DEX ; BNE w                      ; n iterations, one write each
///       INC $22                                    ; passes completed
///       [WAI]
///       BRA main
/// ```
fn work_loop(n: u16, wai: bool) -> Vec<u8> {
    let mut p = vec![0x18, 0xFB, 0xC2, 0x10, 0xA9, 0x80, 0x8D, 0x00, 0x42];
    p.extend_from_slice(&[0xA2, n as u8, (n >> 8) as u8]); // main: LDX #n
    p.extend_from_slice(&[0x86, 0x12, 0xCA, 0xD0, 0xFB]); // w: STX $12; DEX; BNE w
    p.extend_from_slice(&[0xE6, 0x22]); // INC $22: passes completed
    if wai {
        p.push(0xCB);
    }
    let main = 9i32;
    let here = p.len() as i32 + 2;
    p.extend_from_slice(&[0x80, (main - here) as i8 as u8]); // BRA main
    p
}

/// `loop: LDA $4210 ; BPL loop ; BRA loop` — a vblank-flag poll with no
/// writes: the canonical "idle in a tight loop" shape.
fn poll_loop() -> Vec<u8> {
    vec![0xAD, 0x10, 0x42, 0x10, 0xFB, 0x80, 0xF9]
}

/// `loop: INC $10 ; BRA loop` — a loop that writes every pass, so it is
/// busy work, not waiting.
fn counter_loop() -> Vec<u8> {
    vec![0xE6, 0x10, 0x80, 0xFC]
}

struct Snapshot(Vec<u8>);
impl StateWriter for Snapshot {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), StateError> {
        self.0.extend_from_slice(bytes);
        Ok(())
    }
}

/// FNV-1a over a byte stream.
fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
    }
    h
}

const FNV_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

/// Hashes video and audio and counts audio samples.
#[derive(Default)]
struct Sink {
    video: u64,
    audio: u64,
    audio_samples: usize,
}

impl CoreSink for Sink {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        let mut h = self.video ^ u64::from(y);
        for p in pixels {
            h = (h ^ u64::from(p.palette_index)).wrapping_mul(0x100_0000_01b3);
        }
        self.video = h;
    }
    fn audio(&mut self, samples: &[i16]) {
        self.audio_samples += samples.len();
        for s in samples {
            self.audio = fnv(self.audio, &s.to_le_bytes());
        }
    }
    fn event(&mut self, _ev: CoreEvent) {}
}

/// Everything a determinism comparison needs from one run.
#[derive(Debug, PartialEq, Eq)]
struct Run {
    state: u64,
    video: u64,
    audio: u64,
    audio_samples: usize,
    nmi_count: u8,
    passes: u8,
    master_cycles: u64,
}

fn state_hash(core: &SnesCore) -> u64 {
    let mut h = FNV_BASIS;
    for region in StateRegion::ALL {
        let mut w = Snapshot(Vec::new());
        core.system().save_region(region, &mut w).expect("save");
        h = fnv(h, &w.0);
    }
    h
}

fn run_frames(core: &mut SnesCore, frames: u32) -> Run {
    let mut sink = Sink {
        video: FNV_BASIS,
        audio: FNV_BASIS,
        ..Sink::default()
    };
    for _ in 0..frames {
        let _ = core.step(Step::Frame, &mut sink);
    }
    let wram = &core.system().bus.wram;
    Run {
        state: state_hash(core),
        video: sink.video,
        audio: sink.audio,
        audio_samples: sink.audio_samples,
        nmi_count: wram[0x20],
        passes: wram[0x22],
        master_cycles: core.system().master_cycles,
    }
}

const FRAMES: u32 = 30;

/// The four programs the pinned run covers.
fn programs() -> [Vec<u8>; 4] {
    [
        work_loop(6000, true),
        work_loop(100, true),
        poll_loop(),
        counter_loop(),
    ]
}

/// A core running `program` with `configure` applied before the first frame.
fn core_with(program: &[u8], configure: impl FnOnce(&mut SnesCore)) -> SnesCore {
    let mut core = SnesCore::load(&plain(program)).expect("loads");
    configure(&mut core);
    core
}

/// The default machine, pinned. These values were captured on the tree
/// BEFORE the W30-08 overclock plumbing existed, so this is the
/// "byte-identical to today" proof law 6 asks for, not a self-comparison:
/// state (every save-state region), video, audio, and the NMI count, work
/// done and master cycles after 30 frames of four different programs.
///
/// The FIRST column (state) was re-pinned when `PPU_` gained the `$212D` TS
/// byte: it hashes the save regions, and the PPU region is one byte longer.
/// Video, audio, NMI count, work done and master cycles are the original
/// pre-overclock values, unchanged.
///
/// Re-pinned for ticket W37-00b (the per-scanline DRAM refresh stall): the
/// state hash and the master-cycle count move (the last instruction of a
/// frame ends a few master cycles later), while video, audio, NMI count and
/// work done stay exactly the pre-overclock values. Still a comparison
/// against the build before the overclock plumbing, not a self-comparison.
#[test]
fn default_machine_is_byte_identical_to_the_pre_overclock_build() {
    const PINNED: [(u64, u64, u64, usize, u8, u8, u64); 4] = [
        (
            2900243004634852721,
            17085499001889628965,
            3322592713610917373,
            23931,
            30,
            15,
            10721050,
        ),
        (
            5708542737912139829,
            17085499001889628965,
            3322592713610917373,
            23931,
            30,
            31,
            10721040,
        ),
        (
            7061622125700991210,
            17085499001889628965,
            3322592713610917373,
            23931,
            0,
            0,
            10721046,
        ),
        (
            221239786516649221,
            17085499001889628965,
            3322592713610917373,
            23931,
            0,
            0,
            10721040,
        ),
    ];
    for (prog, want) in programs().iter().zip(PINNED) {
        let got = run_frames(&mut core_with(prog, |_| {}), FRAMES);
        assert_eq!(
            (
                got.state,
                got.video,
                got.audio,
                got.audio_samples,
                got.nmi_count,
                got.passes,
                got.master_cycles
            ),
            want
        );
    }
}

/// Asking for 100% explicitly is the same machine as never asking, for all
/// three chip multipliers.
#[test]
fn explicit_hundred_percent_is_the_default_machine() {
    for prog in programs() {
        let base = run_frames(&mut core_with(&prog, |_| {}), FRAMES);
        let set = run_frames(
            &mut core_with(&prog, |c| {
                assert_eq!(c.set_cpu_overclock(100), 100);
                assert_eq!(c.set_sa1_overclock(100), 100);
                assert_eq!(c.set_gsu_overclock(100), 100);
            }),
            FRAMES,
        );
        assert_eq!(base, set);
    }
}

/// Whole multiples of 100, clamped to 100..=400, for both new settings.
#[test]
fn overclock_settings_round_and_clamp() {
    let mut core = core_with(&counter_loop(), |_| {});
    assert_eq!((core.cpu_overclock(), core.sa1_overclock()), (100, 100));
    assert_eq!(core.set_cpu_overclock(250), 200);
    assert_eq!(core.set_cpu_overclock(0), 100);
    assert_eq!(core.set_cpu_overclock(9000), 400);
    assert_eq!(core.set_sa1_overclock(399), 300);
    assert_eq!(core.set_sa1_overclock(1), 100);
    assert_eq!(core.set_sa1_overclock(u16::MAX), 400);
    assert!(!core.has_sa1());
}

/// Instructions run in `frames` frames (`StepResult::cycles` counts
/// instructions for this core).
fn instructions(core: &mut SnesCore, frames: u32) -> u64 {
    let mut sink = Sink::default();
    (0..frames)
        .map(|_| core.step(Step::Frame, &mut sink).cycles)
        .sum()
}

/// The 65816 gets `m` times the instructions per frame.
#[test]
fn cpu_overclock_scales_instructions_per_frame() {
    let base = instructions(&mut core_with(&counter_loop(), |_| {}), FRAMES) as f64;
    for (percent, m) in [(200, 2.0), (300, 3.0), (400, 4.0)] {
        let got = instructions(
            &mut core_with(&counter_loop(), |c| {
                c.set_cpu_overclock(percent);
            }),
            FRAMES,
        ) as f64;
        let ratio = got / base;
        assert!(
            (m * 0.97..=m * 1.03).contains(&ratio),
            "{percent}% ran {ratio:.3}x the instructions (want {m}x)"
        );
    }
}

/// What must NOT move: the frames' audio sample count, the NMI count (one
/// per frame) and the master clock after N frames — an overclocked CPU
/// does more per frame, not more frames.
#[test]
fn cpu_overclock_leaves_audio_frames_and_master_clock_alone() {
    for prog in [work_loop(6000, true), counter_loop()] {
        let base = run_frames(&mut core_with(&prog, |_| {}), FRAMES);
        for percent in [200, 400] {
            let fast = run_frames(
                &mut core_with(&prog, |c| {
                    c.set_cpu_overclock(percent);
                }),
                FRAMES,
            );
            assert_eq!(fast.audio_samples, base.audio_samples, "{percent}%");
            assert_eq!(fast.audio, base.audio, "{percent}%: audio content");
            assert_eq!(
                fast.nmi_count, base.nmi_count,
                "{percent}%: one NMI a frame"
            );
            // The master clock ends on a frame boundary either way; the
            // instruction that crosses it may overshoot by one instruction.
            let drift = fast.master_cycles.abs_diff(base.master_cycles);
            assert!(drift < 64, "{percent}%: master clock drifted {drift}");
        }
    }
}

/// A faster CPU finishes a job that overran: the work loop that needs
/// about two frames at 100% completes (about) twice as often at 200%.
#[test]
fn cpu_overclock_finishes_the_overrunning_job_sooner() {
    let base = run_frames(&mut core_with(&work_loop(6000, true), |_| {}), FRAMES);
    let fast = run_frames(
        &mut core_with(&work_loop(6000, true), |c| {
            c.set_cpu_overclock(200);
        }),
        FRAMES,
    );
    assert!(
        fast.passes > base.passes,
        "{} passes at 100%, {} at 200%",
        base.passes,
        fast.passes
    );
}

/// Frames (of `frames`, after `skip` warm-up frames) in which the S-CPU
/// reported idle, reading `frame_idle` after each frame step.
fn idle_frames(core: &mut SnesCore, skip: u32, frames: u32) -> u32 {
    let mut sink = Sink::default();
    let mut idle = 0;
    for f in 0..skip + frames {
        let _ = core.step(Step::Frame, &mut sink);
        if f >= skip && core.frame_idle() {
            idle += 1;
        }
    }
    idle
}

/// A program that finishes its frame's work early and `WAI`s is idle in
/// every frame.
#[test]
fn a_program_that_finishes_early_and_waits_is_idle_every_frame() {
    let mut core = core_with(&work_loop(100, true), |_| {});
    assert_eq!(idle_frames(&mut core, 3, 20), 20);
    assert!(
        core.frame_idle_cycles() > 100_000,
        "most of the frame idled"
    );
}

/// A vblank-flag poll (`LDA $4210 / BPL`), the other way games wait, is
/// idle too.
#[test]
fn a_tight_polling_loop_is_idle() {
    let mut core = core_with(&poll_loop(), |_| {});
    assert_eq!(idle_frames(&mut core, 3, 20), 20);
}

/// A loop that stores on every pass is work, not waiting.
#[test]
fn a_busy_loop_that_writes_is_never_idle() {
    let mut core = core_with(&counter_loop(), |_| {});
    assert_eq!(idle_frames(&mut core, 3, 20), 0);
    assert_eq!(core.frame_idle_cycles(), 0);
}

/// The slowdown signal itself: a job needing about 1.5 frames of CPU time
/// misses its `WAI` in every other frame at 100%, and with the CPU at 200%
/// it fits and the program idles every frame again.
#[test]
fn an_overrunning_program_is_not_idle_until_the_cpu_is_overclocked() {
    let at = |percent: u16| {
        let mut core = core_with(&work_loop(6000, true), |c| {
            c.set_cpu_overclock(percent);
        });
        idle_frames(&mut core, 3, 20)
    };
    let slow = at(100);
    assert!(
        (5..=15).contains(&slow),
        "overran in some frames, not all or none: idle in {slow}/20"
    );
    assert_eq!(at(200), 20);
}

/// The observer is read-only: asking for it every frame changes neither
/// the machine nor its output (and the pinned-default test above already
/// proves the machine with the observer present equals the one without).
#[test]
fn reading_the_idle_signal_does_not_change_the_machine() {
    for prog in programs() {
        let plain_run = run_frames(&mut core_with(&prog, |_| {}), FRAMES);
        let mut core = core_with(&prog, |_| {});
        let mut sink = Sink {
            video: FNV_BASIS,
            audio: FNV_BASIS,
            ..Sink::default()
        };
        let mut seen = 0u64;
        for _ in 0..FRAMES {
            let _ = core.step(Step::Frame, &mut sink);
            seen += u64::from(core.frame_idle())
                + core.frame_idle_cycles()
                + u64::from(core.sa1_frame_idle());
        }
        let _ = seen;
        assert_eq!(state_hash(&core), plain_run.state);
        assert_eq!(sink.video, plain_run.video);
        assert_eq!(sink.audio, plain_run.audio);
    }
}

// ---- SA-1 ---------------------------------------------------------------

/// An SA-1 cart whose S-CPU spins on `counter_loop` and whose SA-1 runs
/// `sa1_program` from `$8200`, booted through `$2203/$2204/$2200` the way
/// the W17 tests do it.
fn sa1_core(sa1_program: &[u8], configure: impl FnOnce(&mut SnesCore)) -> SnesCore {
    use crate::cpu::CpuBus;
    let mut rom = image(0x23, 0x35, &counter_loop());
    rom[0x200..0x200 + sa1_program.len()].copy_from_slice(sa1_program);
    let mut core = SnesCore::load(&rom).expect("SA-1 cart loads");
    assert!(core.has_sa1());
    configure(&mut core);
    let bus = &mut core.system_mut().bus;
    bus.write(0x00_2203, 0x00);
    bus.write(0x00_2204, 0x82); // CRV: SA-1 reset vector $8200
    bus.write(0x00_2200, 0x00); // clear Reset
    core
}

/// `loop: NOP ; BRA loop` — reads only.
const SA1_NOP_LOOP: [u8; 3] = [0xEA, 0x80, 0xFD];
/// `loop: INC $00 ; BRA loop` — a store every pass.
const SA1_INC_LOOP: [u8; 4] = [0xE6, 0x00, 0x80, 0xFC];

fn sa1_ops(core: &mut SnesCore, frames: u32) -> u64 {
    let mut sink = Sink::default();
    for _ in 0..frames {
        let _ = core.step(Step::Frame, &mut sink);
    }
    core.system()
        .bus
        .sa1
        .as_ref()
        .unwrap()
        .instructions_executed
}

/// 100% (explicit) is the cartridge's own clock, bit for bit.
#[test]
fn sa1_overclock_of_100_is_the_default_machine() {
    let base = run_frames(&mut sa1_core(&SA1_NOP_LOOP, |_| {}), 10);
    let set = run_frames(
        &mut sa1_core(&SA1_NOP_LOOP, |c| {
            c.set_sa1_overclock(100);
        }),
        10,
    );
    assert_eq!(base, set);
}

/// The SA-1 gets about `m` times the instructions per frame. "About": the
/// catch-up loop spends its credit in whole SA-1 instructions per S-CPU
/// step, so the scaling is a little under `m` (measured on this fixture:
/// 1.82x, 2.91x, 3.64x for 2x, 3x, 4x) — the same kind of shortfall the
/// Super FX shows on real carts. Real SA-1 cartridges measure closer: see
/// `rf-harness/tests/snes_overclock_roms.rs`.
#[test]
fn sa1_overclock_scales_sa1_instructions() {
    let base = sa1_ops(&mut sa1_core(&SA1_NOP_LOOP, |_| {}), 10) as f64;
    assert!(base > 10_000.0, "the SA-1 ran: {base}");
    let mut last = 1.0;
    for (percent, m) in [(200, 2.0), (300, 3.0), (400, 4.0)] {
        let got = sa1_ops(
            &mut sa1_core(&SA1_NOP_LOOP, |c| {
                c.set_sa1_overclock(percent);
            }),
            10,
        ) as f64;
        let ratio = got / base;
        assert!(
            (m * 0.80..=m * 1.02).contains(&ratio) && ratio > last,
            "{percent}% ran {ratio:.3}x the SA-1 instructions (want about {m}x)"
        );
        last = ratio;
    }
}

/// The SA-1 overclock does not touch the S-CPU's frames, NMIs or audio.
#[test]
fn sa1_overclock_leaves_frames_and_audio_alone() {
    let base = run_frames(&mut sa1_core(&SA1_NOP_LOOP, |_| {}), 10);
    let fast = run_frames(
        &mut sa1_core(&SA1_NOP_LOOP, |c| {
            c.set_sa1_overclock(400);
        }),
        10,
    );
    assert_eq!(fast.audio_samples, base.audio_samples);
    assert_eq!(fast.nmi_count, base.nmi_count);
    assert_eq!(fast.master_cycles, base.master_cycles);
}

/// The SA-1's own idle observer: a read-only loop idles, a storing loop
/// does not.
#[test]
fn sa1_frame_idle_follows_the_sa1_cpu() {
    let mut idle = sa1_core(&SA1_NOP_LOOP, |_| {});
    let mut busy = sa1_core(&SA1_INC_LOOP, |_| {});
    let mut sink = Sink::default();
    for _ in 0..5 {
        let _ = idle.step(Step::Frame, &mut sink);
        let _ = busy.step(Step::Frame, &mut sink);
    }
    assert!(idle.sa1_frame_idle());
    assert!(!busy.sa1_frame_idle());
    assert!(
        !core_with(&counter_loop(), |_| {}).sa1_frame_idle(),
        "no SA-1, no signal"
    );
}

/// Master cycles the rest of the machine saw pass for each of the first
/// `steps` instructions, beside what each cost at the console's real clock.
fn step_costs(program: &[u8], percent: u16, steps: usize) -> Vec<(u64, u64)> {
    let mut core = core_with(program, |c| {
        c.set_cpu_overclock(percent);
    });
    // `step` re-applies the multiplier from the core's setting; one frame
    // call is not wanted here, so apply it to the system directly.
    core.system_mut().cpu_clock_multiplier = u64::from(percent / 100);
    (0..steps)
        .map(|_| {
            let before = core.system().master_cycles;
            core.system_mut().step().expect("implemented opcode");
            let sys = core.system();
            (sys.master_cycles - before, sys.last_instr_master_cycles)
        })
        .collect()
}

/// Ticket W30-08: an instruction that reads an APU port is charged its full
/// cost at a 300% CPU clock (so the SPC700 handshake keeps its 100% relative
/// timing), while the branch beside it, and a read of a non-APU register,
/// are scaled.
#[test]
fn apu_port_instructions_are_charged_unscaled() {
    // loop: LDA $2140 ; BRA loop
    let port = [0xAD, 0x40, 0x21, 0x80, 0xFB];
    let costs = step_costs(&port, 300, 8);
    for pair in costs.chunks(2) {
        let (lda_scaled, lda_real) = pair[0];
        let (bra_scaled, bra_real) = pair[1];
        assert_eq!(lda_scaled, lda_real, "APU port read must not be scaled");
        assert!(bra_scaled * 3 <= bra_real + 3 && bra_scaled * 3 + 3 >= bra_real);
    }
    // The same shape on a WRAM port ($2180) is scaled like any other.
    let wram = [0xAD, 0x80, 0x21, 0x80, 0xFB];
    let (scaled, real) = step_costs(&wram, 300, 1)[0];
    assert!(scaled * 3 <= real + 3 && scaled * 3 + 3 >= real);
    // At 100% nothing changes.
    let (scaled, real) = step_costs(&port, 100, 1)[0];
    assert_eq!(scaled, real);
}

/// The APU port window is `$2140-$217F` in the system banks only.
#[test]
fn apu_port_window_is_the_mirrored_range_in_system_banks() {
    use crate::cpu::is_apu_port;
    assert!(is_apu_port(0x00_2140));
    assert!(is_apu_port(0x3F_217F));
    assert!(is_apu_port(0x80_2143));
    assert!(is_apu_port(0xBF_2160));
    assert!(!is_apu_port(0x00_213F));
    assert!(!is_apu_port(0x00_2180));
    assert!(!is_apu_port(0x7E_2140), "bank $7E is WRAM");
    assert!(!is_apu_port(0x40_2140));
}

/// A second Reset pulse on `$2200` (CCNT bit 5) reboots the SA-1 at the new
/// reset vector instead of leaving it in its old code (wide-snes Super Mario
/// World re-points the SA-1 this way at boot and hung without it).
#[test]
fn a_second_sa1_reset_pulse_reboots_at_the_new_vector() {
    use crate::cpu::CpuBus;
    // $8200: enable SA-1 I-RAM writes, store $11 to I-RAM $00, idle.
    let mut program = vec![
        0xA9, 0xFF, 0x8D, 0x2A, 0x22, 0xA9, 0x11, 0x85, 0x00, 0x80, 0xFE,
    ];
    program.resize(0x10, 0xEA);
    // $8210: store $22 to I-RAM $01, idle.
    program.extend_from_slice(&[0xA9, 0x22, 0x85, 0x01, 0x80, 0xFE]);
    let mut core = sa1_core(&program, |_| {});
    let iram = |core: &SnesCore, i: usize| core.system().bus.sa1.as_ref().unwrap().iram[i];
    sa1_ops(&mut core, 2);
    assert_eq!(iram(&core, 0), 0x11, "first boot ran");
    assert_eq!(iram(&core, 1), 0x00);
    let bus = &mut core.system_mut().bus;
    bus.write(0x00_2200, 0x20); // hold Reset
    bus.write(0x00_2203, 0x10);
    bus.write(0x00_2204, 0x82); // new vector $8210
    sa1_ops(&mut core, 1);
    core.system_mut().bus.write(0x00_2200, 0x00); // release
    sa1_ops(&mut core, 2);
    assert_eq!(iram(&core, 1), 0x22, "rebooted at the new vector");
}
