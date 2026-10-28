//! Scratch explorer for the Mega Man 2 script (ROM-gated). Not part of the ticket.
mod w38_08_mm2_common;
use rf_core_api::{EmulatorCore, PixelLayer, PpuPixel, ResetKind, CoreEvent, CoreSink};
use rf_nes::core::NesCore;
use w38_08_mm2_common::*;

struct Cap(Vec<u8>);
impl CoreSink for Cap {
    fn video_scanline(&mut self, y: u16, p: &[PpuPixel]) {
        if self.0.is_empty() { self.0 = vec![0; 256 * 240 * 4]; }
        for (x, q) in p.iter().take(256).enumerate() {
            let [r, g, b] = rf_renderer::palette::NES_PALETTE[usize::from(q.palette_index & 0x3F)];
            let o = (usize::from(y) * 256 + x) * 4;
            if usize::from(y) < 240 { self.0[o..o + 4].copy_from_slice(&[r, g, b, 255]); }
            let _ = PixelLayer::Sprite;
        }
    }
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

#[test]
#[ignore = "explorer"]
fn explore() {
    let raw = load_rom(&rom_path().expect("rom"));
    let script = std::env::var("MM2_SCRIPT").expect("MM2_SCRIPT");
    let shots: Vec<usize> = std::env::var("SHOTS").unwrap_or_default().split(',').filter_map(|s| s.parse().ok()).collect();
    let out = std::env::var("OUT").unwrap_or("/private/tmp/claude-502/-Users-bmatthews-Code/10a3a02b-3b5c-46ad-afc7-f01185980ef7/scratchpad/w38-08".into());
    std::fs::create_dir_all(&out).unwrap();
    let ins = inputs_of(&pads(&script));
    let mut core = NesCore::from_ines_bytes(&raw).unwrap();
    core.reset(ResetKind::Hard);
    let scan = std::env::var("SCAN").is_ok();
    let mut hist: Vec<Vec<u8>> = Vec::new();
    for (f, i) in ins.iter().enumerate() {
        let mut c = Cap(Vec::new());
        if let Ok(a) = std::env::var("POKE") { if f >= 2000 { use rf_nes::CpuBus as _; core.bus_mut().write(u16::from_str_radix(&a, 16).unwrap(), 0x1C); } }
        core.run_frame(i, &mut c);
        if scan && f >= 2000 { hist.push((0..0x800u32).map(|a| core.peek(a)).collect()); }
        if shots.contains(&f) {
            let png = rf_renderer::png::encode_rgba(&c.0, 256, 240);
            std::fs::write(format!("{out}/mm2_{f}.png"), png).unwrap();
        }
    }
    if scan {
        for a in 0..0x800usize {
            let v: Vec<u8> = hist.iter().map(|h| h[a]).collect();
            let (mx, mn) = (*v.iter().max().unwrap(), *v.iter().min().unwrap());
            let dec = v.windows(2).filter(|w| w[1] < w[0] && w[0] <= 0x1C).count();
            let inc = v.windows(2).filter(|w| w[1] > w[0] && w[1] <= 0x1C).count();
            let distinct: std::collections::BTreeSet<u8> = v.iter().copied().collect();
            if mx == 0x1C && distinct.len() >= 3 && distinct.len() <= 29 && dec >= 2 && inc <= 6 && mn <= 4 {
                println!("HEALTH? {a:#x} min {mn} distinct {} dec {dec} inc {inc}", distinct.len());
            }
        }
    }
    println!("ran {} frames", ins.len());
}
