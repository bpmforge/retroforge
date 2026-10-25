//! The SNES half of the code-coverage recorder (ticket W36-08;
//! `docs/design/REBUILD_LAB.md` §5, shared shape in `rf_core_api::coverage`).
//!
//! ## What is recorded, and from where
//!
//! Every instruction the **main CPU (5A22/65816)** fetches, keyed by program
//! bank, PC and the width state in force: M, X and E decide how many bytes an
//! immediate takes, so the state is part of the instruction's identity (see
//! [`mode_index`]). The recorder runs in [`SnesSystem::step`] around
//! `Cpu::step` and at the interrupt dispatch, because that is where the facts
//! are: an indirect target is the PC right after the instruction (before any
//! interrupt moves it) and the handler a NMI/IRQ reached is known only once
//! the dispatch ran.
//!
//! The SA-1 and Super FX are separate CPUs with their own steppers; their
//! code is never recorded. What *is* counted is the main CPU fetching from
//! memory a coprocessor owns ([`CoverageLog::coproc_exec`]), since such code
//! cannot be in the ROM atlas.
//!
//! ## Pay for use, observation only
//!
//! One `Option::is_some` per instruction when off. Armed, everything reads
//! through [`SnesBus::peek`] / [`SnesBus::target`] (`&self`), so no register
//! read fires and the machine cannot tell it is watched; the armed-versus-
//! unarmed test hashes the whole machine.

use rf_core_api::coverage::{mode_bit, OPCODE, OPERAND};
use rf_core_api::{
    CodeAddr, CoverageLog, IndirectEdge, IndirectKind, InterruptEntry, InterruptKind,
};

use crate::cpu::{flags, CpuBus};
use crate::mapping::{Target, WRAM_LEN};
use crate::system::SnesSystem;
use crate::trace::opcode_info;

/// The decode-mode index of a CPU state: 0 for emulation mode (`E = 1`, where
/// `M` and `X` are forced), else `1 + m8 + 2 * x8` for native mode
/// (`m8`/`x8` set when the accumulator/index registers are 8 bits wide).
/// These are the five states a 65816 can be in (fullsnes, "SNES CPU 65XX
/// Microprocessor": `E` forces `M` and `X`). Shared with
/// `rf_core_api::coverage` and `rf-rebuild`, which index code by it.
#[must_use]
pub const fn mode_index(e: bool, p: u8) -> u8 {
    if e {
        0
    } else {
        1 + (p & flags::M != 0) as u8 + 2 * (p & flags::X != 0) as u8
    }
}

/// What the pre-step half learned about the instruction about to run.
#[derive(Clone, Copy)]
pub(crate) struct CoverageBefore {
    opcode: u8,
    site: CodeAddr,
}

impl SnesSystem {
    /// Arm or disarm the recorder; arming starts a fresh log.
    pub(crate) fn set_coverage(&mut self, on: bool) {
        self.coverage = on.then(|| self.fresh_log());
    }

    fn fresh_log(&self) -> Box<CoverageLog> {
        Box::new(CoverageLog::with_volatile_len(self.bus.rom.len(), WRAM_LEN))
    }

    /// Hand over the log so far and keep recording into a fresh one.
    pub(crate) fn take_coverage(&mut self) -> Option<CoverageLog> {
        let fresh = self.coverage.is_some().then(|| self.fresh_log())?;
        self.coverage.replace(fresh).map(|b| *b)
    }

    /// A [`CodeAddr`] for `pbr:pc` in the width state `mode`.
    fn code_addr(&self, pbr: u8, pc: u16, mode: u8) -> CodeAddr {
        let addr = (u32::from(pbr) << 16) | u32::from(pc);
        CodeAddr {
            cpu: pc,
            pbr,
            mode,
            rom: self
                .bus
                .rom_offset(addr)
                .and_then(|o| u32::try_from(o).ok()),
        }
    }

    /// Pre-step half: mark the instruction's bytes and remember what the
    /// post-step half needs. `None` when this step fetches nothing (the CPU
    /// is halted in `WAI`/`STP`).
    #[inline(never)]
    pub(crate) fn coverage_before(&mut self) -> Option<CoverageBefore> {
        if self.cpu.stopped {
            return None;
        }
        let cpu = &self.cpu;
        let (pbr, pc) = (cpu.pbr, cpu.pc);
        let mode = mode_index(cpu.e, cpu.p);
        let opcode = self.bus.peek(cpu.pc24());
        let len = opcode_info(opcode).1.operand_len(cpu.a8(), cpu.i8()) + 1;
        let site = self.code_addr(pbr, pc, mode);
        let bus = &self.bus;
        let log = self.coverage.as_deref_mut()?;
        log.instructions_recorded += 1;
        for i in 0..len {
            // The fetch address wraps inside the program bank.
            let addr = (u32::from(pbr) << 16) | u32::from(pc.wrapping_add(i as u16));
            let flag = if i == 0 {
                OPCODE | mode_bit(mode)
            } else {
                OPERAND
            };
            match bus.target(addr) {
                Target::Rom(o) => log.mark_prg(o, flag),
                Target::Wram(o) => log.mark_ram(o as u32, flag),
                other => {
                    if i == 0 {
                        if owned_by_coprocessor(other) {
                            log.coproc_exec += 1;
                        } else {
                            log.other_exec += 1;
                        }
                    }
                }
            }
        }
        Some(CoverageBefore { opcode, site })
    }

    /// Post-step half: the indirect edge the instruction just completed.
    #[inline(never)]
    pub(crate) fn coverage_after(&mut self, before: CoverageBefore) {
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
        let target = self.code_addr(
            self.cpu.pbr,
            self.cpu.pc,
            mode_index(self.cpu.e, self.cpu.p),
        );
        if let Some(log) = self.coverage.as_deref_mut() {
            log.note_indirect(IndirectEdge {
                site: before.site,
                target,
                kind,
            });
        }
    }

    /// An NMI/IRQ was just dispatched: note where it landed.
    #[inline(never)]
    pub(crate) fn coverage_note_interrupt(&mut self, kind: InterruptKind) {
        self.coverage_note(kind);
    }

    /// The CPU was just reset: note the reset vector's target.
    pub(crate) fn coverage_note_reset(&mut self) {
        self.coverage_note(InterruptKind::Reset);
    }

    fn coverage_note(&mut self, kind: InterruptKind) {
        if self.coverage.is_none() {
            return;
        }
        let target = self.code_addr(
            self.cpu.pbr,
            self.cpu.pc,
            mode_index(self.cpu.e, self.cpu.p),
        );
        if let Some(log) = self.coverage.as_deref_mut() {
            log.note_interrupt(InterruptEntry { kind, target });
        }
    }
}

/// Memory a coprocessor owns: SA-1 I-RAM/BW-RAM/registers, Super FX RAM and
/// registers, Cx4, ST010 and OBC1 RAM, the DSP-1 ports.
fn owned_by_coprocessor(t: Target) -> bool {
    matches!(
        t,
        Target::Sa1IRam(_)
            | Target::Sa1BwRam(_)
            | Target::Sa1Register(_)
            | Target::Sa1Bitmap(_)
            | Target::GsuRam(_)
            | Target::GsuRegister(_)
            | Target::Cx4Ram(_)
            | Target::Cx4Register(_)
            | Target::Obc1Bits(_)
            | Target::Obc1Register(_)
            | Target::St010Ram(_)
            | Target::St010Register(_)
            | Target::Dsp1Dr
            | Target::Dsp1Sr
    )
}
