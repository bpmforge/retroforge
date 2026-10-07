//! **SNES pixels are shown in the SNES's own colours** (ticket W7-20).
//!
//! Until W7-20 `rf_renderer::FrameBuffer` resolved every `palette_index`
//! through the NES 2C02 table, so SNES games were drawn in NES colours —
//! measured on a commercial title, every pixel differed from CGRAM. This
//! runs RF-Scroller-S through the real `EmuStepper` (and so through its
//! `CountingSink` wrapper, which has to forward the palette) into a
//! `FrameBuffer`, and checks every pixel against `rf_snes::debug::
//! cgram_rgb` of its index — the debugger's palette view, an independent
//! conversion.

use std::path::Path;

use retroforge::stepper::EmuStepper;
use rf_core_api::{CoreEvent, CoreSink, OverlayPixel, PpuPixel};

const FIXTURE: &str = "../../fixtures/snes/rf-scroller-s/build/rf-scroller-s.sfc";

/// Forwards to a `FrameBuffer` and keeps the indices of the same frame.
struct Both {
    fb: rf_renderer::FrameBuffer,
    indices: Vec<PpuPixel>,
}

impl CoreSink for Both {
    fn palette_scanline(&mut self, y: u16, palette: &[u16], brightness: u8) {
        self.fb.palette_scanline(y, palette, brightness);
    }
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        self.fb.video_scanline(y, pixels);
        self.indices.extend_from_slice(pixels);
    }
    fn overlay_scanline(&mut self, y: u16, pixels: &[OverlayPixel]) {
        self.fb.overlay_scanline(y, pixels);
    }
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _ev: CoreEvent) {}
}

#[test]
fn every_snes_pixel_is_the_cgram_colour_of_its_index() {
    let rom = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE)).expect("fixture");
    let mut stepper = EmuStepper::open(&rom).expect("opens");
    let mut sink = Both {
        fb: rf_renderer::FrameBuffer::with_size(256, 224),
        indices: Vec::new(),
    };
    for _ in 0..60 {
        sink.indices.clear();
        stepper.step_frame(&mut sink);
    }
    let cgram = stepper.snes_debug_snapshot().expect("SNES").cgram;
    assert_eq!(sink.indices.len(), 256 * 224);
    let palettes = sink.fb.line_palettes();
    assert!(
        palettes.iter().all(Option::is_some),
        "every line had a palette"
    );
    let mut distinct = std::collections::BTreeSet::new();
    for (i, (px, rgba)) in sink
        .indices
        .iter()
        .zip(sink.fb.rgba().chunks_exact(4))
        .enumerate()
    {
        let brightness = palettes[i / 256].as_ref().unwrap().brightness;
        let want = rf_renderer::bgr555_to_rgb(
            u16::from_le_bytes([
                cgram[usize::from(px.palette_index) * 2],
                cgram[usize::from(px.palette_index) * 2 + 1],
            ]),
            brightness,
        );
        if brightness == 15 {
            assert_eq!(
                want,
                rf_snes::debug::cgram_rgb(&cgram, px.palette_index),
                "the two conversions agree at full brightness"
            );
        }
        assert_eq!(&rgba[..3], &want, "pixel {i} (index {})", px.palette_index);
        distinct.insert(px.palette_index);
    }
    assert!(distinct.len() > 1, "the frame must not be a single colour");
}

/// The one converter matches the debugger's on every BGR555 word.
#[test]
fn bgr555_matches_the_debuggers_cgram_rgb_on_all_32768_words() {
    for word in 0u16..0x8000 {
        let cgram = word.to_le_bytes();
        assert_eq!(
            rf_renderer::bgr555_to_rgb(word, 15),
            rf_snes::debug::cgram_rgb(&cgram, 0),
            "word {word:#06x}"
        );
    }
}
