//! The SA-1's 65C816 as a second recorded core (ticket W40-03).
//!
//! The SA-1 board carries a second 65816 (fullsnes, "SNES Cart SA-1": a
//! 10.74 MHz 65C816 with 2 KiB I-RAM and up to 2 MiB BW-RAM). This module
//! gives it what the main CPU already has: a **coverage recorder**
//! ([`crate::coverage`]) and the **block executor** ([`crate::executor`]),
//! each with its own log / atlas, so the two cores are keyed separately: an
//! atlas built from the S-CPU's recording describes what the S-CPU ran, the
//! SA-1 atlas what the SA-1 ran, and the same ROM offset can be in both.
//!
//! ## Where it hooks
//!
//! The SA-1 instruction boundary is inside [`crate::sa1::Sa1State::step`]
//! (called many times per S-CPU step from the catch-up loop), so the
//! per-instruction halves run there, around `Cpu::step`, on the SA-1's own
//! [`Sa1Bus`]. Peek-only, like the S-CPU's: `Sa1Bus::peek` and `target` are
//! `&self` (the `$230C` bit reader and every other side-effecting register
//! read go through `read`, never here), so an armed or lifted SA-1 runs
//! bit-identically (`tests` below hash the whole machine).
//!
//! ## What differs from the S-CPU
//!
//! - **Addresses.** Fetches resolve through the SA-1's own map
//!   (`sa1_side_target`, live `CXB`/`DXB`/`EXB`/`FXB`), not the S-CPU's.
//!   Both CPUs see the same ROM through different bank windows, so atlas
//!   nodes are keyed by the **ROM offset the bus really read** and the
//!   offsets are comparable across the two atlases.
//! - **Volatile code.** I-RAM and BW-RAM fetches are the SA-1's "work-RAM
//!   code": one volatile space, I-RAM at `0..iram_len`, BW-RAM at
//!   `iram_len..`. They dispatch as `Fetch::Ram` (counted `fallback_ram`).
//! - **No static placement.** The ISA cannot place SA-1 bank windows
//!   statically (`Wdc65816::rom_offset`), so the atlas is observed-only:
//!   its instructions are exactly those the recording saw, branch targets
//!   stay unresolved, and nothing is `Inferred`.
//! - **Entries.** RESET is the `$2203/$2204` vector taken when `CCNT` bit 5
//!   is released (every release is a Reset entry), NMI/IRQ the `$2205-$2208`
//!   vectors; none live at `$FFxx`, so `add_vector_seeds` does not apply.
//! - **No function hooks.** The SA-1 executor carries no hook frames (the
//!   hook sink and Lua host belong to the main core); it dispatches,
//!   counts and checks indirect landings.

use rf_core_api::coverage::{mode_bit, OPCODE, OPERAND};
use rf_core_api::{
    CodeAddr, CoverageLog, IndirectEdge, IndirectKind, InterruptEntry, InterruptKind,
};
use rf_rebuild::{Dispatch, Fetch, RebuildExec};

use crate::coverage::mode_index;
use crate::cpu::{Cpu, CpuBus};
use crate::mapping::Target;
use crate::sa1::Sa1Bus;
use crate::trace::opcode_info;

/// The recorder and executor state of the SA-1 core. Session observation
/// like the S-CPU's `coverage`/`rebuild` fields: never saved, never read by
/// the simulation.
#[derive(Default)]
pub struct Sa1Obs {
    pub(crate) coverage: Option<Box<CoverageLog>>,
    pub(crate) rebuild: Option<Box<RebuildExec>>,
}

/// What the pre-step half learned about the instruction about to run.
#[derive(Clone, Copy)]
pub(crate) struct Before {
    opcode: u8,
    site: CodeAddr,
    indirect: Option<(u32, CodeAddr)>,
}

/// The decode mode and `(pbr, pc)` of a CPU, the thing an interrupt
/// preempts.
pub(crate) type Resume = (u8, u16, u8);

/// Where the SA-1 CPU fetches from, as the executor and recorder see it.
#[derive(Clone, Copy)]
enum Place {
    Rom(usize),
    /// Offset in the volatile space (I-RAM first, then BW-RAM).
    Volatile(usize),
    Other,
}

fn place_of(target: Target, iram_len: usize) -> Place {
    match target {
        Target::Rom(o) => Place::Rom(o),
        Target::Sa1IRam(i) => Place::Volatile(i),
        Target::Sa1BwRam(i) => Place::Volatile(iram_len + i),
        _ => Place::Other,
    }
}

fn place(bus: &Sa1Bus<'_>, addr: u32, iram_len: usize) -> Place {
    place_of(bus.target(addr), iram_len)
}

fn code_addr_at(target: Target, pbr: u8, pc: u16, mode: u8, iram_len: usize) -> CodeAddr {
    CodeAddr {
        cpu: pc,
        pbr,
        mode,
        rom: match place_of(target, iram_len) {
            Place::Rom(o) => u32::try_from(o).ok(),
            _ => None,
        },
    }
}

fn code_addr(bus: &Sa1Bus<'_>, pbr: u8, pc: u16, mode: u8, iram_len: usize) -> CodeAddr {
    let addr = (u32::from(pbr) << 16) | u32::from(pc);
    code_addr_at(bus.target(addr), pbr, pc, mode, iram_len)
}

impl Sa1Obs {
    /// Anything to do per instruction?
    #[inline]
    pub(crate) fn active(&self) -> bool {
        self.coverage.is_some() || self.rebuild.is_some()
    }

    /// A fresh log sized for this board.
    pub(crate) fn fresh_log(rom_len: usize, board: rf_cart::Sa1Board) -> Box<CoverageLog> {
        Box::new(CoverageLog::with_volatile_len(
            rom_len,
            board.iram_len + board.bwram_len,
        ))
    }

    /// Pre-step half: record the instruction's bytes and ask the executor
    /// where it sits in the atlas. `None` when this step fetches nothing
    /// (the CPU is halted in `WAI`/`STP`).
    #[inline(never)]
    pub(crate) fn before(
        &mut self,
        cpu: &Cpu,
        bus: &Sa1Bus<'_>,
        board: rf_cart::Sa1Board,
    ) -> Option<Before> {
        if cpu.stopped {
            return None;
        }
        let (pbr, pc) = (cpu.pbr, cpu.pc);
        let mode = mode_index(cpu.e, cpu.p);
        let addr = cpu.pc24();
        let opcode = bus.peek(addr);
        let len = opcode_info(opcode).1.operand_len(cpu.a8(), cpu.i8()) + 1;
        let site = code_addr(bus, pbr, pc, mode, board.iram_len);
        if let Some(log) = self.coverage.as_deref_mut() {
            log.instructions_recorded += 1;
            for i in 0..len {
                // The fetch address wraps inside the program bank.
                let at = (u32::from(pbr) << 16) | u32::from(pc.wrapping_add(i as u16));
                let flag = if i == 0 {
                    OPCODE | mode_bit(mode)
                } else {
                    OPERAND
                };
                match place(bus, at, board.iram_len) {
                    Place::Rom(o) => log.mark_prg(o, flag),
                    Place::Volatile(o) => log.mark_ram(o as u32, flag),
                    Place::Other => {
                        if i == 0 {
                            log.other_exec += 1;
                        }
                    }
                }
            }
        }
        let indirect = self.rebuild.as_deref_mut().and_then(|x| {
            let fetch = match place(bus, addr, board.iram_len) {
                Place::Rom(o) => u32::try_from(o).map_or(Fetch::Other, Fetch::Rom),
                Place::Volatile(_) => Fetch::Ram,
                Place::Other => Fetch::Other,
            };
            match x.dispatch_at(pbr, pc, fetch, mode) {
                Dispatch::Atlas {
                    node,
                    indirect: true,
                } => Some((node, site)),
                _ => None,
            }
        });
        Some(Before {
            opcode,
            site,
            indirect,
        })
    }

    /// Post-step half (before any interrupt is delivered, which the SA-1
    /// does at the start of its next step): the indirect landing the
    /// instruction just completed, noted in the log and checked against the
    /// atlas.
    #[inline(never)]
    pub(crate) fn after(
        &mut self,
        before: Before,
        cpu: &Cpu,
        bus: &Sa1Bus<'_>,
        board: rf_cart::Sa1Board,
    ) {
        let target = code_addr(
            bus,
            cpu.pbr,
            cpu.pc,
            mode_index(cpu.e, cpu.p),
            board.iram_len,
        );
        if let (Some(x), Some((node, site))) = (self.rebuild.as_deref_mut(), before.indirect) {
            x.check_target(node, site, target);
        }
        let kind = match before.opcode {
            0x6C | 0x7C | 0xDC => IndirectKind::JmpIndirect,
            0xFC => IndirectKind::JsrIndirect,
            0x60 => IndirectKind::Rts,
            0x6B => IndirectKind::Rtl,
            0x40 => IndirectKind::Rti,
            0x00 => IndirectKind::Brk,
            0x02 => IndirectKind::Cop,
            _ => return,
        };
        if let Some(log) = self.coverage.as_deref_mut() {
            log.note_indirect(IndirectEdge {
                site: before.site,
                target,
                kind,
            });
        }
    }

    /// What an interrupt delivery is about to preempt.
    #[inline]
    pub(crate) fn resume_point(cpu: &Cpu) -> Resume {
        (cpu.pbr, cpu.pc, mode_index(cpu.e, cpu.p))
    }

    /// The SA-1 was just (re)booted: its PC is the reset vector. Called
    /// before the bus exists (a held SA-1 returns before it is built), so
    /// the vector is placed through the live bank registers directly.
    #[inline(never)]
    pub(crate) fn note_reset(&mut self, cpu: &Cpu, banks: &crate::mapping::Sa1RomBanks) {
        let Some(log) = self.coverage.as_deref_mut() else {
            return;
        };
        let target = crate::mapping::sa1_side_target(banks, cpu.pbr, cpu.pc);
        log.note_interrupt(InterruptEntry {
            kind: InterruptKind::Reset,
            target: code_addr_at(
                target,
                cpu.pbr,
                cpu.pc,
                mode_index(cpu.e, cpu.p),
                banks.board.iram_len,
            ),
        });
    }

    /// An NMI/IRQ sequence just ran (the CPU is at the handler); `resume`
    /// is what [`Self::resume_point`] read before it.
    #[inline(never)]
    pub(crate) fn note_interrupt(
        &mut self,
        kind: InterruptKind,
        resume: Resume,
        cpu: &Cpu,
        bus: &Sa1Bus<'_>,
        board: rf_cart::Sa1Board,
    ) {
        self.note_entry(kind, cpu, bus, board);
        if let Some(x) = self.rebuild.as_deref_mut() {
            x.note_interrupt_at(resume.0, resume.1, resume.2);
        }
    }

    fn note_entry(
        &mut self,
        kind: InterruptKind,
        cpu: &Cpu,
        bus: &Sa1Bus<'_>,
        board: rf_cart::Sa1Board,
    ) {
        let Some(log) = self.coverage.as_deref_mut() else {
            return;
        };
        let target = code_addr(
            bus,
            cpu.pbr,
            cpu.pc,
            mode_index(cpu.e, cpu.p),
            board.iram_len,
        );
        log.note_interrupt(InterruptEntry { kind, target });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::SnesSystem;

    /// A 32 KiB SA-1 image (LoROM header, chipset `$35` = SA-1 + RAM +
    /// battery). The S-CPU (reset vector `$8000`) programs the SA-1's reset
    /// vector to `$8100` and releases it, then spins. The SA-1 counts X
    /// down in a loop, calls a subroutine and returns (an `RTS`: an
    /// indirect landing), then spins.
    fn image() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        let base = 0x7FC0;
        rom[base + 0x15] = 0x23;
        rom[base + 0x16] = 0x35;
        rom[base + 0x17] = 6;
        rom[base + 0x18] = 3;
        let sum: u16 = 0xBEEF;
        rom[base + 0x1C..base + 0x1E].copy_from_slice(&(sum ^ 0xFFFF).to_le_bytes());
        rom[base + 0x1E..base + 0x20].copy_from_slice(&sum.to_le_bytes());
        rom[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes());
        let main: &[u8] = &[
            0xA9, 0x00, 0x8D, 0x03, 0x22, // LDA #0 ; STA $2203 (CRV lo)
            0xA9, 0x81, 0x8D, 0x04, 0x22, // LDA #$81 ; STA $2204 (CRV hi)
            0xA9, 0x00, 0x8D, 0x00, 0x22, // LDA #0 ; STA $2200 (release Reset)
            0x80, 0xFE, // BRA *
        ];
        rom[..main.len()].copy_from_slice(main);
        let sa1: &[u8] = &[
            // The stack is I-RAM page 1, write-protected from the SA-1 side
            // until `CIWP` ($222A) opens it (fullsnes, SA-1 Memory Control).
            0xA9, 0xFF, 0x8D, 0x2A, 0x22, // LDA #$FF ; STA $222A
            0xA2, 0x05, // LDX #5
            0xCA, 0xD0, 0xFD, // DEX ; BNE -3
            0x20, 0x10, 0x81, // JSR $8110
            0x80, 0xFE, // BRA *
        ];
        rom[0x100..0x100 + sa1.len()].copy_from_slice(sa1);
        rom[0x110..0x113].copy_from_slice(&[0xA9, 0x33, 0x60]); // LDA #$33 ; RTS
        rom
    }

    const STEPS: usize = 300;

    fn run(rom: &[u8], arm: impl FnOnce(&mut SnesSystem)) -> SnesSystem {
        let mut s = SnesSystem::load(rom).expect("SA-1 cart loads");
        arm(&mut s);
        for _ in 0..STEPS {
            s.step().expect("steps");
        }
        s
    }

    fn record(rom: &[u8]) -> CoverageLog {
        let mut s = run(rom, |s| {
            let board = s.bus.sa1.as_ref().unwrap().board;
            s.sa1_obs.coverage = Some(Sa1Obs::fresh_log(s.bus.rom.len(), board));
        });
        *s.sa1_obs.coverage.take().unwrap()
    }

    #[test]
    fn the_recorder_sees_the_sa1_reset_its_bytes_and_its_rts() {
        let rom = image();
        let log = record(&rom);
        assert!(
            log.instructions_recorded > 10,
            "{}",
            log.instructions_recorded
        );
        // The SA-1's reset entry is its own vector, at ROM offset $100.
        let reset = log
            .interrupts
            .iter()
            .find(|e| e.kind == InterruptKind::Reset)
            .expect("reset entry");
        assert_eq!((reset.target.cpu, reset.target.rom), (0x8100, Some(0x100)));
        assert_eq!(reset.target.mode, 0, "emulation mode");
        for off in [
            0x100, 0x102, 0x105, 0x107, 0x108, 0x10A, 0x10D, 0x110, 0x112,
        ] {
            assert_ne!(log.prg_flags[off] & OPCODE, 0, "opcode at {off:#x}");
        }
        assert_ne!(log.prg_flags[0x101] & OPERAND, 0);
        // The S-CPU's code (offset 0) is not in the SA-1's log.
        assert_eq!(log.prg_flags[0], 0);
        let rts = log
            .indirect
            .iter()
            .find(|e| e.kind == IndirectKind::Rts)
            .expect("RTS edge");
        assert_eq!(rts.site.rom, Some(0x112));
        assert_eq!(rts.target.rom, Some(0x10D));
    }

    #[test]
    fn a_sa1_atlas_dispatches_its_own_code_with_no_miss() {
        let rom = image();
        let log = record(&rom);
        let atlas = crate::rebuild::build_sa1_atlas_from_image(&rom, &log).expect("builds");
        assert!(atlas.stats.observed_insns >= 9, "{:?}", atlas.stats);
        let s = run(&rom, |s| {
            s.sa1_obs.rebuild = Some(Box::new(RebuildExec::new(atlas.clone())));
        });
        let st = s.sa1_obs.rebuild.as_deref().unwrap().stats();
        assert!(st.observed > 10, "{st:?}");
        assert_eq!(
            (st.fallback, st.fallback_width, st.dispatch_misses),
            (0, 0, 0)
        );
        assert!(st.indirect_checked >= 1, "the RTS is checked: {st:?}");
        // The S-CPU has no executor of its own here.
        assert!(s.rebuild.is_none());
    }

    #[test]
    fn observing_the_sa1_does_not_change_the_machine() {
        let rom = image();
        let log = record(&rom);
        let atlas = crate::rebuild::build_sa1_atlas_from_image(&rom, &log).expect("builds");
        let fingerprint = |s: &SnesSystem| {
            let sa1 = s.bus.sa1.as_ref().unwrap();
            let c = &sa1.cpu;
            (
                s.master_cycles,
                s.cpu.pc24(),
                (c.a, c.x, c.y, c.pc, c.p, c.pbr),
                (sa1.credit, sa1.instructions_executed),
                sa1.iram.clone(),
                sa1.bwram.clone(),
            )
        };
        let off = fingerprint(&run(&rom, |_| {}));
        let armed = fingerprint(&run(&rom, |s| {
            let board = s.bus.sa1.as_ref().unwrap().board;
            s.sa1_obs.coverage = Some(Sa1Obs::fresh_log(s.bus.rom.len(), board));
        }));
        let lifted = fingerprint(&run(&rom, |s| {
            s.sa1_obs.rebuild = Some(Box::new(RebuildExec::new(atlas.clone())));
        }));
        assert!(off.3 .1 > 10, "the SA-1 ran: {off:?}");
        assert_eq!(off, armed);
        assert_eq!(off, lifted);
    }

    #[test]
    fn sa1_code_in_iram_is_volatile_code() {
        let mut rom = image();
        // SA-1 program: LDA #$A9 ; STA $3000 ; LDA #$33 ; STA $3001 ;
        // LDA #$60 ; STA $3002 ; JSR $3000 ; BRA *  -- builds `LDA #$33 ; RTS`
        // in I-RAM and calls it. (CIWP at reset write-protects I-RAM from the
        // SA-1 side, so the program first writes `$FF` to SIWP-equivalent
        // `CIWP` ($222A).)
        let sa1: &[u8] = &[
            0xA9, 0xFF, 0x8D, 0x2A, 0x22, // LDA #$FF ; STA $222A (CIWP)
            0xA9, 0xA9, 0x8D, 0x00, 0x30, // LDA #$A9 ; STA $3000
            0xA9, 0x33, 0x8D, 0x01, 0x30, // LDA #$33 ; STA $3001
            0xA9, 0x60, 0x8D, 0x02, 0x30, // LDA #$60 ; STA $3002
            0x20, 0x00, 0x30, // JSR $3000
            0x80, 0xFE, // BRA *
        ];
        rom[0x100..0x100 + sa1.len()].copy_from_slice(sa1);
        let log = record(&rom);
        let ram: usize = log.ram_exec.iter().filter(|&&b| b != 0).count();
        assert_eq!(ram, 3, "the three bytes of the I-RAM routine");
        assert_ne!(log.ram_exec[0] & OPCODE, 0);
        assert_eq!(log.other_exec, 0);
        let atlas = crate::rebuild::build_sa1_atlas_from_image(&rom, &log).expect("builds");
        let s = run(&rom, |s| {
            s.sa1_obs.rebuild = Some(Box::new(RebuildExec::new(atlas.clone())));
        });
        let st = s.sa1_obs.rebuild.as_deref().unwrap().stats();
        assert!(st.fallback_ram >= 2, "{st:?}");
        assert_eq!(
            st.fallback_no_node + st.fallback_width + st.fallback_other,
            0
        );
    }

    /// The SA-1 takes an NMI from the S-CPU through its own vector (`$2205`):
    /// the recorder notes the handler as an NMI entry, the executor counts
    /// the entry and checks the handler's `RTI` landing.
    #[test]
    fn sa1_interrupt_entries_are_recorded_and_counted() {
        let mut rom = image();
        rom[0..2].copy_from_slice(&[0x80, 0xFE]); // S-CPU: BRA *
        let sa1: &[u8] = &[
            0xA9, 0xFF, 0x8D, 0x2A, 0x22, // CIWP: open I-RAM (the RTI pulls from it)
            0x80, 0xFE, // BRA *
        ];
        rom[0x100..0x100 + sa1.len()].copy_from_slice(sa1);
        rom[0x1000..0x1002].copy_from_slice(&[0xEA, 0x40]); // handler at $9000: NOP ; RTI
        let drive = |s: &mut SnesSystem| {
            s.bus.write(0x00_2203, 0x00);
            s.bus.write(0x00_2204, 0x81); // reset vector $8100
            s.bus.write(0x00_2205, 0x00);
            s.bus.write(0x00_2206, 0x90); // NMI vector $9000
            s.bus.write(0x00_2200, 0x00); // release Reset
            for _ in 0..20 {
                s.step().expect("steps");
            }
            s.bus.write(0x00_220A, 0x10); // CIE: enable NMI-from-SNES
            s.bus.write(0x00_2200, 0x10); // CCNT: raise it
            for _ in 0..20 {
                s.step().expect("steps");
            }
        };
        let mut s = SnesSystem::load(&rom).expect("loads");
        let board = s.bus.sa1.as_ref().unwrap().board;
        s.sa1_obs.coverage = Some(Sa1Obs::fresh_log(s.bus.rom.len(), board));
        drive(&mut s);
        let log = *s.sa1_obs.coverage.take().unwrap();
        let nmi = log
            .interrupts
            .iter()
            .find(|e| e.kind == InterruptKind::Nmi)
            .expect("NMI entry");
        assert_eq!((nmi.target.cpu, nmi.target.rom), (0x9000, Some(0x1000)));
        assert!(log.indirect.iter().any(|e| e.kind == IndirectKind::Rti));

        let atlas = crate::rebuild::build_sa1_atlas_from_image(&rom, &log).expect("builds");
        let mut s = SnesSystem::load(&rom).expect("loads");
        s.sa1_obs.rebuild = Some(Box::new(RebuildExec::new(atlas)));
        drive(&mut s);
        let st = s.sa1_obs.rebuild.as_deref().unwrap().stats();
        assert_eq!(st.interrupt_entries, 1, "{st:?}");
        assert!(st.indirect_checked >= 1, "{st:?}");
        assert_eq!((st.fallback, st.dispatch_misses), (0, 0), "{st:?}");
    }
}
