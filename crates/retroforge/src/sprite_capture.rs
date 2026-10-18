//! Sprites as console-free boxes (ticket W34-02; `docs/design/HD2D_PLAN.md`
//! section 2.3).
//!
//! `rf-enhance` may not name a console, so the shell decodes each machine's
//! OAM into `rf_enhance::heights::SpriteBox`es (screen pixels, top-left) for
//! the player detector. Reads only: OAM is a side-effect-free borrow
//! (`EmuStepper::oam`, the SNES PPU's `oam`), so this cannot perturb a run.
//!
//! - NES: 64 four-byte entries, 8x8 or 8x16 by PPUCTRL bit 5; a sprite shows
//!   one line below its Y byte, and Y >= 240 never shows
//!   ([nesdev PPU OAM](https://www.nesdev.org/wiki/PPU_OAM)).
//! - SNES: 128 entries plus a high table giving X's ninth bit and the size
//!   select, sizes from `$2101` bits 5-7; Y wraps at 256, so a tall sprite
//!   near the bottom shows at the top (fullsnes "SNES PPU Sprites (OBJs)").

use rf_enhance::heights::SpriteBox;

/// The NES's visible sprites.
pub fn nes_sprites(oam: &[u8; 256], height: u8, screen: (i32, i32), out: &mut Vec<SpriteBox>) {
    out.clear();
    for e in &rf_debugger::oam::decode_oam(oam) {
        let y = i32::from(e.y) + 1;
        let x = i32::from(e.x);
        if e.y >= 240 || x >= screen.0 || y >= screen.1 {
            continue;
        }
        out.push(SpriteBox {
            x: x as i16,
            y: y as i16,
            w: 8,
            h: height,
            behind_bg: e.priority_behind_bg,
            tile: u16::from(e.tile),
        });
    }
}

/// The SNES's visible sprites. `oam` is the 544-byte table, `obj_size` the
/// `$2101` size select.
pub fn snes_sprites(oam: &[u8], obj_size: u8, screen: (i32, i32), out: &mut Vec<SpriteBox>) {
    out.clear();
    let (small, big) = rf_snes::debug::obj_sizes(obj_size);
    for e in rf_snes::debug::decode_oam(oam) {
        let (w, h) = if e.large { big } else { small };
        let (w, h) = (i32::from(w), i32::from(h));
        // Y wraps: a sprite that runs past line 255 re-enters at the top.
        let mut y = i32::from(e.y) + 1;
        if y + h > 256 {
            y -= 256;
        }
        let x = i32::from(e.x);
        if x + w <= 0 || x >= screen.0 || y + h <= 0 || y >= screen.1 {
            continue;
        }
        out.push(SpriteBox {
            x: x as i16,
            y: y as i16,
            w: w as u8,
            h: h as u8,
            behind_bg: e.priority == 0,
            tile: e.tile,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nes_sprites_show_one_line_low_and_offscreen_ones_do_not_show() {
        let mut oam = [0xFFu8; 256];
        oam[0..4].copy_from_slice(&[99, 7, 0x20, 40]);
        let mut out = Vec::new();
        nes_sprites(&oam, 16, (256, 240), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].x, out[0].y, out[0].w, out[0].h), (40, 100, 8, 16));
        assert!(out[0].behind_bg);
    }

    #[test]
    fn snes_sprites_read_the_high_table_for_size_and_the_left_edge() {
        let mut oam = vec![0u8; 544];
        for i in 0..128 {
            oam[i * 4 + 1] = 240; // parked at the bottom
        }
        // Sprite 3: x = -8 (ninth bit set, low byte 0xF8), large.
        oam[12..16].copy_from_slice(&[0xF8, 50, 5, 0x30]);
        oam[0x200] |= 0b11 << 6;
        // Obj size 0: 8x8 and 16x16.
        let mut out = Vec::new();
        snes_sprites(&oam, 0, (256, 224), &mut out);
        let s = out.iter().find(|s| s.tile == 5).expect("sprite 3");
        assert_eq!((s.x, s.y, s.w, s.h), (-8, 51, 16, 16));
    }
}
