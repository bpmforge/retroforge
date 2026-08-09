//! Ticket W4-06b criterion 4 (FR-DBG-002): "memory viewer panel over the
//! live core, read-only and non-perturbing" — the vacuity-trap-required
//! proof (ticket brief (c)): "a memory-viewer test asserting 'reads some
//! bytes' is worthless. Write known values via `EmuStepper::poke_bus` and
//! assert the viewer surfaces exactly those at exactly those addresses."
//!
//! This test never touches `crate::debug_dock`'s egui panel (that half is
//! the ticket's accepted unverifiable UI wiring) — it drives the same two
//! functions that panel calls: `EmuStepper::wram_snapshot`/`prg_ram` (the
//! live read) and `rf_debugger::memory_view::build_rows`/`byte_at` (the
//! pure-data reshaping), exactly as `crate::debug_dock::memory_ui` does.

use retroforge::stepper::EmuStepper;

/// Same minimal, deterministic fixture `crate::stepper`'s own test module
/// uses (`stepper.rs`'s private `synthetic_nrom` helper is not reachable
/// from an integration test, so this is a small, deliberate duplicate):
/// mapper 0, all-zero PRG/CHR, so the reset/IRQ vectors resolve to `$0000`
/// (zeroed RAM, opcode `$00` = `BRK`) — a deterministic infinite loop that
/// never needs real game code to drive `EmuStepper::from_ines_bytes`.
fn synthetic_nrom() -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&rf_cart::nes::INES_MAGIC);
    data.push(1); // 1x16KiB PRG
    data.push(1); // 1x8KiB CHR
    data.extend_from_slice(&[0u8; 10]); // flags6/7 + 8 reserved => mapper 0, iNES 1.0
    data.extend(vec![0u8; 16 * 1024]);
    data.extend(vec![0u8; 8 * 1024]);
    data
}

fn stepper() -> EmuStepper {
    EmuStepper::from_ines_bytes(&synthetic_nrom()).expect("valid synthetic NROM image")
}

/// **The vacuity-trap-required proof.** Two known values, poked into TWO
/// disjoint live ranges (WRAM and PRG-RAM — advisor-flagged: a viewer that
/// only covered WRAM would miss every address this project's own example
/// profile annotates, `profiles/nes/rf-scroller-demo/profile.toml`'s
/// `player_x`/`camera_x` at `$6029`/`$602B`), must come back at EXACTLY
/// those addresses through the exact pipeline the Memory panel uses.
/// Mutation: off-by-one either `EmuStepper::wram_snapshot`'s loop index or
/// `rf_debugger::memory_view::byte_at`'s offset arithmetic, and this
/// fails.
#[test]
fn poked_bytes_surface_at_exactly_their_addresses_in_both_live_ranges() {
    let mut s = stepper();
    s.poke_bus(0x0042, 0xAB);
    s.poke_bus(0x0000, 0x11); // first byte of WRAM
    s.poke_bus(0x07FF, 0x22); // last byte of WRAM
    s.poke_bus(0x6029, 0xCD); // rf-scroller-demo's own player_x address
    s.poke_bus(0x602B, 0xEF); // rf-scroller-demo's own camera_x address
    s.poke_bus(0x7FFF, 0x33); // last byte of PRG-RAM

    let wram = s.wram_snapshot();
    let prg_ram = s.prg_ram();

    let wram_rows = rf_debugger::memory_view::build_rows(0x0000, &wram);
    let prg_rows = rf_debugger::memory_view::build_rows(0x6000, prg_ram);

    assert_eq!(
        rf_debugger::memory_view::byte_at(&wram_rows, 0x0042),
        Some(0xAB)
    );
    assert_eq!(
        rf_debugger::memory_view::byte_at(&wram_rows, 0x0000),
        Some(0x11)
    );
    assert_eq!(
        rf_debugger::memory_view::byte_at(&wram_rows, 0x07FF),
        Some(0x22)
    );
    assert_eq!(
        rf_debugger::memory_view::byte_at(&prg_rows, 0x6029),
        Some(0xCD)
    );
    assert_eq!(
        rf_debugger::memory_view::byte_at(&prg_rows, 0x602B),
        Some(0xEF)
    );
    assert_eq!(
        rf_debugger::memory_view::byte_at(&prg_rows, 0x7FFF),
        Some(0x33)
    );

    // Negative check: the WRAM poke at $0042 must not leak into PRG-RAM at
    // the analogous offset $6042 (a legitimate, never-poked PRG-RAM
    // address — still `Some(0)`, not `Some(0xAB)`) — proves the two
    // ranges are distinct backing storage, not aliased.
    assert_eq!(
        rf_debugger::memory_view::byte_at(&prg_rows, 0x6042),
        Some(0)
    );
    // And $6042 (a PRG-RAM address) must not resolve against the WRAM
    // rows at all — it's outside WRAM's $0000-$07FF range entirely.
    assert_eq!(rf_debugger::memory_view::byte_at(&wram_rows, 0x6042), None);
}

/// "Non-perturbing" is a real, checkable claim here, not just an unenforced
/// adjective: reading both live ranges (the exact operation the Memory
/// panel performs every repaint) must not change anything the
/// determinism suite considers observable state. Mutation: swap
/// `wram_snapshot`'s `Self::peek` calls for a bus method that ticks a
/// counter (mirroring the real `mem_read`/`chr_peek` hazard the ticket
/// brief cites), and this fails.
#[test]
fn reading_both_live_ranges_repeatedly_does_not_perturb_reachable_state() {
    let mut s = stepper();
    s.poke_bus(0x6029, 0x42);
    let before = s.state_hash();
    for _ in 0..50 {
        let _ = s.wram_snapshot();
        let _ = s.prg_ram();
    }
    assert_eq!(
        s.state_hash(),
        before,
        "repeatedly reading WRAM/PRG-RAM for the memory viewer must not perturb reachable state"
    );
}
