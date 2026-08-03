//! `Ppu` test tree — one module per ticket W1-04a acceptance criterion,
//! following `crate::system::tests`' own layout convention:
//!
//! - `scroll_registers` — criterion 2: `v`/`t`/`x`/`w` semantics for the
//!   `$2000`/`$2005`/`$2006`/`$2002` write/read sequences.
//! - `read_buffer` — the `$2007` delayed-read/palette-bypass quirk the
//!   ticket brief calls out by name as "a classic silent-wrongness source".
//! - `fetch_pipeline` — criterion 1 (W1-04a half): an end-to-end
//!   tick-driven proof that the dot table in `background.rs` (unit-tested
//!   there in isolation) actually fetches real NT/AT/pattern bytes and
//!   reloads the shift registers at the right dots.
//! - `frame_timing` — criterion 1 (W1-04b half): the odd/even-frame
//!   idle-dot skip, and the scanline-boundary prefetch oracle W1-04a's own
//!   tests explicitly left unproven (see that file's module doc).
//! - `sink_emission` — criterion 3: `Ppu::drain` actually calls
//!   `CoreSink::video_scanline` with the expected per-pixel data.
//! - `sprite_evaluation` — ticket W1-05a: secondary OAM evaluation, the
//!   8-sprite limit (and that it's genuinely invisible in the *rendered*
//!   output, not just in a count), the buggy overflow-flag diagonal scan
//!   (with tests specifically discriminating it from a naive "9th sprite
//!   exists" implementation), the `OAMADDR` dots-257-320 reset, sprite
//!   compositing (flip, 8x16 mode, left-8-px mask, BG/sprite priority),
//!   and the one-scanline pipeline delay. `crate::ppu::sprites`'s own
//!   module doc is this test module's source-citation authority.
//!
//! nesdev source for every derived `v`/`t` bit pattern below:
//! [nesdev.org/wiki/PPU_scrolling](https://www.nesdev.org/wiki/PPU_scrolling)
//! (`crate::ppu::scroll`'s module doc quotes the same pseudocode this test
//! module's expected values are hand-computed from).
mod fetch_pipeline;
mod frame_timing;
mod read_buffer;
mod scroll_registers;
mod sink_emission;
mod sprite_evaluation;

use super::Ppu;
use rf_cart::Mirroring;

/// A `Ppu` over 8 KiB of zeroed CHR ROM (NROM-shaped), horizontal
/// mirroring — the common case every test in this tree that doesn't care
/// about mirroring specifics starts from.
pub(super) fn test_ppu() -> Ppu {
    Ppu::new(vec![0u8; 0x2000], false, Mirroring::Horizontal)
}
