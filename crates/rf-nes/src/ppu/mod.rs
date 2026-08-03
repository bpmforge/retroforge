//! The 2C02 PPU (ticket W1-04a): loopy `v`/`t`/`x`/`w` scroll registers, the
//! background nametable/attribute/pattern fetch pipeline, and per-scanline
//! indexed-pixel output via [`rf_core_api::CoreSink`].
//!
//! ## Scope fence (this ticket vs. later ones)
//!
//! This module implements **background rendering only**. It deliberately
//! does NOT implement:
//! - Sprite evaluation, secondary OAM, the 8-sprite limit, or sprite-0 hit
//!   (ticket W1-05a/W1-05b) — every emitted pixel this ticket produces is
//!   [`rf_core_api::PixelLayer::Background`] or
//!   [`rf_core_api::PixelLayer::Backdrop`], never `Sprite`.
//! - The odd-frame dot-339 skip, and the exact VBlank/NMI edge-suppression
//!   races `ppu_vbl_nmi` tests (ticket W1-04b/W1-05b per the conductor's
//!   W1-04a pre-flight notes). The VBlank/sprite0/overflow flag bits of
//!   `$2002` exist and are set/cleared at the dots nesdev documents (see
//!   [`tick`](Ppu::tick)'s doc), but nothing here connects them to
//!   [`crate::cpu::CpuBus::nmi_line`] — [`crate::system::NesBus`] still
//!   returns that trait method's `false` default, unchanged. A half-modeled
//!   NMI with no golden-frame oracle to check it against would risk
//!   silently perturbing CPU-visible behavior for a criterion this ticket
//!   doesn't claim; wiring it up belongs to whichever later ticket actually
//!   gets a test-ROM oracle for it.
//! - A `Mapper` trait / CHR bank switching (routed the same way W1-02
//!   routed NROM's PRG logic: mapper 0 has no CHR banking, and no second
//!   mapper ticket exists yet to inform a trait's shape — see
//!   `crate::system` module doc's "Mapper scope" section for the precedent
//!   this follows).
//!
//! ## Scheduling: lock-step, not catch-up (`docs/design/EMULATION_CORES.md`
//! §1)
//!
//! §1 permits either catch-up scheduling or ticking the PPU in lock-step
//! per CPU cycle in Accuracy mode ("simpler to reason about, ~10-20%
//! slower"), and requires that "both paths must produce identical state,
//! and CI diffs them on the test-ROM suite." This ticket builds **lock-step
//! only**: [`crate::system::NesBus`] ticks this PPU exactly 3 dots per bus
//! cycle from inside its own `master_cycle` advance (see that module's
//! `tick_master`). The catch-up path, and the CI diff between the two
//! paths §1 requires, are NOT built here — that is an explicit debt owed to
//! a later ticket, the same way W1-02 routed its mapper-trait extraction
//! and reset-sequence gaps to the tickets that inherited them.
//!
//! ## `CoreSink` emission seam
//!
//! [`rf_core_api::CoreSink::video_scanline`] takes `&mut dyn CoreSink`, but
//! this PPU is ticked from deep inside [`crate::system::NesBus`]'s
//! [`crate::cpu::CpuBus::read`]/`write` — which have no sink parameter, by
//! design (giving `CpuBus` a sink would mean every mock bus in
//! `crate::cpu::tests` needs one too, and would be the first step toward
//! `Cpu` knowing about video output at all). So ticking and sink emission
//! are decoupled: [`Ppu::tick`] appends each completed visible scanline (256
//! [`rf_core_api::PpuPixel`]s) to an internal, bounded queue
//! (`completed`, capacity one frame — 240 rows — since a real integration
//! drains at each frame boundary, matching the "frame-boundary
//! `save_state`" convention `EMULATION_CORES.md` §1 already establishes);
//! [`Ppu::drain`] (called by [`crate::system::NesBus::drain_video`]) flushes
//! that queue through a real `&mut dyn CoreSink`, one `video_scanline` call
//! per row, oldest first. No `EmulatorCore` implementation exists in this
//! crate yet (out of this ticket's write scope) — a later ticket wires
//! `run_frame` to call `drain_video` once per frame; this ticket proves the
//! mechanism with a test-only `CoreSink` that records the calls it
//! receives.
//!
//! ## `palette_index` semantics (public commitment, binds W1-04b/W3-xx)
//!
//! Each emitted [`rf_core_api::PpuPixel::palette_index`] is the raw 6-bit
//! **value** read out of PPU palette RAM at render time (0-63, top 2 bits
//! of the stored byte masked off — nesdev.org/wiki/PPU_palettes: "6-bit
//! palette data"), *not* the 0-31 palette-RAM address. This is a deliberate
//! choice: emitting the address would require the renderer to resolve
//! colors against an out-of-band palette-RAM snapshot, and any scanline
//! rendered before a mid-frame palette write would then resolve against the
//! *wrong* (later) snapshot at a frame-boundary `StateView`. Emitting the
//! already-resolved value is immune to that — each pixel carries the exact
//! byte the hardware would have used for that dot, mid-frame writes
//! included. Also out of scope here (a later ticket's problem, per
//! `EMULATION_CORES.md` §2.2): `$2001`'s greyscale/emphasis bits have
//! nowhere to ride in `PpuPixel` today; the renderer applies those via a
//! LUT variant once that plumbing exists.
//!
//! ## Sources (cite for every cycle-timing-sensitive fact, project law)
//!
//! - [nesdev.org/wiki/PPU_scrolling](https://www.nesdev.org/wiki/PPU_scrolling) —
//!   `v`/`t`/`x`/`w` bit layout, the `$2000`/`$2005`/`$2006` write
//!   pseudocode, coarse-X/Y increment pseudocode, hori(v)/vert(v) copy
//!   pseudocode, and the NT/AT address formulas.
//! - [nesdev.org/wiki/PPU_rendering](https://www.nesdev.org/wiki/PPU_rendering) —
//!   the per-scanline dot diagram (fetch windows, shift-register reload at
//!   dots 9,17,...,257 and the 321-336 prefetch region, the dot-257
//!   hori(v)=hori(t) copy, the pre-render-line dot-280-304 vert(v)=vert(t)
//!   window, VBlank set at dot 1 of scanline 241, 262 scanlines x 341 dots).
//! - [nesdev.org/wiki/PPU_registers](https://www.nesdev.org/wiki/PPU_registers) —
//!   `$2002`/`$2007` register behavior, including the delayed-read buffer
//!   and its palette-range bypass, and that the three `$2002` status flags
//!   "are automatically cleared on dot 1 of the prerender scanline".
//! - [nesdev.org/wiki/PPU_palettes](https://www.nesdev.org/wiki/PPU_palettes) —
//!   the 32-byte palette RAM mirror and the entry-0-shared-between-
//!   background-and-sprite-palettes quirk ($3F10 aliases $3F00, and by the
//!   same documented mechanism $3F14/$3F18/$3F1C alias $3F04/$3F08/$3F0C).

mod background;
mod mem;
mod scroll;

#[cfg(test)]
mod tests;

use rf_cart::Mirroring;
use rf_core_api::{CoreSink, PixelLayer, PpuPixel};

/// Scanline 261 is the pre-render line (some sources call it -1);
/// represented as an unsigned value here purely to avoid a signed
/// scanline counter elsewhere in this module — nesdev.org/wiki/PPU_rendering
/// uses both `-1` and `261` interchangeably for the same line.
const PRERENDER_SCANLINE: u16 = 261;
const POSTRENDER_SCANLINE: u16 = 240;
const VBLANK_START_SCANLINE: u16 = 241;
const DOTS_PER_SCANLINE: u16 = 341;

const STATUS_VBLANK: u8 = 0x80;
const STATUS_SPRITE0_HIT: u8 = 0x40;
const STATUS_SPRITE_OVERFLOW: u8 = 0x20;

/// One fully-rendered visible scanline queued for [`Ppu::drain`], paired
/// with its `y` index (0-239) — see [`crate::CoreSink::video_scanline`]'s
/// signature, which this struct exists to defer calling until a real sink
/// is available (module doc's "`CoreSink` emission seam" section).
struct CompletedScanline {
    y: u16,
    pixels: [PpuPixel; 256],
}

/// The 2C02 PPU. See the module doc for scope, the `CoreSink` emission
/// seam, and the `palette_index` semantics commitment.
pub struct Ppu {
    // ---- CPU-visible registers ($2000-$2007) ----
    pub(super) ctrl: u8,
    pub(super) mask: u8,
    pub(super) status: u8,
    oam_addr: u8,
    oam: [u8; 256],

    // ---- loopy scroll registers (nesdev.org/wiki/PPU_scrolling) ----
    pub(super) v: u16,
    pub(super) t: u16,
    pub(super) x: u8,
    pub(super) w: bool,

    /// `$2007`'s internal read-data buffer (delayed-read quirk).
    read_buffer: u8,

    // ---- PPU-side memory ----
    /// Pattern table data (CHR ROM/RAM), `$0000-$1FFF` on the PPU bus.
    pub(super) chr: Vec<u8>,
    chr_is_ram: bool,
    /// Nametable RAM: 4 logical 1 KiB banks addressed via `mirroring`, laid
    /// out physically as documented in `mem.rs`. Sized for the
    /// [`Mirroring::FourScreen`] case (4 distinct banks); `Horizontal`/
    /// `Vertical` only ever address the first two.
    pub(super) vram: [u8; 0x1000],
    /// Palette RAM, `$3F00-$3F1F` on the PPU bus (32 bytes, mirrored
    /// through `$3FFF` — see `mem.rs`).
    pub(super) palette: [u8; 32],
    mirroring: Mirroring,

    // ---- background fetch pipeline (see `background.rs`) ----
    pub(super) bg_pattern_shift_lo: u16,
    pub(super) bg_pattern_shift_hi: u16,
    pub(super) bg_attr_shift_lo: u16,
    pub(super) bg_attr_shift_hi: u16,
    pub(super) nt_latch: u8,
    pub(super) at_latch: u8,
    pub(super) pt_lo_latch: u8,
    pub(super) pt_hi_latch: u8,

    // ---- dot/scanline counters ----
    pub(super) scanline: u16,
    pub(super) dot: u16,

    // ---- scanline output ----
    line_buffer: [PpuPixel; 256],
    completed: Vec<CompletedScanline>,
}

/// A fully-transparent placeholder pixel used to fill freshly-allocated
/// buffers before the first real pixel is written; never observed by a
/// caller (every one of the 256 slots is overwritten during
/// [`Ppu::tick`]'s visible-scanline pixel output before [`Ppu::drain`] can
/// ever see it).
const BLANK_PIXEL: PpuPixel = PpuPixel {
    palette_index: 0,
    layer: PixelLayer::Backdrop,
    sprite_id: None,
    priority: 0,
    dropped_by_limit: false,
};

impl Ppu {
    /// Build a PPU over one cartridge's CHR data (ticket W1-02's
    /// `NesRom::chr_rom`/`chr_is_ram`) and its header-declared nametable
    /// [`Mirroring`]. Initial dot/scanline position is `(PRERENDER, 0)` — a
    /// convenient, but not power-on-verified, starting point (no golden
    /// frame exists yet to check real cold-boot PPU state against; see the
    /// module doc's scope fence).
    pub fn new(chr: Vec<u8>, chr_is_ram: bool, mirroring: Mirroring) -> Self {
        Ppu {
            ctrl: 0,
            mask: 0,
            status: 0,
            oam_addr: 0,
            oam: [0; 256],
            v: 0,
            t: 0,
            x: 0,
            w: false,
            read_buffer: 0,
            chr,
            chr_is_ram,
            vram: [0; 0x1000],
            palette: [0; 32],
            mirroring,
            bg_pattern_shift_lo: 0,
            bg_pattern_shift_hi: 0,
            bg_attr_shift_lo: 0,
            bg_attr_shift_hi: 0,
            nt_latch: 0,
            at_latch: 0,
            pt_lo_latch: 0,
            pt_hi_latch: 0,
            scanline: PRERENDER_SCANLINE,
            dot: 0,
            line_buffer: [BLANK_PIXEL; 256],
            completed: Vec::with_capacity(240),
        }
    }

    /// The full 256-byte OAM, for [`crate::system::NesBus::oam`] (tests,
    /// and the future sprite-evaluation ticket).
    pub fn oam(&self) -> &[u8; 256] {
        &self.oam
    }

    /// Current `OAMADDR` (test/debug visibility, same as the old
    /// `ppu_stub` this ticket replaces).
    pub fn oam_addr(&self) -> u8 {
        self.oam_addr
    }

    /// The exact side effect of one `OAMDATA` write — factored out because
    /// OAM DMA (`$4014`) drives it 256 times without going through the
    /// normal register-write path (see `crate::system::NesBus::run_oam_dma`'s
    /// doc: DMA behaves *as if* the CPU wrote `OAMDATA` repeatedly).
    pub fn write_oam_data(&mut self, value: u8) {
        self.oam[self.oam_addr as usize] = value;
        self.oam_addr = self.oam_addr.wrapping_add(1);
    }

    /// Advance the PPU by exactly one dot (`crate::system::NesBus` calls
    /// this 3 times per CPU/master cycle — see that module's `tick_master`
    /// and this module doc's "Scheduling" section).
    ///
    /// Implements the nesdev.org/wiki/PPU_rendering dot diagram: 262
    /// scanlines (0-239 visible, 240 post-render, 241-260 vblank, 261
    /// pre-render) x 341 dots. On visible/pre-render lines, while
    /// rendering is enabled (`$2001` bits 3 or 4): the background fetch
    /// pipeline runs (see `background.rs`), coarse X increments at dots
    /// 8,16,...,256,328,336 and the shift registers reload at dots
    /// 9,17,...,257,329,337 (both derived from the reload fact nesdev
    /// states verbatim — see `background.rs`'s doc), Y increments at dot
    /// 256, hori(v)=hori(t) copies at dot 257, and (pre-render line only)
    /// vert(v)=vert(t) copies on every dot in 280..=304. Visible lines
    /// additionally output one pixel per dot in 1..=256 and queue the
    /// completed line for [`Ppu::drain`] after dot 256. The pre-render
    /// line's dot 1 clears the vblank/sprite-0-hit/overflow status bits
    /// ("automatically cleared on dot 1 of the prerender scanline" per
    /// nesdev.org/wiki/PPU_registers); scanline 241's dot 1 sets the vblank
    /// bit. The dot-339 odd-frame skip is NOT implemented (module doc's
    /// scope fence; W1-04b).
    pub fn tick(&mut self) {
        self.process_dot();
        self.advance_counters();
    }

    fn process_dot(&mut self) {
        match self.scanline {
            0..=239 => self.process_render_dot(true),
            PRERENDER_SCANLINE => {
                if self.dot == 1 {
                    self.status &= !(STATUS_VBLANK | STATUS_SPRITE0_HIT | STATUS_SPRITE_OVERFLOW);
                }
                self.process_render_dot(false);
            }
            VBLANK_START_SCANLINE if self.dot == 1 => {
                self.status |= STATUS_VBLANK;
            }
            // Post-render (240) and the rest of vblank (241-260, beyond
            // dot 1): genuinely idle, nothing to do.
            POSTRENDER_SCANLINE | VBLANK_START_SCANLINE..=260 => {}
            _ => {}
        }
    }

    fn advance_counters(&mut self) {
        if self.dot >= DOTS_PER_SCANLINE - 1 {
            self.dot = 0;
            self.scanline = if self.scanline == PRERENDER_SCANLINE {
                0
            } else {
                self.scanline + 1
            };
        } else {
            self.dot += 1;
        }
    }

    fn rendering_enabled(&self) -> bool {
        self.mask_show_background() || self.mask_show_sprites()
    }

    pub(super) fn mask_show_background(&self) -> bool {
        self.mask & 0x08 != 0
    }

    pub(super) fn mask_show_background_left8(&self) -> bool {
        self.mask & 0x02 != 0
    }

    fn mask_show_sprites(&self) -> bool {
        self.mask & 0x10 != 0
    }

    /// Queue every completed-but-undrained scanline through `sink`, oldest
    /// first, then clear the queue (module doc's "`CoreSink` emission
    /// seam"). Safe to call at any time, including mid-frame; a real
    /// integration is expected to call it once per frame.
    pub fn drain(&mut self, sink: &mut dyn CoreSink) {
        for line in self.completed.drain(..) {
            sink.video_scanline(line.y, &line.pixels);
        }
    }

    fn finish_scanline(&mut self) {
        self.completed.push(CompletedScanline {
            y: self.scanline,
            pixels: self.line_buffer,
        });
    }
}
