//! Ticket W36-06b: the two presentation captures the margin redraw leans on.
//!
//! * `Ppu::bg_under` (NesBus::bg_under): the background colour under every dot, even where a sprite won
//!   it; and it changes nothing the simulation sees (the same program, run with it off and on, ends
//!   with identical state and identical pixels).
//! * `DrawnSprite::oam_index`: which OAM entry a drawn tile came from (nesdev wiki "PPU OAM": entry
//!   i is bytes 4i..4i+3: y, tile, attributes, x; a sprite is drawn from scanline y + 1).

use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PixelLayer, PpuPixel, ResetKind};
use rf_nes::core::NesCore;

/// Sets the backdrop to $21 and sprite palette 0 colour 1 to $30, writes two sprites (entry 0 at
/// x=100 y=50 tile 7; entry 5 at x=120 y=90 tile 7, flipped), turns rendering on and loops.
#[rustfmt::skip]
fn rom() -> Vec<u8> {
    let mut prg = vec![0u8; 16 * 1024];
    let code: &[u8] = &[
        0xA9, 0x3F, 0x8D, 0x06, 0x20, 0xA9, 0x00, 0x8D, 0x06, 0x20, // PPUADDR = $3F00
        0xA9, 0x21, 0x8D, 0x07, 0x20,                               // backdrop = $21
        0xA9, 0x3F, 0x8D, 0x06, 0x20, 0xA9, 0x11, 0x8D, 0x06, 0x20, // PPUADDR = $3F11
        0xA9, 0x30, 0x8D, 0x07, 0x20,                               // sprite colour 1 = $30
        0xA9, 0x00, 0x8D, 0x03, 0x20,                               // OAMADDR = 0
        0xA9, 50, 0x8D, 0x04, 0x20, 0xA9, 7, 0x8D, 0x04, 0x20, 0xA9, 0, 0x8D, 0x04, 0x20, 0xA9, 100, 0x8D, 0x04, 0x20,
        0xA9, 20,  0x8D, 0x03, 0x20,                               // OAMADDR = 20 (entry 5)
        0xA9, 90, 0x8D, 0x04, 0x20, 0xA9, 7, 0x8D, 0x04, 0x20, 0xA9, 0x40, 0x8D, 0x04, 0x20, 0xA9, 120, 0x8D, 0x04, 0x20,
        0xA9, 0x1E, 0x8D, 0x01, 0x20,                               // show background and sprites
    ];
    prg[..code.len()].copy_from_slice(code);
    let forever = 0x8000 + code.len() as u16;
    prg[code.len()..code.len() + 3].copy_from_slice(&[0x4C, forever as u8, (forever >> 8) as u8]);
    prg[0x3FFC] = 0x00;
    prg[0x3FFD] = 0x80;
    let mut chr = vec![0u8; 8 * 1024];
    chr[7 * 16..7 * 16 + 8].fill(0xFF); // tile 7: the whole 8x8 is colour 1 (low plane set)
    let mut out = rf_cart::nes::INES_MAGIC.to_vec();
    out.extend_from_slice(&[1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    out.extend(prg);
    out.extend(chr);
    out
}

/// Every scanline of the last frame.
#[derive(Default)]
struct Frame(Vec<PpuPixel>);

impl CoreSink for Frame {
    fn video_scanline(&mut self, y: u16, p: &[PpuPixel]) {
        if y == 0 {
            self.0.clear();
        }
        self.0.extend_from_slice(p);
    }
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

fn run(capture: bool) -> (NesCore, Frame) {
    let mut core = NesCore::from_ines_bytes(&rom()).expect("loads");
    core.reset(ResetKind::Hard);
    core.bus_mut().set_tile_capture(capture);
    core.bus_mut().set_bg_under_capture(capture);
    let mut frame = Frame::default();
    for _ in 0..4 {
        core.run_frame(&InputFrame::empty(), &mut frame);
    }
    (core, frame)
}

#[test]
fn the_background_under_a_sprite_dot_is_recorded_and_the_sprite_knows_its_oam_entry() {
    let (core, frame) = run(true);
    assert_eq!(frame.0.len(), 256 * 240);
    let under = core.bus().bg_under();
    assert_eq!(under.len(), 256 * 240);
    // Entry 0 covers x 100..108, y 51..59 with colour $30; the backdrop under it is $21.
    let at = |x: usize, y: usize| y * 256 + x;
    let p = frame.0[at(104, 55)];
    assert_eq!(
        (p.layer, p.sprite_id, p.palette_index),
        (PixelLayer::Sprite, Some(0), 0x30)
    );
    assert_eq!(
        under[at(104, 55)],
        0x21,
        "the background colour under the sprite"
    );
    // Outside any sprite the two agree (the backdrop).
    assert_eq!(frame.0[at(10, 10)].palette_index, 0x21);
    assert_eq!(under[at(10, 10)], 0x21);
    // Entry 5 is drawn from its own index.
    let q = frame.0[at(124, 95)];
    assert_eq!((q.layer, q.sprite_id), (PixelLayer::Sprite, Some(5)));
    // The drawn-tile list names the entries.
    let drawn = core.bus().completed_sprites();
    let first = drawn.iter().find(|s| s.oam_index == 0).expect("entry 0");
    assert_eq!((first.x, first.y, first.tile), (100, 51, 7));
    let sixth = drawn.iter().find(|s| s.oam_index == 5).expect("entry 5");
    assert_eq!((sixth.x, sixth.y, sixth.flip_x), (120, 91, true));
}

#[test]
fn the_capture_is_off_by_default_and_changes_nothing_the_machine_sees() {
    let (off, frame_off) = run(false);
    assert!(off.bus().bg_under().is_empty(), "allocated only on request");
    let (on, frame_on) = run(true);
    assert_eq!(frame_off.0, frame_on.0, "identical pixels");
    // Identical state: the full-machine hash over the required chunks.
    let hash = |c: &NesCore| {
        let mut sum = 0xcbf2_9ce4_8422_2325_u64;
        for a in 0..0x800u32 {
            sum = (sum ^ u64::from(c.peek(a))).wrapping_mul(0x0100_0000_01b3);
        }
        for b in c.bus().oam().iter() {
            sum = (sum ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
        }
        sum
    };
    assert_eq!(hash(&off), hash(&on));
    assert_eq!(off.bus().frame_count(), on.bus().frame_count());
}
