//! HDMA: the per-scanline table walker (ticket W7-07;
//! `docs/design/EMULATION_CORES.md` §3.2).
//!
//! HDMA is what makes gradients, wavy effects and most split-screen HUDs
//! work — and it is why a mode-7 perspective demo looks like perspective
//! rather than a flat rotated plane.

use crate::bus::SnesBus;
use crate::cpu::CpuBus;
use rf_cart::SnesMapMode;

/// A bus with a writable "register" the transfers can be observed at.
///
/// `$2180` is the WRAM data port: it accepts bytes and puts them
/// somewhere readable, which makes it the one `$21xx` register a test can
/// verify an HDMA landed in without a PPU in the way.
fn bus() -> SnesBus {
    let mut b = SnesBus::new(vec![0; 32 * 1024], 0, SnesMapMode::LoRom);
    // Point the WRAM port at $1:0000, well clear of the low mirror.
    b.write(0x00_2181, 0x00);
    b.write(0x00_2182, 0x00);
    b.write(0x00_2183, 0x01);
    b
}

/// Put an HDMA table in WRAM and arm channel 0 to feed `$2180`.
fn arm(b: &mut SnesBus, table: &[u8], control: u8) {
    for (i, v) in table.iter().enumerate() {
        b.wram[0x0400 + i] = *v;
    }
    b.write(0x00_4300, control);
    b.write(0x00_4301, 0x80); // B-bus $2180
    b.write(0x00_4302, 0x00); // table at $00:0400
    b.write(0x00_4303, 0x04);
    b.write(0x00_4304, 0x00);
    b.write(0x00_420C, 0x01); // HDMAEN channel 0
}

fn transferred(b: &SnesBus, n: usize) -> Vec<u8> {
    b.wram[0x1_0000..0x1_0000 + n].to_vec()
}

/// The basic walk: a line count, then that many lines of data.
#[test]
fn a_line_count_transfers_once_and_then_waits() {
    let mut b = bus();
    // 3 lines, no repeat: one byte transferred on the RELOAD line only.
    arm(&mut b, &[0x03, 0xAA, 0x00], 0x00);
    b.hdma_init();
    for _ in 0..3 {
        b.hdma_run_line();
    }
    assert_eq!(
        transferred(&b, 2),
        vec![0xAA, 0x00],
        "without the repeat flag the unit transfers once per reload, not per line"
    );
}

/// **The repeat flag (bit 7) transfers on EVERY line.** That is the
/// difference between a gradient and a single band.
#[test]
fn the_repeat_flag_transfers_on_every_line() {
    let mut b = bus();
    // 0x83 = repeat + 3 lines, then three data bytes, then terminator.
    arm(&mut b, &[0x83, 0x11, 0x22, 0x33, 0x00], 0x00);
    b.hdma_init();
    for _ in 0..3 {
        b.hdma_run_line();
    }
    assert_eq!(
        transferred(&b, 3),
        vec![0x11, 0x22, 0x33],
        "repeat consumes one table byte per line"
    );
}

/// **`$00` terminates the channel for the frame** — it is the table's end
/// marker, not a zero-length entry.
///
/// Treating it as "transfer nothing and carry on" walks off the end of
/// the table into whatever follows it in memory.
#[test]
fn a_zero_line_count_terminates_the_channel() {
    let mut b = bus();
    arm(&mut b, &[0x81, 0x55, 0x00, 0xFF, 0xFF, 0xFF], 0x00);
    b.hdma_init();
    for _ in 0..8 {
        b.hdma_run_line();
    }
    assert!(
        b.dma.channels[0].hdma_done,
        "the $00 entry ended the channel"
    );
    assert_eq!(
        transferred(&b, 3),
        vec![0x55, 0x00, 0x00],
        "nothing past the terminator may be transferred"
    );
}

/// Indirect mode: the table holds POINTERS, and the data comes from the
/// bank in `$43x7`.
#[test]
fn indirect_mode_dereferences_the_table() {
    let mut b = bus();
    // Table: 1 line, pointer $0500. Data at $00:0500.
    arm(&mut b, &[0x81, 0x00, 0x05, 0x00], 0x40); // bit 6 = indirect
    b.write(0x00_4307, 0x00); // indirect bank
    b.wram[0x0500] = 0x7E;
    b.hdma_init();
    b.hdma_run_line();
    assert_eq!(
        transferred(&b, 1),
        vec![0x7E],
        "the byte must come from the pointer, not from the table"
    );
}

/// **HDMA initialises at the start of a FRAME, not when `$420C` is
/// written.** A game that enables a channel mid-frame gets nothing until
/// the next frame — which is what undisbeliever's `hdmaen_latch_test`
/// checks.
#[test]
fn enabling_a_channel_mid_frame_does_not_transfer_until_the_next_init() {
    let mut b = bus();
    arm(&mut b, &[0x81, 0x99, 0x00], 0x00);

    // Start the "frame" with the channel DISABLED: init marks it done.
    b.write(0x00_420C, 0x00);
    b.hdma_init();

    // Now enable it mid-frame. Hardware latches HDMAEN at init, so this
    // channel stays inert for the rest of the frame — the behaviour
    // undisbeliever's hdmaen_latch_test exists to check.
    b.write(0x00_420C, 0x01);
    for _ in 0..4 {
        b.hdma_run_line();
    }
    assert_eq!(
        transferred(&b, 1),
        vec![0x00],
        "a channel enabled after init must not transfer this frame"
    );

    // The next frame's init picks it up.
    b.hdma_init();
    b.hdma_run_line();
    assert_eq!(transferred(&b, 1), vec![0x99]);
}

/// A disabled channel is inert even after an init.
#[test]
fn a_disabled_channel_transfers_nothing() {
    let mut b = bus();
    arm(&mut b, &[0x81, 0x42, 0x00], 0x00);
    b.write(0x00_420C, 0x00); // HDMAEN cleared
    b.hdma_init();
    for _ in 0..4 {
        b.hdma_run_line();
    }
    assert_eq!(transferred(&b, 1), vec![0x00]);
    assert_eq!(b.hdma_run_line(), 0, "and costs nothing");
}

/// The transfer patterns, including the one that alternates.
#[test]
fn the_transfer_patterns_have_the_documented_shapes() {
    use crate::dma::Channel;
    let pattern = |bits: u8| {
        Channel {
            control: bits,
            ..Channel::default()
        }
        .pattern()
        .to_vec()
    };
    assert_eq!(pattern(0), vec![0]);
    assert_eq!(pattern(1), vec![0, 1]);
    assert_eq!(pattern(2), vec![0, 0]);
    assert_eq!(pattern(3), vec![0, 0, 1, 1]);
    assert_eq!(pattern(4), vec![0, 1, 2, 3]);
    assert_eq!(
        pattern(5),
        vec![0, 1, 0, 1],
        "pattern 5 ALTERNATES between two registers"
    );
    assert_eq!(pattern(6), pattern(2));
    assert_eq!(pattern(7), pattern(3));
}

/// HDMA is charged for the bus it steals.
#[test]
fn hdma_costs_master_cycles() {
    let mut b = bus();
    arm(&mut b, &[0x81, 0x01, 0x00], 0x00);
    b.hdma_init();
    let cost = b.hdma_run_line();
    assert!(cost > 0, "a transfer must not be free");
    // Once terminated it costs nothing.
    for _ in 0..4 {
        b.hdma_run_line();
    }
    assert_eq!(b.hdma_run_line(), 0);
}

/// **HDMA and MDMA share one bus.** A channel doing HDMA is not available
/// for a general-purpose transfer at the same time, and the registers
/// they share (`$43x5`/`$43x6`) mean the two uses genuinely collide.
#[test]
fn hdma_and_mdma_share_the_channel_registers() {
    let mut b = bus();
    arm(&mut b, &[0x81, 0x00, 0x05, 0x00], 0x40); // indirect
    b.write(0x00_4307, 0x00);
    b.wram[0x0500] = 0x33;
    b.hdma_init();
    b.hdma_run_line();
    // The indirect pointer lives in `count`, the very register MDMA uses
    // as its byte count — so an MDMA armed afterwards sees whatever HDMA
    // left there. Hardware behaves the same way; a game must not do both.
    assert_ne!(
        b.dma.channels[0].count, 0,
        "HDMA leaves its pointer in the MDMA byte-count register"
    );
}

/// Multiple channels walk independently.
#[test]
fn channels_walk_their_own_tables() {
    let mut b = bus();
    // Channel 0 -> $2180, channel 1 -> $2180 as well, different tables.
    for (i, v) in [0x81u8, 0xA1, 0x00].iter().enumerate() {
        b.wram[0x0400 + i] = *v;
    }
    for (i, v) in [0x81u8, 0xB2, 0x00].iter().enumerate() {
        b.wram[0x0600 + i] = *v;
    }
    b.write(0x00_4300, 0x00);
    b.write(0x00_4301, 0x80);
    b.write(0x00_4302, 0x00);
    b.write(0x00_4303, 0x04);
    b.write(0x00_4304, 0x00);
    b.write(0x00_4310, 0x00);
    b.write(0x00_4311, 0x80);
    b.write(0x00_4312, 0x00);
    b.write(0x00_4313, 0x06);
    b.write(0x00_4314, 0x00);
    b.write(0x00_420C, 0x03);
    b.hdma_init();
    b.hdma_run_line();
    assert_eq!(
        transferred(&b, 2),
        vec![0xA1, 0xB2],
        "both channels transfer, in channel order"
    );
}

/// **A per-scanline CGRAM gradient**: what PeterLemon's RedSpaceHDMA
/// actually does, in a form that runs without a fetched ROM (ticket
/// W7-07's criterion 4).
///
/// That ROM is EXCLUDED from the golden suite, and the reason is
/// structural rather than a defect: its whole picture is the backdrop,
/// so every pixel is palette index 0 on every line and an index-domain
/// hash cannot see the effect at all (see the exclusion entry in
/// `tests/peterlemon_golden.rs`). Tracing it showed the HDMA machinery
/// doing exactly the right thing -- `$2121` <- 0, then `$2122` <- `$1F`,
/// `$1E`, `$1D` ... one step every 7 lines, 32 steps over 224 lines --
/// so this test pins that behaviour where the golden cannot.
///
/// Transfer unit 3 is the one this needs: two registers, written twice
/// each (`b, b, b+1, b+1`), which is how a colour is written as an
/// address followed by a 15-bit value.
#[test]
fn hdma_writes_a_different_backdrop_colour_on_each_line() {
    let mut b = bus();
    // Three entries, each holding for one line: CGADD=0 twice, then the
    // colour low/high bytes. Red descending, exactly as the ROM does.
    let table = [
        0x01, 0x00, 0x00, 0x1F, 0x00, // line 0: colour $001F
        0x01, 0x00, 0x00, 0x1E, 0x00, // line 1: colour $001E
        0x01, 0x00, 0x00, 0x1D, 0x00, // line 2: colour $001D
        0x00, // terminator
    ];
    for (i, v) in table.iter().enumerate() {
        b.wram[0x0400 + i] = *v;
    }
    b.write(0x00_4300, 0x03); // unit 3: b, b, b+1, b+1
    b.write(0x00_4301, 0x21); // B-bus $2121 (CGADD), so b+1 is $2122
    b.write(0x00_4302, 0x00);
    b.write(0x00_4303, 0x04);
    b.write(0x00_4304, 0x00);
    b.write(0x00_420C, 0x01);

    b.hdma_init();
    let mut seen = Vec::new();
    for _ in 0..3 {
        b.hdma_run_line();
        seen.push(b.ppu.cgram[0]);
    }

    assert_eq!(
        seen,
        vec![0x001F, 0x001E, 0x001D],
        "each line must leave its own colour in CGRAM[0] - a gradient that \
         wrote one colour for the whole frame is the bug this catches"
    );
}
