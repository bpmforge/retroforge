//! A per-title diagnostic probe for the boot census's buckets (ticket
//! W14-11). Local only: it takes ROM paths from the environment and is
//! `#[ignore]`d, so no gate and no plain `cargo test` can start it.
//!
//! This is the tool that found W14-09 and all four defects of W14-10. It
//! instruction-steps a title, then samples where the 65816 and the SPC700
//! spend their time and prints the loops each is stuck in, the PPU/APU
//! register state, and — on request — disassembly, ARAM dumps, ROM byte
//! searches, a ring of the last port changes and a ring of the last PCs.
//!
//! ```text
//! PROBE_ROMS=a.zip:b.zip            colon-separated archives or bare ROMs (required)
//! PROBE_INSTR=6000000               CPU instructions to run first (default 30M)
//! PROBE_SAMPLE=20000                instructions to sample after that
//! PROBE_MODE=frames PROBE_FRAMES=N  instead: step N frames, report the first varied frame
//!                                    (and, since W14-39's follow-up, the cumulative CPU
//!                                    instruction count at that frame — `StepResult::cycles`
//!                                    is this core's instruction count, not master cycles,
//!                                    per `SnesCore::step`'s own doc — so an A/B run against
//!                                    another tree's `title_probe` answers "did the SAME
//!                                    frame-visible event take about the same number of
//!                                    CPU instructions" (a per-instruction cycle-cost bug,
//!                                    if not) or "did it take many more instructions" (a
//!                                    poll loop whose exit condition the corrected pacing
//!                                    changed, not a cycle-costing bug))
//! PROBE_M7=1                        with frames mode: print Mode 7 state and palette diversity
//! PROBE_DIS=bb:start:end[,...]      65816 disassembly ranges (hex, end exclusive)
//! PROBE_ARAM=start:end[,...]        ARAM hex dumps
//! PROBE_FIND=hex[,hex]              search ARAM for byte patterns
//! PROBE_FINDROM=hex[,hex]           search the ROM file (LoROM address shown)
//! PROBE_PORTS=1                     print the last 40 APU port changes with both PCs
//! PROBE_DSP=1                      print the S-DSP register file and per-voice envelope state (W7-08)
//! PROBE_RING=1 / PROBE_SPCRING=1    print the last distinct CPU / SPC PCs
//! PROBE_ALLPC=1                     print every sampled CPU PC, sorted
//! PROBE_STOP_ON_SPC_STOP=1          stop early when the SPC700 halts under a running program
//! PROBE_TIMERLOG=1                  print every SPC timer 0-2 enable/target transition
//! PROBE_PACKETLOG=1                 print every SPC X-register (command index) and ARAM
//!                                    dp$01 change, plus totals of X-register vs. $2140
//!                                    (port 0) changes over the whole run
//! PROBE_MATHPC=hex[,hex]            print the $4204-$4217 hardware multiply/divide
//!                                    unit's state whenever the CPU is about to execute
//!                                    an instruction at one of these 24-bit PCs (W14-24)
//! PROBE_ACCESSWIN=start:end         sum bus accesses and master cycles charged over
//!                                    every instruction from PC start (inclusive) to
//!                                    end (exclusive), 24-bit hex (W14-24)
//! PROBE_SPCREGPC=hex[,hex]          print the SPC700's A/X/Y/SP and the four bytes
//!                                    above SP (what a RET would pop) whenever the
//!                                    SPC700 is about to execute an instruction at
//!                                    one of these 16-bit ARAM PCs (W14-27)
//! PROBE_SPCPCCOUNT=hex[,hex]        count genuine transitions into each of these
//!                                    16-bit ARAM PCs over the *entire* run (unlike
//!                                    PROBE_SPCRING's ring, this is never evicted —
//!                                    use it to ask "how many times total", not just
//!                                    "what does recent history look like") (W14-27)
//! PROBE_SDUMP=hex[,hex]             print e/p/sp/d/a/x/y, the 12 bytes above
//!                                    the stack pointer, and the 6 bytes at
//!                                    bank-$00 $0000-$0005 whenever the CPU is
//!                                    about to execute an instruction at one of
//!                                    these 24-bit PCs — checks whether an
//!                                    about-to-run RTI/RTS/RTL is about to pop a
//!                                    real return address, or static ROM/open-bus
//!                                    content because SP has wandered into
//!                                    non-WRAM space (W14-26); `d` (direct page)
//!                                    and the $0000-$0005 dump added in W14-28 to
//!                                    catch a `JML [$0000]`/`JML [$0003]` vector
//!                                    trampoline (bank-$00-fixed per the 65816
//!                                    spec, independent of D/DBR/PBR) pointing
//!                                    somewhere other than what the game intended
//! PROBE_SDUMP_TABLE=1               with PROBE_SDUMP, additionally decode the
//!                                    matched PC as a `JSR`/`JMP ($nnnn,X)` table
//!                                    dispatch and print the table address/entry —
//!                                    only meaningful when the matched opcode
//!                                    really is one of those two (W14-26)
//! PROBE_SPWIN=start:end             log every instruction's opcode byte and the
//!                                    stack pointer AFTER it runs, across this
//!                                    decimal instruction-count window (`n`, end
//!                                    exclusive) — attributes an SP drift to the
//!                                    exact opcode that moved it (W14-26)
//! PROBE_NMIW=1                      print `n` and the PC after every instruction that changes
//!                                    $4200 NMITIMEN (W14-60): names the writer of an NMI-disable
//!                                    that a later WAI never wakes from
//! PROBE_IRQLOG=N                    log the first N H/V-IRQ events (edge-
//!                                    detected post-instruction, not a new
//!                                    core field — see below), then totals
//!                                    only: an ARMLOG line whenever $4200
//!                                    (NMITIMEN) or $4207-$420A (HTIME/
//!                                    VTIME) changes, with the old/new value
//!                                    and the raster (line, dot) it changed
//!                                    at; an IRQLOG line on every rising
//!                                    edge of the IRQ-pending flag (an
//!                                    assertion) with the raster and the
//!                                    CPU PC/P at that instant; an ACKLOG
//!                                    line on every falling edge that is
//!                                    NOT explained by an ARMLOG disabling
//!                                    IRQs the same instant (i.e. a $4211
//!                                    read, the only other way the flag
//!                                    clears) with the raster; and a
//!                                    TRAMPLOG line whenever the bytes at
//!                                    the native IRQ vector's indirect-jump
//!                                    target change (the "vector
//!                                    trampoline" a raster chain rewrites
//!                                    to redirect the next IRQ without
//!                                    touching $FFEE itself), decoded once
//!                                    at start from whatever opcode sits at
//!                                    $00:[$FFEE] (`$6C`/`$7C` JMP (abs[,X])
//!                                    or `$DC` JML [abs] follow the pointer
//!                                    they encode; anything else — a
//!                                    vector pointing straight into WRAM
//!                                    dispatch code being the common case —
//!                                    watches the vector's own target
//!                                    address instead); also an
//!                                    INIDISPLOG line on every `$2100`
//!                                    forced_blank/brightness edge, since
//!                                    that register is what the trace is
//!                                    ultimately trying to explain (W14-29);
//!                                    a RDNMILOG line on every edge of
//!                                    `Timing::nmi_flag` — SET at a vblank
//!                                    edge, CLEARED (with the CPU PC) when
//!                                    a `$4210` read consumes a pending
//!                                    bit7=1 — which is the complete RDNMI
//!                                    read/return story with no new
//!                                    `rf-snes` field, since that flag only
//!                                    ever changes for those two reasons;
//!                                    an HVBJOYLOG line on every ENTER/EXIT
//!                                    edge of `Timing::in_vblank()`, the
//!                                    level `$4212` bit 7 reports; and an
//!                                    NMILOG line whenever the CPU PC lands
//!                                    on the NMI vector (native `$FFEA` or
//!                                    emulation `$FFFA`), the same
//!                                    dispatch detection the post-run
//!                                    `nmi_entries` sample already uses,
//!                                    but live across the whole
//!                                    `PROBE_IRQLOG` window instead of only
//!                                    the last 20000 instructions (W14-35)
//! PROBE_SPCMEMWATCH=hex[,hex]       print the SPC700 PC and old/new byte value
//!                                    whenever one of these absolute 16-bit ARAM
//!                                    addresses changes value — used to find who
//!                                    wrote a suspect direct-page cell (e.g. a
//!                                    track-pointer low/high byte pair) rather
//!                                    than only observing its final value. CAVEAT:
//!                                    it (like PROBE_SPCREGPC) samples once per
//!                                    65816 instruction, AFTER `core.step` — the
//!                                    printed `spcpc` is wherever the SPC700 has
//!                                    reached by then, not necessarily the PC of
//!                                    the instruction that produced the change,
//!                                    since several SPC700 instructions can run
//!                                    inside one 65816 step (W14-30). It also
//!                                    reads raw ARAM bytes, so it is blind to
//!                                    $00F4-$00F7 (the port registers) — those
//!                                    are backed by `ports_in`/`ports_out`, not
//!                                    the `aram` array; use PROBE_APUPORTLOG.
//! PROBE_APUPORTLOG=1                print n, SPC pc, and all four
//!                                    ports_in/ports_out bytes whenever either
//!                                    ports_in[0] (CPU-sent index) or
//!                                    ports_in[1] (CPU-sent data) changes — the
//!                                    accepted-transfer trace for an APU upload
//!                                    protocol built on $F4 (index)/$F5 (data)
//!                                    (W14-30)
//! PROBE_WATCH=hex[,hex]              print `n`, the PC that just ran, and
//!                                    old/new bytes whenever one of these 24-bit
//!                                    addresses' value changes from one
//!                                    instruction boundary to the next — finds
//!                                    the exact writer of an unexpected memory
//!                                    change (e.g. a corrupted vector-table
//!                                    pointer byte) without knowing its PC in
//!                                    advance (W14-28). CAVEAT (W14-38): this
//!                                    reads via `SnesBus::peek`, which for a
//!                                    write-only PPU register (`$2100`-
//!                                    `$213F`, e.g. `$212C` TM) falls through
//!                                    to `open_bus` (`bus.rs`'s
//!                                    `read_register_pure` returns `None` for
//!                                    those offsets) — so watching one of
//!                                    those addresses tracks whatever value
//!                                    last crossed the bus for ANY reason,
//!                                    not that register's actual latched
//!                                    content. Never watch a write-only PPU
//!                                    register this way; use PROBE_OAM or add
//!                                    a write-side probe instead.
//! PROBE_OAM=1                        decode all 128 OAM entries the way
//!                                    `obj::decode_sprite` does and print the
//!                                    ones whose Y span overlaps the visible
//!                                    0..224 lines, plus (for the first 20)
//!                                    the top-left texel's composed colour
//!                                    index via `bg::fetch_pixel` — answers
//!                                    "are there genuinely on-screen sprites
//!                                    with non-transparent tile data" without
//!                                    trusting `oam_nonzero`/`cgram_nonzero`
//!                                    byte counts, which say nothing about
//!                                    position or transparency (W14-38)
//! PROBE_RENDERNOW=1                  after the instruction-step sample,
//!                                    call `Ppu::render_scanline` for lines
//!                                    0..224 (the same call PROBE_M7's
//!                                    FRAMES-mode `distinct_indices_now`
//!                                    uses) and print how many distinct
//!                                    palette indices the compositor
//!                                    actually produces right now —
//!                                    answers "does the renderer draw more
//!                                    than a flat frame" at an arbitrary
//!                                    PROBE_INSTR sample point, not only at
//!                                    a FRAMES-mode frame boundary; pair
//!                                    with PROBE_OAM to ask whether
//!                                    on-screen non-transparent OAM/CGRAM
//!                                    content the decode finds is actually
//!                                    reaching the composited picture
//!                                    (W14-42)
//! PROBE_MODE=ppuwrites                a register-write watch for the
//!                                    composition-relevant PPU registers
//!                                    ($2100 INIDISP, $212C TM, $212D TS,
//!                                    $2130/$2131 CGWSEL/CGADSUB, and the
//!                                    $2123-$212B window enable/position/
//!                                    logic block) that `PROBE_WATCH`
//!                                    cannot see, since those addresses
//!                                    are write-only and `SnesBus::peek`
//!                                    falls through to open bus for all of
//!                                    them (see `PROBE_WATCH`'s own doc).
//!                                    Compares the PPU's own DECODED
//!                                    fields across instruction boundaries
//!                                    instead of raw bus bytes. Fast-
//!                                    forwards `PROBE_FRAMES - 1` frames
//!                                    with `Step::Frame` (cheap — no
//!                                    per-instruction cost paid for the
//!                                    frames that are not of interest),
//!                                    then switches to `Step::Instruction`
//!                                    for the final frame plus up to
//!                                    `PROBE_INSTR` more instructions
//!                                    (default 200_000) and prints the PC
//!                                    every time one of those fields
//!                                    changes (W14-51).
//! ```
//!
//! Example (the W14-10 trace): `PROBE_INSTR=3000000 PROBE_PORTS=1
//! PROBE_ROMS="$HOME/Games/Roms/snes/Wild Guns (USA).zip" cargo test
//! --release -p rf-harness --test title_probe -- --ignored --nocapture`.
//!
//! Nothing it prints is a verdict; it is where to look next.
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, PpuPixel, Step};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

#[derive(Default)]
struct Sink {
    lines: u64,
    varied: bool,
    first: Option<u8>,
    // W14-29: independent of `first`/`varied` above (which compare against
    // the very first pixel ever seen, across the whole run) — this is
    // reset every frame by the FRAMES-mode loop and answers "did THIS
    // frame's own rendered picture, as `emit_frame` reconstructed it
    // (mid-frame register replay included), contain more than one
    // palette index" — used to tell "the game is genuinely still drawing
    // a flat colour" apart from "varied output exists but never differs
    // from pixel zero specifically".
    frame_indices: std::collections::HashSet<u8>,
}
impl CoreSink for Sink {
    fn video_scanline(&mut self, _y: u16, pixels: &[PpuPixel]) {
        self.lines += 1;
        for p in pixels {
            match self.first {
                None => self.first = Some(p.palette_index),
                Some(f) if f != p.palette_index => self.varied = true,
                _ => {}
            }
            self.frame_indices.insert(p.palette_index);
        }
    }
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

fn rom_bytes(path: &Path) -> Option<Vec<u8>> {
    let raw = std::fs::read(path).ok()?;
    if !raw.starts_with(b"PK\x03\x04") {
        return Some(raw);
    }
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(raw)).ok()?;
    for index in 0..archive.len() {
        let Ok(mut file) = archive.by_index(index) else {
            break;
        };
        if !file.is_file() {
            continue;
        }
        let mut buf = Vec::new();
        if (&mut file).take(64 << 20).read_to_end(&mut buf).is_err() {
            continue;
        }
        if rf_cart::Cartridge::load(&buf).is_ok() {
            return Some(buf);
        }
    }
    None
}

#[test]
#[ignore = "local diagnostic: set PROBE_ROMS to real ROM paths"]
fn probe() {
    let frames: usize = std::env::var("PROBE_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);
    for path in std::env::var("PROBE_ROMS").unwrap().split(':') {
        let bytes = rom_bytes(Path::new(path)).unwrap();
        let mut core = rf_snes::core::SnesCore::load(&bytes).unwrap();
        let mut sink = Sink::default();
        if std::env::var("PROBE_MODE").as_deref() == Ok("ppuwrites") {
            // W14-51: `PROBE_WATCH` is blind on `$2100`-`$213F` (write-only
            // PPU registers fall through `SnesBus::peek` to open bus, per
            // that probe's own doc) — so a register-write watch on the
            // composition-relevant PPU registers ($2100 INIDISP, $212C TM,
            // $212D TS, $2130/$2131 CGWSEL/CGADSUB, $2123-$212B the window
            // enable/position/logic block) has to compare the PPU's own
            // DECODED state across instruction boundaries instead of raw
            // bus bytes. Fast-forwards `PROBE_FRAMES - 1` frames with
            // `Step::Frame` (cheap), then switches to `Step::Instruction`
            // for the final frame (plus PROBE_INSTR extra instructions,
            // default 200_000) and prints the PC every time one of those
            // fields changes value.
            let target_frames = frames.saturating_sub(1);
            for _ in 0..target_frames {
                core.step(Step::Frame, &mut sink);
            }
            let cap: u64 = std::env::var("PROBE_INSTR")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(200_000);
            #[derive(Clone, PartialEq, Debug)]
            struct PpuWriteSnap {
                forced_blank: bool,
                brightness: u8,
                tm: [bool; 4],
                obj: bool,
                ts: u8,
                clip: u8,
                prevent: u8,
                enable: u8,
                win_enable: [(bool, bool); 6],
                main_mask: u8,
                sub_mask: u8,
                w1_left: u8,
                w1_right: u8,
                w2_left: u8,
                w2_right: u8,
            }
            let snap = |c: &rf_snes::core::SnesCore| {
                let p = &c.system().bus.ppu;
                PpuWriteSnap {
                    forced_blank: p.forced_blank,
                    brightness: p.brightness,
                    tm: [
                        p.bgs[0].enabled,
                        p.bgs[1].enabled,
                        p.bgs[2].enabled,
                        p.bgs[3].enabled,
                    ],
                    obj: p.obj_enabled,
                    ts: p.ts,
                    clip: p.color_math.clip_mode,
                    prevent: p.color_math.prevent_mode,
                    enable: p.color_math.enable,
                    win_enable: p.windows.enable,
                    main_mask: p.windows.main_mask,
                    sub_mask: p.windows.sub_mask,
                    w1_left: p.windows.w1_left,
                    w1_right: p.windows.w1_right,
                    w2_left: p.windows.w2_left,
                    w2_right: p.windows.w2_right,
                }
            };
            let mut prev = snap(&core);
            // W14-51: alongside the PPU write watch, also track the APU
            // port pair ($2140-$2143) — answers "is this stretch
            // audio-gated" (ongoing CPU<->SPC handshake traffic) or
            // purely a CPU-side WRAM countdown (ports never move) without
            // needing a second full run.
            let mut apu_prev = core.system().bus.apu.ports_in;
            let mut n: u64 = 0;
            let mut changes: u64 = 0;
            let mut apu_changes: u64 = 0;
            while n < cap {
                core.step(Step::Instruction, &mut sink);
                n += 1;
                let cur = snap(&core);
                if cur != prev {
                    let pc = core.system().cpu.pc24();
                    println!("    PPUWRITE n={n} pc={pc:06X} {prev:?} -> {cur:?}");
                    changes += 1;
                    prev = cur;
                }
                let apu_cur = core.system().bus.apu.ports_in;
                if apu_cur != apu_prev {
                    apu_changes += 1;
                    apu_prev = apu_cur;
                }
            }
            println!(
                "PPUWRITES done frame={} changes={changes} apu_port_changes={apu_changes} n={n} {}",
                core.system().bus.timing.frame,
                Path::new(path).file_name().unwrap().to_string_lossy()
            );
            continue;
        }
        // Ticket W18-04 follow-up (coordinator-directed): a GSU PC
        // histogram over a real, frame-driven run — `Step::Instruction`
        // in a loop so we sample the GSU's PC/last-opcode/counters after
        // EVERY 65816 instruction (the same granularity `SnesSystem::step`
        // interleaves the GSU on), not just at frame boundaries, so a
        // tight GSU-side poll loop shows up as one or two dominant PCs
        // rather than being averaged away.
        if std::env::var("PROBE_MODE").as_deref() == Ok("gsuhist") {
            let sample: u64 = std::env::var("PROBE_GSUHIST_INSTR")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(3_000_000);
            let mut hist: HashMap<(u8, u16, u8), u64> = HashMap::new();
            let regword = |sys: &rf_snes::system::SnesSystem, off: u16| -> u16 {
                let gsu = sys.bus.gsu.as_ref().unwrap();
                let lo = gsu.regs.peek(0x3000 + off).unwrap_or(0);
                let hi = gsu.regs.peek(0x3001 + off).unwrap_or(0);
                u16::from_le_bytes([lo, hi])
            };
            for i in 0..sample {
                core.step(Step::Instruction, &mut sink);
                let Some(gsu) = core.system().bus.gsu.as_ref() else {
                    break;
                };
                *hist
                    .entry((gsu.regs.pbr(), gsu.regs.r15(), gsu.regs.last_opcode))
                    .or_default() += 1;
                if std::env::var("PROBE_GSUHIST_REGS").is_ok() && i % 300_000 == 0 {
                    let sys = core.system();
                    println!(
                        "    GSUREGWATCH i={i} R1={:04X} R2={:04X} R3={:04X} R4={:04X} \
                         R5={:04X} R6={:04X} R12={:04X} R13={:04X}",
                        regword(sys, 2),
                        regword(sys, 4),
                        regword(sys, 6),
                        regword(sys, 8),
                        regword(sys, 10),
                        regword(sys, 12),
                        regword(sys, 24),
                        regword(sys, 26),
                    );
                }
            }
            println!(
                "=== GSUHIST {}",
                Path::new(path).file_name().unwrap().to_string_lossy()
            );
            print_gsu_reg_report(&core);
            let mut top: Vec<_> = hist.into_iter().collect();
            top.sort_by(|a, b| b.1.cmp(&a.1));
            println!("    distinct (PBR:R15,opcode) samples: {}", top.len());
            for ((pbr, r15, op), n) in top.into_iter().take(20) {
                println!("    GSUHIST {pbr:02X}:{r15:04X} op={op:02X} samples={n}");
            }
            continue;
        }
        if std::env::var("PROBE_MODE").as_deref() == Ok("frames") {
            let mut first_varied: Option<usize> = None;
            let mut first_varied_instr: Option<u64> = None;
            let mut total_instr: u64 = 0;
            let frame_indices_log = std::env::var("PROBE_FRAME_INDICES").is_ok();
            for f in 0..frames {
                sink.frame_indices.clear();
                let step_result = core.step(Step::Frame, &mut sink);
                total_instr += step_result.cycles;
                if frame_indices_log {
                    let sys = core.system();
                    println!(
                        "      FRAMEIDX f={f} distinct_this_frame={} sample={:?} \
                         forced_blank={} bg_mode={} line_writes[216]={:?} \
                         line_writes[7]={:?} line_writes[100]={:?}",
                        sink.frame_indices.len(),
                        sink.frame_indices.iter().take(6).collect::<Vec<_>>(),
                        sys.bus.ppu.forced_blank,
                        sys.bus.ppu.bg_mode,
                        sys.bus.ppu.line_writes_for_test(216),
                        sys.bus.ppu.line_writes_for_test(7),
                        sys.bus.ppu.line_writes_for_test(100),
                    );
                }
                if sink.varied && first_varied.is_none() {
                    first_varied = Some(f);
                    first_varied_instr = Some(total_instr);
                    break;
                }
            }
            println!(
                "FRAMES varied_at={:?} total_instr_at_varied={:?} {}",
                first_varied,
                first_varied_instr,
                Path::new(path).file_name().unwrap().to_string_lossy()
            );
            if std::env::var("PROBE_M7").is_ok() {
                {
                    let sys = core.system();
                    let ppu = &sys.bus.ppu;
                    println!(
                        "    frame={} mode={} forced_blank={} bright={} tm=[{}{}] m7={:?} bg1 hofs={} vofs={}",
                        sys.bus.timing.frame,
                        ppu.bg_mode,
                        ppu.forced_blank,
                        ppu.brightness,
                        (0..4)
                            .map(|i| if ppu.bgs[i].enabled { '1' } else { '0' })
                            .collect::<String>(),
                        if ppu.obj_enabled { "+obj" } else { "" },
                        ppu.mode7,
                        ppu.bgs[0].hofs,
                        ppu.bgs[0].vofs
                    );
                    // W14-42: is the flat composited frame explained by
                    // colour math/window forcing every pixel to the same
                    // fixed colour (a real hardware effect), or does the
                    // compositor drop real BG/OBJ content for no register
                    // reason? Cheap to rule the first out directly.
                    println!(
                        "    color_math={:?} windows={:?} bg1={:?}",
                        ppu.color_math, ppu.windows, ppu.bgs[0]
                    );
                }
                let sys2 = core.system_mut();
                let mut idx = std::collections::HashSet::new();
                for y in 0..224u16 {
                    for px in sys2.bus.ppu.render_scanline(y).pixels {
                        idx.insert(px.palette_index);
                    }
                }
                println!(
                    "    distinct_indices_now={} sample={:?}",
                    idx.len(),
                    idx.iter().take(8).collect::<Vec<_>>()
                );
            }
            print_sa1_reg_report(&core);
            // Ticket W18-05: `frames` mode never called this (only the
            // default, PROBE_INSTR-budget mode and `gsuhist` did), so a
            // GSU title's stuck-cause trace had to fall back to `gsuhist`
            // or the slow default mode even though `frames` is the mode
            // that actually answers "does it ever render" — added here so
            // `PROBE_GSUREGS=1 PROBE_MODE=frames` reports the GSU core
            // state (PC, GO, IRQ, cache/stall counters, plot/rpix calls)
            // at the exact frame count `boot_census` itself uses.
            print_gsu_reg_report(&core);
            continue;
        }
        // instruction-step the whole run, logging port/timer changes in a ring buffer
        let mut ring: std::collections::VecDeque<String> = std::collections::VecDeque::new();
        let mut last = ([0u8; 4], [0u8; 4], [false; 3]);
        let mut n: u64 = 0;
        let mut pcring: std::collections::VecDeque<u32> = std::collections::VecDeque::new();
        let mut ring_last = u32::MAX;
        let mut spcring: std::collections::VecDeque<u16> = std::collections::VecDeque::new();
        let mut spc_last = u16::MAX;
        // W14-27: spcring is capped at 3000 entries and evicts the oldest
        // on overflow, so its printed contents alone cannot answer "how
        // many times total" for a run with more than 3000 distinct SPC
        // PC transitions. This counter is never evicted.
        let mut spc_pc_transitions: u64 = 0;
        let mut spcpc_counts: std::collections::HashMap<u16, u64> =
            std::collections::HashMap::new();
        let timerlog = std::env::var("PROBE_TIMERLOG").is_ok();
        let mut timer_last = [false, false, false];
        let packetlog = std::env::var("PROBE_PACKETLOG").is_ok();
        // W14-30: watch a set of absolute 16-bit ARAM addresses and print
        // whenever one of them changes value, with the SPC PC that ran the
        // instruction which produced the change. Built to find who writes
        // a suspect direct-page cell (a track-pointer low/high byte pair)
        // rather than only ever seeing its value after the fact via
        // PROBE_ARAM/PROBE_SPCREGPC.
        let spcmemwatch: Vec<u16> = std::env::var("PROBE_SPCMEMWATCH")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| u16::from_str_radix(s, 16).unwrap())
            .collect();
        let mut spcmemwatch_last: HashMap<u16, u8> = HashMap::new();
        // W14-30: trace the accepted-transfer sequence of an APU upload
        // protocol built on $F4 (CPU-sent index)/$F5 (CPU-sent data) —
        // ports_in/ports_out are separate arrays from `aram`, so
        // PROBE_SPCMEMWATCH cannot see them. Prints on every change to
        // either ports_in[0] or ports_in[1] (whichever moved), plus the
        // other three port bytes, so the (index, data) pairs the CPU
        // wrote can be diffed against the ROM's own byte-run.
        let apuportlog = std::env::var("PROBE_APUPORTLOG").is_ok();
        let mut apuportlog_last = [u8::MAX; 4];
        let mut x_last = u8::MAX;
        let mut port0_last = u8::MAX;
        let mut dp1_last = u8::MAX;
        let mut x_changes: u64 = 0;
        let mut port0_changes: u64 = 0;
        // W14-24: log the $4204-$4217 hardware divider's state whenever the
        // CPU is about to execute an instruction at one of these 24-bit
        // PCs (comma-separated hex, e.g. "c404fd,c40505") — used to check
        // whether a game's read of $4214/$4215 lands while the divider is
        // still stepping (`busy()==true`), which would hand it a partial
        // shift-register value instead of the finished quotient.
        let mathpcs: Vec<u32> = std::env::var("PROBE_MATHPC")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| u32::from_str_radix(s, 16).unwrap())
            .collect();
        // W14-26: dump the stack pointer and the bytes above it whenever
        // the CPU is about to execute an instruction at one of these
        // 24-bit PCs (comma-separated hex) — used to check whether an
        // `RTI`/`RTS`/`RTL` about to run is about to pop a return address
        // that actually points back into the interrupted code, or into
        // garbage (e.g. WRAM the game never wrote a return address into).
        let sdumps: Vec<u32> = std::env::var("PROBE_SDUMP")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| u32::from_str_radix(s, 16).unwrap())
            .collect();
        // W14-26: log every instruction's opcode byte and the stack
        // pointer's value AFTER it runs, across the instruction-count
        // window `PROBE_SPWIN=start:end` (decimal `n`, end exclusive) —
        // used to attribute a stack-pointer drift to the exact opcode
        // that moved it, rather than a coarser per-PC sample.
        let spwin: Option<(u64, u64)> = std::env::var("PROBE_SPWIN").ok().map(|s| {
            let (a, b) = s.split_once(':').unwrap();
            (a.parse().unwrap(), b.parse().unwrap())
        });
        // W14-28: watch a handful of 24-bit addresses for any change in
        // value between one instruction boundary and the next, printing
        // the PC that just ran and the old/new bytes — finds the writer
        // of an unexpected memory change (a corrupted vector-table byte,
        // here) without having to guess its PC first.
        let watch: Vec<u32> = std::env::var("PROBE_WATCH")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| u32::from_str_radix(s, 16).unwrap())
            .collect();
        let mut watch_prev: HashMap<u32, u8> = HashMap::new();
        // W14-24 (coordinator review): measure, rather than assert, what
        // `SnesSystem::step`'s `spent` actually contains between two
        // 24-bit PCs — "PROBE_ACCESSWIN=start:end". Accumulates every
        // instruction's `last_instr_accesses`/`last_instr_master_cycles`
        // from the point the CPU is about to execute `start` up to (not
        // including) the point it is about to execute `end`, and prints
        // the totals plus `spent/6` when `end` is reached. Answers
        // exactly the question `MathUnit::tick`'s doc makes a claim
        // about: how many bus accesses and master cycles actually occur
        // in the window a game spaces its own divide-latency wait with,
        // and whether that master-cycle figure has any internal-cycle
        // content in it at all (it does not — see the doc).
        let accesswin: Option<(u32, u32)> = std::env::var("PROBE_ACCESSWIN").ok().map(|s| {
            let (a, b) = s.split_once(':').unwrap();
            (
                u32::from_str_radix(a, 16).unwrap(),
                u32::from_str_radix(b, 16).unwrap(),
            )
        });
        let mut accesswin_active = false;
        let mut accesswin_accesses: u64 = 0;
        let mut accesswin_master_cycles: u64 = 0;
        // W14-29: IRQ event trace for a per-scanline raster chain that
        // rewrites its own vector trampoline. Everything here is
        // reconstructed post-instruction from existing `SnesSystem` state
        // (edge detection) — no new field is added to `rf-snes` for a
        // diagnostic (that would drag save-state/determinism review in for
        // nothing).
        let irqlog_max: u64 = std::env::var("PROBE_IRQLOG")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let mut irqlog_printed: u64 = 0;
        let (mut arm_events, mut assert_events, mut ack_events, mut tramp_events) =
            (0u64, 0u64, 0u64, 0u64);
        let mut nmitimen_last: u8 = core.system().bus.nmitimen.0;
        let mut htime_last: u16 = core.system().bus.irq.htime;
        let mut vtime_last: u16 = core.system().bus.irq.vtime;
        let mut irq_fired_last: bool = core.system().bus.irq.fired;
        // Ad hoc, folded into PROBE_IRQLOG rather than a new env var: the
        // whole point of this ticket is explaining why the screen stays
        // blank, so a `$2100` INIDISP (forced_blank/brightness) edge log
        // is exactly as relevant as the IRQ trace itself.
        let mut inidisp_last: (bool, u8) = {
            let p = &core.system().bus.ppu;
            (p.forced_blank, p.brightness)
        };
        let mut inidisp_events: u64 = 0;
        // Find the trampoline bytes to watch. `$00:[$FFEE]` is the native
        // IRQ vector; if the opcode sitting there is a JMP (abs)/(abs,X)
        // or JML [abs], the actual jump target is read through the
        // pointer that instruction encodes, and THAT address (not the
        // fixed vector target) is what a raster chain rewrites without
        // ever touching $FFEE. If the vector instead points straight into
        // WRAM (as here — Mystic Quest's `$FFEE` targets $000117, a low-
        // WRAM address, not ROM), the vector's own target IS the
        // trampoline: the game writes its dispatch code there directly
        // and can rewrite it in place. Either way we end up watching a
        // fixed address for byte changes; which address depends on what
        // decodes at the vector at watch-setup time. Uninitialised WRAM
        // reads as `$00` (`BRK`) before the game's own boot code has
        // written real bytes there, which is not one of the three
        // opcodes above and is handled the same as "no indirection": we
        // fall back to watching the vector target itself.
        let tramp_watch: Option<(u32, u8)> = if irqlog_max > 0 {
            let sys0 = core.system();
            let irq_vec = u32::from(rf_snes::cpu::CpuBus::peek(&sys0.bus, 0xFFEE))
                | (u32::from(rf_snes::cpu::CpuBus::peek(&sys0.bus, 0xFFEF)) << 8);
            let op = rf_snes::cpu::CpuBus::peek(&sys0.bus, irq_vec);
            let (watch_addr, width) = match op {
                0x6C | 0x7C | 0xDC => {
                    let lo = rf_snes::cpu::CpuBus::peek(&sys0.bus, irq_vec + 1);
                    let hi = rf_snes::cpu::CpuBus::peek(&sys0.bus, irq_vec + 2);
                    let ptr = u32::from(lo) | (u32::from(hi) << 8);
                    (ptr, if op == 0xDC { 3 } else { 2 })
                }
                _ => (irq_vec, 8),
            };
            println!(
                "      TRAMPLOG watching: irq_vec={irq_vec:06X} op_at_vec={op:02X} watch={watch_addr:06X}..+{width}"
            );
            Some((watch_addr, width))
        } else {
            None
        };
        let mut tramp_last: Vec<u8> = tramp_watch
            .map(|(ptr, width)| {
                (0..width)
                    .map(|i| rf_snes::cpu::CpuBus::peek(&core.system().bus, ptr + u32::from(i)))
                    .collect()
            })
            .unwrap_or_default();
        // W14-35: RDNMI ($4210 bit7)/HVBJOY ($4212 bit7) and NMI-dispatch
        // tracing for the raster/IRQ family (25 titles polling one of
        // these two registers with NMI entered 0-2 times). Both register
        // bits are edge-equivalent to existing public state, so — same
        // "no new core field for a diagnostic" discipline as the rest of
        // PROBE_IRQLOG — no `rf-snes` field was added:
        // `Timing::nmi_flag` changes for exactly two reasons (set true at
        // `vblank_start` in `Timing::advance`, cleared by
        // `Timing::read_rdnmi`), so watching it post-instruction reports
        // every RDNMI set/clear with no separate read hook; HVBJOY bit 7
        // is `Timing::in_vblank()`, a pure level with no read side effect
        // at all, so its transitions are the whole story. NMI dispatch
        // itself is detected the same way the PROBE_SAMPLE phase already
        // detects it below (PC landing on the CPU's own NMI vector) but
        // continuously across the whole PROBE_IRQLOG window instead of
        // only the post-run 20000-instruction sample.
        let mut nmiflag_last: bool = core.system().bus.timing.nmi_flag;
        let mut hvbjoy_last: bool = core.system().bus.timing.in_vblank();
        let nmi_vec_native: u32 = {
            let b = &core.system().bus;
            u32::from(rf_snes::cpu::CpuBus::peek(b, 0xFFEA))
                | (u32::from(rf_snes::cpu::CpuBus::peek(b, 0xFFEB)) << 8)
        };
        let nmi_vec_emu: u32 = {
            let b = &core.system().bus;
            u32::from(rf_snes::cpu::CpuBus::peek(b, 0xFFFA))
                | (u32::from(rf_snes::cpu::CpuBus::peek(b, 0xFFFB)) << 8)
        };
        let (mut rdnmi_set_events, mut rdnmi_clear_events) = (0u64, 0u64);
        let (mut hvbjoy_enter_events, mut hvbjoy_exit_events) = (0u64, 0u64);
        let mut nmi_dispatch_events: u64 = 0;
        let mut prev_pc: u32 = 0;
        let cap: u64 = std::env::var("PROBE_INSTR")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30_000_000);
        while n < cap {
            core.step(Step::Instruction, &mut sink);
            n += 1;
            let sys = core.system();
            let apu = &sys.bus.apu;
            for &addr in &spcmemwatch {
                let cur = apu.aram[addr as usize];
                match spcmemwatch_last.get(&addr) {
                    Some(&prev) if prev != cur => {
                        println!(
                            "      SPCMEMWATCH n={n} addr={addr:04X} spcpc={:04X} {prev:02X}->{cur:02X}",
                            apu.cpu.pc
                        );
                        spcmemwatch_last.insert(addr, cur);
                    }
                    Some(_) => {}
                    None => {
                        spcmemwatch_last.insert(addr, cur);
                    }
                }
            }
            if apuportlog
                && (apu.ports_in[0] != apuportlog_last[0] || apu.ports_in[1] != apuportlog_last[1])
            {
                println!(
                    "      APUPORTLOG n={n} spcpc={:04X} ports_in={:02X?} ports_out={:02X?}",
                    apu.cpu.pc, apu.ports_in, apu.ports_out
                );
                apuportlog_last = apu.ports_in;
            }
            let pcv = sys.cpu.pc24()
                | if std::env::var("PROBE_RINGP").is_ok() {
                    u32::from(sys.cpu.p) << 24
                } else {
                    0
                };
            if std::env::var("PROBE_NMIW").is_ok() {
                let cur = sys.bus.nmitimen.0;
                if cur != nmitimen_last {
                    println!(
                        "      NMIW n={n} pc_after={:06X} $4200 {nmitimen_last:02X}->{cur:02X}",
                        sys.cpu.pc24()
                    );
                    nmitimen_last = cur;
                }
            }
            if irqlog_max > 0 {
                let line = sys.bus.timing.line;
                let dot = sys.bus.timing.dot();
                let nmitimen_now = sys.bus.nmitimen.0;
                let htime_now = sys.bus.irq.htime;
                let vtime_now = sys.bus.irq.vtime;
                let disabling_now = sys.bus.nmitimen.irq_mode() == rf_snes::regs::IrqMode::Off
                    && rf_snes::regs::NmiTimen(nmitimen_last).irq_mode()
                        != rf_snes::regs::IrqMode::Off;
                if nmitimen_now != nmitimen_last {
                    arm_events += 1;
                    if irqlog_printed < irqlog_max {
                        println!(
                            "      ARMLOG n={n} line={line} dot={dot} $4200: {nmitimen_last:02X}->{nmitimen_now:02X}"
                        );
                        irqlog_printed += 1;
                    }
                    nmitimen_last = nmitimen_now;
                }
                if htime_now != htime_last || vtime_now != vtime_last {
                    arm_events += 1;
                    if irqlog_printed < irqlog_max {
                        println!(
                            "      ARMLOG n={n} line={line} dot={dot} htime: {htime_last:03X}->{htime_now:03X} vtime: {vtime_last:03X}->{vtime_now:03X}"
                        );
                        irqlog_printed += 1;
                    }
                    htime_last = htime_now;
                    vtime_last = vtime_now;
                }
                let fired_now = sys.bus.irq.fired;
                if fired_now && !irq_fired_last {
                    assert_events += 1;
                    if irqlog_printed < irqlog_max {
                        println!(
                            "      IRQLOG n={n} line={line} dot={dot} ASSERT pc={:06X} p={:02X}",
                            pcv & 0x00FF_FFFF,
                            sys.cpu.p
                        );
                        irqlog_printed += 1;
                    }
                } else if !fired_now && irq_fired_last && !disabling_now {
                    // A falling edge not explained by $4200 disabling IRQs
                    // this same instant is the only other way `fired`
                    // clears: a `$4211` read (`IrqTimer::read_timeup`).
                    ack_events += 1;
                    if irqlog_printed < irqlog_max {
                        println!(
                            "      ACKLOG n={n} line={line} dot={dot} pc={:06X}",
                            pcv & 0x00FF_FFFF
                        );
                        irqlog_printed += 1;
                    }
                }
                irq_fired_last = fired_now;
                let inidisp_now = (sys.bus.ppu.forced_blank, sys.bus.ppu.brightness);
                if inidisp_now != inidisp_last {
                    inidisp_events += 1;
                    if irqlog_printed < irqlog_max {
                        println!(
                            "      INIDISPLOG n={n} line={line} dot={dot} pc={:06X} \
                             forced_blank:{}->{} bright:{}->{}",
                            pcv & 0x00FF_FFFF,
                            inidisp_last.0,
                            inidisp_now.0,
                            inidisp_last.1,
                            inidisp_now.1
                        );
                        irqlog_printed += 1;
                    }
                    inidisp_last = inidisp_now;
                }
                if let Ok(target_line) = std::env::var("PROBE_LINEWRITES") {
                    if let Ok(target_line) = target_line.parse::<u16>() {
                        let len = sys.bus.ppu.line_writes_for_test(target_line).len();
                        thread_local! {
                            static LAST: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
                        }
                        let last = LAST.with(std::cell::Cell::get);
                        if len != last {
                            println!(
                                "      LINEWRITES n={n} line={line} dot={dot} target_line={target_line} \
                                 len: {last}->{len} contents={:?}",
                                sys.bus.ppu.line_writes_for_test(target_line)
                            );
                            LAST.with(|c| c.set(len));
                        }
                    }
                }
                if let Some((ptr, width)) = tramp_watch {
                    let cur: Vec<u8> = (0..width)
                        .map(|i| rf_snes::cpu::CpuBus::peek(&sys.bus, ptr + u32::from(i)))
                        .collect();
                    if cur != tramp_last {
                        tramp_events += 1;
                        if irqlog_printed < irqlog_max {
                            println!(
                                "      TRAMPLOG n={n} line={line} dot={dot} pc={:06X} \
                                 [{ptr:06X}..+{width}]: {tramp_last:02x?}->{cur:02x?}",
                                pcv & 0x00FF_FFFF
                            );
                            irqlog_printed += 1;
                        }
                        tramp_last = cur;
                    }
                }
                let nmiflag_now = sys.bus.timing.nmi_flag;
                if nmiflag_now && !nmiflag_last {
                    rdnmi_set_events += 1;
                    if irqlog_printed < irqlog_max {
                        println!("      RDNMILOG n={n} line={line} dot={dot} SET (vblank edge)");
                        irqlog_printed += 1;
                    }
                } else if !nmiflag_now && nmiflag_last {
                    rdnmi_clear_events += 1;
                    if irqlog_printed < irqlog_max {
                        println!(
                            "      RDNMILOG n={n} line={line} dot={dot} pc={:06X} CLEARED (a $4210 read saw bit7=1)",
                            pcv & 0x00FF_FFFF
                        );
                        irqlog_printed += 1;
                    }
                }
                nmiflag_last = nmiflag_now;
                let hvbjoy_now = sys.bus.timing.in_vblank();
                if hvbjoy_now && !hvbjoy_last {
                    hvbjoy_enter_events += 1;
                    if irqlog_printed < irqlog_max {
                        println!("      HVBJOYLOG n={n} line={line} dot={dot} ENTER vblank");
                        irqlog_printed += 1;
                    }
                } else if !hvbjoy_now && hvbjoy_last {
                    hvbjoy_exit_events += 1;
                    if irqlog_printed < irqlog_max {
                        println!("      HVBJOYLOG n={n} line={line} dot={dot} EXIT vblank");
                        irqlog_printed += 1;
                    }
                }
                hvbjoy_last = hvbjoy_now;
                if pcv & 0x00FF_FFFF == nmi_vec_native || pcv & 0x00FF_FFFF == nmi_vec_emu {
                    nmi_dispatch_events += 1;
                    if irqlog_printed < irqlog_max {
                        println!(
                            "      NMILOG n={n} line={line} dot={dot} DISPATCH pc={:06X} p={:02X}",
                            pcv & 0x00FF_FFFF,
                            sys.cpu.p
                        );
                        irqlog_printed += 1;
                    }
                }
            }
            if let Some((start, end)) = spwin {
                if n > start && n <= end {
                    let op = rf_snes::cpu::CpuBus::peek(&sys.bus, prev_pc);
                    println!(
                        "      SPWIN n={n} prev_pc={prev_pc:06X} op={op:02X} sp={:04X}",
                        sys.cpu.sp
                    );
                }
            }
            for &addr in &watch {
                let now = rf_snes::cpu::CpuBus::peek(&sys.bus, addr);
                if let Some(&old) = watch_prev.get(&addr) {
                    if old != now {
                        println!(
                            "      WATCH n={n} addr={addr:06X} old={old:02X} new={now:02X} prev_pc={prev_pc:06X}"
                        );
                    }
                }
                watch_prev.insert(addr, now);
            }
            if let Some((start, end)) = accesswin {
                // `prev_pc` is the address the instruction that JUST ran
                // (this iteration's `core.step`) was fetched from, since
                // nothing else changes `sys.cpu.pc24()` between
                // iterations.
                if accesswin_active && prev_pc == end {
                    println!(
                        "      ACCESSWIN n={n} window={start:06X}..{end:06X} \
                         accesses={accesswin_accesses} \
                         spent_master_cycles={accesswin_master_cycles} \
                         spent_master_cycles/6={}",
                        accesswin_master_cycles / 6
                    );
                    accesswin_active = false;
                }
                if !accesswin_active && prev_pc == start {
                    accesswin_active = true;
                    accesswin_accesses = 0;
                    accesswin_master_cycles = 0;
                }
                if accesswin_active {
                    accesswin_accesses += sys.last_instr_accesses;
                    accesswin_master_cycles += sys.last_instr_master_cycles;
                }
            }
            prev_pc = pcv & 0x00FF_FFFF;
            let spcv = apu.cpu.pc;
            if spcv != spc_last {
                spcring.push_back(spcv);
                spc_pc_transitions += 1;
                // W14-27: spcring's 3000-entry cap evicts old history, so
                // it cannot answer "how many times total" for an SPC PC of
                // interest over a run with more transitions than that.
                // PROBE_SPCPCCOUNT counts genuine transitions (not samples
                // of an already-parked PC) into each given 16-bit ARAM PC
                // across the whole run.
                if let Ok(list) = std::env::var("PROBE_SPCPCCOUNT") {
                    for tok in list.split(',') {
                        if let Ok(target) = u16::from_str_radix(tok, 16) {
                            if spcv == target {
                                *spcpc_counts.entry(target).or_insert(0u64) += 1;
                            }
                        }
                    }
                }
                spc_last = spcv;
                if spcring.len() > 3000 {
                    spcring.pop_front();
                }
            }
            if std::env::var("PROBE_STOP_ON_SPC_STOP").is_ok()
                && apu.cpu.stopped
                && apu.boot.is_running()
            {
                break;
            }
            if packetlog {
                let dp1 = apu.aram[0x0001];
                if dp1 != dp1_last {
                    println!(
                        "      DP01LOG n={n} spc={:04X} dp$01: {:02X}->{:02X}",
                        apu.cpu.pc, dp1_last, dp1
                    );
                    dp1_last = dp1;
                }
                if apu.cpu.x != x_last {
                    x_changes += 1;
                    if x_last != u8::MAX {
                        println!(
                            "      PACKETLOG n={n} spc={:04X} X: {:02X}->{:02X} ports_in={:02x?}",
                            apu.cpu.pc, x_last, apu.cpu.x, apu.ports_in
                        );
                    }
                    x_last = apu.cpu.x;
                }
                if apu.ports_in[0] != port0_last {
                    port0_changes += 1;
                    port0_last = apu.ports_in[0];
                }
            }
            if timerlog {
                let t = [
                    apu.timers[0].enabled,
                    apu.timers[1].enabled,
                    apu.timers[2].enabled,
                ];
                if t != timer_last {
                    println!(
                        "      TIMERLOG n={n} spc={:04X} en={:?} target={:?}",
                        apu.cpu.pc,
                        t,
                        [
                            apu.timers[0].target,
                            apu.timers[1].target,
                            apu.timers[2].target
                        ]
                    );
                    timer_last = t;
                }
            }
            if !mathpcs.is_empty() && mathpcs.contains(&(pcv & 0x00FF_FFFF)) {
                let m = &sys.bus.math;
                println!(
                    "      MATHLOG n={n} pc={:06X} busy={} wrdiv={:04X} rddiv={:04X} rdmpy={:04X} a={:04X} x={:04X} y={:04X}",
                    pcv & 0x00FF_FFFF,
                    m.busy(),
                    m.wrdiv,
                    m.rddiv,
                    m.rdmpy,
                    sys.cpu.a,
                    sys.cpu.x,
                    sys.cpu.y
                );
            }
            // W14-27: print SPC700 register + top-of-stack state whenever
            // the SPC700 is about to execute an instruction at one of the
            // given 16-bit ARAM PCs. Used to catch the A/Y register and
            // the RET-popped return address at a suspected push-address/
            // RET computed-jump dispatch (see PROBE_SPCREGPC in the
            // module doc).
            if let Ok(list) = std::env::var("PROBE_SPCREGPC") {
                for tok in list.split(',') {
                    if let Ok(target) = u16::from_str_radix(tok, 16) {
                        if apu.cpu.pc == target {
                            println!(
                                "      SPCPCLOG n={n} pc={:04X} a={:02X} x={:02X} y={:02X} sp={:02X} \
                                 psw={:02X} p={} \
                                 stack01={:02X} stack02={:02X} stack03={:02X} stack04={:02X}",
                                apu.cpu.pc,
                                apu.cpu.a,
                                apu.cpu.x,
                                apu.cpu.y,
                                apu.cpu.sp,
                                apu.cpu.psw,
                                apu.cpu.psw & 0x20 != 0,
                                apu.aram[0x0100 | ((apu.cpu.sp.wrapping_add(1)) as usize)],
                                apu.aram[0x0100 | ((apu.cpu.sp.wrapping_add(2)) as usize)],
                                apu.aram[0x0100 | ((apu.cpu.sp.wrapping_add(3)) as usize)],
                                apu.aram[0x0100 | ((apu.cpu.sp.wrapping_add(4)) as usize)],
                            );
                        }
                    }
                }
            }
            if !sdumps.is_empty() && sdumps.contains(&(pcv & 0x00FF_FFFF)) {
                let sp = sys.cpu.sp;
                let bytes: Vec<u8> = (0..12)
                    .map(|i| {
                        rf_snes::cpu::CpuBus::peek(&sys.bus, u32::from(sp.wrapping_add(1 + i)))
                    })
                    .collect();
                let vec0: Vec<u8> = (0..6)
                    .map(|i| rf_snes::cpu::CpuBus::peek(&sys.bus, i))
                    .collect();
                println!(
                    "      SDUMP n={n} pc={:06X} e={} p={:02X} sp={:04X} d={:04X} dbr={:02X} a={:04X} x={:04X} y={:04X} wmadd={:06X} vec0000={vec0:02x?} stack[sp+1..+12]={bytes:02x?}",
                    pcv & 0x00FF_FFFF,
                    sys.cpu.e,
                    sys.cpu.p,
                    sp,
                    sys.cpu.d,
                    sys.cpu.dbr,
                    sys.cpu.a,
                    sys.cpu.x,
                    sys.cpu.y,
                    sys.bus.wram_port.address
                );
                // Optional: decode this PC as if it were a `JSR ($nnnn,X)`/
                // `JMP ($nnnn,X)` table dispatch — `base` is the next two
                // bytes (the operand), the table entry lives at
                // `PBR:(base+X)`, and its 16-bit contents is the would-be
                // jump target. Only meaningful when the matched PC really
                // is one of those two opcodes — gated behind its own env
                // var so it cannot be misread as part of an unrelated
                // instruction's state (W14-26 almost was, once).
                if std::env::var("PROBE_SDUMP_TABLE").is_ok() {
                    let opbank = (pcv & 0x00FF_0000) >> 16;
                    let opoff = (pcv & 0xFFFF) as u16;
                    let base = u16::from(rf_snes::cpu::CpuBus::peek(
                        &sys.bus,
                        u32::from(opoff.wrapping_add(1)) | (opbank << 16),
                    )) | (u16::from(rf_snes::cpu::CpuBus::peek(
                        &sys.bus,
                        u32::from(opoff.wrapping_add(2)) | (opbank << 16),
                    )) << 8);
                    let table_off = base.wrapping_add(sys.cpu.x);
                    let table_addr = (opbank << 16) | u32::from(table_off);
                    let lo = rf_snes::cpu::CpuBus::peek(&sys.bus, table_addr);
                    let hi = rf_snes::cpu::CpuBus::peek(
                        &sys.bus,
                        (opbank << 16) | u32::from(table_off.wrapping_add(1)),
                    );
                    println!(
                        "      SDUMP-TABLE table_base={base:04X} table_addr={table_addr:06X} table_entry={hi:02X}{lo:02X}"
                    );
                }
            }
            if pcv != ring_last {
                pcring.push_back(pcv);
                ring_last = pcv;
                if pcring.len() > 6000 {
                    pcring.pop_front();
                }
            }
            let now = (
                apu.ports_in,
                apu.ports_out,
                [
                    apu.timers[0].enabled,
                    apu.timers[1].enabled,
                    apu.cpu.stopped,
                ],
            );
            if now != last {
                let line = format!("n={n} cpu={:06X} A={:04X} spc={:04X} stop={} ipl={} boot={:?} in={:02x?} out={:02x?} ten={:?}", sys.cpu.pc24(), sys.cpu.a, apu.cpu.pc, apu.cpu.stopped, apu.ipl_enabled, apu.boot.state, now.0, now.1, now.2);
                ring.push_back(line);
                if ring.len() > 40 {
                    ring.pop_front();
                }
                last = now;
            }
        }
        if std::env::var("W1426_SP").is_ok() {
            println!("      W1426_SP final sp={:04X}", core.system().cpu.sp);
        }
        if std::env::var("PROBE_PORTS").is_ok() {
            for l in &ring {
                println!("      {l}");
            }
        }
        if packetlog {
            println!(
                "      PACKETLOG totals: x_register_changes={x_changes} ports_in0_changes={port0_changes}"
            );
        }
        if irqlog_max > 0 {
            println!(
                "      IRQLOG totals (whole run): arm_events={arm_events} assert_events={assert_events} \
                 ack_events={ack_events} tramp_events={tramp_events} inidisp_events={inidisp_events} \
                 (printed first {irqlog_printed})"
            );
            println!(
                "      IRQLOG totals (whole run): rdnmi_set_events={rdnmi_set_events} \
                 rdnmi_clear_events={rdnmi_clear_events} hvbjoy_enter_events={hvbjoy_enter_events} \
                 hvbjoy_exit_events={hvbjoy_exit_events} nmi_dispatch_events={nmi_dispatch_events}"
            );
        }
        if !spcpc_counts.is_empty() {
            let mut counts: Vec<_> = spcpc_counts.iter().collect();
            counts.sort_by_key(|(pc, _)| **pc);
            println!(
                "      SPCPCCOUNT (whole run, not evicted): {}",
                counts
                    .iter()
                    .map(|(pc, n)| format!("{pc:04X}x{n}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        if std::env::var("PROBE_SPCRING").is_ok() {
            println!(
                "    spcring: total_distinct_pc_transitions={spc_pc_transitions} (ring holds last {})",
                spcring.len()
            );
            println!(
                "    spcring: {}",
                spcring
                    .iter()
                    .map(|p| format!("{p:04X}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        if std::env::var("PROBE_RING").is_ok() {
            println!(
                "    ring: {}",
                pcring
                    .iter()
                    .map(|p| if std::env::var("PROBE_RINGP").is_ok() {
                        format!("{:06X}:{:02X}", p & 0xFF_FFFF, p >> 24)
                    } else {
                        format!("{p:06X}")
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        let _ = frames;
        // sample CPU PCs over 20000 instructions
        let mut pcs: HashMap<u32, u32> = HashMap::new();
        let mut spc: HashMap<u16, u32> = HashMap::new();
        let (mut nmi_hits, mut irq_hits) = (0u32, 0u32);
        let vec = |sys: &rf_snes::system::SnesSystem, at: u32| -> u32 {
            u32::from(rf_snes::cpu::CpuBus::peek(&sys.bus, at))
                | (u32::from(rf_snes::cpu::CpuBus::peek(&sys.bus, at + 1)) << 8)
        };
        let (nmi_vec, irq_vec) = (vec(core.system(), 0xFFEA), vec(core.system(), 0xFFEE));
        let sample: usize = std::env::var("PROBE_SAMPLE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20000);
        for _ in 0..sample {
            core.step(Step::Instruction, &mut sink);
            let pcv = core.system().cpu.pc24();
            if pcv == nmi_vec {
                nmi_hits += 1;
            }
            if pcv == irq_vec {
                irq_hits += 1;
            }
            *pcs.entry(pcv).or_default() += 1;
            *spc.entry(core.system().bus.apu.cpu.pc).or_default() += 1;
        }
        // W14-42: same "compose the actual picture right now" check
        // PROBE_M7 already does in FRAMES mode (render_scanline for every
        // visible line, dedupe palette_index), but usable from the
        // instruction-step path too — answers "with the OAM/CGRAM/TM
        // state PROBE_OAM already decoded above, does the compositor
        // actually PRODUCE more than a flat frame" at an arbitrary
        // PROBE_INSTR sample point, not only at a FRAMES-mode frame
        // boundary. Must run before `sys`/`ppu` below borrow `core`
        // immutably for the rest of this function.
        if std::env::var("PROBE_RENDERNOW").is_ok() {
            let sys_mut = core.system_mut();
            let mut idx = std::collections::HashSet::new();
            for y in 0..224u16 {
                for px in sys_mut.bus.ppu.render_scanline(y).pixels {
                    idx.insert(px.palette_index);
                }
            }
            let sample: Vec<u8> = idx.iter().take(8).copied().collect();
            println!(
                "  RENDERNOW distinct_indices={} sample={sample:?}",
                idx.len()
            );
        }
        let sys = core.system();
        let ppu = &sys.bus.ppu;
        let apu = &sys.bus.apu;
        let mut top: Vec<_> = pcs.iter().collect();
        top.sort_by(|a, b| b.1.cmp(a.1));
        let mut stop: Vec<_> = spc.iter().collect();
        stop.sort_by(|a, b| b.1.cmp(a.1));
        println!(
            "=== {}",
            Path::new(path).file_name().unwrap().to_string_lossy()
        );
        println!("  varied={} lines={} forced_blank={} bright={} mode={} tm=[{}] ts={:#x} cgram_nonzero={} vram_nonzero={} oam_nonzero={}",
            sink.varied, sink.lines, ppu.forced_blank, ppu.brightness, ppu.bg_mode,
            (0..4).map(|i| if ppu.bgs[i].enabled {'1'} else {'0'}).collect::<String>() + if ppu.obj_enabled {"+obj"} else {""},
            ppu.ts,
            ppu.cgram.iter().filter(|c| **c != 0).count(), ppu.vram.iter().filter(|b| **b != 0).count(), ppu.oam.iter().filter(|b| **b != 0).count());
        println!(
            "  nmitimen={:?} apu.boot_running={} spc.stopped={} ports_in={:02x?} ports_out={:02x?}",
            sys.bus.nmitimen,
            apu.boot.is_running(),
            apu.cpu.stopped,
            apu.ports_in,
            apu.ports_out
        );
        // W14-38: is $212C's OBJ-only main screen (all BGs off) genuinely
        // empty because every sprite sits off the visible 224-line
        // picture (a legitimate "not shown yet" state), or does a sprite
        // sit on-screen while the composer still produces nothing (a
        // renderer defect this ticket's write_scope covers)? Decodes all
        // 128 OAM entries the same way `obj::decode_sprite` does. The
        // on-screen test below is a coarse approximation, NOT
        // `obj::intersects`'s own (private, per-scanline, OAMADDR-
        // rotation-aware) test: it treats Y as unsigned 0..255 (no
        // hardware wraparound-onto-top-of-screen case) and only asks
        // whether the sprite's Y span overlaps 0..224 for ANY row, which
        // is enough to answer "would this sprite ever be visible on some
        // line", the question this diagnostic exists for.
        if std::env::var("PROBE_OAM").is_ok() {
            let mut onscreen = 0usize;
            for i in 0..128u8 {
                let s = rf_snes::ppu::obj::decode_sprite(ppu, i);
                let y_end = u16::from(s.y) + s.height;
                let on = (u16::from(s.y)..y_end).any(|y| y < 224);
                if on && (s.x > -(s.width as i16) && s.x < 256) {
                    onscreen += 1;
                    if onscreen <= 20 {
                        // Same base/character math `obj::draw_sprite` uses
                        // for this sprite's top-left texel, so a wrong
                        // name-base or genuinely-blank VRAM shows up as
                        // colour=0 (transparent) here, not just eventually
                        // as a uniform frame.
                        let base = ppu.obj_name_base << 13
                            | if s.second_page {
                                (ppu.obj_name_select + 1) << 12
                            } else {
                                0
                            };
                        let colour = rf_snes::ppu::bg::fetch_pixel(ppu, base, s.tile, 0, 0, 4);
                        println!(
                            "    OAM[{i}] x={} y={} w={} h={} tile={:03X} pal={} pri={} \
                             name_base={:#x} top_left_colour={}",
                            s.x,
                            s.y,
                            s.width,
                            s.height,
                            s.tile,
                            s.palette,
                            s.priority,
                            base,
                            colour
                        );
                    }
                }
            }
            println!("  OAM on-screen sprites (x in -width..256, y wraps onto 0..224): {onscreen}");
        }
        if std::env::var("PROBE_ALLPC").is_ok() {
            let mut all: Vec<_> = pcs.keys().collect();
            all.sort();
            println!(
                "  allpc: {}",
                all.iter()
                    .map(|p| format!("{p:06X}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
        println!(
            "  distinct_pc={} top: {}",
            pcs.len(),
            top.iter()
                .take(6)
                .map(|(pc, n)| format!("{pc:06X}x{n}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        println!(
            "  spc distinct_pc={} top: {}",
            spc.len(),
            stop.iter()
                .take(4)
                .map(|(pc, n)| format!("{pc:04X}x{n}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
        struct Pk<'a>(&'a rf_snes::bus::SnesBus);
        impl rf_snes::trace::TracePeek for Pk<'_> {
            fn peek(&self, addr: u32) -> u8 {
                rf_snes::cpu::CpuBus::peek(self.0, addr)
            }
        }
        let pk = Pk(&sys.bus);
        let cpu = &sys.cpu;
        for (pc, _) in top.iter().take(3) {
            let pbr = (**pc >> 16) as u8;
            let lo = (**pc & 0xFFFF) as u16;
            let (_, bytes, text) = rf_snes::trace::disassemble(pbr, lo, cpu.p, cpu.e, &pk);
            println!("    cpu {pc:06X}: {bytes:02x?} {text}");
        }
        if let Ok(pat) = std::env::var("PROBE_FINDROM") {
            for hexpat in pat.split(',') {
                let needle: Vec<u8> = (0..hexpat.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&hexpat[i..i + 2], 16).unwrap())
                    .collect();
                let hits: Vec<String> = bytes
                    .windows(needle.len())
                    .enumerate()
                    .filter(|(_, w)| *w == &needle[..])
                    .map(|(i, _)| {
                        format!(
                            "{i:06X}(lo {:02X}:{:04X})",
                            0x80 + i / 0x8000,
                            0x8000 + i % 0x8000
                        )
                    })
                    .collect();
                println!("    findrom {hexpat}: {}", hits.join(" "));
            }
        }
        if let Ok(pat) = std::env::var("PROBE_FIND") {
            for hexpat in pat.split(',') {
                let needle: Vec<u8> = (0..hexpat.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&hexpat[i..i + 2], 16).unwrap())
                    .collect();
                let hits: Vec<String> = apu
                    .aram
                    .windows(needle.len())
                    .enumerate()
                    .filter(|(_, w)| *w == &needle[..])
                    .map(|(i, _)| format!("{i:04X}"))
                    .collect();
                println!("    find {hexpat}: {}", hits.join(" "));
            }
        }
        for spec in std::env::var("PROBE_DIS")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
        {
            // "pbr:start:end" forward disassembly
            let parts: Vec<&str> = spec.split(':').collect();
            let pbr = u8::from_str_radix(parts[0], 16).unwrap();
            let mut pc = u16::from_str_radix(parts[1], 16).unwrap();
            let end = u32::from_str_radix(parts[2], 16).unwrap();
            while u32::from(pc) < end {
                let (n, bytes, text) = rf_snes::trace::disassemble(pbr, pc, cpu.p, cpu.e, &pk);
                println!("    dis {pbr:02X}{pc:04X}: {bytes:02x?} {text}");
                pc = pc.wrapping_add(n as u16);
            }
        }
        for spec in std::env::var("PROBE_ARAM")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
        {
            let parts: Vec<&str> = spec.split(':').collect();
            let a = usize::from_str_radix(parts[0], 16).unwrap();
            let b = usize::from_str_radix(parts[1], 16).unwrap();
            for row in (a..b).step_by(16) {
                println!(
                    "    aram {row:04X}: {:02x?}",
                    &apu.aram[row..(row + 16).min(b)]
                );
            }
        }
        for (pc, _) in stop.iter().take(3) {
            let a = usize::from(**pc);
            println!(
                "    spc {pc:04X}: {:02x?}",
                &apu.aram[a..(a + 10).min(apu.aram.len())]
            );
        }
        println!("    timers: {:?} test={:#04x}", apu.timers, apu.test);
        println!("    dma: {:?}", sys.bus.dma);
        if let Ok(list) = std::env::var("PROBE_PEEK") {
            let vals: Vec<String> = list
                .split(',')
                .map(|a| {
                    let addr = u32::from_str_radix(a, 16).unwrap();
                    format!("{a}={:02X}", rf_snes::cpu::CpuBus::peek(&sys.bus, addr))
                })
                .collect();
            println!("    peek: {}", vals.join(" "));
        }
        // PROBE_VRAM=hexbyteaddr:len dumps raw VRAM bytes (cluster A).
        if let Ok(spec) = std::env::var("PROBE_VRAM") {
            let (a, l) = spec.split_once(':').unwrap();
            let a = usize::from_str_radix(a, 16).unwrap();
            let l = l.parse::<usize>().unwrap();
            println!(
                "    vram {a:04X} (vmadd={:04X} vmain={:02X}): {:02x?}",
                sys.bus.vram_address,
                sys.bus.vmain,
                &sys.bus.ppu.vram[a..(a + l).min(sys.bus.ppu.vram.len())]
            );
        }
        println!(
            "    timing: line={} dot={} frame={} hdmaen={:#04x}",
            sys.bus.timing.line,
            sys.bus.timing.dot(),
            sys.bus.timing.frame,
            sys.bus.hdmaen
        );
        println!("    irq: {:?} mode={:?} nmi_vec={nmi_vec:04X} nmi_entries={nmi_hits} irq_vec={irq_vec:04X} irq_entries={irq_hits} cpu.stopped={}", sys.bus.irq, sys.bus.nmitimen.irq_mode(), sys.cpu.stopped);
        println!("    spc regs: a={:02x} x={:02x} y={:02x} ; F4-F7 in(spc reads)={:02x?} out(cpu reads)={:02x?} timers en={:?} counters={:?}",
            apu.cpu.a, apu.cpu.x, apu.cpu.y, apu.ports_in, apu.ports_out,
            apu.timers.iter().map(|t| t.enabled).collect::<Vec<_>>(), apu.timers.iter().map(|t| t.peek_counter()).collect::<Vec<_>>());
        println!(
            "    echo: write_disabled={} base_page={:02X} (base={:04X}) delay={:02X} dir={:02X}",
            apu.dsp.echo.write_disabled,
            apu.dsp.echo.base_page,
            u16::from(apu.dsp.echo.base_page) << 8,
            apu.dsp.echo.delay,
            apu.dsp.dir,
        );
        if std::env::var("PROBE_DSP").is_ok() {
            println!("    dsp regs: {:02x?}", &apu.dsp.regs[..]);
            for (i, v) in apu.dsp.voices.iter().enumerate() {
                println!(
                    "    dsp v{i}: stage={:?} level={} adsr={} gain={:02x} keyed={} pitch={:04x} srcn={:02x}",
                    v.envelope.stage, v.envelope.level, v.envelope.adsr_enabled, v.envelope.gain, v.keyed_on, v.pitch, v.srcn
                );
            }
        }
        print_sa1_reg_report(&core);
        print_gsu_reg_report(&core);
    }
}

/// Ticket W17-04 acceptance #4: print any `$22xx` write this run made to an
/// offset `rf-snes` has no register for (see
/// `rf_snes::sa1::Sa1Regs::unknown_write_offsets`'s doc) — a clean SA-1
/// title should print nothing here. `PROBE_SA1REGS=1` opts in; silent
/// (and free) for the 1000+ non-SA-1 titles this probe also runs against.
fn print_sa1_reg_report(core: &rf_snes::core::SnesCore) {
    if std::env::var("PROBE_SA1REGS").is_err() {
        return;
    }
    let Some(sa1) = core.system().bus.sa1.as_ref() else {
        return;
    };
    if sa1.regs.unknown_write_offsets.is_empty() {
        println!("    sa1 unknown register writes: none");
    } else {
        println!(
            "    sa1 unknown register writes: {:?}",
            sa1.regs.unknown_write_offsets
        );
    }
}

/// Ticket W18-01: the GSU-register equivalent of `print_sa1_reg_report`
/// (see `rf_snes::gsu::Gsu::unknown_write_offsets`'s doc). `PROBE_GSUREGS=1`
/// opts in; silent (and free) for every non-GSU title this probe also runs
/// against.
///
/// Ticket W18-02 extends this with the instruction core's own state: PC
/// (`PBR:R15`), the most recently fetched opcode byte, and the total
/// instruction count — so a title whose GSU program gets "stuck" (the
/// register window sits uniform, per the census, because the core parked
/// on a WAIT-shaped condition this slice does not model, or because it
/// looped on a real bug) shows exactly where, rather than only "GO is
/// still set".
fn print_gsu_reg_report(core: &rf_snes::core::SnesCore) {
    if std::env::var("PROBE_GSUREGS").is_err() {
        return;
    }
    let Some(gsu) = core.system().bus.gsu.as_ref() else {
        return;
    };
    if gsu.regs.unknown_write_offsets.is_empty() {
        println!("    gsu unknown register writes: none");
    } else {
        println!(
            "    gsu unknown register writes: {:?}",
            gsu.regs.unknown_write_offsets
        );
    }
    println!(
        "    gsu core: PBR:R15={:02X}:{:04X} last_opcode={:02X} last_cost={} \
         instructions={} go={} irq={} plot_calls={} rpix_calls={}",
        gsu.regs.pbr(),
        gsu.regs.r15(),
        gsu.regs.last_opcode,
        gsu.regs.last_cost(),
        gsu.regs.instructions_executed(),
        gsu.regs.go(),
        gsu.regs.irq_pending(),
        gsu.regs.plot_calls,
        gsu.regs.rpix_calls,
    );
    // Ticket W18-04 follow-up (coordinator-directed trace of the Star Fox
    // census regression): the code-cache hit/miss and ROM/RAM-buffer
    // stall counters, plus how many times GO was set/cleared and by whom
    // (an SNES-side write vs the GSU's own STOP), so a stuck title's
    // report says WHY it is spending cycles the way it is, not just where
    // its PC sits.
    let cbr_lo = gsu.regs.peek(0x303E).unwrap_or(0);
    let cbr_hi = gsu.regs.peek(0x303F).unwrap_or(0);
    println!(
        "    gsu counters: cache_hits={} cache_misses={} rom_stalls={} ram_stalls={} \
         go_set={} go_clear={} go_clear_by_stop={} credit={} cbr={:04X}",
        gsu.regs.cache_hits,
        gsu.regs.cache_misses,
        gsu.regs.rom_stall_events,
        gsu.regs.ram_stall_events,
        gsu.regs.go_set_events,
        gsu.regs.go_clear_events,
        gsu.regs.go_cleared_by_stop_events,
        gsu.credit,
        u16::from_le_bytes([cbr_lo, cbr_hi]),
    );
}
