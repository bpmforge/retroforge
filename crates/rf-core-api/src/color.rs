//! Pure BGR555 colour arithmetic shared by everything that has to produce a
//! colour from SNES palette words: the renderer's frame path and the
//! widescreen canvas compose (`rf_enhance::canvas`).
//!
//! It lives here, not in `rf-renderer`, because `rf-enhance` must not depend
//! on the renderer (ARCHITECTURE.md section 3) and both need the same
//! arithmetic; `rf-renderer::palette` re-exports it unchanged. It reads no
//! state and has no policy: which operator applies to which pixel is the
//! caller's decision.

/// One BGR555 word as RGB888 at a master brightness (ticket W7-20).
///
/// Blue is in the HIGH bits (fullsnes "SNES Color Palette": "Bit 0-4 Red,
/// 5-9 Green, 10-14 Blue"). Each channel scales by `*255/31` so 31 is 255,
/// the same conversion `rf_snes::debug::cgram_rgb` uses (a test pins the
/// two equal at full brightness). Brightness N in `1..=15` scales by
/// `(N+1)/16` and 0 is black (fullsnes "2100h - INIDISP").
#[must_use]
pub fn bgr555_to_rgb(word: u16, brightness: u8) -> [u8; 3] {
    if brightness == 0 {
        return [0, 0, 0];
    }
    let scale = u32::from(brightness.min(15)) + 1;
    let ch = |shift: u16| -> u8 {
        let full = u32::from((word >> shift) & 0x1F) * 255 / 31;
        (full * scale / 16) as u8
    };
    [ch(0), ch(5), ch(10)]
}

/// Combine a main and a sub/fixed BGR555 colour (ticket W7-21): per 5-bit
/// channel, add or subtract, clamp to `0..=31`, then halve if asked —
/// fullsnes "SNES PPU Color-Math" (`$2131` bits 6-7). The same arithmetic
/// as `rf_snes::ppu::window::ColorMath::blend`.
#[must_use]
pub fn color_math_bgr555(main: u16, sub: u16, op: crate::ColorMathOp) -> u16 {
    use crate::ColorMathOp as Op;
    let (subtract, half) = match op {
        Op::None => return main,
        Op::Add => (false, false),
        Op::AddHalf => (false, true),
        Op::Subtract => (true, false),
        Op::SubtractHalf => (true, true),
    };
    let mut out = 0u16;
    for shift in [0u16, 5, 10] {
        let m = i32::from((main >> shift) & 0x1F);
        let s = i32::from((sub >> shift) & 0x1F);
        let v = if subtract { m - s } else { m + s };
        let v = if half { v / 2 } else { v };
        out |= (v.clamp(0, 31) as u16) << shift;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_math_adds_subtracts_clamps_and_halves_per_channel() {
        use crate::ColorMathOp as Op;
        let c = |r: u16, g: u16, b: u16| r | (g << 5) | (b << 10);
        assert_eq!(
            color_math_bgr555(c(10, 10, 10), c(5, 5, 5), Op::Add),
            c(15, 15, 15)
        );
        assert_eq!(
            color_math_bgr555(c(30, 0, 0), c(10, 0, 0), Op::Add),
            c(31, 0, 0)
        );
        assert_eq!(
            color_math_bgr555(c(30, 0, 0), c(10, 0, 0), Op::AddHalf),
            c(20, 0, 0)
        );
        assert_eq!(
            color_math_bgr555(c(2, 9, 0), c(10, 4, 0), Op::Subtract),
            c(0, 5, 0)
        );
        assert_eq!(
            color_math_bgr555(c(10, 0, 0), c(4, 0, 0), Op::SubtractHalf),
            c(3, 0, 0)
        );
        assert_eq!(
            color_math_bgr555(c(7, 8, 9), c(31, 31, 31), Op::None),
            c(7, 8, 9)
        );
    }

    #[test]
    fn bgr555_puts_blue_high_and_full_scale_at_255() {
        assert_eq!(bgr555_to_rgb(0x001F, 15), [255, 0, 0]);
        assert_eq!(bgr555_to_rgb(0x03E0, 15), [0, 255, 0]);
        assert_eq!(bgr555_to_rgb(0x7C00, 15), [0, 0, 255]);
        assert_eq!(bgr555_to_rgb(0x7FFF, 0), [0, 0, 0], "brightness 0 is black");
        // (N+1)/16: brightness 7 is half.
        assert_eq!(bgr555_to_rgb(0x001F, 7), [127, 0, 0]);
    }
}
