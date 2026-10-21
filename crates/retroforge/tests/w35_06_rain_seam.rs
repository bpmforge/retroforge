//! W35-06 probe (ROM-gated): the registers ALttP's rain frame carries.
//! `cargo test --release -p retroforge --test w35_06_rain_seam -- --ignored --nocapture`

#[allow(dead_code)]
mod w33_07_common;

use rf_core_api::InputFrame;
use w33_07_common::*;

#[test]
#[ignore = "probe"]
fn rain_registers() {
    let Some(mut rig) = Rig::open(ALTTP.0, ALTTP.1, None) else {
        return;
    };
    rig.script(&alttp_to_free_play());
    for n in 0..3 {
        rig.st
            .latch_and_advance_frame(InputFrame::empty(), &mut rig.sink);
        let b = rig.sink.bundle.take(rig.st.frame_count());
        println!("frame {n} lines {} sub {}", b.lines.len(), b.sub.len());
        if n == 2 {
            let y = 100usize;
            let pal = rig.sink.fb.line_palettes()[y].clone().unwrap();
            let sub = b.sub[y].as_ref().unwrap();
            for x in (0..256usize).step_by(24) {
                let m = b.video[y * 256 + x];
                let sp = sub.pixels[x];
                let rgba = &rig.sink.fb.rgba()[(y * rig.sink.fb.width() + x) * 4..][..3];
                let w = pal.words[usize::from(m.palette_index)];
                let sw = pal.words[usize::from(sp.palette_index)];
                println!(
                    "  x{x}: main {:?}/{} word {w:04x} | sub {:?}/{} word {sw:04x} op {:?} fixed {} black {} | fb {rgba:?} plain {:?}",
                    m.layer, m.palette_index, sp.layer, sp.palette_index, sp.op, sp.fixed, sp.main_black,
                    rf_core_api::color::bgr555_to_rgb(w, pal.brightness)
                );
            }
        }
        for y in [8usize, 40, 100, 160, 215] {
            let Some(Some(r)) = b.lines.get(y) else {
                println!("  y{y}: no regs");
                continue;
            };
            let sub = b.sub.get(y).and_then(|s| s.as_ref());
            let opaque = sub.map_or(0, |s| s.pixels.iter().filter(|p| !p.fixed).count());
            println!(
                "  y{y}: tm {:02x} ts {:02x} en {:02x} sub {} half {} usesub {} prev {} clip {} fixed {:04x} bd {:04x} br {} | sub-screen opaque dots {opaque}/256 ops {:?}",
                r.tm, r.ts, r.math_enable, r.subtract, r.half, r.use_subscreen, r.prevent_mode, r.clip_mode, r.fixed_color, r.backdrop_color, r.brightness,
                sub.and_then(|s| s.pixels.iter().find(|p| !p.fixed).map(|p| p.op))
            );
        }
    }
}

/// Where the session is when the finder snapshots for its trials.
#[test]
#[ignore = "probe"]
fn session_trace() {
    let Some(mut rig) = Rig::open(ALTTP.0, ALTTP.1, None) else {
        return;
    };
    rig.script(&alttp_to_free_play());
    rig.find = Some(retroforge::scout_find::ScoutFind::new(true));
    for tok in alttp_play().split_whitespace() {
        rig.script(tok);
        let r = |a: u32| rig.st.peek_bus(0x7E_0000 + a);
        println!(
            "{tok:>6} f{} 10:{:02x} 11:{:02x} 1b:{:02x} x:{:02x}{:02x} y:{:02x}{:02x}",
            rig.st.frame_count(),
            r(0x10),
            r(0x11),
            r(0x1B),
            r(0x23),
            r(0x22),
            r(0x21),
            r(0x20)
        );
    }
}
