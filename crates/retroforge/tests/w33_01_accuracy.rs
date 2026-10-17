//! Ticket W33-01 / law 6: the widescreen channels never touch the accuracy
//! stream. A real SNES core is run twice from power-on over the same frames,
//! the bundle builder capturing the sub-screen and per-line registers the
//! second time; `FrameBundle::video` (and the hash the mode-invariant harness
//! takes of it) must match frame for frame, with the channels empty when not
//! asked and filled when asked.

use retroforge::mode_invariant::hash_indexed_video;
use rf_core_api::{
    CoreEvent, CoreSink, EmulatorCore, FrameBundleBuilder, InputFrame, LineRegisters, PpuPixel,
    SubPixel,
};
use rf_snes::core::SnesCore;

struct Sink(FrameBundleBuilder);

impl CoreSink for Sink {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        self.0.video_scanline(y, pixels);
    }
    fn video_scanline_widened(&mut self, y: u16, pixels: &[PpuPixel], pad: usize) {
        let w = usize::from(self.0.width());
        self.0
            .video_scanline(y, pixels.get(pad..pad + w).unwrap_or(pixels));
    }
    fn sub_scanline(&mut self, y: u16, pixels: &[SubPixel], fixed: u16) {
        self.0.sub_scanline(y, pixels, fixed);
    }
    fn wants_line_registers(&self) -> bool {
        self.0.wants_line_registers()
    }
    fn line_registers(&mut self, y: u16, regs: &LineRegisters) {
        self.0.line_registers(y, regs);
    }
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _event: CoreEvent) {}
}

fn run(capture: bool, wide: bool) -> Vec<(String, usize, usize)> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/snes/rf-scroller-s/build/rf-scroller-s.sfc");
    let rom = std::fs::read(path).expect("SNES fixture");
    let mut core = SnesCore::load(&rom).expect("valid image");
    if wide {
        core.set_widescreen(512, rf_snes::ppu::WidenMask::ALL);
    }
    let mut sink = Sink(FrameBundleBuilder::new(256, 224));
    sink.0.set_capture_wide(capture);
    (0..40)
        .map(|n| {
            core.run_frame(&InputFrame::empty(), &mut sink);
            let b = sink.0.take(n);
            (
                hash_indexed_video(&b.video),
                b.lines.iter().flatten().count(),
                b.sub.len(),
            )
        })
        .collect()
}

#[test]
fn capturing_the_widescreen_channels_leaves_the_accuracy_hash_alone() {
    for wide in [false, true] {
        let off = run(false, wide);
        let on = run(true, wide);
        let hashes =
            |v: &[(String, usize, usize)]| v.iter().map(|f| f.0.clone()).collect::<Vec<_>>();
        assert_eq!(hashes(&off), hashes(&on), "video hash, wide = {wide}");
        assert!(
            off.iter().all(|f| f.1 == 0 && f.2 == 0),
            "not asked: channels empty (wide = {wide})"
        );
        assert!(
            on.iter().all(|f| f.1 == 224 && f.2 == 224),
            "asked: one register set per visible line, a slot per line (wide = {wide})"
        );
    }
}
