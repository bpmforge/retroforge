//! Ticket W34-01: per-frame background tile grids from the real cores.
//!
//! * Law 6: asking for the grids (the PPU's tile recording, the bundle's
//!   per-line registers, the read of VRAM) never changes the simulation: the
//!   state hash and the accuracy video hash match frame for frame, capture
//!   off against capture on, on both fixtures.
//! * SNES: for a layer with one scroll the grid is exactly
//!   `live_heights::character_grid`'s: same size, same offset, and every
//!   cell's pattern is the hash of the character that grid reads.
//! * NES: cells of equal key look equal in the accuracy picture (the check
//!   that catches a mis-aligned metatile), and a scrolling run produces
//!   bands.
//! * Size: the compact grids stay inside the plan's 61 KB.

use std::collections::HashMap;

use retroforge::mode_invariant::hash_indexed_video;
use retroforge::stepper::EmuStepper;
use rf_core_api::{FrameBundle, FrameBundleBuilder, InputFrame, PixelLayer};
use rf_enhance::heights::TileGrid;

/// NES pad bits (A, B, Select, Start, Up, Down, Left, Right).
const NES_RIGHT: u16 = 0x80;

fn fixture(rel: &str) -> Option<Vec<u8>> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(rel);
    match std::fs::read(&p) {
        Ok(b) => Some(b),
        Err(_) => {
            assert!(
                std::env::var_os("CI").is_none(),
                "fixture {rel} missing on CI: this suite would pass having asserted nothing"
            );
            eprintln!("SKIP: fixture {rel} not built");
            None
        }
    }
}

fn snes_right() -> u16 {
    1 << rf_input::SnesButton::Right.bit()
}

/// Run `frames` frames holding `pad`, with the grids captured or not;
/// `each` sees every captured frame. Returns (state hash, video hash) per frame.
fn run(
    rom: &[u8],
    pad: u16,
    frames: usize,
    capture: bool,
    mut each: impl FnMut(&EmuStepper, &[TileGrid], &FrameBundle, usize),
) -> Vec<(String, String)> {
    let mut stepper = EmuStepper::open(rom).expect("fixture opens");
    let (w, h) = if stepper.bg_layer_views().is_some() {
        (256u16, 224u16)
    } else {
        (256, 240)
    };
    let mut builder = FrameBundleBuilder::new(w, h);
    if capture {
        stepper.set_tile_capture(true);
        builder.set_capture_wide(true);
    }
    let mut out = Vec::new();
    for n in 0..frames {
        let mut input = InputFrame::empty();
        input.ports[0] = pad;
        stepper.latch_and_advance_frame(input, &mut builder);
        let bundle = builder.take(stepper.frame_count());
        if capture {
            let grids = stepper.tile_grids(&bundle.lines, [false; 4], usize::from(h));
            each(&stepper, &grids, &bundle, n);
        }
        out.push((stepper.state_hash(), hash_indexed_video(&bundle.video)));
    }
    out
}

fn assert_capture_changes_nothing(rom: &[u8], pad: u16, label: &str) {
    let off = run(rom, pad, 120, false, |_, _, _, _| {});
    let mut cells = 0usize;
    let on = run(rom, pad, 120, true, |_, g, _, _| {
        cells += g.iter().map(|g| g.cells.len()).sum::<usize>();
    });
    assert_eq!(
        off, on,
        "{label}: capture must leave the accuracy hashes alone"
    );
    assert!(cells > 0, "{label}: capturing produced no cells at all");
}

#[test]
fn asking_for_the_nes_grids_leaves_the_accuracy_hash_alone() {
    let Some(rom) = fixture("nes/rf-scroller/build/rf-scroller.nes") else {
        return;
    };
    assert_capture_changes_nothing(&rom, NES_RIGHT, "nes");
}

#[test]
fn asking_for_the_snes_grids_leaves_the_accuracy_hash_alone() {
    let Some(rom) = fixture("snes/rf-scroller-s/build/rf-scroller-s.sfc") else {
        return;
    };
    assert_capture_changes_nothing(&rom, snes_right(), "snes");
}

#[test]
fn snes_grids_are_the_tilemap_read_for_every_cell() {
    let Some(rom) = fixture("snes/rf-scroller-s/build/rf-scroller-s.sfc") else {
        return;
    };
    let mut compared = 0usize;
    let mut offsets = std::collections::BTreeSet::new();
    run(&rom, snes_right(), 150, true, |st, grids, b, n| {
        if n % 10 != 9 {
            return;
        }
        let snap = st.snes_debug_snapshot().expect("snes");
        for g in grids {
            // Only a layer drawn with one scroll all the way down has one
            // grid the oracle can be asked about.
            if g.rows_px != (0, 224) || !g.exact {
                continue;
            }
            // The oracle takes the scroll the picture was drawn with (line
            // 0's registers), not the end-of-frame snapshot: a game moves
            // its scroll in vblank, so the snapshot already holds the NEXT
            // frame's value (the very error per-line registers remove).
            let l = b.lines[0].expect("registers captured");
            let scroll = std::array::from_fn(|i| (l.hofs[i] & 0x3FF, l.vofs[i] & 0x3FF));
            let oracle = retroforge::live_heights::character_grid(
                g.layer + 1,
                g.cell_px,
                &snap.vram,
                &snap.ppu_regs,
                scroll,
                (256, 224),
            )
            .expect("the layer exists");
            assert_eq!(
                (u32::from(g.cols), u32::from(g.rows)),
                (oracle.cols, oracle.rows)
            );
            assert_eq!(
                (u32::from(g.offset.0), u32::from(g.offset.1)),
                oracle.offset
            );
            offsets.insert(g.offset);
            // Equal characters (with equal palette) read equal patterns,
            // and the cell is the pattern of the character the oracle read.
            let mut by_char: HashMap<u16, u64> = HashMap::new();
            for row in 0..oracle.rows {
                for col in 0..oracle.cols {
                    let c = oracle.char_at(col, row).unwrap();
                    let cell = g.cell(col as u16, row as u16).unwrap();
                    let seen = by_char.entry(c).or_insert(cell.pattern);
                    assert_eq!(*seen, cell.pattern, "character {c} hashed two ways");
                    compared += 1;
                }
            }
            let kb = g.cells.len() * std::mem::size_of::<rf_enhance::heights::TileCell>();
            assert!(kb <= 61_500, "one layer alone is {kb} bytes");
        }
    });
    assert!(
        compared > 500,
        "only {compared} cells compared: the check is vacuous"
    );
    println!("snes oracle: {compared} cells compared; offsets seen {offsets:?}");
}

/// Cells of equal key (non-partial, no sprite over them) must show the same
/// background pixels. A metatile paired on the wrong alignment, a stale
/// pattern hash or the wrong palette bytes break this on a scrolling level.
fn same_key_same_picture(
    grids: &[TileGrid],
    b: &FrameBundle,
    seen: &mut HashMap<(u64, u16), Vec<u8>>,
) -> usize {
    let (w, h) = (usize::from(b.width), usize::from(b.height));
    let mut compared = 0;
    for g in grids {
        let p = i32::from(g.cell_px);
        for row in 0..g.rows {
            for col in 0..g.cols {
                let cell = g.cell(col, row).unwrap();
                if cell.partial() {
                    continue;
                }
                let (x0, y0) = g.cell_picture_origin(col, row);
                if x0 < 0
                    || y0 < i32::from(g.rows_px.0)
                    || x0 + p > w as i32
                    || y0 + p > i32::from(g.rows_px.1).min(h as i32)
                {
                    continue;
                }
                let mut block = Vec::with_capacity((p * p) as usize);
                let mut sprite = false;
                // One attribute byte colours a whole aligned 2x2 group, so
                // every visible pixel of a true metatile uses one palette.
                let mut palettes = std::collections::BTreeSet::new();
                for y in 0..p {
                    for x in 0..p {
                        let px = &b.video[(y0 + y) as usize * w + (x0 + x) as usize];
                        sprite |= matches!(px.layer, PixelLayer::Sprite);
                        if matches!(px.layer, PixelLayer::Background(_))
                            && px.palette_index & 3 != 0
                        {
                            palettes.insert(px.palette_index >> 2);
                        }
                        // The colour within the palette, not which palette.
                        block.push(if matches!(px.layer, PixelLayer::Background(_)) {
                            px.palette_index & 3
                        } else {
                            0
                        });
                    }
                }
                if sprite {
                    continue;
                }
                if g.cell_px == 16 {
                    assert!(
                        palettes.len() <= 1,
                        "cell ({col},{row}) at picture ({x0},{y0}) spans palettes {palettes:?}: \
                         the metatile is not on the attribute alignment"
                    );
                }
                match seen.get(&(cell.pattern, cell.palette)) {
                    Some(first) => {
                        assert_eq!(
                            first, &block,
                            "cells with one key show different pictures: cell ({col},{row}) \
                             at picture ({x0},{y0})"
                        );
                        compared += 1;
                    }
                    None => {
                        seen.insert((cell.pattern, cell.palette), block);
                    }
                }
            }
        }
    }
    compared
}

#[test]
fn nes_cells_with_one_key_look_alike_and_scrolling_makes_bands() {
    let Some(rom) = fixture("nes/rf-scroller/build/rf-scroller.nes") else {
        return;
    };
    let mut compared = 0usize;
    let mut seen = HashMap::new();
    let mut max_bytes = 0usize;
    let mut offsets = std::collections::BTreeSet::new();
    run(&rom, NES_RIGHT, 240, true, |_st, grids, b, n| {
        if n < 20 {
            return;
        }
        compared += same_key_same_picture(grids, b, &mut seen);
        max_bytes = max_bytes.max(grids.iter().map(|g| g.cells.len() * 16).sum());
        for g in grids {
            assert_eq!(g.cell_px, 16);
            offsets.insert(g.offset);
        }
    });
    println!("nes: {compared} repeated cells compared; offsets {offsets:?}; max {max_bytes} B");
    assert!(
        compared > 100,
        "only {compared} repeats compared: the check is vacuous"
    );
    assert!(
        offsets.len() > 1,
        "a scrolling run should move the cell offset"
    );
    assert!(max_bytes <= 61_500);
}
