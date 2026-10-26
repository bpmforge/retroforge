//! The SNES half of the block executor (ticket W36-08b;
//! `docs/design/REBUILD_LAB.md` section 4 and the W36-08b section).
//!
//! The executor is the interpreter with a map. Around [`SnesSystem::step`]
//! this module asks `rf_rebuild::RebuildExec` where the next instruction
//! sits in the atlas, **keyed by `(ROM offset, mode)`** where the mode is the
//! 65816 width state in force (`E`, `M`, `X`; see
//! [`crate::coverage::mode_index`]), and then the core runs its own unchanged
//! `Cpu::step`. Nothing here executes an instruction, touches a flag or a bus
//! cycle: peeks go through [`crate::bus::SnesBus`]'s `&self` accessors, so an
//! installed atlas cannot change the machine, which the bit-identity tests
//! (`tests/rebuild_executor.rs`) and the SMW lockstep prove.
//!
//! What it records, per instruction:
//!
//! - an atlas hit (observed or inferred), or a fallback with its reason: no
//!   node for the offset in any mode, **a node only in other widths** (width
//!   mismatch, counted apart), work-RAM code, or non-ROM, non-RAM code
//!   (coprocessor memory, SRAM). Nothing is done about a fallback: the
//!   interpreter was running anyway.
//! - for the indirect transfers (`JMP (a)`/`(a,X)`/`[a]`, `JML [a]`,
//!   `JSR (a,X)`, `RTS`, `RTL`, `RTI`, `BRK`, `COP`), where control landed,
//!   compared with the targets the recording saw; a miss is counted and
//!   logged, never acted on.
//! - with function hooks installed, call/return frames (below).
//!
//! ## Hook frames on the 65816
//!
//! A frame remembers the full 24-bit address its return should reach.
//! `JSR abs` and `JSR (abs,X)` return to `PBR:pc+3` (`RTS` pulls the 16-bit
//! address and adds one; the bank does not change), `JSL` to `PBR:pc+4`
//! (`RTL` pulls PC then PBR). `BRK`/`COP` return to `pc+2` (the signature
//! byte is skipped), and a hardware NMI/IRQ to the interrupted `pc`. In
//! native mode the interrupt sequence pushes PBR and `RTI` restores it, so
//! the return lands in the interrupted bank; in emulation mode the sequence
//! pushes no PBR, clears it, and `RTI` does not restore it, so the return
//! lands in bank 0 (fullsnes, "SNES CPU 65XX Microprocessor", interrupt
//! table; `Cpu::interrupt` and the `BRK`/`RTI` arms of `cpu/ops.rs` do
//! exactly this). Events are queued in the executor and delivered by
//! [`crate::core::SnesCore`], which owns the sink.
//!
//! Executor state is derived from `pc` and the atlas: never saved, never
//! read by the simulation. The shadow stack belongs to the machine that was
//! running, so a savestate load must call
//! [`crate::core::SnesCore::reset_hook_frames`].

use rf_core_api::CodeAddr;
use rf_rebuild::{Dispatch, Fetch, RebuildExec};

use crate::coverage::mode_index;
use crate::cpu::CpuBus;
use crate::mapping::Target;
use crate::system::SnesSystem;

/// What the step about to run does to the hooked call stack.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookStep {
    None,
    /// `JSR`, `JSL`, `BRK` or `COP`; `ret` is the 24-bit address its return
    /// lands on.
    Enter {
        ret: u32,
    },
    /// `RTS`, `RTL` or `RTI`.
    Return,
}

/// What the executor found before a step.
#[derive(Clone, Copy)]
pub(crate) struct RebuildBefore {
    /// An indirect transfer to compare with the atlas's recorded targets.
    indirect: Option<(u32, CodeAddr)>,
    hook: HookStep,
}

impl SnesSystem {
    /// Pre-step half: where is the next instruction in the atlas? `None`
    /// when Rebuild is off or this step fetches nothing (the CPU is halted
    /// in `WAI`/`STP`).
    #[inline(never)]
    pub(crate) fn rebuild_before(&mut self) -> Option<RebuildBefore> {
        let x = self.rebuild.as_deref_mut()?;
        let cpu = &self.cpu;
        if cpu.stopped {
            return None;
        }
        let (pbr, pc) = (cpu.pbr, cpu.pc);
        let mode = mode_index(cpu.e, cpu.p);
        let addr = cpu.pc24();
        let bus = &self.bus;
        let fetch = match bus.target(addr) {
            // Not while the Super FX holds the ROM bus: the CPU would read
            // the vector table or open bus, not `rom[o]` (W37-02).
            Target::Rom(o) if !bus.rom_bus_held_by_gsu() => {
                u32::try_from(o).map_or(Fetch::Other, Fetch::Rom)
            }
            Target::Wram(_) => Fetch::Ram,
            _ => Fetch::Other,
        };
        let rom = match fetch {
            Fetch::Rom(o) => Some(o),
            _ => None,
        };
        let indirect = match x.dispatch_at(pbr, pc, fetch, mode) {
            Dispatch::Atlas {
                node,
                indirect: true,
            } => Some((
                node,
                CodeAddr {
                    cpu: pc,
                    pbr,
                    mode,
                    rom,
                },
            )),
            _ => None,
        };
        // Peeked, not taken from the atlas node: a call from code the atlas
        // has no node for (work-RAM code) still pushes a frame.
        let hook = if x.hooks_active() {
            let bank = u32::from(pbr) << 16;
            let next = |n: u16| bank | u32::from(pc.wrapping_add(n));
            match bus.peek(addr) {
                0x20 | 0xFC => HookStep::Enter { ret: next(3) },
                0x22 => HookStep::Enter { ret: next(4) },
                // The software interrupts skip their signature byte; in
                // emulation mode the return lands in bank 0 (module doc).
                0x00 | 0x02 => HookStep::Enter {
                    ret: if cpu.e {
                        u32::from(pc.wrapping_add(2))
                    } else {
                        next(2)
                    },
                },
                0x60 | 0x6B | 0x40 => HookStep::Return,
                _ => HookStep::None,
            }
        } else {
            HookStep::None
        };
        Some(RebuildBefore { indirect, hook })
    }

    /// Post-step half, run before any interrupt is dispatched (where the
    /// recorder's post-step half runs): compare an indirect landing with the
    /// recording and tell the hook stack where the call or return went.
    #[inline(never)]
    pub(crate) fn rebuild_after(&mut self, before: RebuildBefore) {
        let (pbr, pc, mode) = (
            self.cpu.pbr,
            self.cpu.pc,
            mode_index(self.cpu.e, self.cpu.p),
        );
        let addr = self.cpu.pc24();
        let rom = self
            .bus
            .rom_offset(addr)
            .and_then(|o| u32::try_from(o).ok());
        let Some(x) = self.rebuild.as_deref_mut() else {
            return;
        };
        if let Some((node, site)) = before.indirect {
            x.check_target(
                node,
                site,
                CodeAddr {
                    cpu: pc,
                    pbr,
                    mode,
                    rom,
                },
            );
        }
        match before.hook {
            HookStep::Enter { ret } => x.landed_at(rom, mode, ret),
            HookStep::Return => x.returned(addr),
            HookStep::None => {}
        }
    }

    /// What an interrupt dispatch is about to preempt: `(pbr, pc, mode, e)`,
    /// `None` when Rebuild is off. Read before the dispatch.
    #[inline]
    pub(crate) fn rebuild_resume_point(&self) -> Option<(u8, u16, u8, bool)> {
        self.rebuild.as_ref()?;
        let c = &self.cpu;
        Some((c.pbr, c.pc, mode_index(c.e, c.p), c.e))
    }

    /// An NMI/IRQ sequence just ran (the CPU is at the handler); `resume` is
    /// what [`Self::rebuild_resume_point`] read before it.
    #[inline(never)]
    pub(crate) fn rebuild_note_interrupt(&mut self, resume: Option<(u8, u16, u8, bool)>) {
        let Some((pbr, pc, mode, e)) = resume else {
            return;
        };
        let (now_mode, addr) = (mode_index(self.cpu.e, self.cpu.p), self.cpu.pc24());
        let rom = self
            .bus
            .rom_offset(addr)
            .and_then(|o| u32::try_from(o).ok());
        let Some(x) = self.rebuild.as_deref_mut() else {
            return;
        };
        x.note_interrupt_at(pbr, pc, mode);
        if x.hooks_active() {
            // Emulation mode pushes no PBR: the return lands in bank 0.
            let bank = if e { 0 } else { u32::from(pbr) };
            x.landed_at(rom, now_mode, (bank << 16) | u32::from(pc));
        }
    }

    /// Whether any function is hooked (the core skips event delivery when
    /// not).
    #[inline]
    pub(crate) fn hooks_active(&self) -> bool {
        self.rebuild
            .as_deref()
            .is_some_and(RebuildExec::hooks_active)
    }
}
