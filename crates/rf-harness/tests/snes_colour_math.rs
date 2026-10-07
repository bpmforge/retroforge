//! **SNES colour math reaches the picture** (ticket W7-21).
//!
//! W7-16 added `CoreSink::sub_scanline` and closed saying rf-snes emits
//! it; nothing ever called it, and no sink blended it, so translucency
//! and fades by colour math were never drawn. This boots a synthetic
//! LoROM that turns the screen on with an all-backdrop picture, enables
//! colour math on the backdrop and sets the fixed colour to full red —
//! the classic "add a colour to the whole screen" fade. CGRAM stays zero
//! (black), so without math every pixel is black and with it every pixel
//! is red: the channel is called, and the renderer blends it.

use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, OverlayPixel, PpuPixel, Step, SubPixel};
use rf_snes::core::SnesCore;

fn rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    let code = [
        0xA9, 0x0F, 0x8D, 0x00, 0x21, // LDA #$0F, STA $2100: screen on, full brightness
        0xA9, 0x20, 0x8D, 0x31, 0x21, // LDA #$20, STA $2131: math on the backdrop, add
        0xA9, 0x3F, 0x8D, 0x32, 0x21, // LDA #$3F, STA $2132: fixed colour red 31
        0x80, 0xFE, // BRA *
    ];
    rom[..code.len()].copy_from_slice(&code);
    for (i, b) in b"RF COLOUR MATH TEST  ".iter().enumerate() {
        rom[0x7FC0 + i] = *b;
    }
    rom[0x7FD5] = 0x20; // LoROM, slow
    rom[0x7FD6] = 0x00;
    rom[0x7FD7] = 0x08;
    rom[0x7FFC] = 0x00; // reset -> $8000
    rom[0x7FFD] = 0x80;
    rom
}

/// A `FrameBuffer` that also counts the sub-screen calls it saw.
struct Counting {
    fb: rf_renderer::FrameBuffer,
    subs: usize,
    ops: Vec<rf_core_api::ColorMathOp>,
}

impl CoreSink for Counting {
    fn palette_scanline(&mut self, y: u16, palette: &[u16], brightness: u8) {
        self.fb.palette_scanline(y, palette, brightness);
    }
    fn sub_scanline(&mut self, y: u16, pixels: &[SubPixel], fixed_color: u16) {
        self.subs += 1;
        self.ops.extend(pixels.iter().map(|p| p.op));
        assert_eq!(fixed_color, 0x001F, "red 31 as BGR555");
        self.fb.sub_scanline(y, pixels, fixed_color);
    }
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        self.fb.video_scanline(y, pixels);
    }
    fn overlay_scanline(&mut self, y: u16, pixels: &[OverlayPixel]) {
        self.fb.overlay_scanline(y, pixels);
    }
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _ev: CoreEvent) {}
}

#[test]
fn backdrop_colour_math_adds_the_fixed_colour_to_every_pixel() {
    let mut core = SnesCore::load(&rom()).expect("loads");
    let mut sink = Counting {
        fb: rf_renderer::FrameBuffer::with_size(256, 224),
        subs: 0,
        ops: Vec::new(),
    };
    // A few frames: the program runs in the first, the next frames draw
    // with its registers latched on every line.
    for _ in 0..4 {
        sink.subs = 0;
        sink.ops.clear();
        core.step(Step::Frame, &mut sink);
    }
    assert_eq!(sink.subs, 224, "one sub-screen per visible line");
    assert!(sink
        .ops
        .iter()
        .all(|op| *op == rf_core_api::ColorMathOp::Add));
    for (i, px) in sink.fb.rgba().chunks_exact(4).enumerate() {
        assert_eq!(px, [255, 0, 0, 255], "pixel {i}: black + red 31");
    }
}
