fn main() {
    let dir = std::env::var("RF_PETERLEMON_PPU")
        .unwrap_or_else(|_| "roms/snes/peterlemon-ppu".to_string());
    let rom = std::fs::read(format!("{dir}/StarWars.sfc")).unwrap();
    let mut s = rf_snes::SnesSystem::load(&rom).unwrap();
    let mut best = (0u64, 0usize, 0i16, 0i16);
    let mut last_frame = 0u64;
    for _ in 0..40_000_000u64 {
        if s.step().is_err() { break; }
        let f = s.bus.timing.frame;
        if f != last_frame && f % 20 == 0 && f <= 400 {
            last_frame = f;
            let mut ppu = s.bus.ppu.clone();
            let mut nonzero = 0usize;
            for y in 0..224u16 {
                nonzero += ppu.render_scanline(y).pixels.iter().filter(|p| p.palette_index != 0).count();
            }
            let m = s.bus.ppu.mode7;
            println!("frame {f:4}: non-backdrop={nonzero:6}  a={:5} d={:5} x0={:5} y0={:5} over={}",
                m.a, m.d, m.x0, m.y0, m.screen_over);
            if nonzero > best.1 { best = (f, nonzero, m.a, m.d); }
        }
        if s.bus.timing.frame > 400 { break; }
    }
    println!("best: frame {} with {} non-backdrop pixels (a={} d={})", best.0, best.1, best.2, best.3);
}
