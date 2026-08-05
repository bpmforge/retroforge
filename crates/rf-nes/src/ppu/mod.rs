//! The 2C02 PPU (ticket W1-04a): loopy `v`/`t`/`x`/`w` scroll registers, the
//! background nametable/attribute/pattern fetch pipeline, and per-scanline
//! indexed-pixel output via [`rf_core_api::CoreSink`].
//!
//! ## Scope fence (this ticket vs. later ones)
//!
//! This module implements **background rendering** (W1-04a/W1-04b),
//! **sprite evaluation + compositing** (ticket W1-05a; see `sprites.rs`
//! for the sprite-specific module doc: secondary OAM evaluation, the
//! 8-sprite-per-scanline limit, the buggy overflow-flag diagonal scan, and
//! the BG/sprite priority multiplexer), and, as of ticket W1-05b,
//! **sprite-0 hit** (`STATUS_SPRITE0_HIT`, `sprites.rs`'s `output_pixel`)
//! and **VBlank/NMI wiring**: [`Ppu::nmi_line`] is the combinatorial
//! `$2000` bit 7 (NMI enable) AND `$2002` bit 7 (VBlank flag) level
//! [`crate::system::NesBus`] forwards through
//! [`crate::cpu::CpuBus::nmi_line`] — as of ticket W1-05c, through a
//! one-PPU-dot-delayed latch rather than a live passthrough (see this
//! module's "Sub-CPU-cycle VBlank/NMI race timing" section below and
//! `NesBus::tick_master`'s doc for why). The CPU still edge-detects
//! whatever level it forwards once per bus cycle (W1-01b's `CountingBus`),
//! which is exactly what reproduces nesdev.org/wiki/NMI's documented "By
//! toggling `NMI_output` (`PPUCTRL.7`) during vertical blank without
//! reading `PPUSTATUS`, a program can cause `/NMI` to be pulled low
//! multiple times" behavior for free: no separate multi-fire bookkeeping
//! is needed here, only an honest (if slightly delayed) level.
//!
//! ## Sub-CPU-cycle VBlank/NMI race timing (ticket W1-05c) — reachable
//! after all, at genuine PPU-dot resolution
//!
//! nesdev.org/wiki/PPU_frame_timing's "VBL Flag Timing" table describes a
//! **sub-CPU-cycle** race — reading `$2002` one PPU dot before the flag is
//! set, on the same dot, or one dot later each has different, specific
//! read-value/suppression behavior. W1-05b's module doc (superseded by
//! this section) claimed this was architecturally unreachable because
//! [`crate::system::NesBus`] only ever samples register state at whole
//! *CPU-cycle* (3-PPU-dot) boundaries. That claim was correct about the
//! mechanism and wrong about the conclusion: **register access never
//! needed sub-cycle placement, because the race window itself visits every
//! possible dot residue across successive frames for free.**
//!
//! With rendering disabled (every `ppu_vbl_nmi` sub-ROM's own precondition
//! for its precision-sync routine, `sync_vbl`), a frame is always exactly
//! 341 × 262 = 89342 dots — and 89342 mod 3 = 2, not 0. So the absolute-dot
//! position of any fixed (scanline, dot) target, expressed relative to
//! this crate's CPU-cycle window boundaries (which stay fixed, 3 dots
//! apart, for the life of the `Ppu`), shifts by 2 (mod 3) every frame.
//! Since gcd(2, 3) = 1, that shift visits residues 0, 2, 1, 0, 2, 1, ...
//! over successive frames — every possible alignment, eventually. The real,
//! fetched sub-ROMs exploit exactly this: `common/sync_vbl.s`'s own doc
//! comment says outright "VBL occurs every 29780.67 [CPU] clocks... the
//! loop will effectively read `$2002` one PPU clock later each frame."
//! **Genuine PPU-dot resolution was therefore available the whole time,
//! achieved BY the ROMs' own multi-frame convergence loops, not by this
//! engine splitting a cycle** — this module needed only to correctly model
//! what a register access sees at each of the three reachable residues, not
//! change when access happens relative to `tick_master`.
//!
//! ### The three-part fix
//!
//! 1. **`scroll.rs`'s `read_status`**, at `(VBLANK_START_SCANLINE, dot)`:
//!    `dot == 0` (one PPU clock before the set) returns the not-yet-set
//!    flag and latches `suppress_vblank_this_frame`, canceling the set for
//!    the rest of the frame (W1-05b, unchanged). `dot == 1` (the same PPU
//!    clock as the set) additionally forces the *returned* value to read
//!    as set — nesdev: "the same PPU clock or one later reads it as set" —
//!    while still latching the same suppression (`self.status` itself
//!    never really holds the bit; there is nothing to "un-set" later).
//! 2. **`scroll.rs`'s `read_status`**, at `(PRERENDER_SCANLINE, 1)`: masks
//!    the VBlank bit *out of the returned value only* (the unconditional
//!    `self.status &= !STATUS_VBLANK` at the top of every `$2002` read
//!    already handles real state). This is the clear-side twin of (1)'s
//!    fencepost — `Ppu::tick` runs `process_dot` (which does the real
//!    auto-clear) *before* `advance_counters`, so a read observing
//!    `self.dot == 1` is happening at the same instant as that auto-clear,
//!    before it has executed. Without this, `02-vbl_set_time` and
//!    `03-vbl_clear_time` — which share one `sync_vbl_delay` reference
//!    point 20 scanlines apart (nesdev: the flag "is cleared exactly 20
//!    scanlines after being set") — go one dot out of alignment with each
//!    other: fixing (1) alone passed `02` but broke a previously-passing
//!    `03`. Nesdev's own wiki text doesn't describe this half of the race;
//!    it's justified directly against `03-vbl_clear_time.s`'s own expected
//!    table (christopherpow/nes-test-roms), per MASTER_PROMPT's "the test
//!    ROM wins" rule.
//! 3. **`crate::system::NesBus`'s `nmi_level_latch`** (that module's
//!    `tick_master`/`nmi_line` docs): (1) and (2) are exclusively about
//!    `$2002`'s *read value*; nesdev's "same clock or one later" row is
//!    also about suppressing that frame's NMI, which is a property of when
//!    `crate::cpu::exec::CountingBus` (out of this ticket's write scope)
//!    samples the `/NMI` level, not of anything `read_status` touches. The
//!    VBlank set can land in a bus cycle that isn't a `$2002` access at
//!    all — an intervening `NOP`, say — so `read_status` never gets a
//!    chance to intervene there. `NesBus::tick_master` now snapshots
//!    [`Ppu::nmi_line`] once *per PPU dot* (not once per cycle — a
//!    whole-cycle-wide version was tried first and measured exactly 2 rows
//!    too wide against `05`-`08`'s own tables), so a set landing on a
//!    cycle's last dot stays invisible to that same cycle's end-of-op
//!    sample, matching "one clock later" without `cpu/**` needing to know
//!    anything happened.
//!
//! ### Result: 9/10, `10-even_odd_timing` excepted
//!
//! Measured against the real, fetched `ppu_vbl_nmi` ROMs: `01-vbl_basics`,
//! `02-vbl_set_time`, `03-vbl_clear_time`, `04-nmi_control`,
//! `05-nmi_timing`, `06-suppression`, `07-nmi_on_timing`,
//! `08-nmi_off_timing`, `09-even_odd_frames` all pass. `10-even_odd_timing`
//! still fails, at the same "Clock is skipped too late, relative to
//! enabling BG" sub-test (#3 of 4; #2 and #4 pass) W1-05b originally
//! measured — byte-identical across every experiment this ticket ran
//! (both `CpuBus::read` orderings, every `read_status` model tried),
//! confirming W1-05b's own prediction that it does not share this root
//! cause: it needs a `$2001`-write EFFECT to land earlier, and
//! `write_untimed` already runs before its own cycle's dots — the earliest
//! position reachable without literally moving `master_cycle`'s own
//! per-write increment, which none of this ticket's fixes touch.
//! `crates/rf-harness/waivers.toml` and `docs/STATUS.md`'s W1-05c entry
//! carry the full per-ROM evidence.
//!
//! It deliberately does NOT implement:
//! - A `Mapper` trait / CHR bank switching (routed the same way W1-02
//!   routed NROM's PRG logic: mapper 0 has no CHR banking, and no second
//!   mapper ticket exists yet to inform a trait's shape — see
//!   `crate::system` module doc's "Mapper scope" section for the precedent
//!   this follows).
//! - The 2C02G/H "`OAMADDR` nonzero when rendering starts corrupts the
//!   first 8 OAM bytes" errata (nesdev.org/wiki/PPU_registers: "if the
//!   sprite address (`OAMADDR`, `$2003`) is not zero, the process of
//!   starting sprite evaluation triggers an OAM hardware refresh bug...").
//!   This is a distinct, chip-revision-specific quirk from the
//!   `OAMADDR`-reset-at-dots-257-320 behavior this ticket DOES implement
//!   (see `sprites.rs`); it is not in this ticket's acceptance criteria,
//!   not test-ROM-verified here, and deliberately unimplemented rather than
//!   guessed at.
//! - **`dropped_by_limit` is always `false`** on every pixel this core
//!   emits — see [`rf_core_api::video::PpuPixel::dropped_by_limit`]'s doc
//!   for the full ruling (Brad, 2026-08-03): the sink is accuracy-exact, so
//!   a limit-dropped sprite (excluded from `sprites.rs`'s `active_sprites`
//!   by the 8-sprite cap) never reaches [`Ppu::output_pixel`] at all. Do
//!   not "fix" this to ride the flag on a displaced pixel — that is exactly
//!   what the ruling forbids.
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
//! [`rf_core_api::PpuPixel`]s) to an internal queue (`completed`), whose
//! capacity is *preallocated* for one frame (240 rows), matching the
//! "frame-boundary `save_state`" convention `EMULATION_CORES.md` §1 already
//! establishes — but that preallocation is a hint, not an enforced cap: the
//! queue is **not** bounded, and [`Ppu::tick`] never drops a row. A caller
//! that goes more than one frame without draining grows it further (240
//! more rows per undrained frame) rather than silently losing scanlines —
//! silently dropping frames would be exactly the kind of defect W1-04b's
//! golden-frame comparison exists to catch, so this module refuses to
//! guess at a cap instead. [`Ppu::drain`] (called by
//! [`crate::system::NesBus::drain_video`]) flushes the queue through a real
//! `&mut dyn CoreSink`, one `video_scanline` call per row, oldest first; a
//! real integration is expected to call it once per frame, but nothing
//! here enforces that cadence. No `EmulatorCore` implementation exists in this
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
//! - [nesdev.org/wiki/PPU_sprite_evaluation](https://www.nesdev.org/wiki/PPU_sprite_evaluation) —
//!   the two-phase secondary-OAM scan, the 8-sprite limit, and the buggy
//!   overflow-flag diagonal scan (`sprites.rs`'s own doc quotes its
//!   numbered algorithm, wording normalized, retrieved 2026-08-03, and
//!   cross-checks the overflow bug's mechanics against a second
//!   independent source).
//! - [nesdev.org/wiki/PPU_OAM](https://www.nesdev.org/wiki/PPU_OAM) — the 4
//!   OAM byte layout (Y, tile index incl. 8x16 mode's bank/top-tile split,
//!   attribute bits incl. flip/priority, X), and "Sprite data is delayed by
//!   one scanline; you must subtract 1 from the sprite's Y coordinate
//!   before writing it here" (the one-scanline pipeline delay `sprites.rs`
//!   models as two buffers — see its module doc).
//! - [nesdev.org/wiki/PPU_registers](https://www.nesdev.org/wiki/PPU_registers) —
//!   PPUCTRL bit 5 (sprite size) and bit 3 (8x8 sprite pattern table),
//!   PPUMASK bits 2/4 (show sprites / show sprites in the left 8 pixels),
//!   and "`OAMADDR` is set to 0 during each of ticks 257-320 (the sprite
//!   tile loading interval) of the pre-render and visible scanlines".

mod background;
mod mem;
mod scroll;
mod sprites;

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

/// One sprite copied into secondary OAM by [`Ppu::evaluate_sprites`]
/// (`sprites.rs`): the 4 raw OAM bytes plus the primary-OAM index it came
/// from (needed for [`rf_core_api::PpuPixel::sprite_id`] and OAM-order
/// priority). `y == 0xFF` marks an empty slot, matching hardware's
/// documented "$FF"-initialized secondary OAM (nesdev.org/wiki/PPU_sprite_evaluation).
#[derive(Clone, Copy)]
struct EvaluatedSprite {
    y: u8,
    tile: u8,
    attr: u8,
    x: u8,
    oam_index: u8,
}

const EMPTY_EVALUATED_SPRITE: EvaluatedSprite = EvaluatedSprite {
    y: 0xFF,
    tile: 0,
    attr: 0,
    x: 0,
    oam_index: 0xFF,
};

/// One of the (at most) 8 sprite "output units" [`Ppu::load_sprite_units`]
/// (`sprites.rs`) latches at dot 257 of the PRECEDING scanline: CHR pattern
/// bytes already fetched (not re-read per pixel) — see `sprites.rs`'s
/// module doc "one-scanline pipeline delay" section for why latching here,
/// rather than re-deriving a row from `self.scanline` at arbitrary render
/// time, is deliberate.
#[derive(Clone, Copy)]
struct SpriteUnit {
    pattern_lo: u8,
    pattern_hi: u8,
    attr: u8,
    x: u8,
    oam_index: u8,
}

const EMPTY_SPRITE_UNIT: SpriteUnit = SpriteUnit {
    pattern_lo: 0,
    pattern_hi: 0,
    attr: 0,
    x: 0xFF,
    oam_index: 0xFF,
};

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
    /// Frame parity for the odd-frame idle-dot skip (ticket W1-04b; see
    /// [`tick`](Ppu::tick)'s doc and
    /// [nesdev.org/wiki/PPU_rendering](https://www.nesdev.org/wiki/PPU_rendering)'s
    /// "Odd frame" note). Toggles every time the pre-render line wraps to
    /// scanline 0 (`advance_counters`), regardless of whether that
    /// particular wrap was itself skip-shortened. Not power-on-verified
    /// which parity real hardware starts on (no oracle for that exists
    /// yet — same caveat as this struct's other un-power-on-verified
    /// fields); `false` (even) is this module's arbitrary but documented
    /// choice.
    pub(super) frame_is_odd: bool,
    /// Total frames completed since this PPU was constructed (ticket
    /// W1-05b) — incremented exactly once per frame, alongside
    /// `frame_is_odd`'s toggle in [`Ppu::advance_counters`], regardless of
    /// whether that particular pre-render line was skip-shortened. Exists
    /// so a caller driving a real ROM (`rf-harness`'s blargg-protocol
    /// runner, `crate::system::NesBus::frame_count`) can detect "one whole
    /// frame elapsed" without reaching into `scanline`/`dot` directly —
    /// this module's only externally-meaningful frame-boundary signal.
    frame_count: u64,
    /// Sticky per-frame latch (ticket W1-05b, extended W1-05c) implementing
    /// nesdev.org/wiki/PPU_frame_timing's `$2002`-read VBlank race (see
    /// this module's doc "Sub-CPU-cycle VBlank/NMI race timing" section):
    /// set by `scroll.rs`'s `read_status` both when a `$2002` read happens
    /// exactly one dot before the VBlank-set dot (scanline 241, dot 1) —
    /// nesdev, verbatim, "Reading one PPU clock before reads it as clear
    /// and never sets the flag or generates NMI for that frame" — and when
    /// it happens on that same dot (nesdev: "reading on the same PPU clock
    /// ... reads it as set, clears it"; `read_status` forces the returned
    /// value in that second case, since `self.status` itself never really
    /// holds the bit). Checked (and, if set, suppresses the flag-set) in
    /// [`Ppu::process_dot`] at exactly that dot; cleared again at the
    /// pre-render line's dot 1 alongside the other per-frame status-bit
    /// resets, so it can never leak into a later frame's own VBlank window.
    suppress_vblank_this_frame: bool,

    // ---- sprite evaluation + output units (ticket W1-05a; see
    // `sprites.rs` module doc) ----
    /// Filled by [`Ppu::evaluate_sprites`] during dots 65-256 of a visible
    /// scanline (or cleared without evaluation at pre-render dot 1) — the
    /// sprites that will be rendered on the *next* scanline.
    secondary_oam: [EvaluatedSprite; 8],
    secondary_oam_count: u8,
    /// The render-ready snapshot [`Ppu::load_sprite_units`] latches at dot
    /// 257, used by [`Ppu::output_pixel`]'s sprite compositing while
    /// drawing the CURRENT scanline.
    active_sprites: [SpriteUnit; 8],
    active_sprite_count: u8,

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
            frame_is_odd: false,
            frame_count: 0,
            suppress_vblank_this_frame: false,
            secondary_oam: [EMPTY_EVALUATED_SPRITE; 8],
            secondary_oam_count: 0,
            active_sprites: [EMPTY_SPRITE_UNIT; 8],
            active_sprite_count: 0,
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
    /// bit. **Odd-frame idle-dot skip** (ticket W1-04b) — nesdev.org/wiki/
    /// PPU_rendering, verbatim: "This scanline varies in length, depending
    /// on whether an even or an odd frame is being rendered. For odd
    /// frames, the cycle at the end of the scanline is skipped (this is
    /// done internally by jumping directly from (339,261) to (0,0)...)."
    /// and "this behavior can be bypassed by keeping rendering disabled
    /// until after this scanline has passed" — i.e. the skip only fires
    /// when [`Ppu::rendering_enabled`] is true at dot 339 of the pre-render
    /// line; with rendering disabled every pre-render line is the full 341
    /// dots on every frame. When it fires, dot 339 is followed directly by
    /// dot 0 of scanline 0 (dot 340 never happens that frame: 340 dots
    /// instead of 341, 89341 dots that whole frame instead of 89342). See
    /// [`Ppu::advance_counters`] for the implementation.
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
                    // Ticket W1-05b: a suppression latched during the frame
                    // that's ending must not leak into the frame about to
                    // start — see `suppress_vblank_this_frame`'s doc.
                    self.suppress_vblank_this_frame = false;
                }
                self.process_render_dot(false);
            }
            // Ticket W1-05b: `suppress_vblank_this_frame` (set by
            // `scroll.rs`'s `read_status` when a `$2002` read lands exactly
            // one dot early) skips only the flag *set* — nesdev: "never
            // sets the flag or generates NMI for that frame". Everything
            // else about this dot (there is nothing else) is unaffected.
            VBLANK_START_SCANLINE if self.dot == 1 => {
                if !self.suppress_vblank_this_frame {
                    self.status |= STATUS_VBLANK;
                }
            }
            // Post-render (240) and the rest of vblank (241-260, beyond
            // dot 1): genuinely idle, nothing to do.
            POSTRENDER_SCANLINE | VBLANK_START_SCANLINE..=260 => {}
            _ => {}
        }
    }

    /// Odd-frame idle-dot skip dot (see [`Ppu::tick`]'s doc): the
    /// pre-render line's dot immediately before the one nesdev's diagram
    /// jumps past when the skip fires.
    const ODD_FRAME_SKIP_DOT: u16 = 339;

    fn advance_counters(&mut self) {
        // With rendering enabled, dot 339 of an odd-frame pre-render line
        // jumps straight to (0,0) — dot 340 never happens. Folding this
        // into the same wrap-condition as the ordinary end-of-scanline
        // check below (rather than a separate early return) means the
        // frame-parity toggle only needs to live in the one place both
        // paths already share (`self.scanline == PRERENDER_SCANLINE`).
        let odd_frame_skip = self.scanline == PRERENDER_SCANLINE
            && self.dot == Self::ODD_FRAME_SKIP_DOT
            && self.frame_is_odd
            && self.rendering_enabled();

        if odd_frame_skip || self.dot >= DOTS_PER_SCANLINE - 1 {
            self.dot = 0;
            self.scanline = if self.scanline == PRERENDER_SCANLINE {
                self.frame_is_odd = !self.frame_is_odd;
                self.frame_count += 1;
                0
            } else {
                self.scanline + 1
            };
        } else {
            self.dot += 1;
        }
    }

    /// Total frames completed since construction (see `frame_count`'s
    /// field doc) — [`crate::system::NesBus::frame_count`] forwards this.
    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    /// The PPU's NMI output level (ticket W1-05b): asserted iff `$2000`
    /// bit 7 (NMI enable, "Generate an NMI at the start of the vertical
    /// blanking interval") AND `$2002` bit 7 (the VBlank flag) are both
    /// currently true — nesdev.org/wiki/NMI, verbatim: "The PPU pulls /NMI
    /// low if and only if both `vblank_flag` and `NMI_output` are true."
    /// Pure/combinatorial, no side effects — [`crate::system::NesBus`]
    /// (the [`crate::cpu::CpuBus`] implementor) forwards this directly
    /// through [`crate::cpu::CpuBus::nmi_line`], and the CPU's own
    /// edge-detector (W1-01b, `cpu::exec::CountingBus`, sampled once per
    /// bus cycle) is what turns this level into "multiple NMIs fire if
    /// `NMI_output` is toggled off/on while `vblank_flag` is still set" —
    /// see this module's doc "Scope fence" section.
    pub fn nmi_line(&self) -> bool {
        self.ctrl & 0x80 != 0 && self.status & STATUS_VBLANK != 0
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

    pub(super) fn mask_show_sprites(&self) -> bool {
        self.mask & 0x10 != 0
    }

    /// PPUMASK ($2001) bit 2: "Show sprites in leftmost 8 pixels of
    /// screen" (nesdev.org/wiki/PPU_registers).
    pub(super) fn mask_show_sprites_left8(&self) -> bool {
        self.mask & 0x04 != 0
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
