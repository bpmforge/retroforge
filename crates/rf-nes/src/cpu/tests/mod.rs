//! CPU test tree. `vectors` is the ticket's acceptance-criterion-1 harness
//! (SingleStepTests `nes6502`, vector-availability documented there);
//! `opcode_table` is a vector-independent static cross-check that always
//! runs (no external data needed).
mod json;
mod opcode_table;
mod vectors;
