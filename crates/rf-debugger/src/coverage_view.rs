//! Coverage panel data (ticket W36-01; `docs/design/REBUILD_LAB.md` §5-7):
//! turns a [`rf_core_api::CoverageLog`] into the numbers and the per-bank
//! strip the dock draws. Pure functions of the log, no egui and no core
//! access, so the shell can build one on the core thread (law 4: only that
//! thread touches core state) and ship the small result across.

use rf_core_api::coverage::{OPCODE, OPERAND};
use rf_core_api::CoverageLog;

/// PRG bank granularity of the strip. 16 KiB is the smallest bank every
/// NES mapper here uses; a 32 KiB-banked board simply shows two rows per
/// bank.
pub const STRIP_BANK_BYTES: usize = 16 * 1024;

/// Cells per bank in the strip (each covers `STRIP_BANK_BYTES / 64` = 256
/// bytes, one nesdev "page").
pub const STRIP_CELLS: usize = 64;

/// What a strip cell's bytes did, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellState {
    /// No byte in the cell was ever executed or fetched as an operand.
    Never,
    /// Some byte was an operand but none was an instruction's first byte
    /// (rare: a bank-switch edge, or a partial cell).
    OperandOnly,
    /// At least one byte was an executed opcode.
    Opcode,
}

/// One bank's strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BankStrip {
    pub cells: Vec<CellState>,
    pub opcode_bytes: usize,
    pub operand_bytes: usize,
}

/// The coverage panel's whole payload.
#[derive(Debug, Clone, PartialEq)]
pub struct CoverageSummary {
    pub prg_len: usize,
    pub executed_bytes: usize,
    pub opcode_bytes: usize,
    pub operand_bytes: usize,
    /// `executed_bytes / prg_len`, `0.0..=1.0`.
    pub fraction: f64,
    pub ram_executed_bytes: usize,
    pub indirect_edges: usize,
    pub interrupt_entries: usize,
    pub instructions: u64,
    pub banks: Vec<BankStrip>,
}

fn cell_state(bytes: &[u8]) -> CellState {
    let all = bytes.iter().fold(0u8, |acc, b| acc | b);
    if all & OPCODE != 0 {
        CellState::Opcode
    } else if all & OPERAND != 0 {
        CellState::OperandOnly
    } else {
        CellState::Never
    }
}

fn strip(bank: &[u8]) -> BankStrip {
    let cell_len = bank.len().div_ceil(STRIP_CELLS).max(1);
    BankStrip {
        cells: bank.chunks(cell_len).map(cell_state).collect(),
        opcode_bytes: bank.iter().filter(|&&b| b & OPCODE != 0).count(),
        operand_bytes: bank.iter().filter(|&&b| b & OPERAND != 0).count(),
    }
}

impl CoverageSummary {
    /// Summarise `log`. The last bank is shorter when PRG is not a
    /// multiple of [`STRIP_BANK_BYTES`].
    #[must_use]
    pub fn from_log(log: &CoverageLog) -> Self {
        Self {
            prg_len: log.prg_flags.len(),
            executed_bytes: log.executed_prg_bytes(),
            opcode_bytes: log.opcode_prg_bytes(),
            operand_bytes: log.prg_flags.iter().filter(|&&b| b & OPERAND != 0).count(),
            fraction: log.prg_coverage_fraction(),
            ram_executed_bytes: log.executed_ram_bytes(),
            indirect_edges: log.indirect.len(),
            interrupt_entries: log.interrupts.len(),
            instructions: log.instructions_recorded,
            banks: log.prg_flags.chunks(STRIP_BANK_BYTES).map(strip).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_counts_and_strip_cells() {
        let mut log = CoverageLog::new(2 * STRIP_BANK_BYTES);
        log.mark_prg(0, OPCODE);
        log.mark_prg(1, OPERAND);
        // Second bank, cell 3: operand only.
        log.mark_prg(STRIP_BANK_BYTES + 3 * 256 + 5, OPERAND);
        let s = CoverageSummary::from_log(&log);
        assert_eq!(s.prg_len, 2 * STRIP_BANK_BYTES);
        assert_eq!(
            (s.opcode_bytes, s.operand_bytes, s.executed_bytes),
            (1, 2, 3)
        );
        assert_eq!(s.banks.len(), 2);
        assert_eq!(s.banks[0].cells.len(), STRIP_CELLS);
        assert_eq!(s.banks[0].cells[0], CellState::Opcode);
        assert_eq!(s.banks[0].cells[1], CellState::Never);
        assert_eq!(s.banks[1].cells[3], CellState::OperandOnly);
        assert!(s.fraction > 0.0);
    }

    #[test]
    fn empty_and_short_last_bank_do_not_panic() {
        assert!(CoverageSummary::from_log(&CoverageLog::new(0))
            .banks
            .is_empty());
        let s = CoverageSummary::from_log(&CoverageLog::new(STRIP_BANK_BYTES + 100));
        assert_eq!(s.banks.len(), 2);
        assert!(s.banks[1].cells.len() <= STRIP_CELLS);
        assert!(!s.banks[1].cells.is_empty());
    }
}
