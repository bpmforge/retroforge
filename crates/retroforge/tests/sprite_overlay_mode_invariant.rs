//! Ticket W3-05a's `EmuStepper::state_hash()`-based mode-invariant check
//! (FR-MODE-002) — explicitly asked for in the ticket ("you already have
//! the tool... compare an overlay-on run against an overlay-off run frame
//! by frame").
//!
//! ## Why this is the WEAKER of the two mode-invariant checks — read
//! before trusting it
//!
//! Kept alongside, not instead of, the stronger PPU-internal-state
//! comparison in `crates/rf-nes/src/ppu/tests/sprite_overlay.rs`'s
//! `more_than_8_sprites_on_one_scanline_satisfies_all_four_acceptance_criteria_at_once`.
//! `EmuStepper::state_hash()`'s own doc (`crates/retroforge/src/stepper.rs`)
//! is explicit about its coverage: WRAM, raw OAM (256 bytes), PRG-RAM, CPU
//! registers, `master_cycle`, `frame_count`. It does **not** cover
//! `self.status` (`$2002` — sprite-0 hit, the overflow flag),
//! `secondary_oam`, `active_sprites`, or the rendered framebuffer
//! (explicitly named "output, not state" in that doc). The sprite-limit-
//! bypass overlay, by design (ticket W3-05a), only ever touches
//! PPU-internal fields plus a brand-new `overlay_line_buffer` nothing else
//! reads — so a mutation that perturbed ONLY those fields would leave
//! every byte this hash covers untouched, and this test would still pass.
//! **Verified directly**, not assumed (ticket's mutation-test notes):
//! temporarily making `record_overlay_sprites` also set
//! `STATUS_SPRITE0_HIT` (the ticket's "bump the overflow flag" case, one
//! bit over) made the PPU-level test's `status` equality assertion FAIL
//! immediately (`left: 32, right: 96`), while THIS file's `state_hash`
//! test kept passing unchanged under the identical mutation — `self.status`
//! is simply not one of the bytes `state_hash()` hashes. (A literal
//! "quietly raise the 8-sprite cap" mutation was tried first and rejected
//! as the demonstration here: with fixed-size `[EvaluatedSprite; 8]` /
//! `[SpriteUnit; 8]` backing arrays, that mutation panics with an
//! out-of-bounds write rather than silently corrupting state, which fails
//! every test — including this one — for the wrong, uninteresting reason.
//! The status-flag mutation above is the one that actually isolates what
//! this test does and doesn't see.)
//!
//! This test therefore proves a real but narrower thing: the overlay does
//! not perturb CPU-visible machine state (WRAM/OAM/PRG-RAM/registers/
//! timing) — worth having on its own (it is FR-MODE-002's criterion as
//! literally written), but it is not, by itself, sufficient evidence
//! against a limit-bypass mutation. Report both results, never just this
//! one.
use retroforge::stepper::EmuStepper;

const INES_MAGIC: [u8; 4] = [0x4E, 0x45, 0x53, 0x1A]; // "NES\x1A"

/// A hand-assembled NROM program that writes 10 sprites (Y=50, tile=1,
/// attr=0, non-overlapping X) into OAM via an indexed `$2004` write loop,
/// enables sprite rendering, then loops forever — a real ROM that
/// genuinely exercises `evaluate_sprites`/`record_overlay_sprites`/
/// `output_pixel` every frame from frame 1 onward, unlike
/// `crates/retroforge/tests/determinism.rs`'s two fixtures (one never
/// enables rendering at all — "Rendering is never enabled" is that file's
/// own module-doc quote — the other is `BRK`-forever). Verified
/// instruction-by-instruction (addresses, opcode lengths, the one branch
/// offset) against the 6502 ISA before use, same discipline
/// `determinism.rs`'s own fixture doc uses — not trusted blindly; see
/// [`fixture_actually_writes_the_expected_sprite_table_into_oam`] below for
/// the actual proof the encoding is right.
///
/// | Addr   | Bytes       | Instruction              |
/// |--------|-------------|--------------------------|
/// | $8000  | 78          | SEI                      |
/// | $8001  | A2 FF       | LDX #$FF                 |
/// | $8003  | 9A          | TXS                      |
/// | $8004  | A9 00       | LDA #$00                 |
/// | $8006  | 8D 03 20    | STA $2003 (OAMADDR = 0)  |
/// | $8009  | A2 00       | LDX #$00                 |
/// | $800B  | BD 1E 80    | loop: LDA $801E,X        |
/// | $800E  | 8D 04 20    | STA $2004                |
/// | $8011  | E8          | INX                      |
/// | $8012  | E0 28       | CPX #$28 (40 = 10*4 bytes) |
/// | $8014  | D0 F5       | BNE loop                 |
/// | $8016  | A9 10       | LDA #$10                 |
/// | $8018  | 8D 01 20    | STA $2001 (show sprites) |
/// | $801B  | 4C 1B 80    | forever: JMP forever     |
/// | $801E  | ...         | sprite table, 40 bytes (below) |
#[rustfmt::skip]
const PROGRAM: &[(u16, &[u8])] = &[
    (0x8000, &[0x78]),
    (0x8001, &[0xA2, 0xFF]),
    (0x8003, &[0x9A]),
    (0x8004, &[0xA9, 0x00]),
    (0x8006, &[0x8D, 0x03, 0x20]),
    (0x8009, &[0xA2, 0x00]),
    (0x800B, &[0xBD, 0x1E, 0x80]),
    (0x800E, &[0x8D, 0x04, 0x20]),
    (0x8011, &[0xE8]),
    (0x8012, &[0xE0, 0x28]),
    (0x8014, &[0xD0, 0xF5]),
    (0x8016, &[0xA9, 0x10]),
    (0x8018, &[0x8D, 0x01, 0x20]),
    (0x801B, &[0x4C, 0x1B, 0x80]),
];

const SPRITE_TABLE_ADDR: u16 = 0x801E;

/// 10 sprites, contiguous OAM order, all Y=50 (in-range for scanline 50 —
/// more than the 8-per-scanline limit, same contiguous-layout discipline
/// `crates/rf-nes/src/ppu/tests/sprite_overlay.rs` uses so the buggy
/// overflow scan reliably trips rather than legitimately missing it), tile
/// 1 (filled solid below), non-overlapping X (8, 17, 26, ...).
fn sprite_table() -> Vec<u8> {
    let mut table = Vec::with_capacity(40);
    for i in 0u8..10 {
        table.extend_from_slice(&[50, 1, 0, 8 + i * 9]);
    }
    table
}

fn many_sprites_rom() -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&INES_MAGIC);
    data.push(1); // 1x16KiB PRG
    data.push(1); // 1x8KiB CHR
    data.extend_from_slice(&[0u8; 10]); // flags6/7 + 8 reserved => mapper 0

    let mut prg = vec![0u8; 16 * 1024];
    for (addr, bytes) in PROGRAM {
        let offset = (*addr - 0x8000) as usize;
        prg[offset..offset + bytes.len()].copy_from_slice(bytes);
    }
    let table = sprite_table();
    let table_offset = (SPRITE_TABLE_ADDR - 0x8000) as usize;
    prg[table_offset..table_offset + table.len()].copy_from_slice(&table);
    prg[0x3FFC] = 0x00; // reset vector low  -> $8000
    prg[0x3FFD] = 0x80; // reset vector high
    data.extend(prg);

    let mut chr = vec![0u8; 8 * 1024];
    // Tile 1: solid opaque 8x8 (pattern = 1 everywhere) -- the tile every
    // OAM entry above references, so a real accuracy-mode render of this
    // ROM would genuinely flicker sprites 8-9 (the point of this ticket).
    for row in 0..8 {
        chr[16 + row] = 0xFF;
        chr[16 + 8 + row] = 0x00;
    }
    data.extend(chr);
    data
}

/// Fixture-sanity check (project discipline: prove the fixture does what
/// its doc comment claims before trusting a hash comparison built on top
/// of it — the same lesson `determinism.rs`'s own module doc calls out
/// under "Why the fixture actually reads `$4016`"). One frame is far more
/// than the ~90-cycle setup loop needs, well inside the 29,781-cycle
/// budget.
#[test]
fn fixture_actually_writes_the_expected_sprite_table_into_oam() {
    let mut stepper =
        EmuStepper::from_ines_bytes(&many_sprites_rom()).expect("fixture ROM must be valid");
    let mut sink = rf_renderer::FrameBuffer::new();
    // `EmuStepper` starts mid pre-render scanline (`crates/retroforge/src/
    // stepper.rs`'s own tests document this "boot-artifact partial frame":
    // the very first `step_frame` call only sweeps the tail of that one
    // scanline, ~114 cycles, nowhere near enough for this fixture's
    // ~620-cycle sprite-writing loop to finish) -- skip it, same pattern
    // `determinism.rs`'s fixtures use, before checking a steady-state
    // frame.
    stepper.step_frame(&mut sink); // boot-artifact frame
    stepper.step_frame(&mut sink); // first real frame
    assert_eq!(
        &stepper.oam()[0..40],
        &sprite_table()[..],
        "the indexed $2004 write loop must have copied the sprite table into OAM exactly"
    );
}

fn run_frames(overlay: bool, frames: usize) -> Vec<String> {
    let mut stepper =
        EmuStepper::from_ines_bytes(&many_sprites_rom()).expect("fixture ROM must be valid");
    stepper.set_sprite_overlay_enabled(overlay);
    let mut sink = rf_renderer::FrameBuffer::new();
    let mut hashes = Vec::with_capacity(frames);
    for _ in 0..frames {
        stepper.step_frame(&mut sink);
        hashes.push(stepper.state_hash());
    }
    hashes
}

/// FR-MODE-002, the `state_hash()`-based half — 20 frames, same input log
/// (this fixture takes no input), overlay off vs overlay on, per-frame
/// comparison (not just the final hash — locatable to a frame, same
/// pattern `determinism.rs` uses).
#[test]
fn state_hash_sequence_is_identical_with_the_overlay_on_and_off_over_20_frames() {
    let off = run_frames(false, 20);
    let on = run_frames(true, 20);
    assert_eq!(
        off.len(),
        on.len(),
        "both runs must complete the same number of frames"
    );
    for i in 0..off.len() {
        assert_eq!(
            off[i], on[i],
            "frame {i}: state_hash diverged between overlay on/off -- see this file's \
             module doc for exactly what this hash does and does not cover"
        );
    }
}

/// Sanity check for the toggle itself: default-off, per law 6.
#[test]
fn overlay_is_off_by_default_on_a_freshly_opened_rom() {
    let stepper =
        EmuStepper::from_ines_bytes(&many_sprites_rom()).expect("fixture ROM must be valid");
    assert!(
        !stepper.sprite_overlay_enabled(),
        "law 6: a fresh install boots in Accuracy Mode"
    );
}
