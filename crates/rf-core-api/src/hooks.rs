//! Function hooks: the plain types that cross the push boundary when a core
//! running with a code atlas enters or leaves a hooked function (ticket
//! W36-05; `docs/design/REBUILD_LAB.md` sections 5-7).
//!
//! The core calls [`crate::CoreSink::function_event`] synchronously on the
//! core thread, at an instruction boundary, with a [`HookCtx`] that lends the
//! machine for exactly that call. Nothing here names a console, `rf-rebuild`
//! or a scripting language: a hook host (Lua, Rust closures) lives on the
//! sink side and is never required to be `Send`.
//!
//! **Mod tier.** [`HookCtx::poke`] changes emulated memory. It is the only
//! mutating door a hook gets, and it belongs to the mod boundary of
//! NON_GOALS 13: the caller of `poke` is responsible for checking the
//! `write_memory` capability and recording the write in the ledger before it
//! asks the core to apply it.

use crate::cpu_regs::CpuRegs;

/// A hooked function was entered or left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FunctionEvent {
    /// Control reached the entry of `function` through a call (`JSR`/`BRK`) or
    /// an interrupt, before its first instruction runs. `function` indexes
    /// the installed atlas's function list.
    Enter { function: u32 },
    /// Control returned to the point a hooked frame expected (the
    /// instruction after the `JSR`, or the interrupted instruction), after
    /// the return instruction ran. Frames popped by one return are reported
    /// innermost first.
    Exit { function: u32 },
}

/// The machine, lent to a hook for the duration of one event.
pub trait HookCtx {
    /// CPU registers as they are now (at an instruction boundary).
    fn regs(&self) -> CpuRegs;

    /// One byte at a CPU-bus address without perturbing the machine
    /// (same contract as [`crate::EmulatorCore::peek`]).
    fn peek(&self, addr: u32) -> u8;

    /// Write `value` at `addr`, applied by the core through its bus at this
    /// instruction boundary with no clock cost. Returns whether the core
    /// applied it (a core accepts work RAM and cartridge RAM; anything else
    /// is refused and changes nothing).
    ///
    /// Mod-tier: see the module doc. The core does not check capabilities;
    /// the hook host does, and ledgers the write.
    fn poke(&mut self, addr: u32, value: u8) -> bool;
}
