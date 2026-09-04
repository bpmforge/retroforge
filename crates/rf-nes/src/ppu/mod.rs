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
//! ### Result: 9/10 at W1-05c, 10/10 at W1-05d
//!
//! Measured against the real, fetched `ppu_vbl_nmi` ROMs: W1-05c's three
//! read-side mechanisms above passed `01-vbl_basics` through
//! `09-even_odd_frames` and left `10-even_odd_timing` failing at "Clock is
//! skipped too late, relative to enabling BG" (sub-test #3 of 4) —
//! byte-identical across every experiment that ticket ran (both
//! `CpuBus::read` orderings, every `read_status` model tried), confirming
//! it did not share this root cause.
//!
//! **W1-05d closed it, and the diagnosis above was half right.** It is
//! indeed a `$2001`-write-side question, not a `$2002`-read one — but the
//! fix is not to make the write land earlier (`write_untimed` already runs
//! before its own cycle's dots, the earliest position reachable). It is
//! that the *skip decision* was reading `mask` with no propagation delay at
//! all, where nesdev.org/wiki/PPU_registers's `PPUMASK` section says
//! "toggling rendering takes effect approximately 3-4 dots after the
//! write". [`Ppu::render_enable_pipe`] carries the two-dot latch, the
//! measurement that pins its depth, and why it is deliberately scoped to
//! this one decision. `docs/STATUS.md`'s W1-05c/W1-05d entries carry the
//! per-ROM evidence; `crates/rf-harness/waivers.toml` no longer waives
//! anything in this suite.
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
//! ## Scheduling: lock-step in Accuracy, catch-up in Compatibility
//! (`docs/design/EMULATION_CORES.md` §1 and §5 row 1)
//!
//! §1 permits either catch-up scheduling or ticking the PPU in lock-step
//! per CPU cycle in Accuracy mode ("simpler to reason about, ~10-20%
//! slower"), and requires that "both paths must produce identical state,
//! and CI diffs them on the test-ROM suite."
//!
//! **Accuracy is lock-step** (ticket W1-04a and unchanged since):
//! [`crate::system::NesBus`] ticks this PPU exactly 3 dots per bus cycle
//! from inside its own `master_cycle` advance.
//!
//! **Compatibility is catch-up** (ticket W3-07b): the bus asks
//! [`Ppu::inert_run_len`] how many of the dots ahead are provably inert
//! and advances those in one arithmetic step, using
//! [`Ppu::dots_until_possible_inert`] to avoid re-asking on every dot.
//! Nothing is deferred — the PPU's position is always exact — so the
//! three things that observe this PPU at dot granularity every cycle
//! (`nmi_level_latch`, the A12 drain into the mapper, and the
//! dot-count-keyed state W2-19/W2-01d added) are untouched. That is what
//! lets it meet §1's "identical state" requirement rather than declaring
//! a divergence: `mode_diff` compares the two configs over the whole
//! executed suite and reports exactly one divergence, which belongs to
//! the other §5 switch (open-bus modelling, ticket W3-07).
//!
//! **What it buys, measured rather than assumed.** The dots it can prove
//! inert are ~31% of a frame with rendering off but only ~8% with
//! rendering on, and the inert ones are also the cheapest. Measured on
//! `machine_frame`: rendering off, 693 vs 732 us/frame (**5.4% faster**);
//! rendering on, 1.116 vs 1.124 ms (**parity**, inside run-to-run noise).
//! §1's "~10-20% slower" for lock-step is not what this engine shows, and
//! the reason is that this PPU's idle dots were already nearly free. See
//! `docs/TESTING.md`.
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
//! ## `CoreEvent` emission (ticket W4-00) — a second, sibling drain queue
//!
//! [`rf_core_api::CoreEvent`]/[`rf_core_api::EventMask`] shipped complete in
//! W0-04 but nothing in this crate ever called [`CoreSink::event`] before
//! this ticket. The design follows the "`CoreSink` emission seam" section
//! above exactly: [`Ppu::tick`] and [`Ppu::write_register`] (`$2005`) run
//! deep inside [`crate::system::NesBus`], with no `&mut dyn CoreSink` in
//! reach, so events are queued (`Ppu::events`, a `Vec<CoreEvent>` sibling of
//! `completed`) and flushed by [`Ppu::drain`] alongside the video scanlines,
//! in the same call.
//!
//! **One queue, not two.** `crate::system::NesBus` owns three of the eight
//! emitted variants (`DmaStart`, `OamRewrite`, `MapperIrq` — none of them
//! PPU state), but pushes them into `Ppu::events` too, via the
//! `pub(crate)` [`Ppu::queue_event`]/[`Ppu::event_mask`] pair, rather than
//! keeping a second bus-level queue drained separately. A second queue
//! drained after this one would silently destroy cross-source ordering —
//! `MapperIrq` landing on the same scanline as a `ScrollWrite` (the MMC3
//! raster-split case `docs/design/ENHANCEMENT_RUNTIME.md` §3's stitcher
//! cares about) must drain in the order the two actually happened, and
//! `NesBus::tick_master`'s per-dot loop (module doc, "PPU tick seam") is
//! already the single place bus- and PPU-driven state advance in lockstep,
//! so pushing both kinds of event into the one FIFO that loop naturally
//! visits in order costs nothing extra and preserves that order for free.
//!
//! **Gating is the caller's job, every time** (FR-CORE-006, mirrored from
//! `rf-core-api/tests/mock_core.rs`'s own convention): every call site —
//! four in this module/`scroll.rs`, three in `crate::system::mod` — reads
//! `if self.event_mask.is_subscribed(EventMask::X) { ... push ... }`
//! itself; [`Ppu::queue_event`] does not re-check. `EventMask::NONE` (the
//! `Ppu::new` default, matching `rf_core_api::CoreConfig::event_mask`'s own
//! default and law 6) means every one of those checks is a single `u32` AND
//! against a `const`, evaluated a small, fixed number of times per frame,
//! and the queue itself never grows.
//!
//! **Not emitted:** [`rf_core_api::CoreEvent::MemWatch`] — this ticket's own
//! acceptance list omits it deliberately (it is debugger-configured, not
//! hardware-produced, and belongs to whichever ticket builds watchpoint
//! configuration).
//!
//! **The hazard** (this ticket's `plan.json` note, W3-05a's own near-miss):
//! every site below reads data already in a register (`t`/`x`/`scanline`/
//! `mapper.irq_pending()`, itself a pure getter `crate::system::NesBus`
//! already called for `irq_line()`) — none goes through [`Ppu::mem_read`]
//! or any other side-effecting accessor, so none can perturb
//! [`Ppu::observe_ppu_bus_address`]'s A12 filter or any other simulation
//! state. See `crates/rf-nes/src/ppu/tests/event_emission.rs` and
//! `crates/rf-nes/src/system/tests/events.rs` for the field-by-field proof.
//!
//! ## Sprite-limit-bypass overlay (ticket W3-05a) — a staging decision, not
//! where the reconstruction belongs long-term
//!
//! Brad's 2026-08-03 sink ruling says the bypass reconstructs dropped
//! sprites from OAM via `StateView`/`SpriteHistorian` in `rf-enhance`. That
//! is the right END state and was NOT reachable when this ticket landed:
//! `rf-nes` implemented no `EmulatorCore::state_view` (W2-04's job), and
//! nothing outside this crate could reach CHR pattern data or palette RAM —
//! `rf-enhance` could see *which* sprites OAM held but not render one. So
//! this ticket records the dropped sprites' pixels HERE, inside the PPU,
//! where the pattern/palette data already live, into a side buffer
//! (`sprites.rs`'s `overlay_sprites`/`overlay_line_buffer`) the app
//! composites via a second, independent [`CoreSink::overlay_scanline`]
//! channel — never through [`PpuPixel`]/`video_scanline`, which stays
//! accuracy-exact (`PpuPixel::dropped_by_limit`'s doc). When `StateView`
//! lands, W3-05 is expected to move this to `rf-enhance` per the ruling;
//! this is documented here so a later reader finds a staging decision, not
//! a mistake. See `sprites.rs`'s own doc for the recording/compositing
//! design and the pure-observation argument (must not perturb
//! `self.status`, secondary OAM, the fetch pipeline, or timing, on or off).
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
mod state;

#[cfg(test)]
mod tests;

use rf_cart::Mirroring;
use rf_core_api::{CoreEvent, CoreSink, EventMask, OverlayPixel, PixelLayer, PpuPixel};

/// Scanline 261 is the pre-render line (some sources call it -1);
/// represented as an unsigned value here purely to avoid a signed
/// scanline counter elsewhere in this module — nesdev.org/wiki/PPU_rendering
/// uses both `-1` and `261` interchangeably for the same line.
const PRERENDER_SCANLINE: u16 = 261;
const POSTRENDER_SCANLINE: u16 = 240;
const VBLANK_START_SCANLINE: u16 = 241;
const DOTS_PER_SCANLINE: u16 = 341;

/// How long a refreshed decay-register bit survives, in frames (ticket
/// W2-19). nesdev and blargg's `ppu_open_bus` readme both state "about
/// 600 milliseconds"; at NTSC's ~60.1 Hz that is ~36 frames.
///
/// The ROM brackets this from one side only — it refreshes, waits a full
/// second, and requires 0 — so any value comfortably under ~60 frames
/// passes. 36 is used because it is what the hardware documentation says,
/// not because it is what makes the test go green.
const DECAY_FRAMES: u8 = 36;

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
    /// The overlay layer for this same row (ticket W3-05a; module doc's
    /// "Sprite-limit-bypass overlay" section) — all-transparent whenever
    /// `sprite_overlay_enabled` was `false` for this scanline, since
    /// `sprites.rs`'s `record_overlay_sprites` never populates
    /// `overlay_sprites` in that case. Always present (not `Option`) so
    /// `finish_scanline` never needs to branch on the flag.
    overlay: [OverlayPixel; 256],
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
/// One background tile the PPU actually drew, and where it landed.
///
/// **Recorded at the moment the shift registers are reloaded**, which is
/// the only place the tile index and its attribute bits exist together
/// with the scroll state that positions them. A caller cannot reconstruct
/// this from a frame-end snapshot of `v`: a game that changes scroll
/// mid-frame — which is how nearly every status bar is drawn — would have
/// every tile placed against the wrong scroll (ticket W11-05).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrawnTile {
    /// Screen x of the tile's leftmost pixel. Signed: fine-X scrolling
    /// pushes the first tile of a line partly off the left edge.
    pub x: i16,
    /// Visible scanline the tile's top row lands on.
    pub y: u16,
    /// Pattern index within the pattern table `base` selects.
    pub tile: u8,
    /// `$0000` or `$1000` — `PPUCTRL` bit 4 at fetch time.
    pub base: u16,
    /// Background palette 0-3, from the attribute byte's quadrant.
    pub palette: u8,
}

/// One sprite tile the PPU drew, and where it landed.
///
/// Separate from [`DrawnTile`] because a sprite carries flips that a
/// background tile cannot: a pack's replacement art has to be mirrored
/// the same way the original was, or a character faces the wrong way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrawnSprite {
    /// Screen x of the tile's leftmost pixel.
    pub x: i16,
    /// Screen y of the tile's TOP row. Sprites display at OAM `y + 1`.
    pub y: i16,
    pub tile: u8,
    pub base: u16,
    /// Sprite palette 0-3 (`$3F10`-relative, not `$3F00`).
    pub palette: u8,
    pub flip_x: bool,
    pub flip_y: bool,
}

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
    /// Ticket W11-05: record every background tile drawn this frame.
    /// **Off by default and free when off** — the capture is one branch
    /// at each reload dot, and the vector is never allocated.
    pub(super) tile_capture: bool,
    /// Tiles of the frame currently being drawn.
    pub(super) drawn_tiles: Vec<DrawnTile>,
    /// Sprite tiles of the frame being drawn, and of the last complete
    /// one — the same two-buffer arrangement, for the same reason.
    pub(super) drawn_sprites: Vec<DrawnSprite>,
    pub(super) completed_sprites: Vec<DrawnSprite>,
    /// The last COMPLETE frame's tiles, which is what readers get.
    ///
    /// **Two buffers, because the consumer and the capture window do not
    /// line up.** A frame's visible tiles finish at scanline 239, but the
    /// only frame-boundary signal this crate has is the pre-render line
    /// wrapping to scanline 0 — twenty-two scanlines later, by which time
    /// the NEXT frame's prefetch has already been recorded. A single
    /// buffer therefore hands the reader two tiles of the coming frame
    /// instead of the whole of the finished one, which is exactly what it
    /// did: `drawn_tiles` returned 2 or 3 every frame.
    pub(super) completed_tiles: Vec<DrawnTile>,
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

    /// The PPU's own I/O bus latch — its "decay register" (ticket W2-19;
    /// nesdev.org/wiki/PPU_registers "The PPU I/O bus", and blargg's
    /// `ppu_open_bus` readme, whose table this implements verbatim).
    ///
    /// This is **separate from the CPU bus's open-bus latch**
    /// ([`crate::system::NesBus::open_bus`]) — the `ppu_open_bus` readme
    /// opens by saying exactly that: "Unlike other open-bus addresses,
    /// the PPU ones are separate." Before W2-19 this crate had only the
    /// CPU-side latch and passed it into the PPU, which is why
    /// `ppu_open_bus` failed at its very first check and
    /// `cpu_exec_space`'s PPU arm failed independently with the same
    /// complaint.
    ///
    /// Refresh rules, from the readme's table (`D` = reads back from the
    /// decay register and does NOT refresh it; `-` = driven by the PPU
    /// and DOES refresh it):
    ///
    /// ```text
    /// $2000 DDDDDDDD   $2004 --------
    /// $2001 DDDDDDDD   $2005 DDDDDDDD
    /// $2002 ---DDDDD   $2006 DDDDDDDD
    /// $2003 DDDDDDDD   $2007 -------- non-palette / DD------ palette
    /// ```
    ///
    /// A write to **any** `$2000-$2007` register sets all eight bits.
    /// `false` selects EMULATION_CORES §5's **simplified** open-bus
    /// model instead of the full one (ticket W3-07;
    /// [`rf_core_api::CoreConfig::accuracy_mode`]).
    ///
    /// The two paths differ in exactly one thing: whether the decay
    /// register ages. Accuracy runs [`Ppu::age_decay_register`] once per
    /// frame, so an unrefreshed bit falls to 0 after ~600 ms the way
    /// hardware's does; compatibility skips it, so the latch simply holds
    /// what was last written to it. That "latch with no decay" is the
    /// model most emulators ship and is what THIS crate did before W2-19
    /// added the real one, which is why both sides genuinely exist here —
    /// it is not a fast path invented to have something to diff.
    ///
    /// **Being honest about what it buys:** simplicity, not measurable
    /// speed. Ageing eight bits once per frame is not a hot path. §5's
    /// column for this row is "full | simplified", not fast/slow, and the
    /// reason to build it now is that it is the one §5 switch this crate
    /// can offer both sides of — which is what makes W3-07's diff a real
    /// comparison rather than a path against itself.
    ///
    /// §5 also requires the divergence be **test-suite-visible**, and it
    /// is: `ppu_open_bus`'s tests 3, 5, 7 and 9 all assert that an
    /// unrefreshed bit reaches 0 within a second, so that ROM passes under
    /// Accuracy and fails under Compatibility. A switch that diverged
    /// nowhere would, by §5's own sentence, not be a switch.
    pub(super) accuracy_mode: bool,
    pub(super) decay: u8,
    /// Per-bit time-to-live for [`Ppu::decay`], in frames; 0 means that
    /// bit has already decayed to 0.
    ///
    /// Per-bit rather than one timer for the whole byte because the ROM
    /// requires it: its tests 7 and 9 refresh only *part* of the register
    /// (reading `$2002` refreshes bits 7-5; reading palette `$2007`
    /// refreshes bits 5-0) in a loop for a full second, and then assert
    /// that the *unrefreshed* bits have decayed anyway. A single shared
    /// timer would be kept alive by those reads and fail both.
    pub(super) decay_ttl: [u8; 8],
    /// Two-dot shift register of [`Ppu::rendering_enabled`], pushed once at
    /// the top of every [`Ppu::tick`] (ticket W1-05d). Bit 0 is the value
    /// in effect during the dot being ticked right now, bit 1 the previous
    /// dot's, bit 2 the one two dots back — which is the bit the odd-frame
    /// idle-dot skip decision reads (see [`Ppu::advance_counters`]).
    ///
    /// **Why a delay exists at all**: nesdev.org/wiki/PPU_registers's
    /// `PPUMASK` section, verbatim — "Toggling rendering takes effect
    /// approximately 3-4 dots after the write. This delay is required by
    /// Battletoads to avoid a crash." A `$2001` write is therefore *not*
    /// visible to rendering logic on the very next dot, which is what the
    /// unlatched `self.mask` read this replaced assumed.
    ///
    /// **Why exactly two dots, and only here**: the depth is measured
    /// against the ROM, not read off that "3-4" (which counts from a
    /// hardware write instant this engine does not model — `NesBus::write`
    /// applies `write_untimed` *before* its own cycle's three dots, so a
    /// write lands at the start of its cycle's dot window, up to three dots
    /// ahead of the physical latch). `10-even_odd_timing` pins the value
    /// exactly: its sub-tests 2 and 3 enable BG one PPU dot apart (blargg's
    /// `sync_vbl_delay` with A=4 vs A=5) and both expect X=8, i.e. hardware
    /// skips for the earlier write and not the later one. Instrumented,
    /// those two writes land at pre-render dots 337 and 338 in this
    /// engine's coordinates, and the skip decision runs after dot 339 — so
    /// "enabled at dot 339 - 2" is the only depth that accepts 337 and
    /// rejects 338. The latch is deliberately confined to the skip
    /// decision rather than applied to `rendering_enabled` globally: no
    /// test in this tree measures the delay anywhere else, and
    /// `sprite_hit_tests` 09/11 plus both golden frames encode the current
    /// undelayed behavior for the fetch pipeline. Widening it is a real
    /// hardware refinement, but it must be re-verified against those,
    /// not assumed.
    render_enable_pipe: u8,
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
    /// The render-ready snapshot [`Ppu::run_sprite_fetch_dot`] latches
    /// across dots 257-320, used by [`Ppu::output_pixel`]'s sprite
    /// compositing while drawing the CURRENT scanline.
    active_sprites: [SpriteUnit; 8],
    active_sprite_count: u8,
    /// The most recent sprite pattern-table LOW byte fetched by
    /// [`Ppu::run_sprite_fetch_dot`] (ticket W2-03), held from that slot's
    /// phase-5 dot until its phase-7 dot latches the completed
    /// [`SpriteUnit`] — the same "reload at the second half of a two-part
    /// fetch" shape [`Ppu::pt_lo_latch`]/[`Ppu::pt_hi_latch`] already use
    /// for background tiles, needed here because the fetch is now spread
    /// across real dots instead of computed all at once.
    sprite_pattern_lo_latch: u8,

    // ---- sprite-limit-bypass overlay (ticket W3-05a; module doc's
    // "Sprite-limit-bypass overlay" section, `sprites.rs`'s module doc) ----
    /// Opt-in switch (default `false` — CLAUDE.md law 6, FR-MODE-002).
    /// Gates only whether [`sprites::Ppu::record_overlay_sprites`] (called
    /// from `evaluate_sprites`) does its extra OAM scan/CHR fetch work each
    /// scanline — never anything in the accuracy path itself.
    sprite_overlay_enabled: bool,
    /// The sprites the 8-per-scanline limit dropped, as evaluated during
    /// THIS scanline's dot 65 (empty whenever `sprite_overlay_enabled` was
    /// `false` at record time) — the overlay's analogue of `secondary_oam`,
    /// with the exact same one-scanline-pipeline-delay relationship to
    /// [`overlay_active_sprites`] that `secondary_oam` has to
    /// `active_sprites` (`sprites.rs` module doc's "one-scanline pipeline
    /// delay" section): NOT read directly by [`Ppu::output_pixel`], which
    /// would otherwise see this scanline's own dot-65 write partway through
    /// its own dot 1-256 pixel loop (dots 1-64 before the write, 65-256
    /// after) — a genuine mid-scanline tear, not a one-scanline-early
    /// symptom. [`Ppu::reset_sprite_output_units`] copies this into
    /// `overlay_active_sprites` at dot 257, the same latch point
    /// `active_sprite_count = secondary_oam_count` already uses, so by the
    /// time the scanline that renders it starts, the render-time copy is
    /// frozen and uniform across every x.
    overlay_sprites: Vec<SpriteUnit>,
    /// The render-ready snapshot of `overlay_sprites`, latched by
    /// [`Ppu::reset_sprite_output_units`] at dot 257 of the PRECEDING
    /// scanline — [`Ppu::overlay_pixel`]'s only reader, mirroring
    /// `active_sprites`'s own role for the accuracy sprite layer (see
    /// `overlay_sprites`'s doc for why reading `overlay_sprites` directly
    /// would tear mid-scanline).
    overlay_active_sprites: Vec<SpriteUnit>,
    /// The overlay layer being built for the CURRENT scanline, written
    /// pixel-by-pixel by `sprites.rs`'s `output_pixel` alongside (never
    /// instead of) `line_buffer` — a brand-new field nothing on the
    /// accuracy path reads, which is exactly what makes writing it
    /// structurally incapable of perturbing `self.status`/`line_buffer`/
    /// timing.
    overlay_line_buffer: [OverlayPixel; 256],

    // ---- scanline output ----
    line_buffer: [PpuPixel; 256],
    completed: Vec<CompletedScanline>,

    // ---- MMC3 A12 edge filter (ticket W2-03; see `ppu/mem.rs`'s module
    // doc "A12 rising-edge detection" section) ----
    /// Monotonic PPU-dot counter, incremented once per [`Ppu::tick`] —
    /// the filter's own time base (nesdev's "3 CPU cycles" translated to
    /// "9 PPU dots", exact because `NesBus::tick_master` guarantees
    /// exactly 3 dots per CPU cycle always).
    dot_clock: u64,
    /// `dot_clock` at the most recent CPU read of `$2007`, and the value
    /// that read returned — the state behind the double-read quirk
    /// (ticket W2-01d).
    ///
    /// blargg's `dmc_dma_during_read4/double_2007_read` names it in its
    /// own header: "Double read of $2007 sometimes ignores extra read,
    /// and puts odd things into buffer". It provokes the case with
    /// `lda $20F7,x` where `x = $10`: `$20F7 + $10 = $2107` **crosses a
    /// page**, so the 6502 issues its dummy read at `$2007` and the real
    /// read at `$2107` (also `$2007` after mirroring) on back-to-back
    /// cycles.
    ///
    /// On hardware the second of those two reads reports the value the
    /// first one already presented — the PPU cannot drive a fresh byte
    /// one cycle later — while the read buffer and `v` still advance
    /// twice. That is the difference between this crate's old
    /// `33 44 55 66 77` and the ROM's first accepted variant
    /// `22 44 55 66 77`: same internal progress, one stale value out.
    ///
    /// Held in dots rather than CPU cycles because that is the clock this
    /// module owns: [`NesBus::read`] performs the register read *before*
    /// ticking the cycle's three dots, so two reads on consecutive CPU
    /// cycles are exactly three dots apart.
    last_2007_read_dot: Option<u64>,
    last_2007_read_value: u8,
    /// The `dot_clock` value at which A12 was first observed low since
    /// the last consumed rising edge; `None` while A12 is high or while a
    /// low period hasn't been sampled yet.
    a12_low_since: Option<u64>,
    /// Filtered A12 rising edges recorded since the last
    /// [`Ppu::take_a12_edges`] drain.
    pending_a12_edges: u32,

    // ---- CoreEvent emission (ticket W4-00; module doc's "`CoreEvent`
    // emission" section) ----
    /// Which [`CoreEvent`] variants to queue. `NONE` on a freshly
    /// constructed `Ppu` (law 6 / matches [`rf_core_api::CoreConfig`]'s own
    /// default) — every gating check below is then a single `u32` AND
    /// against a `const`, and [`Ppu::events`] never grows.
    event_mask: EventMask,
    /// Ticket W13-02e: the debugger's watchpoints. Not part of the
    /// machine's state — it is a debugger setting, so `state.rs` does not
    /// serialize it and a save state cannot silently carry someone else's
    /// watchpoints into your session.
    watches: rf_core_api::WatchTable,
    /// Events queued since the last [`Ppu::drain`], oldest first — the
    /// `CoreEvent` sibling of `completed` (module doc). Holds BOTH this
    /// PPU's own tick/register-write-driven events AND, pushed via
    /// [`Ppu::queue_event`], `crate::system::NesBus`'s bus-level ones
    /// (`DmaStart`/`OamRewrite`/`MapperIrq`) — one FIFO so cross-source
    /// order is preserved (module doc).
    events: Vec<CoreEvent>,
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
};

/// A transparent overlay pixel — [`Ppu::new`]'s initial `overlay_line_buffer`
/// fill and `sprites.rs`'s `overlay_pixel`'s "nothing to draw" result.
const BLANK_OVERLAY_PIXEL: OverlayPixel = OverlayPixel {
    palette_index: 0,
    opaque: false,
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
            tile_capture: false,
            drawn_tiles: Vec::new(),
            completed_tiles: Vec::new(),
            drawn_sprites: Vec::new(),
            completed_sprites: Vec::new(),
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
            accuracy_mode: true,
            decay: 0,
            decay_ttl: [0; 8],
            render_enable_pipe: 0,
            frame_count: 0,
            suppress_vblank_this_frame: false,
            secondary_oam: [EMPTY_EVALUATED_SPRITE; 8],
            secondary_oam_count: 0,
            active_sprites: [EMPTY_SPRITE_UNIT; 8],
            active_sprite_count: 0,
            sprite_pattern_lo_latch: 0,
            sprite_overlay_enabled: false,
            overlay_sprites: Vec::new(),
            overlay_active_sprites: Vec::new(),
            overlay_line_buffer: [BLANK_OVERLAY_PIXEL; 256],
            line_buffer: [BLANK_PIXEL; 256],
            completed: Vec::with_capacity(240),
            dot_clock: 0,
            last_2007_read_dot: None,
            last_2007_read_value: 0,
            a12_low_since: None,
            pending_a12_edges: 0,
            event_mask: EventMask::NONE,
            watches: rf_core_api::WatchTable::new(),
            events: Vec::new(),
        }
    }

    /// The full 256-byte OAM, for [`crate::system::NesBus::oam`] (tests,
    /// and the future sprite-evaluation ticket).
    pub fn oam(&self) -> &[u8; 256] {
        &self.oam
    }

    /// The 4 KiB nametable VRAM, as it currently stands (ticket W4-06d).
    ///
    /// **Non-observing, and that is the whole requirement.** A plain
    /// borrow — deliberately NOT a read through [`Ppu::mem_read`], which
    /// unconditionally feeds `observe_ppu_bus_address`, the A12
    /// rising-edge filter MMC3's scanline IRQ counter depends on
    /// (`mem.rs`'s "A12 rising-edge detection" section). The same
    /// distinction `chr_peek` exists for, and it matters more here: a
    /// debug viewer repaints continuously, so an observing accessor would
    /// perturb IRQ timing on every frame the panel is open — W3-05a's
    /// hazard class, invisible to pixel comparison and fatal to mapper
    /// behaviour.
    ///
    /// Mirrors [`Ppu::oam`] exactly in shape for the same reason: a
    /// borrow cannot have a side effect, so the guarantee is structural
    /// rather than something a future edit has to remember.
    pub fn vram(&self) -> &[u8; 0x1000] {
        &self.vram
    }

    /// The 32-byte palette RAM (`$3F00-$3F1F`), as it currently stands
    /// (ticket W4-06d). Non-observing, same reasoning as [`Ppu::vram`].
    ///
    /// Returned RAW, without the `$3F10/$14/$18/$1C` → `$3F00/$04/$08/$0C`
    /// backdrop-mirroring the PPU applies on read: a debugger should show
    /// what is actually stored, and a viewer that wants the mirrored view
    /// can apply the rule itself. Hiding a difference between the raw
    /// bytes and the rendered result is precisely what a palette viewer
    /// exists to reveal.
    pub fn palette(&self) -> &[u8; 32] {
        &self.palette
    }

    /// Current `OAMADDR` (test/debug visibility, same as the old
    /// `ppu_stub` this ticket replaces).
    /// Ticket W11-05: start or stop recording the tiles the PPU draws.
    ///
    /// Pay-for-use (`ARCHITECTURE` §5): with this off the render path
    /// costs one already-predicted branch per reload dot and allocates
    /// nothing. Turning it off also drops what was collected, so a
    /// disabled capture cannot leave a stale frame's tiles behind for a
    /// caller to mistake for the current one.
    pub fn set_tile_capture(&mut self, on: bool) {
        self.tile_capture = on;
        if !on {
            self.drawn_tiles = Vec::new();
            self.completed_tiles = Vec::new();
            self.drawn_sprites = Vec::new();
            self.completed_sprites = Vec::new();
        }
    }

    /// The background tiles of the frame just completed.
    ///
    /// Named for what it returns: the PUBLISHED buffer, not the one still
    /// filling. Empty unless [`Ppu::set_tile_capture`] is on.
    #[must_use]
    pub fn completed_tiles(&self) -> &[DrawnTile] {
        &self.completed_tiles
    }

    /// The sprite tiles of the frame just completed (ticket W11-14).
    #[must_use]
    pub fn completed_sprites(&self) -> &[DrawnSprite] {
        &self.completed_sprites
    }

    /// The cartridge's CHR, pattern tables included.
    ///
    /// A plain non-observing borrow, exactly like [`Ppu::vram`] — reading
    /// it cannot perturb A12 edge timing, which is why that method's doc
    /// calls non-observing the load-bearing property. Needed because an
    /// HD pack rule for a CHR-RAM game is keyed on the tile's BYTES
    /// rather than its index.
    #[must_use]
    pub fn chr(&self) -> &[u8] {
        &self.chr
    }

    /// Is CHR writable (CHR-RAM) rather than fixed ROM?
    #[must_use]
    pub fn chr_is_ram(&self) -> bool {
        self.chr_is_ram
    }

    pub fn oam_addr(&self) -> u8 {
        self.oam_addr
    }

    /// Whether the sprite-limit-bypass overlay is currently recording
    /// (ticket W3-05a; module doc's "Sprite-limit-bypass overlay" section).
    /// `false` on a freshly constructed `Ppu` (law 6: a fresh install boots
    /// in Accuracy Mode).
    #[must_use]
    pub fn sprite_overlay_enabled(&self) -> bool {
        self.sprite_overlay_enabled
    }

    /// Opt into (or out of) the sprite-limit-bypass overlay. Pure
    /// state-toggle — see module doc: turning this on or off never touches
    /// `self.status`, secondary OAM, the fetch pipeline, or timing, either
    /// at the moment of the call or on any later scanline.
    pub fn set_sprite_overlay_enabled(&mut self, enabled: bool) {
        self.sprite_overlay_enabled = enabled;
    }

    /// Current `CoreEvent` subscription mask (ticket W4-00; module doc's
    /// "`CoreEvent` emission" section). `pub(crate)`: only
    /// `crate::system::NesBus` needs to read this, to gate its OWN
    /// bus-level events (`DmaStart`/`OamRewrite`/`MapperIrq`) before
    /// queuing them here — everything else configures the mask through
    /// [`Ppu::set_event_mask`] (or, for a `NesBus`, its own
    /// `set_event_mask`) rather than inspecting it.
    pub(crate) fn event_mask(&self) -> EventMask {
        self.event_mask
    }

    /// Set the `CoreEvent` subscription mask (ticket W4-00). `pub`, unlike
    /// [`Ppu::event_mask`] above, for the same reason
    /// [`Ppu::set_sprite_overlay_enabled`] is: `benches/event_emission.rs`
    /// and this module's own tests configure a bare `Ppu` directly, with no
    /// `NesBus`/`EmulatorCore` in the loop. `NONE` (Accuracy mode's
    /// default, law 6) until called.
    /// Install the debugger's watchpoints (ticket W13-02e), pushed to the
    /// PPU for the same reason [`Ppu::set_event_mask`] is: this is where
    /// the event queue lives, so this is where a hit can be reported from
    /// without either bus needing its own channel.
    pub fn set_watches(&mut self, watches: rf_core_api::WatchTable) {
        self.watches = watches;
    }

    /// The installed watchpoints — read by both buses on every access,
    /// which is why `WatchTable::is_armed` is checked first.
    pub(crate) fn watches(&self) -> &rf_core_api::WatchTable {
        &self.watches
    }

    /// Report a watch hit, if this access trips one and anybody is
    /// subscribed. Observation only: it queues an event on the channel
    /// `EventMask` already gates and touches nothing else.
    pub(crate) fn note_watch_access(
        &mut self,
        space: rf_core_api::WatchSpace,
        access: rf_core_api::WatchAccess,
        addr: u32,
        value: u8,
    ) {
        if !self.watches.is_armed() || !self.event_mask().is_subscribed(EventMask::MEM_WATCH) {
            return;
        }
        if let Some(id) = self.watches.hit(space, access, addr, value) {
            self.queue_event(CoreEvent::MemWatch { id });
        }
    }

    pub fn set_event_mask(&mut self, mask: EventMask) {
        self.event_mask = mask;
    }

    /// Select the accuracy (`true`) or compatibility (`false`) open-bus
    /// model — see [`Ppu::accuracy_mode`] (ticket W3-07).
    pub fn set_accuracy_mode(&mut self, accuracy: bool) {
        self.accuracy_mode = accuracy;
    }

    /// Push one already-gated event onto the drain queue (ticket W4-00;
    /// module doc's "One queue, not two" section) — `pub(crate)` so
    /// `crate::system::NesBus` can queue its own bus-level events into the
    /// SAME FIFO this `Ppu`'s tick/register-write-driven events use,
    /// preserving cross-source temporal order. Does NOT itself check
    /// [`Ppu::event_mask`] — every call site (in this module, `scroll.rs`,
    /// and `crate::system::mod`) does that itself before constructing `ev`,
    /// per FR-CORE-006 ("there is nothing cheaper to construct than don't
    /// construct at all" — [`CoreEvent`]'s own doc).
    pub(crate) fn queue_event(&mut self, ev: CoreEvent) {
        self.events.push(ev);
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
    /// The catch-up scheduler's predicate (ticket W3-07b;
    /// `docs/design/EMULATION_CORES.md` §5 row 1): how many dots from the
    /// current position are **provably inert** — no state change other
    /// than the dot counters themselves.
    ///
    /// Returns 0 if the dot about to be processed is not inert. This is
    /// the "compute when the next observable event is" half of §5's
    /// catch-up row; [`Ppu::skip_inert_dots`] is the "jump to it" half.
    ///
    /// ## Why each region is inert, one clause per observer
    ///
    /// An inert dot must change **nothing** three separate subsystems can
    /// see at dot granularity (the analysis W3-07 filed with this ticket):
    /// `nmi_line` (sampled into `NesBus::nmi_level_latch` before every
    /// dot, and `ppu_vbl_nmi` 05-08 is 10/10 on that being per-dot), the
    /// A12 rising edges `NesBus::tick_ppu_dot` drains into the mapper
    /// every dot, and the dot-count-keyed state W2-19/W2-01d added.
    ///
    /// | scanlines | inert when | why |
    /// |---|---|---|
    /// | 0-239 | rendering off, and dot 0 or dot > 256 | with rendering off `process_render_dot` runs nothing but `output_pixel` (dots 1-256) and `finish_scanline` (dot 256); every other branch in it is gated on `rendering_enabled()` |
    /// | 240 | always | post-render: `process_dot`'s own comment, "genuinely idle, nothing to do" |
    /// | 241 | dot != 1 | dot 1 sets `STATUS_VBLANK` and queues `CoreEvent::VblankStart` — the one dot in vblank that moves `nmi_line` |
    /// | 242-260 | always | the rest of vblank, same idle arm |
    /// | 261 | rendering off, and dot != 1 | dot 1 clears vblank/sprite-0/overflow (moves `nmi_line`) and clears the sprite units; with rendering off the rest of `process_render_dot(false)` is entirely gated off |
    ///
    /// **Rendering-enabled dots are never inert**, which is not just
    /// conservatism — it is what makes A12 safe without a second
    /// predicate. Every PPU-side A12 edge comes from a background or
    /// sprite pattern fetch, and those only happen with rendering on. A
    /// CPU-side edge (a `$2006`/`$2007` access) cannot be missed either,
    /// and that falls out of there being **no cached run**: the predicate
    /// is re-derived from live state every time the bus asks, so a
    /// register access that changed `mask`, `v` or the vblank flag is
    /// already reflected in the next answer. A cached-run-plus-invalidation
    /// design was considered and rejected — the predicate is a `match` and
    /// three comparisons against three `process_dot` calls, so the cache
    /// would have bought a few operations per CPU cycle in exchange for an
    /// invalidation obligation on every one of `NesBus`'s paths into the
    /// PPU, which is precisely the class of bug this ticket's design note
    /// warns about.
    ///
    /// It is also what keeps `render_enable_pipe` correct, and that one is
    /// worth stating because a future edit to the table above could break
    /// it silently: `advance_counters` reads `render_enable_pipe & 0b100`
    /// at scanline 261 dot 339 for the odd-frame skip. Scanline 261 is
    /// only ever inert with rendering **off**, so the pipe is being fed a
    /// constant 0 across any run that reaches dot 339 — and 0 is what the
    /// skip check needs to see. The run is additionally capped below so
    /// dot 339 is always processed by a real `tick`.
    ///
    /// ## Capping
    ///
    /// A run never crosses a scanline boundary, and stops short of dot
    /// 339: the last dot a run may cover is 338, so dots 339 and 340
    /// always go through [`Ppu::tick`]. That single cap retires every
    /// wrap-related hazard at once — the odd-frame skip, `frame_count`,
    /// `age_decay_register`, and the end-of-scanline video emission all
    /// live in `advance_counters`, which a skipped dot never calls. The
    /// cost is ~20 predicate re-evaluations per frame, which the
    /// measurement in `docs/TESTING.md` says is not where the time goes.
    pub(crate) fn inert_run_len(&self) -> u16 {
        const LAST_SKIPPABLE_DOT: u16 = 338;
        if self.dot > LAST_SKIPPABLE_DOT {
            return 0;
        }
        let rendering = self.rendering_enabled();
        let inert_through = match self.scanline {
            0..=239 => {
                if rendering {
                    return 0;
                }
                // Dot 0 alone, then everything past the last output dot.
                if self.dot == 0 {
                    0
                } else if self.dot > 256 {
                    LAST_SKIPPABLE_DOT
                } else {
                    return 0;
                }
            }
            POSTRENDER_SCANLINE => LAST_SKIPPABLE_DOT,
            VBLANK_START_SCANLINE => {
                if self.dot == 1 {
                    return 0;
                }
                if self.dot == 0 {
                    0
                } else {
                    LAST_SKIPPABLE_DOT
                }
            }
            242..=260 => LAST_SKIPPABLE_DOT,
            PRERENDER_SCANLINE => {
                if rendering || self.dot == 1 {
                    return 0;
                }
                if self.dot == 0 {
                    0
                } else {
                    LAST_SKIPPABLE_DOT
                }
            }
            _ => return 0,
        };
        inert_through - self.dot + 1
    }

    /// A lower bound on how many dots from here [`Ppu::inert_run_len`] is
    /// guaranteed to keep returning 0, assuming `mask` does not change
    /// (ticket W3-07b).
    ///
    /// **This is the half of the scheduler that makes it worth having,
    /// and the measurement says so.** Without it the bus evaluated the
    /// predicate once per dot — 89342 times a frame — to save work on the
    /// 8% of dots that are inert with rendering on, and those are the
    /// *cheapest* dots (their `process_dot` already falls straight
    /// through). Measured, that traded 1.092 ms/frame for 1.155 ms: the
    /// "catch-up" scheduler was 6% SLOWER than lock-step. With this, the
    /// predicate is evaluated a handful of times per frame instead.
    ///
    /// A lower bound is enough, and deliberately so: answering early
    /// costs one extra predicate evaluation, while answering late costs a
    /// missed inert run. Neither is a correctness failure — the caller
    /// only ever *ticks* while this answer is in force, which is exactly
    /// what Accuracy does (see `NesBus::catch_up_ppu_dots`) — so the
    /// pre-render line just returns "to the end of this scanline" rather
    /// than reasoning about the odd-frame skip's variable frame length.
    ///
    /// The caller discards the answer when `mask` may have changed, which
    /// is the only input to it that a CPU access can move. That is an
    /// optimisation, not a safety requirement.
    pub(crate) fn dots_until_possible_inert(&self) -> u16 {
        debug_assert_eq!(self.inert_run_len(), 0, "only meaningful when not inert");
        const LAST_SKIPPABLE_DOT: u16 = 338;
        if self.dot > LAST_SKIPPABLE_DOT {
            // Dots 339 and 340 are never skippable (see `inert_run_len`'s
            // capping section); the next candidate is dot 0 of the next
            // scanline.
            return DOTS_PER_SCANLINE - self.dot;
        }
        match self.scanline {
            // Rendering visible and pre-render lines have no inert dot at
            // all, so the true answer runs to post-render dot 0 — up to
            // 240 scanlines away. It is deliberately NOT computed that
            // way: 240 x 341 is 81840, which does not fit in the `u16`
            // these counters use, and in release that silently wraps.
            // (It did: the first version of this function returned a
            // wrapped value and `mode_diff` hung.) One scanline is a
            // lower bound, it cannot overflow, and it already cuts the
            // predicate from 89342 evaluations a frame to 262.
            0..=239 if self.rendering_enabled() => DOTS_PER_SCANLINE - self.dot,
            PRERENDER_SCANLINE if self.rendering_enabled() => DOTS_PER_SCANLINE - self.dot,
            // Rendering off: only dots 1-256 of a visible line are
            // non-inert.
            0..=239 => 257u16.saturating_sub(self.dot).max(1),
            // (241,1) and (261,1) with rendering off: exactly one dot.
            _ => 1,
        }
    }

    /// Advance `n` provably-inert dots without processing them (ticket
    /// W3-07b) — the "jump" half of the catch-up scheduler.
    ///
    /// The PPU's position is never left stale: this does in O(1) exactly
    /// what `n` calls to [`Ppu::tick`] would have done to the counters,
    /// and by [`Ppu::inert_run_len`]'s contract those calls would have
    /// done nothing else. That is the whole safety argument — there is no
    /// deferral, so no observer needs a flush protocol and none of the
    /// three dot-granularity observers changes at all.
    ///
    /// # Panics
    /// Debug-asserts that the caller respected `inert_run_len`'s cap.
    pub(crate) fn skip_inert_dots(&mut self, n: u16) {
        debug_assert!(n > 0 && self.dot + n <= 339, "run must not reach dot 339");
        self.dot_clock += u64::from(n);
        self.dot += n;
        // `tick` shifts one bit in per dot; across an inert run
        // `rendering_enabled()` cannot change (nothing inert writes
        // `mask`), so the pipe fills with n copies of one bit. Only the
        // low three bits are ever read, so a run of 3 or more saturates.
        let bit = u8::from(self.rendering_enabled());
        self.render_enable_pipe = if n >= 8 {
            if bit == 1 {
                u8::MAX
            } else {
                0
            }
        } else {
            let mut pipe = self.render_enable_pipe;
            for _ in 0..n {
                pipe = (pipe << 1) | bit;
            }
            pipe
        };
    }

    pub fn tick(&mut self) {
        self.dot_clock += 1;
        // Ticket W1-05d: sampled before `process_dot` because nothing in a
        // dot's own processing writes `mask` — this is the value in effect
        // *during* this dot, which is what the odd-frame skip decision two
        // dots later needs (see `render_enable_pipe`'s doc).
        self.render_enable_pipe =
            (self.render_enable_pipe << 1) | u8::from(self.rendering_enabled());
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
                // Ticket W4-00: unconditional, regardless of
                // `suppress_vblank_this_frame` above — that latch is only
                // about the `$2002` READ VALUE race (`scroll.rs`'s
                // `read_status` doc), a property of software polling.
                // Physically the PPU enters vertical blank at this dot on
                // EVERY frame; `CoreEvent::VblankStart`'s own doc ("PPU
                // entered vertical blank") describes that hardware fact,
                // not whether a CPU read of `$2002` happened to observe it.
                if self.event_mask.is_subscribed(EventMask::VBLANK_START) {
                    self.queue_event(CoreEvent::VblankStart);
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
            && self.render_enable_pipe & 0b100 != 0;

        if odd_frame_skip || self.dot >= DOTS_PER_SCANLINE - 1 {
            self.dot = 0;
            self.scanline = if self.scanline == PRERENDER_SCANLINE {
                self.frame_is_odd = !self.frame_is_odd;
                self.frame_count += 1;
                if self.accuracy_mode {
                    self.age_decay_register();
                }
                // Ticket W4-00: the pre-render line wrapping to scanline 0
                // is the only frame-boundary signal this crate has (module
                // doc, "`CoreSink` emission seam" section: no
                // `EmulatorCore::run_frame` exists here yet) — the outgoing
                // frame's `FrameEnd` and the incoming one's `FrameStart`
                // both land at this exact instant, in that order (matches
                // `CoreEvent::FrameEnd`/`FrameStart`'s own doc: "after the
                // last scanline" / "before the first scanline").
                if self.event_mask.is_subscribed(EventMask::FRAME_END) {
                    self.queue_event(CoreEvent::FrameEnd);
                }
                if self.event_mask.is_subscribed(EventMask::FRAME_START) {
                    self.queue_event(CoreEvent::FrameStart);
                }
                0
            } else {
                let next = self.scanline + 1;
                // Ticket W11-05: clear a frame's tiles when the PRE-RENDER
                // line BEGINS, not when it ends.
                //
                // The pre-render line's dots 329/337 prefetch the first two
                // tiles of scanline 0 — they belong to the frame about to
                // be drawn. Clearing at the pre-render WRAP (the natural
                // place, and where this code first put it) recorded those
                // two and then immediately wiped them, so every frame was
                // missing its top-left two tiles. The test caught it as
                // 7678 tiles instead of 7680.
                if next == PRERENDER_SCANLINE && self.tile_capture {
                    // PUBLISH the frame that just finished, then start
                    // collecting the next one — the pre-render line's own
                    // prefetch (dots 329/337) belongs to the coming frame.
                    self.completed_tiles = std::mem::take(&mut self.drawn_tiles);
                    self.completed_sprites = std::mem::take(&mut self.drawn_sprites);
                }
                next
            };
        } else {
            self.dot += 1;
        }
    }

    /// Age every live bit of the decay register by one frame, clearing
    /// the bits whose time is up (ticket W2-19).
    ///
    /// **Why frames and not dots.** nesdev and blargg's readme both give
    /// the decay time as "about 600 milliseconds", explicitly approximate
    /// — "some decay sooner, depending on the NES and temperature". A
    /// dot-accurate countdown would spend 8 decrements every one of the
    /// 89,342 dots in a frame to model a quantity the hardware itself
    /// does not hold precisely; ticking once per frame costs nothing on
    /// the hot path and is ~17 ms of resolution against a ~600 ms
    /// constant. What the ROM actually measures is coarse: it refreshes,
    /// waits a full second, and requires zero.
    fn age_decay_register(&mut self) {
        for bit in 0..8 {
            if self.decay_ttl[bit] > 0 {
                self.decay_ttl[bit] -= 1;
                if self.decay_ttl[bit] == 0 {
                    self.decay &= !(1 << bit);
                }
            }
        }
    }

    /// Age the decay register by one frame from a unit test, without
    /// ticking ~89,000 dots to get there (ticket W2-19).
    ///
    /// The decay tests need to advance tens of frames of *decay time*
    /// while performing a register access per frame; running the real
    /// dot loop for that would turn three sub-millisecond tests into
    /// multi-second ones and would drag in rendering state they are not
    /// about. The ROM itself remains the end-to-end check that the
    /// per-frame ageing is wired into `tick`.
    #[cfg(test)]
    pub(crate) fn age_decay_register_for_test(&mut self) {
        self.age_decay_register();
    }

    /// Set the decay-register bits selected by `mask` from `value`, and
    /// restart their decay clocks. Bits outside `mask` keep both their
    /// value and their remaining time — that distinction is exactly what
    /// `ppu_open_bus`'s tests 7 and 9 check.
    pub(super) fn refresh_decay(&mut self, value: u8, mask: u8) {
        self.decay = (self.decay & !mask) | (value & mask);
        for bit in 0..8 {
            if mask & (1 << bit) != 0 {
                self.decay_ttl[bit] = DECAY_FRAMES;
            }
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
    /// seam"), THEN every queued `CoreEvent` through `sink.event`, oldest
    /// first (ticket W4-00; module doc's "`CoreEvent` emission" section) —
    /// video before events is an arbitrary but harmless ordering choice
    /// (nothing documents or requires interleaving `CoreSink::event` calls
    /// with `video_scanline` calls; `CoreEvent::Scanline`'s own payload
    /// already carries the `y` a consumer would use to correlate the two).
    /// Safe to call at any time, including mid-frame; a real integration is
    /// expected to call it once per frame.
    pub fn drain(&mut self, sink: &mut dyn CoreSink) {
        // Ticket W3-05a: the overlay channel is gated on the CURRENT flag
        // (not a per-scanline stored one) so the default accuracy path
        // (overlay never enabled) pays for exactly zero extra `CoreSink`
        // calls — `line.overlay` is already guaranteed all-transparent
        // whenever the overlay was off at RECORD time (module doc), so
        // this gate is a pure perf skip, never a correctness difference.
        let overlay_on = self.sprite_overlay_enabled;
        for line in self.completed.drain(..) {
            sink.video_scanline(line.y, &line.pixels);
            if overlay_on {
                sink.overlay_scanline(line.y, &line.overlay);
            }
        }
        for ev in self.events.drain(..) {
            sink.event(ev);
        }
    }

    fn finish_scanline(&mut self) {
        // Ticket W4-00: `self.scanline` is still the completing line's own
        // index here (dot 256, before `advance_counters` ever runs) — the
        // same value `line.y` below carries into `video_scanline`, matching
        // `CoreEvent::Scanline`'s own doc ("same value as the `y` passed to
        // `video_scanline`").
        if self.event_mask.is_subscribed(EventMask::SCANLINE) {
            self.queue_event(CoreEvent::Scanline(self.scanline));
        }
        self.completed.push(CompletedScanline {
            y: self.scanline,
            pixels: self.line_buffer,
            overlay: self.overlay_line_buffer,
        });
    }
}
