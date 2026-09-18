//! SA-1 determinism and mid-DMA save-state round trip (ticket W17-04
//! acceptance #2).
//!
//! `rf-snes` has no crate-local "determinism" or "mode-invariant" test
//! suite of its own (those names belong to `crates/retroforge/tests/
//! mode_invariant_*.rs`, out of this ticket's `write_scope`) — this file
//! is the SA-1-specific equivalent the ticket asks for: a synthetic SA-1
//! cart, hand-assembled rather than pulled from `tests/system.rs` (no
//! such fixture exists there; the closest prior art is `state.rs`'s
//! private `sa1_lorom_image` test helper, which this mirrors but extends
//! with real 65816 code for both CPUs so the interleave, the W17-04 cost
//! model and both CPUs' own memories are all actually exercised, not
//! just their register windows).

use rf_core_api::{StateError, StateReader, StateWriter};
use rf_snes::cpu::CpuBus;
use rf_snes::state::StateRegion;
use rf_snes::SnesSystem;

/// A minimal SA-1 LoROM image whose main CPU program hands the SA-1 CPU
/// its reset vector, unprotects I-RAM (CIWP `$222A`) and BW-RAM (SBWE
/// `$2226`, a shared gate — see `Sa1Regs::bwram_writable`'s doc), releases
/// SA-1 reset, then loops forever bumping a WRAM counter. The SA-1
/// program (placed at `$8100`, well clear of the 30-byte main program)
/// loops forever bumping an I-RAM counter and a BW-RAM counter — every
/// iteration of both loops fetches its own opcodes from ROM, so a run
/// exercises `Target::Rom`, `Target::Sa1IRam` and `Target::Sa1BwRam` on
/// both sides, which is what the W17-04 cost model needs to be worth
/// running.
///
/// Assembled by hand (verified with a throwaway assembler script, not
/// shipped) rather than depending on a disassembler/assembler crate this
/// project does not have — see `docs/design/EMULATION_CORES.md` §3.5 for
/// the citation trail on the registers it pokes.
fn sa1_test_image() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];

    // Main CPU program, ROM offset 0 (CPU $8000, the reset vector below).
    let main: &[u8] = &[
        0xA9, 0x00, // LDA #$00        (SA-1 CRV lo)
        0x8D, 0x03, 0x22, // STA $2203
        0xA9, 0x81, // LDA #$81        (SA-1 CRV hi -> vector $8100)
        0x8D, 0x04, 0x22, // STA $2204
        0xA9, 0xFF, // LDA #$FF
        0x8D, 0x2A, 0x22, // STA $222A  (CIWP: unprotect all I-RAM chunks, SA-1 side)
        0xA9, 0x80, // LDA #$80
        0x8D, 0x26, 0x22, // STA $2226  (SBWE: unprotect BW-RAM, both sides)
        0xA9, 0x00, // LDA #$00
        0x8D, 0x00, 0x22, // STA $2200  (release SA-1 reset)
        0xEE, 0x00, 0x00, // loop: INC $0000 (WRAM counter)
        0x80, 0xFB, // BRA loop
    ];
    rom[..main.len()].copy_from_slice(main);

    // SA-1 CPU program, ROM offset $100 (CPU $8100, via CRV above).
    let sa1: &[u8] = &[
        0xEE, 0x00, 0x30, // loop: INC $3000 (I-RAM)
        0xEE, 0x00, 0x60, // INC $6000       (BW-RAM)
        0x80, 0xF8, // BRA loop
    ];
    rom[0x100..0x100 + sa1.len()].copy_from_slice(sa1);

    let base = 0x7FC0;
    rom[base + 0x15] = 0x23; // map mode: LoROM + SA-1
    rom[base + 0x16] = 0x35; // chipset: ROM+SA-1+RAM+battery
    rom[base + 0x17] = 5; // ROM size: 2^5 KiB = 32 KiB, matching this vec exactly
    rom[base + 0x18] = 3; // RAM (BW-RAM) size: 2^3 KiB = 8 KiB
    let checksum: u16 = 0xBEEF;
    rom[base + 0x1C..base + 0x1E].copy_from_slice(&(checksum ^ 0xFFFF).to_le_bytes());
    rom[base + 0x1E..base + 0x20].copy_from_slice(&checksum.to_le_bytes());
    rom[base + 0x3C] = 0x00;
    rom[base + 0x3D] = 0x80; // reset vector $8000
    rom
}

struct MemStream {
    buf: Vec<u8>,
    at: usize,
}
impl StateWriter for MemStream {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), StateError> {
        self.buf.extend_from_slice(bytes);
        Ok(())
    }
}
impl StateReader for MemStream {
    fn read_exact(&mut self, out: &mut [u8]) -> Result<(), StateError> {
        let end = self.at + out.len();
        out.copy_from_slice(&self.buf[self.at..end]);
        self.at = end;
        Ok(())
    }
}

/// Concatenate every `StateRegion` in `StateRegion::ALL` order into one
/// buffer — a full-machine snapshot, byte for byte.
fn full_snapshot(system: &SnesSystem) -> Vec<u8> {
    let mut out = Vec::new();
    for region in StateRegion::ALL {
        let mut s = MemStream {
            buf: Vec::new(),
            at: 0,
        };
        system.save_region(region, &mut s).expect("save region");
        out.extend_from_slice(&s.buf);
    }
    out
}

/// Ticket W17-04 acceptance #2: two independent runs of the same SA-1
/// cart, for the same number of `SnesSystem::step` calls, must reach
/// bit-identical machine state — the W17-04 contention flags are derived
/// only from this step's own bus traffic (never from wall-clock, threads,
/// or hash-map iteration order), so nothing here should be able to
/// diverge.
#[test]
fn sa1_execution_is_deterministic_across_independent_runs() {
    let rom = sa1_test_image();

    let run = |steps: usize| -> Vec<u8> {
        let mut system = SnesSystem::load(&rom).expect("SA-1 cart loads");
        for _ in 0..steps {
            system.step().expect("no undefined opcode");
        }
        full_snapshot(&system)
    };

    // Long enough to boot the SA-1 core, run both loops many times over,
    // and let the WRAM/I-RAM/BW-RAM counters wrap at least once (each is
    // one byte).
    const STEPS: usize = 4000;
    let a = run(STEPS);
    let b = run(STEPS);
    assert_eq!(
        a, b,
        "identical runs of the same SA-1 cart must not diverge"
    );

    // Sanity: both CPUs actually ran, not just the main one — otherwise
    // this test would pass trivially even if `Sa1State::step` were never
    // called.
    let mut system = SnesSystem::load(&rom).expect("SA-1 cart loads");
    for _ in 0..STEPS {
        system.step().expect("no undefined opcode");
    }
    assert_ne!(system.bus.wram[0], 0, "main CPU's loop must have run");
    let sa1 = system.bus.sa1.as_ref().expect("SA-1 installed");
    assert_ne!(
        sa1.iram[0], 0,
        "SA-1's loop must have run and touched I-RAM"
    );
    assert_ne!(
        sa1.bwram[0], 0,
        "SA-1's loop must have run and touched BW-RAM"
    );
}

/// Ticket W17-04 acceptance #2: a save-state round trip taken while a
/// normal DMA is armed but not yet triggered (fullsnes "SNES Cart SA-1
/// DMA Transfers": for an I-RAM destination "transfer starts after
/// writing 2236h" — DCNT/SDA/DTC are latched well before that) must
/// resume identically to a run that was never interrupted.
#[test]
fn sa1_dma_setup_round_trips_at_a_mid_transfer_save_point() {
    let rom = sa1_test_image();

    // Reference: configure and trigger the DMA in one continuous session.
    let mut reference = SnesSystem::load(&rom).expect("SA-1 cart loads");
    reference.bus.write(0x00_222A, 0xFF); // CIWP: unprotect every I-RAM chunk
    reference.bus.write(0x00_2230, 0x80); // DCNT: enable, source=ROM, dest=I-RAM
    reference.bus.write(0x00_2232, 0x10); // SDA lo: ROM offset $000010
    reference.bus.write(0x00_2233, 0x00);
    reference.bus.write(0x00_2234, 0x00);
    reference.bus.write(0x00_2238, 0x04); // DTC: 4 bytes
    reference.bus.write(0x00_2239, 0x00);
    reference.bus.write(0x00_2236, 0x00); // DDA (I-RAM dest, lo): starts the transfer

    // Round-tripped: identical setup, but save+reload the Cart and Mapper
    // regions (fullsnes DMA state and the SA-1 board both live there —
    // see `StateRegion::Cart`'s doc) right before the triggering write.
    let mut before_trigger = SnesSystem::load(&rom).expect("SA-1 cart loads");
    before_trigger.bus.write(0x00_222A, 0xFF);
    before_trigger.bus.write(0x00_2230, 0x80);
    before_trigger.bus.write(0x00_2232, 0x10);
    before_trigger.bus.write(0x00_2233, 0x00);
    before_trigger.bus.write(0x00_2234, 0x00);
    before_trigger.bus.write(0x00_2238, 0x04);
    before_trigger.bus.write(0x00_2239, 0x00);

    let mut cart_stream = MemStream {
        buf: Vec::new(),
        at: 0,
    };
    let mut mapper_stream = MemStream {
        buf: Vec::new(),
        at: 0,
    };
    before_trigger
        .save_region(StateRegion::Cart, &mut cart_stream)
        .expect("save cart");
    before_trigger
        .save_region(StateRegion::Mapper, &mut mapper_stream)
        .expect("save mapper");

    let mut restored = SnesSystem::load(&rom).expect("SA-1 cart loads");
    restored
        .load_region(StateRegion::Cart, &mut cart_stream)
        .expect("load cart");
    restored
        .load_region(StateRegion::Mapper, &mut mapper_stream)
        .expect("load mapper");
    restored.bus.write(0x00_2236, 0x00); // trigger, same as the reference run

    let ref_sa1 = reference.bus.sa1.as_ref().expect("SA-1 installed");
    let restored_sa1 = restored.bus.sa1.as_ref().expect("SA-1 installed");
    assert_eq!(
        &restored_sa1.iram[0..4],
        &ref_sa1.iram[0..4],
        "the transfer must copy the same 4 ROM bytes into I-RAM whether or \
         not a save/load happened in between"
    );
    assert_eq!(
        &restored_sa1.iram[0..4],
        &rom[0x10..0x14],
        "sanity: the copied bytes really did come from ROM offset $10"
    );
}
