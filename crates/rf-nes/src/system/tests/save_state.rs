//! Save-state roundtrip tests (ticket W2-04, FR-STATE-002).
//!
//! The headline test is the acceptance criterion itself, stated the way the
//! ticket states it: **state -> run N -> hash** must equal **restore -> run
//! N -> hash**. That shape is chosen deliberately over the weaker "the
//! bytes round-trip": a byte-identical reload proves only that the encoder
//! and decoder agree with each other, while re-running the machine proves
//! the state actually *drives* identically — which is the property save
//! states exist for, and the only one that catches a dropped field.

use rf_core_api::{CoreSink, PpuPixel, StateError, StateReader, StateWriter};

/// Drains video without keeping it: these tests compare machine state, and
/// `crate::ppu`'s frame-boundary rule only needs the queue emptied.
struct NullSink;

impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _ev: rf_core_api::CoreEvent) {}
}

use crate::cpu::Cpu;
use crate::state::StateRegion;
use crate::system::tests::bus_with_pattern_rom;
use crate::system::NesBus;
use crate::CpuBus;

/// In-memory `StateWriter`/`StateReader` pair. `rf-state`'s container is a
/// different crate (and an upper layer this one may not depend on), so the
/// tests here exercise the payload encoding directly.
#[derive(Default)]
struct MemStream {
    bytes: Vec<u8>,
    read_pos: usize,
}

impl StateWriter for MemStream {
    fn write_all(&mut self, buf: &[u8]) -> Result<(), StateError> {
        self.bytes.extend_from_slice(buf);
        Ok(())
    }
}

impl StateReader for MemStream {
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), StateError> {
        let end = self.read_pos + buf.len();
        if end > self.bytes.len() {
            return Err(StateError::Io(format!(
                "state stream exhausted: wanted {} more bytes, {} remain",
                buf.len(),
                self.bytes.len() - self.read_pos
            )));
        }
        buf.copy_from_slice(&self.bytes[self.read_pos..end]);
        self.read_pos = end;
        Ok(())
    }
}

/// Saves every region of `(cpu, bus)` into one stream per region.
fn save_all(cpu: &Cpu, bus: &NesBus) -> Vec<MemStream> {
    StateRegion::ALL
        .iter()
        .map(|region| {
            let mut stream = MemStream::default();
            bus.save_region(cpu, *region, &mut stream)
                .unwrap_or_else(|e| panic!("save_region({region:?}) failed: {e}"));
            stream
        })
        .collect()
}

fn load_all(cpu: &mut Cpu, bus: &mut NesBus, streams: &[MemStream]) {
    for (region, stream) in StateRegion::ALL.iter().zip(streams) {
        let mut replay = MemStream {
            bytes: stream.bytes.clone(),
            read_pos: 0,
        };
        bus.load_region(cpu, *region, &mut replay)
            .unwrap_or_else(|e| panic!("load_region({region:?}) failed: {e}"));
    }
}

/// A digest of everything a save state claims to carry. Deliberately built
/// by re-saving rather than by reaching into fields: if a field is missing
/// from the encoding, it is missing from this hash too, so this function
/// alone could never catch a dropped field — which is exactly why the tests
/// below compare *machine behavior after N frames*, not just this value.
fn state_bytes(cpu: &Cpu, bus: &NesBus) -> Vec<u8> {
    save_all(cpu, bus)
        .into_iter()
        .flat_map(|s| s.bytes)
        .collect()
}

/// Runs `frames` frames' worth of CPU steps, draining the PPU so the
/// frame-boundary rule holds at the end.
fn run_frames(cpu: &mut Cpu, bus: &mut NesBus, frames: u32) {
    for _ in 0..frames {
        let start = bus.frame_count();
        while bus.frame_count() == start {
            cpu.step(bus);
        }
        bus.drain_video(&mut NullSink);
    }
}

/// FR-STATE-002, verbatim from the ticket: `state -> run N -> hash ==
/// restore -> run N -> hash`.
#[test]
fn frame_boundary_snapshot_restore_roundtrip_runs_identically() {
    let mut bus = bus_with_pattern_rom(2, 1);
    let mut cpu = Cpu::power_on(&mut bus);

    // Get the machine somewhere non-trivial first: a state taken at
    // power-on would pass even if half the fields were dropped.
    run_frames(&mut cpu, &mut bus, 12);
    let saved = save_all(&cpu, &bus);
    let saved_bytes = state_bytes(&cpu, &bus);

    // Continue from the live machine.
    run_frames(&mut cpu, &mut bus, 20);
    let live_after = state_bytes(&cpu, &bus);

    // Restore into a FRESH machine (not the same one rewound) so anything
    // the encoding forgot shows up as a difference rather than surviving
    // in place.
    let mut restored_bus = bus_with_pattern_rom(2, 1);
    let mut restored_cpu = Cpu::power_on(&mut restored_bus);
    load_all(&mut restored_cpu, &mut restored_bus, &saved);
    assert_eq!(
        state_bytes(&restored_cpu, &restored_bus),
        saved_bytes,
        "a freshly restored machine must encode identically to the one that was saved"
    );

    run_frames(&mut restored_cpu, &mut restored_bus, 20);
    assert_eq!(
        state_bytes(&restored_cpu, &restored_bus),
        live_after,
        "20 frames after a restore must reach the same state as 20 frames after the save"
    );
}

/// The anti-tamper half, and the reason the test above restores into a
/// fresh machine: corrupt one byte of one region and the roundtrip must
/// FAIL. Without this, a save/restore that silently did nothing at all
/// would pass the test above.
#[test]
fn a_single_corrupted_byte_changes_the_restored_machine() {
    let mut bus = bus_with_pattern_rom(2, 1);
    let mut cpu = Cpu::power_on(&mut bus);
    run_frames(&mut cpu, &mut bus, 8);
    let saved = save_all(&cpu, &bus);
    let honest = state_bytes(&cpu, &bus);

    // Flip a bit in the CPU_ region's first byte (the accumulator).
    let mut tampered: Vec<MemStream> = saved
        .iter()
        .map(|s| MemStream {
            bytes: s.bytes.clone(),
            read_pos: 0,
        })
        .collect();
    tampered[0].bytes[0] ^= 0xFF;

    let mut restored_bus = bus_with_pattern_rom(2, 1);
    let mut restored_cpu = Cpu::power_on(&mut restored_bus);
    load_all(&mut restored_cpu, &mut restored_bus, &tampered);
    assert_ne!(
        state_bytes(&restored_cpu, &restored_bus),
        honest,
        "a corrupted state must not restore to the same machine -- if this passes, the \
         roundtrip test above is vacuous"
    );
}

/// Every region must be non-empty except where emptiness is meaningful:
/// `MAPR` is empty for NROM, which has no registers at all. A region that
/// silently encoded nothing would make the roundtrip test above pass
/// vacuously for that region.
#[test]
fn every_region_writes_the_bytes_it_claims_to() {
    let mut bus = bus_with_pattern_rom(2, 1);
    let cpu = Cpu::power_on(&mut bus);
    let sizes: Vec<(StateRegion, usize)> = StateRegion::ALL
        .iter()
        .zip(save_all(&cpu, &bus))
        .map(|(r, s)| (*r, s.bytes.len()))
        .collect();

    for (region, len) in &sizes {
        match region {
            // NROM has no mapper registers -- see `Mapper::save_state`'s
            // default, which is correct rather than a stub.
            StateRegion::Mapper => assert_eq!(*len, 0, "NROM MAPR must be empty"),
            StateRegion::Wram => assert_eq!(*len, 0x0800, "WRAM is the real 2 KiB"),
            StateRegion::Oam => assert_eq!(*len, 256, "OAM_ is the 256-byte sprite table"),
            StateRegion::Cgram => assert_eq!(*len, 32, "CGRM is 32 bytes of palette RAM"),
            StateRegion::Cart => assert_eq!(*len, 0x2000, "CART is the 8 KiB PRG-RAM window"),
            other => assert!(*len > 0, "{other:?} encoded nothing at all"),
        }
    }
}

/// FR-CORE-012's disk side: battery RAM is exposed and replaceable, and a
/// wrong-sized `.sav` is refused rather than padded.
#[test]
fn battery_ram_roundtrips_and_refuses_a_wrong_sized_image() {
    let mut bus = bus_with_pattern_rom(2, 1);
    bus.write(0x6000, 0xA5);
    bus.write(0x7FFF, 0x5A);

    let image = bus.battery_ram().to_vec();
    assert_eq!(image.len(), 0x2000);
    assert_eq!(image[0], 0xA5);
    assert_eq!(image[0x1FFF], 0x5A);

    let mut fresh = bus_with_pattern_rom(2, 1);
    fresh
        .set_battery_ram(&image)
        .expect("same-size image loads");
    assert_eq!(fresh.peek(0x6000), 0xA5);
    assert_eq!(fresh.peek(0x7FFF), 0x5A);

    let err = fresh
        .set_battery_ram(&image[..0x1000])
        .expect_err("a truncated .sav must be refused");
    assert!(
        format!("{err}").contains("battery RAM is 4096 bytes"),
        "the refusal must name both sizes, got: {err}"
    );
}

/// The frame-boundary rule (`docs/design/SAVE_STATES.md` §2) is enforced,
/// not just documented: saving with undrained scanlines is refused with a
/// diagnostic that says what would have been lost.
#[test]
fn saving_mid_frame_with_undrained_output_is_refused() {
    let mut bus = bus_with_pattern_rom(2, 1);
    let mut cpu = Cpu::power_on(&mut bus);

    // Enable rendering so visible scanlines are actually produced, then
    // step TWO frames without draining: `Ppu::new` starts on the
    // pre-render line, so the first `frame_count` increment arrives after
    // roughly one scanline, before any visible row exists.
    bus.write(0x2001, 0x08);
    for _ in 0..2 {
        let start = bus.frame_count();
        while bus.frame_count() == start {
            cpu.step(&mut bus);
        }
    }

    let mut stream = MemStream::default();
    let err = bus
        .save_region(&cpu, StateRegion::Ppu, &mut stream)
        .expect_err("undrained scanlines must refuse the save");
    let message = format!("{err}");
    assert!(
        message.contains("frame-boundary only") && message.contains("undrained"),
        "the refusal must explain itself, got: {message}"
    );
}

/// Builds a synthetic NES 2.0 MMC5 image declaring `byte10` for
/// PRG-RAM/PRG-NVRAM (ticket W14-22) -- e.g. `0x77` for an ETROM-shaped
/// 8 KiB volatile + 8 KiB NVRAM = 16 KiB cartridge, matching Uncharted
/// Waters' real header.
fn nes2_mmc5_with_prg_ram(byte10: u8) -> crate::system::NesRom {
    let prg_banks: u16 = 2; // 2 x 16 KiB = 32 KiB, enough for MMC5's own asserts
    let chr_banks: u16 = 1; // 1 x 8 KiB
    let flags6 = 0x50u8; // mapper low nibble 5
    let flags7 = 0x08u8; // NES 2.0 identifier bits, mapper high nibble 0
    let byte8 = 0u8;
    let byte9 = 0u8;

    let mut data = Vec::new();
    data.extend_from_slice(&rf_cart::nes::INES_MAGIC);
    data.push(prg_banks as u8);
    data.push(chr_banks as u8);
    data.push(flags6);
    data.push(flags7);
    data.push(byte8);
    data.push(byte9);
    data.push(byte10);
    data.extend_from_slice(&[0u8; 5]); // bytes 11-15
    data.extend(vec![0xEAu8; prg_banks as usize * 16 * 1024]);
    data.extend(vec![0u8; chr_banks as usize * 8 * 1024]);
    crate::system::NesRom::from_ines_bytes(&data).expect("valid NES 2.0 MMC5 image")
}

/// Ticket W14-22, acceptance: "the PRG RAM save-state chunk ... carr[ies]
/// the real size; a round-trip test covers a 16 KiB cart."
#[test]
fn cart_chunk_round_trips_a_16_kib_etrom_shaped_cartridge() {
    let mut bus = NesBus::new(nes2_mmc5_with_prg_ram(0x77));
    let cpu = Cpu::power_on(&mut bus);
    assert_eq!(
        bus.rom().prg_ram_size + bus.rom().prg_nvram_size,
        16 * 1024,
        "header sizing: 8 KiB volatile + 8 KiB NVRAM"
    );

    // Unlock PRG RAM writes and put distinct bytes on each of the two
    // chips ($5113=0 -> chip 0, $5113=4 -> chip 1).
    bus.write(0x5102, 0x02);
    bus.write(0x5103, 0x01);
    bus.write(0x5113, 0);
    bus.write(0x6000, 0x11);
    bus.write(0x5113, 4);
    bus.write(0x6000, 0x22);

    let mut stream = MemStream::default();
    bus.save_region(&cpu, StateRegion::Cart, &mut stream)
        .expect("CART chunk must save a 16 KiB cartridge");
    assert_eq!(
        stream.bytes.len(),
        16 * 1024,
        "the chunk carries the real size"
    );

    let mut restored = NesBus::new(nes2_mmc5_with_prg_ram(0x77));
    let mut restored_cpu = Cpu::power_on(&mut restored);
    let mut replay = MemStream {
        bytes: stream.bytes.clone(),
        read_pos: 0,
    };
    restored
        .load_region(&mut restored_cpu, StateRegion::Cart, &mut replay)
        .expect("CART chunk must load back into a matching 16 KiB cartridge");

    restored.write(0x5113, 0);
    assert_eq!(
        restored.read(0x6000),
        0x11,
        "chip 0 survived the round trip"
    );
    restored.write(0x5113, 4);
    assert_eq!(
        restored.read(0x6000),
        0x22,
        "chip 1 survived the round trip"
    );
}
