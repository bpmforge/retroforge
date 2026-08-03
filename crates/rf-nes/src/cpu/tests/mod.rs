//! CPU test tree. `vectors` is the ticket's acceptance-criterion-1 harness
//! (SingleStepTests `nes6502`, vector-availability documented there);
//! `opcode_table` is a vector-independent static cross-check that always
//! runs (no external data needed); `interrupts` is ticket W1-01b's
//! acceptance-criterion-2 evidence — hand-derived from nesdev.org/wiki/
//! CPU_interrupts, since the nes6502 vectors don't cover interrupts at
//! all (see that module's doc).
mod interrupts;
mod json;
mod opcode_table;
mod vectors;
