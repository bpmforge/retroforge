//! DSP-1 Raster (`0Ah`/`1Ah`) output drained by DMA/HDMA (ticket W16-11).
//!
//! ## Why this exists even though no traced title uses it
//!
//! W16-11's probe (`crates/rf-snes/tests/dsp1_probe.rs`,
//! `dsp1_raster_drain_trace`) found that Super Mario Kart and Pilotwings
//! both drain every Raster byte with plain CPU `LDA`/reads — 600 frames,
//! zero DMA/HDMA units sourced from the DSP-1 window in either title (see
//! the ticket note). The DSP-1 manual (§5.4.2) and fullsnes both document
//! the raster stream as DMA-drained hardware behaviour, so another
//! DSP-1 title could still rely on it even though these two do not — and
//! `SnesBus::read`/`write` already route a DMA/HDMA A-bus access through
//! the same `target()` a CPU access uses (see `bus.rs`'s `run_channel`/
//! `hdma_transfer_unit`, both of which call `self.read`/`self.write`
//! rather than touching `rom`/`wram` directly). These tests exist to
//! prove that claim rather than merely state it, and to catch a
//! regression if a future change gives DMA a separate, bypassing path.
//!
//! Both tests build a REFERENCE `Dsp1` instance driven the ordinary CPU
//! way (`write_dr`/`read_dr`) and check the DMA/HDMA-drained values
//! against it — the manual's protocol is "the same bytes, in the same
//! order", not a specific numeric matrix, so that is what is asserted.

use crate::bus::SnesBus;
use crate::cpu::CpuBus;
use crate::dsp1::Dsp1;
use rf_cart::{DspWindow, SnesMapMode};

/// Projection parameters shared with `dsp1.rs`'s own
/// `raster_scale_shrinks_toward_the_viewer`/`raster_stream_terminates_on_8000h`
/// tests: `Azs = 135deg`, a plausible in-range camera setup.
const PROJECTION: [u16; 7] = [0, 0, 1000, 0, 256, 0, 0x6000];

/// The plain-LoROM DSP-1 window (snes9x `M_DSP1_LOROM`; `rf_cart::snes`'s
/// private `dsp_window_for` builds the identical value for a LoROM image
/// at or under the DSP-1B threshold — reproduced here because that
/// function is not exported, only its result type is).
fn window() -> DspWindow {
    DspWindow {
        banks: [0x20..=0x3F, 0xA0..=0xBF],
        dr: 0x8000..=0xBFFF,
        sr: 0xC000..=0xFFFF,
    }
}

fn bus_with_dsp1() -> SnesBus {
    let mut b = SnesBus::new(vec![0; 32 * 1024], 0, SnesMapMode::LoRom);
    b.install_dsp1(window());
    b
}

/// Write a 16-bit word to the DR the way a CPU does: low byte, then high.
fn write_word(b: &mut SnesBus, addr: u32, word: u16) {
    b.write(addr, (word & 0xFF) as u8);
    b.write(addr, (word >> 8) as u8);
}

/// Start a Raster session at `vs` through the bus's DR window (CPU-style
/// writes), after a Projection Parameter Setting call — the sequence
/// every DSP-1 raster user follows (Manual §5.4.1/§5.4.2).
fn start_raster(b: &mut SnesBus, addr: u32, vs: u16) {
    b.write(addr, 0x02);
    for &p in &PROJECTION {
        write_word(b, addr, p);
    }
    // Idle-with-pending-output + a fresh opcode write abandons the
    // Projection output and dispatches immediately (`Dsp1::write_dr`'s
    // documented idle-path behaviour) — no need to drain it first.
    b.write(addr, 0x0A);
    write_word(b, addr, vs);
}

/// The reference: the same setup, driven directly against a bare `Dsp1`
/// via its public `write_dr`/`read_dr`, for `lines` scanlines (4 words
/// each). This is what a CPU polling loop would see.
fn reference_lines(vs: u16, lines: usize) -> Vec<[i16; 4]> {
    let mut d = Dsp1::new();
    d.write_dr(0x02);
    for &p in &PROJECTION {
        d.write_dr((p & 0xFF) as u8);
        d.write_dr((p >> 8) as u8);
    }
    d.write_dr(0x0A);
    d.write_dr((vs & 0xFF) as u8);
    d.write_dr((vs >> 8) as u8);
    (0..lines)
        .map(|_| {
            let mut line = [0i16; 4];
            for w in &mut line {
                let lo = d.read_dr();
                let hi = d.read_dr();
                *w = (u16::from(lo) | (u16::from(hi) << 8)) as i16;
            }
            line
        })
        .collect()
}

/// General-purpose DMA, **fixed A-bus address** (`$43x0` bit 3): the
/// mechanism fullsnes documents for a hardware-register source, since the
/// literal address never has to advance for the DSP-1's own state machine
/// to hand back the next byte (`dsp1_target` matches on being inside the
/// window, not on the specific offset).
#[test]
fn fixed_address_mdma_drains_a_raster_session_byte_for_byte() {
    let mut b = bus_with_dsp1();
    // Point the WRAM port at $01:0000, so the transferred bytes land
    // somewhere observable without going through the PPU's write-twice
    // latch — `tests/dma.rs`'s own pattern.
    b.write(0x00_2181, 0x00);
    b.write(0x00_2182, 0x00);
    b.write(0x00_2183, 0x01);

    let dr_addr = 0x20_8000u32;
    start_raster(&mut b, dr_addr, 10);

    let expected = reference_lines(10, 2);
    let expected_bytes: Vec<u8> = expected
        .iter()
        .flat_map(|line| line.iter().flat_map(|&w| (w as u16).to_le_bytes()))
        .collect();
    assert_eq!(expected_bytes.len(), 16, "2 lines * 4 words * 2 bytes");

    // Channel 0: fixed A-bus at the DR address, B-bus = WMDATA ($2180),
    // one byte per unit, 16 bytes (2 scanlines' worth).
    b.write(0x00_4300, 0x08); // bit 3: fixed address
    b.write(0x00_4301, 0x80); // $2180 WMDATA
    b.write(0x00_4302, dr_addr as u8);
    b.write(0x00_4303, (dr_addr >> 8) as u8);
    b.write(0x00_4304, (dr_addr >> 16) as u8);
    b.write(0x00_4305, 16);
    b.write(0x00_4306, 0);
    b.write(0x00_420B, 0x01);
    b.service_dma();

    assert_eq!(
        &b.wram[0x1_0000..0x1_0000 + 16],
        expected_bytes.as_slice(),
        "DMA-drained raster bytes must match a CPU-driven reference, byte for byte"
    );
    assert_eq!(b.dsp1_trace.mdma_reads, 16);
    assert_eq!(b.dsp1_trace.mdma_channel_starts, 1);
    assert_eq!(b.dsp1_trace.mdma_channel_starts_fixed, 1);
    assert_eq!(
        b.dsp1_trace.cpu_raster_reads, 0,
        "no CPU read touched the DR"
    );
    assert_eq!(b.dsp1_trace.hdma_reads, 0);
}

/// HDMA, **indirect addressing** (Manual's raster note; snes9x/fullsnes'
/// "HDMA indirect mode"): the table (in WRAM) supplies only the per-line
/// count and, once, a 16-bit pointer; the pointer itself is what falls
/// inside the DSP-1 window and is what advances per byte transferred.
/// Two channels split the four raster words across the Mode 7 matrix
/// registers `$211B`(A)-`$211E`(D), each a write-twice 16-bit port
/// (`Mode7::write_register`) fed low-byte-then-high — exactly the order
/// `Dsp1::read_dr` returns a word in.
#[test]
fn indirect_hdma_drains_a_raster_session_into_mode7_registers() {
    let mut b = bus_with_dsp1();
    let dr_addr = 0x20_8000u32;
    start_raster(&mut b, dr_addr, 10);

    let expected = reference_lines(10, 3);

    // Direct-mode tables: only the line-count byte plus the one-time
    // indirect pointer, all in ordinary WRAM well outside the DSP-1
    // window — the window is only ever touched through the INDIRECT
    // pointer, never through the direct table walk itself.
    //
    // $83 = repeat flag set, count 3: transfers on every one of the 3
    // lines (`hdma.rs`'s `the_repeat_flag_transfers_on_every_line`).
    let table_ab = [0x0500u16, 0x0503];
    let table_cd = [0x0510u16, 0x0513];
    b.wram[0x0500] = 0x83;
    b.wram[0x0501] = dr_addr as u8; // pointer low
    b.wram[0x0502] = (dr_addr >> 8) as u8; // pointer high
    b.wram[0x0510] = 0x83;
    b.wram[0x0511] = dr_addr as u8;
    b.wram[0x0512] = (dr_addr >> 8) as u8;
    let _ = (table_ab, table_cd); // (addresses spelled out above for clarity)

    // Channel 0: A/B -> $211B, indirect, pattern 3 = [0,0,1,1].
    b.write(0x00_4300, 0x43);
    b.write(0x00_4301, 0x1B);
    b.write(0x00_4302, 0x00);
    b.write(0x00_4303, 0x05);
    b.write(0x00_4304, 0x00);
    b.write(0x00_4307, (dr_addr >> 16) as u8); // indirect bank

    // Channel 1: C/D -> $211D, same shape.
    b.write(0x00_4310, 0x43);
    b.write(0x00_4311, 0x1D);
    b.write(0x00_4312, 0x10);
    b.write(0x00_4313, 0x05);
    b.write(0x00_4314, 0x00);
    b.write(0x00_4317, (dr_addr >> 16) as u8);

    b.write(0x00_420C, 0x03); // HDMAEN channels 0 and 1
    b.hdma_init();

    for (i, want) in expected.iter().enumerate() {
        b.hdma_run_line();
        assert_eq!(
            [b.ppu.mode7.a, b.ppu.mode7.b, b.ppu.mode7.c, b.ppu.mode7.d],
            *want,
            "scanline {i}: HDMA-drained Mode 7 matrix must match the CPU-driven reference"
        );
    }
    assert_eq!(b.dsp1_trace.hdma_reads, 3 * 8);
    assert_eq!(b.dsp1_trace.hdma_units, 3 * 8);
    assert_eq!(b.dsp1_trace.cpu_raster_reads, 0);
    assert_eq!(b.dsp1_trace.mdma_reads, 0);
}
