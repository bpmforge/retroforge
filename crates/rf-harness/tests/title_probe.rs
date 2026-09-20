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
//! PROBE_M7=1                        with frames mode: print Mode 7 state and palette diversity
//! PROBE_DIS=bb:start:end[,...]      65816 disassembly ranges (hex, end exclusive)
//! PROBE_ARAM=start:end[,...]        ARAM hex dumps
//! PROBE_FIND=hex[,hex]              search ARAM for byte patterns
//! PROBE_FINDROM=hex[,hex]           search the ROM file (LoROM address shown)
//! PROBE_PORTS=1                     print the last 40 APU port changes with both PCs
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
//! PROBE_SDUMP=hex[,hex]             print e/p/sp/a/x/y and the 12 bytes above the
//!                                    stack pointer whenever the CPU is about to
//!                                    execute an instruction at one of these 24-bit
//!                                    PCs — checks whether an about-to-run RTI/RTS/
//!                                    RTL is about to pop a real return address, or
//!                                    static ROM/open-bus content because SP has
//!                                    wandered into non-WRAM space (W14-26)
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
        if std::env::var("PROBE_MODE").as_deref() == Ok("frames") {
            let mut first_varied: Option<usize> = None;
            for f in 0..frames {
                core.step(Step::Frame, &mut sink);
                if sink.varied && first_varied.is_none() {
                    first_varied = Some(f);
                    break;
                }
            }
            println!(
                "FRAMES varied_at={:?} {}",
                first_varied,
                Path::new(path).file_name().unwrap().to_string_lossy()
            );
            if std::env::var("PROBE_M7").is_ok() {
                {
                    let sys = core.system();
                    let ppu = &sys.bus.ppu;
                    println!(
                        "    mode={} forced_blank={} bright={} tm=[{}] m7={:?} bg1 hofs={} vofs={}",
                        ppu.bg_mode,
                        ppu.forced_blank,
                        ppu.brightness,
                        (0..4)
                            .map(|i| if ppu.bgs[i].enabled { '1' } else { '0' })
                            .collect::<String>(),
                        ppu.mode7,
                        ppu.bgs[0].hofs,
                        ppu.bgs[0].vofs
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
            if let Some((start, end)) = spwin {
                if n > start && n <= end {
                    let op = rf_snes::cpu::CpuBus::peek(&sys.bus, prev_pc);
                    println!(
                        "      SPWIN n={n} prev_pc={prev_pc:06X} op={op:02X} sp={:04X}",
                        sys.cpu.sp
                    );
                }
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
                println!(
                    "      SDUMP n={n} pc={:06X} e={} p={:02X} sp={:04X} a={:04X} x={:04X} y={:04X} stack[sp+1..+12]={bytes:02x?}",
                    pcv & 0x00FF_FFFF,
                    sys.cpu.e,
                    sys.cpu.p,
                    sp,
                    sys.cpu.a,
                    sys.cpu.x,
                    sys.cpu.y
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
        print_sa1_reg_report(&core);
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
