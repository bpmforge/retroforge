//! Memory-viewer pure-data layer (FR-DBG-002, DEBUGGER.md §3 "Memory hex"
//! row) — ticket W4-06b criterion 4: "memory viewer panel over the live
//! core, read-only and non-perturbing".
//!
//! Same "no `egui`, no `rf-nes`" posture as every other viewer module in
//! this crate ([`crate`]'s own module doc): this file only reshapes
//! already-read bytes into fixed-width rows. The actual read happens in
//! `crates/retroforge` — the ticket brief's working seam is
//! `EmuStepper::peek` (already side-effect-free, per that method's own
//! doc) plus `EmuStepper::prg_ram` (forwards `NesBus::prg_ram`, also
//! already side-effect-free) — never routed through `StateView` (W4-06a's
//! finding: no production implementer exists).
//!
//! ## Two ranges, not one
//!
//! A memory viewer that only covered WRAM (`$0000-$07FF`) would miss most
//! of what commercial-game `memory_map` annotations actually point at:
//! `profiles/nes/rf-scroller-demo/profile.toml`'s own `player_x`/
//! `camera_x` live at `$6029`/`$602B` — cartridge PRG-RAM, not WRAM.
//! `crates/retroforge::stepper::EmuStepper` exposes both live, read-only
//! ranges; [`build_rows`] below is range-agnostic (it just chunks whatever
//! `bytes` it's given starting at `base_addr`), so the panel calls it once
//! per range.

/// Bytes per row (the conventional hex-dump width).
pub const ROW_WIDTH: usize = 16;

/// One fixed-width row of already-read memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRow {
    /// Address of `bytes[0]`, in whatever address space `bytes` came from
    /// (a WRAM address or a PRG-RAM address — this type carries no
    /// space tag of its own; the caller already knows which range it
    /// asked for).
    pub addr: u32,
    /// `ROW_WIDTH` bytes, except possibly the last row of a range whose
    /// length isn't a multiple of `ROW_WIDTH`.
    pub bytes: Vec<u8>,
}

/// Chunk `bytes` (already read contiguously starting at `base_addr`) into
/// [`ROW_WIDTH`]-wide [`MemoryRow`]s. Pure reshaping — no address
/// translation, no I/O; `base_addr` is wherever the caller's live read
/// already started (module doc).
#[must_use]
pub fn build_rows(base_addr: u32, bytes: &[u8]) -> Vec<MemoryRow> {
    bytes
        .chunks(ROW_WIDTH)
        .enumerate()
        .map(|(i, chunk)| MemoryRow {
            addr: base_addr + (i * ROW_WIDTH) as u32,
            bytes: chunk.to_vec(),
        })
        .collect()
}

/// Look up the current byte value at `addr` within rows built by
/// [`build_rows`] over a range that covers it — the panel's "what's at
/// this address right now" query, and what the viewer's own tests use to
/// assert exact values at exact addresses (ticket vacuity trap (c)).
/// `None` if `addr` falls outside every row (before `base_addr`, past the
/// last full/partial row, or in a gap [`build_rows`] was never asked to
/// cover).
#[must_use]
pub fn byte_at(rows: &[MemoryRow], addr: u32) -> Option<u8> {
    for row in rows {
        let row_len = row.bytes.len() as u32;
        if addr >= row.addr && addr < row.addr + row_len {
            let offset = (addr - row.addr) as usize;
            return Some(row.bytes[offset]);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_rows_chunks_into_fixed_width_rows_with_correct_addresses() {
        let bytes: Vec<u8> = (0u8..48).collect(); // 3 full rows of 16
        let rows = build_rows(0x6000, &bytes);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].addr, 0x6000);
        assert_eq!(rows[1].addr, 0x6010);
        assert_eq!(rows[2].addr, 0x6020);
        assert_eq!(rows[0].bytes, (0u8..16).collect::<Vec<u8>>());
        assert_eq!(rows[1].bytes, (16u8..32).collect::<Vec<u8>>());
        assert_eq!(rows[2].bytes, (32u8..48).collect::<Vec<u8>>());
    }

    #[test]
    fn build_rows_handles_a_short_final_row() {
        let bytes: Vec<u8> = (0u8..20).collect(); // one full row + 4 bytes
        let rows = build_rows(0x0000, &bytes);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].addr, 0x0010);
        assert_eq!(rows[1].bytes, vec![16, 17, 18, 19]);
    }

    #[test]
    fn build_rows_on_empty_bytes_is_empty() {
        assert!(build_rows(0x1234, &[]).is_empty());
    }

    /// Mutation-catching (ticket's own "off-by-one the viewer's
    /// address→byte indexing" class): every byte in the fixture is
    /// distinct from its neighbors, so a row/column index swap or an
    /// off-by-one in `byte_at`'s offset arithmetic changes which value
    /// comes back, not just whether one comes back at all.
    #[test]
    fn byte_at_finds_the_exact_value_at_an_exact_address_across_rows() {
        let bytes: Vec<u8> = (0u8..40).collect();
        let rows = build_rows(0x0300, &bytes);
        assert_eq!(byte_at(&rows, 0x0300), Some(0));
        assert_eq!(byte_at(&rows, 0x0301), Some(1));
        assert_eq!(byte_at(&rows, 0x030F), Some(15)); // last byte of row 0
        assert_eq!(byte_at(&rows, 0x0310), Some(16)); // first byte of row 1
        assert_eq!(byte_at(&rows, 0x0327), Some(39)); // last byte overall
    }

    #[test]
    fn byte_at_returns_none_outside_every_row() {
        let bytes: Vec<u8> = (0u8..16).collect();
        let rows = build_rows(0x0300, &bytes);
        assert_eq!(byte_at(&rows, 0x02FF), None); // just before
        assert_eq!(byte_at(&rows, 0x0310), None); // just past the one row
    }

    #[test]
    fn byte_at_returns_none_in_a_gap_between_two_disjoint_ranges() {
        // Simulates the real WRAM/PRG-RAM split: two `build_rows` calls
        // over disjoint ranges, concatenated, with a large gap between.
        let wram = build_rows(0x0000, &[0xAAu8; 16]);
        let prg_ram = build_rows(0x6000, &[0xBBu8; 16]);
        let all: Vec<MemoryRow> = wram.into_iter().chain(prg_ram).collect();
        assert_eq!(byte_at(&all, 0x0005), Some(0xAA));
        assert_eq!(byte_at(&all, 0x6005), Some(0xBB));
        assert_eq!(byte_at(&all, 0x1000), None); // in the gap
    }
}
