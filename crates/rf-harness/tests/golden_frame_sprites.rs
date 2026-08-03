//! Ticket W1-05a's oracle for acceptance criterion 1 ("secondary OAM
//! evaluation with the 8-sprite limit + buggy overflow-flag behavior"):
//! criterion 1 has no test ROM until W1-05b, exactly the position W1-04a
//! was in when its real `write_scroll` mask bug survived every unit test
//! and was only caught by W1-04b's golden frame (see that file's module
//! doc). This file extends the SAME analytic golden-frame pattern to
//! sprites: expected values computed by a closed-form formula over this
//! fixture's own (x, OAM index) layout -- never by running `rf_nes`'s own
//! PPU and recording whatever came out.
//!
//! ## Fixture design: what a wrong cutoff/priority/leak would change
//!
//! 10 sprites, all sharing one Y-coordinate so they're all "in range" for
//! the same target scanline, in ascending OAM order 0-9:
//!
//! | oam index | x   | palette group | in the first 8? |
//! |-----------|-----|----------------|------------------|
//! | 0-1,3-4,6 | 10,30,70,90,110 | 0 (-> palette value 0x01) | yes |
//! | 2         | 50  | 1 (-> 0x02) | yes |
//! | 5         | 50  | 2 (-> 0x03) | yes, but LOSES to index 2 at the same x |
//! | 7         | 150 | 1 (-> 0x02) | yes |
//! | 8         | 150 | 3 (-> 0x04) | **no** -- the 9th found, dropped by the cap |
//! | 9         | 200 | 2 (-> 0x03) | **no** -- the 10th, dropped, and at an X no other sprite covers |
//!
//! This gives three specific, independently-computed pixels that a bug
//! changes instead of aliasing away:
//! - **x=50**: two IN-LIMIT sprites (2 and 5) fully overlap. A wrong
//!   OAM-order priority (higher index winning) flips the expected value
//!   from `0x02` to `0x03`.
//! - **x=150**: an in-limit sprite (7) is fully overlapped by a DROPPED
//!   one (8, the 9th found). A wrong 8-sprite cutoff (e.g. "last 8 found"
//!   instead of "first 8") flips the expected value from `0x02` to `0x04`.
//! - **x=200**: ONLY a dropped sprite (9, the 10th found) covers this x.
//!   The correct output is the plain backdrop (`0x3F`); any cutoff leak
//!   turns this into `0x03` -- maximally visible, since nothing else could
//!   produce that value there.
//!
//! Sprite 8's Y-coordinate is *also* genuinely in range for the target
//! scanline, so this fixture's evaluation trivially finds a real 9th
//! sprite (no diagonal-drift misalignment needed to demonstrate it) --
//! the buggy scan's false-positive/false-negative *drift* behavior itself
//! is exercised by `crates/rf-nes/src/ppu/tests/sprite_evaluation.rs`
//! (byte-level access to OAM that only the internal crate has); this file
//! proves the straightforward "flag gets set when a real 9th sprite
//! exists" case end-to-end, through the real CPU+PPU+bus pipeline, which
//! is exactly the class of wiring bug (not algorithm bug) this file exists
//! to catch.
//!
//! OAM is populated through real `$2003`/`$2004` writes in two passes (see
//! `build_prg`): the 54 primary-OAM entries this fixture doesn't use are
//! first filled with `$FF` (never in range for any visible scanline),
//! THEN the 10 real sprites are written -- in that order so `OAMADDR` ends
//! non-zero (`0x28`), the ticket's own "non-empty, non-zero starting OAM...
//! non-zero OAMADDR" trap, satisfied by construction. The `$FF` fill isn't
//! just tidiness: `Ppu::new`'s zero-filled OAM leaves every unused index at
//! Y=0, which IS "in range" at scanline 0, so without it the overflow flag
//! would already be set by phantom sprites before this fixture's real
//! sprites are ever evaluated -- which would make this file's overflow-flag
//! assertion pass even with the real 8-sprite-cap/overflow code deleted.
use rf_core_api::{CoreEvent, CoreSink, PpuPixel};
use rf_harness::{find_first_divergence, hash_frame_palette_indices};
use rf_nes::{Cpu, NesBus};

const SCREEN_WIDTH: u16 = 256;
const SCREEN_HEIGHT: u16 = 240;
/// The screen row the fixture's sprites render on (their OAM Y byte is one
/// less, per nesdev.org/wiki/PPU_OAM's "subtract 1... before writing it
/// here" -- see `build_prg`'s sprite table).
const TARGET_ROW: u16 = 100;

const BACKDROP_VALUE: u8 = 0x3F;
const GROUP0_VALUE: u8 = 0x01;
const GROUP1_VALUE: u8 = 0x02;
const GROUP2_VALUE: u8 = 0x03;
const GROUP3_VALUE: u8 = 0x04; // only ever used by the dropped 9th sprite

// ---------------------------------------------------------------------
// Oracle: closed-form, independent of rf_nes (a different crate -- same
// note golden_frame_bg.rs makes: nothing to call into even if this test
// wanted to).
// ---------------------------------------------------------------------

/// `(x, palette value)` for exactly the 8 sprites the 8-sprite cap keeps,
/// in ascending-OAM-index (== priority) order. Indices 8 and 9 are
/// deliberately absent -- the whole point of this fixture.
const IN_LIMIT_SPRITES: [(u16, u8); 8] = [
    (10, GROUP0_VALUE),  // oam index 0
    (30, GROUP0_VALUE),  // 1
    (50, GROUP1_VALUE),  // 2 -- must win over index 5 at the same x
    (70, GROUP0_VALUE),  // 3
    (90, GROUP0_VALUE),  // 4
    (50, GROUP2_VALUE),  // 5 -- must LOSE to index 2 (later in this list)
    (110, GROUP0_VALUE), // 6
    (150, GROUP1_VALUE), // 7 -- must win over the dropped index 8
];

fn expected_pixel_at_target_row(x: u16) -> u8 {
    for &(sx, value) in &IN_LIMIT_SPRITES {
        if x >= sx && x < sx + 8 {
            return value;
        }
    }
    BACKDROP_VALUE
}

/// Every sprite in this fixture is the default 8x8 size and shares the
/// same Y, so all of them cover the SAME 8-scanline band starting at
/// `TARGET_ROW` -- `expected_pixel_at_target_row`'s per-column result
/// applies unchanged to every row in that band; every row outside it is
/// pure backdrop (no sprite anywhere covers it).
fn expected_frame() -> Vec<Vec<u8>> {
    (0..SCREEN_HEIGHT)
        .map(|y| {
            (0..SCREEN_WIDTH)
                .map(|x| {
                    if (TARGET_ROW..TARGET_ROW + 8).contains(&y) {
                        expected_pixel_at_target_row(x)
                    } else {
                        BACKDROP_VALUE
                    }
                })
                .collect()
        })
        .collect()
}

// ---------------------------------------------------------------------
// CHR: one solid-fill 8x8 tile (index 1) in pattern-table bank 0, used by
// every sprite in this fixture -- pattern value 1 at every column and row,
// so every covered pixel is unambiguously opaque (same "solid fill"
// discipline golden_frame_bg.rs uses for backgrounds).
// ---------------------------------------------------------------------

fn build_chr() -> Vec<u8> {
    const CHR_BANK_SIZE: usize = 8 * 1024;
    let mut chr = vec![0u8; CHR_BANK_SIZE];
    let base = 16; // tile index 1 * 16 bytes/tile
    for row in 0..8 {
        chr[base + row] = 0xFF; // lo plane: pattern bit 0 set on every column
        chr[base + 8 + row] = 0x00; // hi plane: pattern stays 01, never 11
    }
    chr
}

// ---------------------------------------------------------------------
// PRG: LDA #imm / STA $abs / JMP $abs only (golden_frame_bg.rs's own
// discipline -- no branches, no relative-offset arithmetic to get wrong).
// ---------------------------------------------------------------------

fn lda_imm(prg: &mut Vec<u8>, v: u8) {
    prg.push(0xA9);
    prg.push(v);
}

fn sta_abs(prg: &mut Vec<u8>, addr: u16) {
    prg.push(0x8D);
    prg.push((addr & 0xFF) as u8);
    prg.push((addr >> 8) as u8);
}

fn jmp_abs(prg: &mut Vec<u8>, addr: u16) {
    prg.push(0x4C);
    prg.push((addr & 0xFF) as u8);
    prg.push((addr >> 8) as u8);
}

/// The 10 sprites' raw OAM bytes `(y, tile, attr, x)`, in primary-OAM
/// index order 0-9 -- `y = TARGET_ROW - 1` for every one of them
/// (nesdev.org/wiki/PPU_OAM's documented one-scanline delay), `tile = 1`
/// (the one solid tile `build_chr` fills), `attr`'s low 2 bits select the
/// sprite palette group.
fn sprite_table() -> [(u8, u8, u8, u8); 10] {
    let y = (TARGET_ROW - 1) as u8;
    [
        (y, 1, 0, 10),  // 0
        (y, 1, 0, 30),  // 1
        (y, 1, 1, 50),  // 2
        (y, 1, 0, 70),  // 3
        (y, 1, 0, 90),  // 4
        (y, 1, 2, 50),  // 5
        (y, 1, 0, 110), // 6
        (y, 1, 1, 150), // 7
        (y, 1, 3, 150), // 8 -- the 9th found: dropped by the cap
        (y, 1, 2, 200), // 9 -- the 10th found: dropped by the cap
    ]
}

fn build_prg() -> (Vec<u8>, u16) {
    const PRG_BASE: u16 = 0x8000;
    let mut prg = Vec::new();

    // --- OAM, in two passes (a real "clear off-screen, then populate"
    // pattern, not just a test convenience):
    //
    // Pass 1: $2003 <- 40 (this fixture's 10 real sprites occupy bytes
    // 0-39), then $FF into the remaining 216 bytes via $2004. Y=$FF is
    // never in range for any visible scanline (255 > the max scanline,
    // 239, regardless of 8x8/8x16 height) -- without this, `Ppu::new`'s
    // zero-filled OAM leaves primary indices 10-63 at Y=0, which IS
    // "in range" at scanline 0 (0 >= 0 && 0 < 0+8), quietly finding 8+
    // phantom sprites and setting the overflow flag for a reason that has
    // nothing to do with this fixture's real 9th sprite -- which would
    // make this file's overflow-flag assertion pass even if the real
    // 8-sprite-cap/overflow-scan code were deleted outright.
    //
    // Pass 2: $2003 <- 0, then the 10 real sprites via $2004, ending
    // OAMADDR at 0x28 (40) -- non-zero, the ticket's own trap.
    lda_imm(&mut prg, 40);
    sta_abs(&mut prg, 0x2003);
    lda_imm(&mut prg, 0xFF);
    for _ in 0..(256 - 40) {
        sta_abs(&mut prg, 0x2004);
    }
    lda_imm(&mut prg, 0x00);
    sta_abs(&mut prg, 0x2003);
    for &(y, tile, attr, x) in &sprite_table() {
        for byte in [y, tile, attr, x] {
            lda_imm(&mut prg, byte);
            sta_abs(&mut prg, 0x2004);
        }
    }

    // --- palette: $2006 -> $3F00, then 32 bytes. Index 0 = backdrop;
    // indices 0x11/0x15/0x19/0x1D = sprite palette groups 0-3's
    // pattern-1 slot (never index 0x10/0x14/0x18/0x1C of each group --
    // pattern is never 0 in this fixture, so those entry-0-aliased slots
    // are never read and are left at 0). ---
    lda_imm(&mut prg, 0x3F);
    sta_abs(&mut prg, 0x2006);
    lda_imm(&mut prg, 0x00);
    sta_abs(&mut prg, 0x2006);
    for i in 0u8..32 {
        let value = match i {
            // $3F10/$14/$18/$1C alias $3F00/$04/$08/$0C (nesdev.org/wiki/
            // PPU_palettes' entry-0-shared-storage quirk, also exercised in
            // `crate::ppu::mem`'s tests) -- must agree with index 0's value
            // or this later write in the same loop clobbers it.
            0x00 | 0x10 => BACKDROP_VALUE,
            0x11 => GROUP0_VALUE,
            0x15 => GROUP1_VALUE,
            0x19 => GROUP2_VALUE,
            0x1D => GROUP3_VALUE,
            _ => 0x00,
        };
        lda_imm(&mut prg, value);
        sta_abs(&mut prg, 0x2007);
    }

    // --- PPUCTRL = 0 (8x8 sprites, sprite pattern table at $0000 --
    // matches `build_chr`'s tile 1 placement) ---
    lda_imm(&mut prg, 0x00);
    sta_abs(&mut prg, 0x2000);

    // --- PPUMASK: show sprites + show sprites in the leftmost 8 pixels.
    // Background is deliberately left OFF -- this fixture is sprites-only,
    // so every non-sprite pixel is unambiguously the backdrop. ---
    lda_imm(&mut prg, 0x14);
    sta_abs(&mut prg, 0x2001);

    // --- idle loop ---
    let idle_addr = PRG_BASE + prg.len() as u16;
    jmp_abs(&mut prg, idle_addr);

    (prg, idle_addr)
}

fn build_rom() -> (Vec<u8>, u16) {
    let (mut prg, idle_addr) = build_prg();
    const PRG_BANK_SIZE: usize = 16 * 1024;
    assert!(
        prg.len() + 4 <= PRG_BANK_SIZE,
        "fixture program ({} bytes) must fit in one 16 KiB PRG bank, with room for the reset vector",
        prg.len()
    );
    prg.resize(PRG_BANK_SIZE, 0xEA);
    prg[PRG_BANK_SIZE - 4] = 0x00;
    prg[PRG_BANK_SIZE - 3] = 0x80;

    let mut rom = Vec::new();
    rom.extend_from_slice(&rf_cart::nes::INES_MAGIC);
    rom.push(1); // 1 PRG bank
    rom.push(1); // 1 CHR bank
    rom.extend_from_slice(&[0u8; 10]); // mapper 0, horizontal mirroring
    rom.extend_from_slice(&prg);
    rom.extend_from_slice(&build_chr());
    (rom, idle_addr)
}

// ---------------------------------------------------------------------
// Capture: identical permissive-recorder + full-frame-finder pattern to
// `golden_frame_bg.rs` (a `CoreSink` that doesn't panic on the
// scanline-0 wraparound between frames, plus a scan for the first clean
// contiguous 0..239 run).
// ---------------------------------------------------------------------

#[derive(Default)]
struct PermissiveSink {
    rows: Vec<(u16, Vec<PpuPixel>)>,
}

impl CoreSink for PermissiveSink {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        self.rows.push((y, pixels.to_vec()));
    }
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _ev: CoreEvent) {}
}

fn find_full_frame(rows: &[(u16, Vec<PpuPixel>)], height: usize) -> Option<usize> {
    for start in 0..rows.len() {
        if rows[start].0 != 0 {
            continue;
        }
        if start + height > rows.len() {
            return None;
        }
        let is_full_run = rows[start..start + height]
            .iter()
            .enumerate()
            .all(|(i, (y, _))| *y == i as u16);
        if is_full_run {
            return Some(start);
        }
    }
    None
}

#[test]
fn synthetic_nrom_sprites_match_the_analytically_computed_golden_frame() {
    let (rom_bytes, idle_addr) = build_rom();
    let mut bus = NesBus::from_ines_bytes(&rom_bytes).expect("valid synthetic NROM image");
    let mut cpu = Cpu::power_on(&mut bus);

    const SETUP_SAFETY_CAP: u32 = 200_000;
    let mut setup_steps = 0u32;
    while cpu.pc != idle_addr {
        cpu.step(&mut bus);
        setup_steps += 1;
        assert!(
            setup_steps < SETUP_SAFETY_CAP,
            "fixture setup never reached its idle loop (pc={:#06x}, expected {:#06x})",
            cpu.pc,
            idle_addr
        );
    }

    let mut discard = PermissiveSink::default();
    bus.drain_video(&mut discard);

    // Sampled (not a single peek at the end): the overflow flag is only
    // held from sprite 8's own scanline-99 phase-2 check through the NEXT
    // pre-render line's dot-1 auto-clear -- most, but not all, of a
    // frame's dot span. A single peek after a fixed step count can land in
    // the brief cleared window and prove nothing; sampling across ~5
    // frames' worth of steps (60,000 CPU steps, chunked) makes landing
    // inside the SET window ~5x over, not a coin flip.
    let mut overflow_ever_observed = false;
    for _ in 0..60 {
        for _ in 0..1_000 {
            cpu.step(&mut bus);
        }
        if bus.peek(0x2002) & 0x20 != 0 {
            overflow_ever_observed = true;
        }
    }

    let mut capture = PermissiveSink::default();
    bus.drain_video(&mut capture);

    let start = find_full_frame(&capture.rows, SCREEN_HEIGHT as usize)
        .expect("at least one complete 240-scanline frame after the idle loop was reached");
    let frame_a = &capture.rows[start..start + SCREEN_HEIGHT as usize];

    let mut actual: Vec<Vec<u8>> = Vec::with_capacity(SCREEN_HEIGHT as usize);
    for (y, pixels) in frame_a {
        assert_eq!(
            pixels.len(),
            SCREEN_WIDTH as usize,
            "scanline {y} pixel count"
        );
        actual.push(pixels.iter().map(|p| p.palette_index).collect());
        for (x, p) in pixels.iter().enumerate() {
            assert!(
                !p.dropped_by_limit,
                "dropped_by_limit is always false on the NES path (ruling) -- scanline {y} pixel {x}"
            );
        }
    }

    let expected = expected_frame();

    // Pixel-level diagnostics first, same discipline as golden_frame_bg.rs:
    // a hash mismatch alone doesn't say WHERE the pipeline went wrong.
    for y in 0..SCREEN_HEIGHT as usize {
        for x in 0..SCREEN_WIDTH as usize {
            assert_eq!(
                actual[y][x], expected[y][x],
                "pixel ({x}, {y}): see this file's module doc for what a mismatch at \
                 x=50/150/200 on row {TARGET_ROW} specifically means"
            );
        }
    }

    let expected_hash = hash_frame_palette_indices(&expected, SCREEN_HEIGHT as usize);
    let actual_hash = hash_frame_palette_indices(&actual, SCREEN_HEIGHT as usize);
    assert_eq!(expected_hash, actual_hash);

    // Overflow flag, straightforward for this fixture (module doc):
    // sprite 8's real Y is genuinely in range, so the very first
    // overflow-phase check (m=0) finds it directly -- no drift needed to
    // demonstrate the flag firing end-to-end through the real pipeline.
    assert!(
        overflow_ever_observed,
        "sprite overflow flag must be set at some point: a genuine 9th in-range sprite exists"
    );

    // Static-scene stability, using `find_first_divergence` as its own
    // 2-checkpoint sequence (this fixture's OAM/palette never change after
    // setup, so every complete frame must hash identically).
    if start + 2 * SCREEN_HEIGHT as usize <= capture.rows.len() {
        let frame_b =
            &capture.rows[start + SCREEN_HEIGHT as usize..start + 2 * SCREEN_HEIGHT as usize];
        let is_contiguous = frame_b.iter().enumerate().all(|(i, (y, _))| *y == i as u16);
        assert!(is_contiguous, "frame B must also be a clean 0..239 run");
        let frame_b_indices: Vec<Vec<u8>> = frame_b
            .iter()
            .map(|(_, pixels)| pixels.iter().map(|p| p.palette_index).collect())
            .collect();
        let frame_b_hash = hash_frame_palette_indices(&frame_b_indices, SCREEN_HEIGHT as usize);
        let divergence =
            find_first_divergence(&[(0u64, actual_hash.clone())], &[(0u64, frame_b_hash)]);
        assert_eq!(
            divergence, None,
            "a static scene must render identically frame to frame"
        );
    }
}
