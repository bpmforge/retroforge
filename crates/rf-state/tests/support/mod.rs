//! Shared fixture-building helpers for rf-state's integration tests.
//! Every field here is a fixed constant — no `SystemTime::now()` — so
//! `golden_container()` builds byte-identical containers across runs and
//! across machines (required for the golden-fixture test in
//! `golden_fixture.rs` to mean anything).

use rf_state::{Container, ContainerError};

pub const GOLDEN_CONSOLE: u8 = 0; // NES
pub const GOLDEN_ROM_SHA256: [u8; 32] = [0xAB; 32];
pub const GOLDEN_EMU_VERSION: &str = "0.1.0";
pub const GOLDEN_TIMESTAMP: u64 = 1_700_000_000;

/// The full core (required) chunk set plus one optional `INPT` chunk, all
/// at version 1, with small deterministic payloads. Includes every
/// required tag so the result is a structurally valid, loadable state.
pub fn golden_container() -> Result<Container, ContainerError> {
    let mut c = Container::new(
        GOLDEN_CONSOLE,
        GOLDEN_ROM_SHA256,
        GOLDEN_EMU_VERSION,
        GOLDEN_TIMESTAMP,
    );
    for (tag, seed) in [
        (*b"CPU_", 1u8),
        (*b"PPU_", 2),
        (*b"APU_", 3),
        (*b"WRAM", 4),
        (*b"VRAM", 5),
        (*b"OAM_", 6),
        (*b"CGRM", 7),
        (*b"MAPR", 8),
        (*b"CART", 9),
    ] {
        c.add_chunk(tag, 1, vec![seed; 16])?;
    }
    c.add_chunk(*b"INPT", 1, vec![0xAA, 0xBB])?;
    Ok(c)
}
