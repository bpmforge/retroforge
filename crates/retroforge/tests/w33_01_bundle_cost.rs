//! Ticket W33-01: what the widescreen channels (sub-screen pixels and
//! per-line registers in `FrameBundle`) cost per frame, measured in process,
//! capture OFF against capture ON, interleaved so machine load hits both
//! alike. Timing, not correctness: `#[ignore]`d.
//!
//! ```text
//! cargo test --release -p retroforge --test w33_01_bundle_cost -- --ignored --nocapture
//! ```
//!
//! Two parts. (1) The SNES core on the RF-Scroller-S fixture through a real
//! `FrameBundleBuilder`: the core-side cost (`Ppu::line_registers` per line
//! plus the builder's `line_registers`/`take`). That fixture has no colour
//! math, so no sub-screen is sent: (2) the builder alone fed the WORST case,
//! a colour-math line on all 224 lines with a 256-dot sub-screen each (what
//! Super Mario World sends), which is the sub-screen copy cost.

use std::time::Instant;

use rf_core_api::{
    ColorMathOp, CoreEvent, CoreSink, EmulatorCore, FrameBundleBuilder, InputFrame, LineRegisters,
    PixelLayer, PpuPixel, SubPixel,
};
use rf_snes::core::SnesCore;

struct Sink {
    bundle: FrameBundleBuilder,
}

impl CoreSink for Sink {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        self.bundle.video_scanline(y, pixels);
    }
    fn sub_scanline(&mut self, y: u16, pixels: &[SubPixel], fixed: u16) {
        self.bundle.sub_scanline(y, pixels, fixed);
    }
    fn wants_line_registers(&self) -> bool {
        self.bundle.wants_line_registers()
    }
    fn line_registers(&mut self, y: u16, regs: &LineRegisters) {
        self.bundle.line_registers(y, regs);
    }
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _event: CoreEvent) {}
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

#[test]
#[ignore = "timing measurement; run deliberately in release"]
fn bundle_channels_cost_per_frame() {
    // (1) The core, capture off vs on.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/snes/rf-scroller-s/build/rf-scroller-s.sfc");
    let rom = std::fs::read(path).expect("SNES fixture");
    let mut core = SnesCore::load(&rom).expect("valid image");
    let mut sink = Sink {
        bundle: FrameBundleBuilder::new(256, 224),
    };
    for _ in 0..30 {
        core.run_frame(&InputFrame::empty(), &mut sink);
    }
    let (mut off, mut on) = (Vec::new(), Vec::new());
    for round in 0..400u64 {
        // Alternate which variant goes first inside a pair, so a drift or a
        // load spike cannot favour one.
        for variant in [round % 2 == 0, round % 2 != 0] {
            sink.bundle.set_capture_wide(variant);
            let t = Instant::now();
            core.run_frame(&InputFrame::empty(), &mut sink);
            let b = sink.bundle.take(round);
            let us = t.elapsed().as_secs_f64() * 1e6;
            std::hint::black_box(&b);
            if variant {
                on.push(us);
            } else {
                off.push(us);
            }
        }
    }
    let (m_off, m_on) = (median(&mut off), median(&mut on));
    println!(
        "CORE frame+take, median of {} each: capture OFF {m_off:.1} us, ON {m_on:.1} us, \
         delta {:.1} us ({:.3} ms)",
        off.len(),
        m_on - m_off,
        (m_on - m_off) / 1000.0
    );

    // (2) The builder alone, worst case: math on every line, a full sub line.
    let px = PpuPixel {
        palette_index: 1,
        layer: PixelLayer::Background(0),
        sprite_id: None,
        priority: 0,
    };
    let line = vec![px; 256];
    let sub = vec![
        SubPixel {
            palette_index: 3,
            layer: PixelLayer::Background(1),
            op: ColorMathOp::Add,
            fixed: false,
            main_black: false,
        };
        // A 32:9 widened line is 256 + 2 * 272 dots; the builder keeps 256.
        800
    ];
    let regs = LineRegisters::default();
    let feed = |b: &mut FrameBundleBuilder, round: u64| {
        let t = Instant::now();
        for y in 0..224u16 {
            if b.wants_line_registers() {
                b.line_registers(y, &regs);
            }
            b.sub_scanline(y, &sub, 0x1234);
            b.video_scanline(y, &line);
        }
        let out = b.take(round);
        std::hint::black_box(&out);
        t.elapsed().as_secs_f64() * 1e6
    };
    let mut builder = FrameBundleBuilder::new(256, 224);
    let (mut off, mut on) = (Vec::new(), Vec::new());
    for round in 0..400u64 {
        for variant in [round % 2 == 0, round % 2 != 0] {
            builder.set_capture_wide(variant);
            let us = feed(&mut builder, round);
            if variant {
                on.push(us);
            } else {
                off.push(us);
            }
        }
    }
    let (m_off, m_on) = (median(&mut off), median(&mut on));
    println!(
        "BUILDER worst case (math on all 224 lines, 800-dot sub line), median of {} each: \
         OFF {m_off:.1} us, ON {m_on:.1} us, delta {:.1} us ({:.3} ms)",
        off.len(),
        m_on - m_off,
        (m_on - m_off) / 1000.0
    );
    // (3) The margin compose on the UI thread: every margin pixel of a 32:9
    // frame (2 x 272 columns x 224 rows), through `margin_color` against the
    // old `front_layer_pixel` + palette lookup, on canvases with cells in
    // three layers.
    use retroforge::layer_canvas::{
        front_layer_pixel, margin_color, LayerCanvases, LayerFrames, LAYERS,
    };
    let mut canvases: [Option<rf_enhance::stitcher::Canvas>; LAYERS] = [None, None, None, None];
    for (n, slot) in canvases.iter_mut().enumerate().take(3) {
        let mut c = rf_enhance::stitcher::Canvas::new();
        let row: Vec<Option<PpuPixel>> = (0..1500)
            .map(|x| {
                (x % (n + 2) != 0).then_some(PpuPixel {
                    palette_index: (x % 200) as u8 + 1,
                    layer: PixelLayer::Background(n as u8),
                    sprite_id: None,
                    priority: 0,
                })
            })
            .collect();
        for y in 0..224 {
            c.blit_row(-600, y, &row);
        }
        *slot = Some(c);
    }
    let layers = LayerCanvases {
        canvases,
        hud: [false; LAYERS],
    };
    let frames = LayerFrames {
        scene: 1,
        origins: [Some((0, 0)); LAYERS],
        row_dx: vec![[0; LAYERS]; 224],
    };
    let mut words = [0u16; 256];
    for (i, w) in words.iter_mut().enumerate() {
        *w = (i as u16 * 37) & 0x7FFF;
    }
    let palette = rf_renderer::LinePalette::from_words(&words, 15);
    let regs = LineRegisters {
        tm: 0x01,
        ts: 0x02,
        math_enable: 0x20,
        half: true,
        use_subscreen: true,
        fixed_color: 0x1111,
        backdrop_color: 0x0421,
        brightness: 15,
        ..LineRegisters::default()
    };
    let (mut old, mut new) = (Vec::new(), Vec::new());
    for _ in 0..100 {
        let t = Instant::now();
        let mut acc = 0u64;
        for row in 0..224usize {
            for dx in (-272i64..0).chain(256..528) {
                let p = front_layer_pixel(&layers, &frames.origins, dx, row as i64, None);
                let rgb =
                    rf_renderer::resolve_index(p.map_or(0, |p| p.palette_index), Some(&palette));
                acc += u64::from(rgb[0]);
            }
        }
        std::hint::black_box(acc);
        old.push(t.elapsed().as_secs_f64() * 1e3);
        let t = Instant::now();
        let mut acc = 0u64;
        for row in 0..224usize {
            for dx in (-272i64..0).chain(256..528) {
                let rgb = margin_color(&layers, &frames, row, dx, None, &regs, &palette);
                acc += u64::from(rgb[0]);
            }
        }
        std::hint::black_box(acc);
        new.push(t.elapsed().as_secs_f64() * 1e3);
    }
    println!(
        "MARGIN COMPOSE (UI thread, 122k margin pixels): old front_layer_pixel {:.2} ms, \
         margin_color {:.2} ms",
        median(&mut old),
        median(&mut new)
    );
    let load = std::process::Command::new("uptime")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    println!("{load}");
}
