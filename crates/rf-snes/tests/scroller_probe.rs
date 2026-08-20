use rf_snes::SnesSystem;
#[test]
#[ignore]
fn probe() {
    let rom = std::fs::read(std::env::var("RF_SCROLLER_S").unwrap()).unwrap();
    let out = std::env::var("RF_DUMP_DIR").unwrap();
    let mut s = SnesSystem::load(&rom).expect("loads");
    for (label, until) in [("t0", 90u64), ("t1", 600)] {
        while s.bus.timing.frame < until {
            s.step().unwrap();
            s.bus.joypads.ports[0] = 0x0100; // hold Right
        }
        let pal = s.palette_rgb();
        let mut ppm = b"P6\n256 224\n255\n".to_vec();
        for y in 0..224u16 {
            for px in s.bus.ppu.render_scanline(y).pixels {
                ppm.extend_from_slice(&pal[px.palette_index as usize]);
            }
        }
        std::fs::write(format!("{out}/scroller_{label}.ppm"), ppm).unwrap();
    }
}
