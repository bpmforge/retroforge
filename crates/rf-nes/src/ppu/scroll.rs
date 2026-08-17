//! CPU-visible register read/write (`$2000-$2007`) and the loopy `v`/`t`/
//! `x`/`w` scroll-register bit manipulation
//! ([nesdev.org/wiki/PPU_scrolling](https://www.nesdev.org/wiki/PPU_scrolling)).
//!
//! `v`/`t` bit layout (nesdev's own diagram, reproduced verbatim):
//! ```text
//! yyy NN YYYYY XXXXX
//! ||| || ||||| +++++-- coarse X scroll
//! ||| || +++++-------- coarse Y scroll
//! ||| ++-------------- nametable select
//! +++----------------- fine Y scroll
//! ```
use super::{Ppu, PRERENDER_SCANLINE, STATUS_VBLANK, VBLANK_START_SCANLINE};
use rf_core_api::{CoreEvent, EventMask, PixelLayer};

impl Ppu {
    /// Dispatch a real (side-effecting) CPU read of register `index`
    /// (`addr & 7`). Only `$2002`/`$2004`/`$2007` are ever driven by the
    /// PPU; every other register is write-only and returns `open_bus`
    /// unchanged, matching real hardware (and the pre-existing `ppu_stub`
    /// behavior this ticket replaces, for the registers this ticket still
    /// doesn't implement reads for).
    pub fn read_register(&mut self, index: u8) -> u8 {
        match index {
            2 => self.read_status(),
            4 => {
                // Bits 2-4 of a sprite's attribute byte are not stored by
                // the OAM at all and always read back clear
                // (nesdev.org/wiki/PPU_OAM "Byte 2"; `ppu_open_bus` test
                // 10 asserts it). Byte 2 of each 4-byte sprite entry, so
                // the low two bits of the address select it.
                let mut value = self.oam[self.oam_addr as usize];
                if self.oam_addr & 0x03 == 0x02 {
                    value &= 0xE3;
                }
                // "$2004 --------": every bit is driven, so every bit
                // refreshes (test 11 checks precisely this by reading
                // $2004 and then $2000).
                self.refresh_decay(value, 0xFF);
                value
            }
            7 => {
                // The double-read quirk: a `$2007` read on the CPU cycle
                // immediately after another one reports the earlier
                // value, while the buffer/`v` side effects below still
                // happen twice (ticket W2-01d; see
                // `Ppu::last_2007_read_dot`).
                let contiguous = self.last_2007_read_dot == Some(self.dot_clock.wrapping_sub(3));
                let fresh = self.read_data();
                let reported = if contiguous {
                    self.last_2007_read_value
                } else {
                    fresh
                };
                self.last_2007_read_dot = Some(self.dot_clock);
                self.last_2007_read_value = reported;
                reported
            }
            // "$2000/$2001/$2003/$2005/$2006 DDDDDDDD": write-only
            // registers read back as the decay register and — test 5 —
            // do NOT refresh it.
            _ => self.decay,
        }
    }

    /// Side-effect-free read of register `index`, for
    /// [`crate::system::NesBus::peek`] (the trace logger's disassembly-only
    /// path — see that method's doc for why it must never mutate PPU
    /// state). Mirrors [`Controller::peek_bit`](crate::system::Controller)'s
    /// existing peek/read split convention.
    pub fn peek_register(&self, index: u8) -> u8 {
        match index {
            2 => (self.status & 0xE0) | (self.decay & 0x1F),
            4 => {
                let value = self.oam[self.oam_addr as usize];
                if self.oam_addr & 0x03 == 0x02 {
                    value & 0xE3
                } else {
                    value
                }
            }
            7 => self.read_buffer,
            _ => self.decay,
        }
    }

    /// Dispatch a CPU write to register `index`.
    pub fn write_register(&mut self, index: u8, value: u8) {
        // "Writing to any PPU register sets the decay register to the
        // value written" (blargg's `ppu_open_bus` readme) — ALL eight
        // bits, for all eight registers, including the read-only $2002.
        // Its test 2 writes $55 to $2002 and then reads $2000 expecting
        // $55 back, which is why this is here rather than in the
        // individual write arms.
        self.refresh_decay(value, 0xFF);
        match index {
            0 => self.write_ctrl(value),
            1 => self.mask = value,
            3 => self.oam_addr = value,
            4 => self.write_oam_data(value),
            5 => self.write_scroll(value),
            6 => self.write_addr(value),
            7 => self.write_data(value),
            _ => {}
        }
    }

    /// `$2000` write: "t: ...GH.. ........ <- d: ......GH" (nesdev) — the
    /// nametable-select bits become `t` bits 10-11. The rest of `ctrl` is
    /// kept verbatim for the increment-size/pattern-table/NMI-enable bits.
    fn write_ctrl(&mut self, value: u8) {
        self.ctrl = value;
        self.t = (self.t & 0xF3FF) | (((value & 0x03) as u16) << 10);
    }

    /// `$2002` read: bits 7-5 are the real vblank/sprite-0/overflow flags,
    /// bits 4-0 pass through `open_bus` (undriven). Reading clears the
    /// vblank flag and resets the `w` write-toggle latch — nesdev.org/wiki/
    /// PPU_registers: "Reading this register has the side effect of
    /// clearing the PPU's internal w register" and reading vblank "return[s]
    /// the current state of this flag and then clear[s] it". Sprite-0-hit
    /// and overflow are NOT cleared by this read (only by the pre-render
    /// line's dot-1 auto-clear — see [`Ppu::tick`]'s doc).
    ///
    /// ## The VBlank-read races (tickets W1-05b, extended W1-05c)
    ///
    /// nesdev.org/wiki/PPU_frame_timing's "VBL Flag Timing" table, verbatim:
    /// "Reading `$2002` within a few PPU clocks of when VBL is set results
    /// in special-case behavior. Reading one PPU clock before reads it as
    /// clear and never sets the flag or generates NMI for that frame.
    /// Reading on the same PPU clock or one later reads it as set, clears
    /// it, and suppresses the NMI for that frame." `self.scanline`/
    /// `self.dot` here reflect PPU state as of the end of the *previous*
    /// bus cycle's ticks (`crate::system::NesBus::read` calls the register
    /// read before `tick_master` advances this cycle's 3 dots) — see
    /// `crate::ppu`'s module doc "Sub-CPU-cycle VBlank/NMI race timing"
    /// section for why this whole-cycle-granular access turns out to still
    /// reach genuine per-dot resolution (the ROMs' own multi-frame
    /// convergence loops visit every reachable residue, this method just
    /// has to answer correctly at each one) and for the NMI-suppression
    /// half, which lives bus-side in `NesBus::nmi_level_latch` instead of
    /// here (this method only ever affects `$2002`'s *read value* and the
    /// flag's own future set/clear, never `nmi_line()` directly).
    ///
    /// - `(VBLANK_START_SCANLINE, 0)` — one PPU clock before the set:
    ///   returns the not-yet-set flag (already correct, unconditionally,
    ///   since the set hasn't run yet) and latches
    ///   `suppress_vblank_this_frame` so [`Ppu::process_dot`]'s upcoming
    ///   (241, 1) tick — due to run later in this SAME bus cycle's 3-dot
    ///   batch — skips the set entirely, matching "never sets the flag...
    ///   for that frame" rather than merely reading stale-clear once.
    /// - `(VBLANK_START_SCANLINE, 1)` — the same PPU clock as the set: the
    ///   read wins the race, so the *returned* value is forced to show set
    ///   even though `self.status` doesn't have the bit yet (the real set,
    ///   due to run later in this same 3-dot batch, is suppressed the same
    ///   way — there is nothing left to "clear" in `self.status` for a bit
    ///   it never really held this frame).
    /// - `(PRERENDER_SCANLINE, 1)` — the pre-render line's own auto-clear
    ///   dot (`Ppu::process_dot`'s `PRERENDER_SCANLINE` arm, which runs
    ///   *after* this read in the same batch, same fencepost as the set
    ///   dot above): masks the VBlank bit out of the *returned* value only
    ///   — `self.status`'s real bit is already handled by the unconditional
    ///   clear at the top of this method, identical to any other `$2002`
    ///   read. Not from the nesdev wiki text (which doesn't describe a
    ///   read-vs-clear race) — justified directly against
    ///   `03-vbl_clear_time.s`'s own expected table instead (per
    ///   MASTER_PROMPT's "the test ROM wins" rule): `02-vbl_set_time` and
    ///   `03-vbl_clear_time` share one `sync_vbl_delay`-established
    ///   reference point 20 scanlines apart (nesdev: the flag "is cleared
    ///   exactly 20 scanlines after being set"), so leaving this fencepost
    ///   asymmetric with the set-side one above shifted the two ROMs one
    ///   dot out of alignment with each other — fixing only the set side
    ///   passed `02` but broke a previously-passing `03`, measured, not
    ///   theorized.
    fn read_status(&mut self) -> u8 {
        // "$2002 ---DDDDD": bits 7-5 are driven by the PPU and refresh
        // the decay register; bits 4-0 read back FROM it and must not
        // refresh it (`ppu_open_bus` tests 6 and 7).
        let mut result = (self.status & 0xE0) | (self.decay & 0x1F);
        self.status &= !STATUS_VBLANK;
        self.w = false;
        match (self.scanline, self.dot) {
            (VBLANK_START_SCANLINE, 0) => self.suppress_vblank_this_frame = true,
            (VBLANK_START_SCANLINE, 1) => {
                result |= STATUS_VBLANK;
                self.suppress_vblank_this_frame = true;
            }
            (PRERENDER_SCANLINE, 1) => result &= !STATUS_VBLANK,
            _ => {}
        }
        // Refresh from the value actually returned, after the vblank
        // fencepost adjustments above — the decay register latches what
        // the bus carried, not what `self.status` happened to hold.
        self.refresh_decay(result, 0xE0);
        result
    }

    /// `$2005` write, first/second per the `w` toggle (nesdev, `d` = the
    /// written byte `HGFEDCBA` MSB-first):
    /// ```text
    /// first write  (w=0): t: ....... ...HGFED <- d: HGFEDCBA[7:3]; x <- d[2:0]; w <- 1
    /// second write (w=1): t: CBA..HG FED..... <- d: HGFEDCBA;      w <- 0
    /// ```
    /// The second write's mask must clear fine Y (t bits 12-14) **and**
    /// coarse Y (t bits 5-9) before OR-ing the new bits in — those are
    /// exactly the fields the `CBA..HGFED.....` diagram marks as replaced;
    /// `..` (bits 10-11, nametable select) and `.....` (bits 0-4, coarse X)
    /// must survive untouched. That clear-mask is `0x03E0 | 0x7000 =
    /// 0x73E0`; keep-mask (ticket W1-04b fix — re-verified against
    /// [nesdev.org/wiki/PPU_scrolling](https://www.nesdev.org/wiki/PPU_scrolling)'s
    /// literal second-write diagram while building this ticket's golden
    /// frame): `!0x73E0 = 0x8C1F`. The keep-mask this replaced, `0x8FFF`,
    /// left bits 5-9 (coarse Y) set instead of clearing them, so a second
    /// write OR'd its new coarse Y onto whatever coarse Y bits `t` already
    /// held (from an earlier `$2005`/`$2006` write) instead of replacing
    /// them — invisible in every existing test here because they all start
    /// from `t == 0`, where OR and replace give the same result, but a real
    /// scroll-reset sequence starting from a nonzero `t` (e.g. right after
    /// pointing `$2006` at a palette address to seed VRAM, as this ticket's
    /// golden-frame fixture does) would leave stale coarse-Y bits behind.
    fn write_scroll(&mut self, value: u8) {
        if !self.w {
            self.t = (self.t & 0xFFE0) | ((value >> 3) as u16);
            self.x = value & 0x07;
        } else {
            self.t = (self.t & 0x8C1F)
                | (((value & 0x07) as u16) << 12)
                | (((value & 0xF8) as u16) << 2);
        }
        self.w = !self.w;
        // Ticket W4-00: fires on BOTH the first (X) and second (Y) write —
        // `CoreEvent::ScrollWrite` always carries both axes, and a
        // consumer wanting only "X changed" can compare against the
        // previous event itself. Reads only `t`/`x`, already-in-hand
        // register state (this ticket's hazard note) — no bus access.
        if self.event_mask.is_subscribed(EventMask::SCROLL_WRITE) {
            let (x, y) = self.effective_scroll();
            self.queue_event(CoreEvent::ScrollWrite {
                x,
                y,
                layer: PixelLayer::Background(0), // NES has exactly one BG layer
            });
        }
    }

    /// Decode the current loopy `t`/`x` registers into a pixel-space scroll
    /// position (ticket W4-00's `CoreEvent::ScrollWrite`) — bit layout per
    /// [nesdev.org/wiki/PPU_scrolling](https://www.nesdev.org/wiki/PPU_scrolling)
    /// (this file's module doc, reproduced at the top): coarse X/Y are 0-31
    /// tile units, fine X/Y are 0-7 pixel units within a tile, and the two
    /// nametable-select bits (`t` bits 10/11) pick which of the two
    /// logical 256x240 screens the coarse value is relative to — folded in
    /// here (`* 256`/`* 240`) so the emitted value is a single
    /// wraparound-free position across a nametable-select flip, the shape
    /// `docs/design/ENHANCEMENT_RUNTIME.md` §3's wideNES-style scroll
    /// stitcher (ticket W4-03a) consumes.
    ///
    /// Emitted unconditionally from [`Ppu::write_scroll`] (`$2005`, both
    /// writes) and, as of ticket W4-03a, conditionally from
    /// [`Ppu::write_addr`] (`$2006`, second write only, gated on
    /// rendering being enabled and the write landing in the active
    /// picture — see that method's own doc for the full derivation and
    /// the measurement backing the gate). `$2006`'s primary purpose really
    /// is general VRAM addressing (CHR/nametable/palette pokes vastly
    /// outnumber scroll splits through this register — measured, not
    /// assumed: over 12,000 vblank/pre-render `$2006` writes per fixture
    /// ROM in W4-03a's pre-flight, against low hundreds of active-picture
    /// ones), so this is deliberately NOT "every `$2006` write" — that
    /// would over-fire for the common case exactly as this doc used to
    /// warn. The narrower gate covers the case that actually matters
    /// (a write that can change what's on screen right now) without the
    /// false-positive cost.
    fn effective_scroll(&self) -> (u16, u16) {
        let coarse_x = self.t & 0x001F;
        let fine_x = u16::from(self.x & 0x07);
        let nt_x = (self.t >> 10) & 0x01;
        let x = nt_x * 256 + coarse_x * 8 + fine_x;

        let coarse_y = (self.t >> 5) & 0x001F;
        let fine_y = (self.t >> 12) & 0x07;
        let nt_y = (self.t >> 11) & 0x01;
        let y = nt_y * 240 + coarse_y * 8 + fine_y;

        (x, y)
    }

    /// `$2006` write, first/second per the `w` toggle (nesdev):
    /// ```text
    /// first write  (w=0): t: .FEDCBA ........ <- d: ..FEDCBA; t[14] <- 0; w <- 1
    /// second write (w=1): t: ....... HGFEDCBA <- d: HGFEDCBA; v <- t;     w <- 0
    /// ```
    ///
    /// ## Ticket W4-03a: `ScrollWrite` also fires from here now, conditionally
    ///
    /// [`Ppu::effective_scroll`]'s doc used to say `$2006` never emits
    /// `ScrollWrite`, reasoning that VRAM-addressing pokes vastly
    /// outnumber real scroll splits through this register. W4-03a's
    /// pre-flight measured that over-fire directly (`investigate_2006_gap`,
    /// a since-removed throwaway harness driving `Alter_Ego.nes` and
    /// `Marble Madness (USA).nes` for 4000 frames each): every single
    /// active-picture (`scanline < 240`) second `$2006` write observed —
    /// 356 and 38 respectively — happened with rendering *disabled*
    /// ([`Ppu::rendering_enabled`]), i.e. ordinary blanked-screen VRAM
    /// setup, not a visible split. Neither ROM was ever coaxed into a real
    /// scrolling gameplay state, so the *positive* case (a real mid-render
    /// split) was never directly observed — recorded here rather than
    /// silently dropped.
    ///
    /// What settles the question isn't ROM archaeology, though: it's this
    /// crate's own render pipeline, already proven correct by the golden
    /// frames. `crate::ppu::background::Ppu::process_render_dot` calls
    /// [`Ppu::copy_horizontal`] at dot 257 of **every** scanline (visible
    /// or pre-render alike), but [`Ppu::copy_vertical`] only
    /// `if !is_visible && (280..=304).contains(&dot)` — the pre-render
    /// line, once per frame. So the *vertical* component of `v` is never
    /// refreshed from `t` during the visible picture by any automatic
    /// hardware path — the only way to change it mid-frame is to write
    /// `v` directly, which only `$2006`'s second write does (`$2005`
    /// only ever touches `t`). nesdev.org/wiki/PPU_scrolling's "Split X/Y
    /// scroll" section confirms this is the documented technique: "Without
    /// the second write to $2006, only the horizontal portion of v will
    /// [be re]loaded from t" mid-screen. A mid-frame *horizontal-only*
    /// split (the common status-bar case with no vertical wrap) genuinely
    /// can be built from `$2005` alone, riding the automatic dot-257
    /// `copy_horizontal` every scanline — but any split touching the
    /// vertical scroll (nametable-crossing status bars, the "classic" NES
    /// case) structurally cannot be, regardless of what any fixture ROM
    /// happens to do.
    ///
    /// So this fires `ScrollWrite` exactly when the write could be a
    /// *visible* raster change: rendering enabled
    /// ([`Ppu::rendering_enabled`]) and `self.scanline` inside the active
    /// picture (`< 240`, matching [`CoreEvent::Scanline`]'s own range).
    /// That gate is what the measurement above validates — it produces
    /// **zero** of the 394 observed false positives (both fixture ROMs'
    /// active-picture writes were 100% rendering-disabled) while still
    /// covering the mid-render case by construction, not by having
    /// witnessed one. Vblank/pre-render `$2006` writes (the overwhelming
    /// majority — over 12,000/22,000 per ROM in the same measurement) are
    /// untouched by this gate and still emit nothing, exactly as before.
    ///
    /// **Named remaining limitation, not fixed by this gate:** a ROM that
    /// sets its *base* (non-split) scroll for the upcoming frame
    /// exclusively through a `$2006` pair during vblank/pre-render
    /// (instead of `$2005`, which some games do — e.g. because `$2006`
    /// can also set the nametable-select bits `$2005`'s second write
    /// cannot) produces no `ScrollWrite` at all: this gate deliberately
    /// skips it (not active picture), and `copy_vertical`/`copy_horizontal`
    /// re-derive `v` from `t` at the pre-render line before the next frame
    /// starts, so the applied value is real but silent to this event.
    /// `rf_enhance::scroll_tracker::ScrollTracker`'s carried-over scroll
    /// value simply stays at whatever it last observed in that case —
    /// narrower than the mid-frame split gap this ticket exists to close
    /// (that gap can only ever misjudge the frame's *starting* position by
    /// however much a $2006-only game's baseline actually moved between
    /// $2005-visible updates, never lose a mid-frame split), and
    /// documented here rather than silently inherited.
    fn write_addr(&mut self, value: u8) {
        if !self.w {
            self.t = (self.t & 0x00FF) | (((value & 0x3F) as u16) << 8);
        } else {
            self.t = (self.t & 0xFF00) | value as u16;
            self.v = self.t;
            // Ticket W2-03: on real hardware the PPU address bus
            // continuously reflects `v` whenever the PPU isn't actively
            // fetching, so committing a new `v` here is itself a bus-address
            // change MMC3's A12 watcher must see -- `mmc3_test_2/source/
            // 3-A12_clocking.s` tests 2-4 exercise exactly this path (no
            // `$2007` access at all, only `$2006` writes). See
            // `ppu/mem.rs`'s module doc "A12 rising-edge detection" section.
            self.observe_ppu_bus_address(self.v & 0x3FFF);
            // Ticket W4-03a: see this method's own doc above for the full
            // derivation of this exact gate (rendering-enabled + active
            // picture). Reads `v` (already just committed above, matching
            // `write_scroll`'s "already-in-hand register state" hazard
            // note) rather than `t`, since a mid-render split cares about
            // what is ACTUALLY being scanned out, i.e. `v`, not the
            // latched-for-next-copy `t` `effective_scroll()` decodes for
            // the `$2005` path -- `v` and `t` are identical at this exact
            // instant anyway (the line just above sets `self.v = self.t`),
            // so this is `effective_scroll()`-equivalent here, not a
            // second decoding rule to maintain.
            if self.event_mask.is_subscribed(EventMask::SCROLL_WRITE)
                && self.rendering_enabled()
                && self.scanline < 240
            {
                let (x, y) = self.effective_scroll();
                self.queue_event(CoreEvent::ScrollWrite {
                    x,
                    y,
                    layer: PixelLayer::Background(0), // NES has exactly one BG layer
                });
            }
        }
        self.w = !self.w;
    }

    /// `$2007` read: delayed by one read (returns the *previous* buffered
    /// byte) except for palette addresses (`$3F00-$3FFF`), which bypass the
    /// buffer and return the 6-bit palette value immediately — nesdev.org/
    /// wiki/PPU_registers: "reading from PPUDATA... returns the contents of
    /// an internal read buffer... effectively delaying PPUDATA reads by
    /// one", and for palette addresses "the referenced 6-bit palette data
    /// is returned immediately... the PPU also performs a normal read from
    /// PPU memory at the specified address, 'underneath' the palette data,
    /// and the result of this read goes into the read buffer". nesdev
    /// doesn't give an exact address formula for that "underneath" read;
    /// this masks the address to the `$2xxx` nametable range (`& 0x2FFF`),
    /// the address-decode aliasing every reference PPU implementation
    /// derives it from (real hardware's VRAM address pins can't distinguish
    /// `$3Fxx` from `$2Fxx` at that stage). Every read advances `v` by
    /// [`Ppu::increment_vram_addr`] regardless of which path was taken.
    fn read_data(&mut self) -> u8 {
        let addr = self.v & 0x3FFF;
        let result = if addr >= 0x3F00 {
            // "$2007 DD------ palette": the palette drives bits 5-0 and
            // refreshes them; bits 7-6 come from the decay register and
            // are NOT refreshed (`ppu_open_bus` tests 8 and 9).
            let value = self.palette_read(addr) & 0x3F;
            self.read_buffer = self.mem_read(addr & 0x2FFF);
            self.refresh_decay(value, 0x3F);
            value | (self.decay & 0xC0)
        } else {
            // "$2007 --------" non-palette: all eight bits driven.
            let value = self.read_buffer;
            self.read_buffer = self.mem_read(addr);
            self.refresh_decay(value, 0xFF);
            value
        };
        self.increment_vram_addr();
        result
    }

    /// `$2007` write: writes through to the PPU memory bus at `v`, then
    /// advances `v` the same way a read does.
    fn write_data(&mut self, value: u8) {
        let addr = self.v & 0x3FFF;
        self.mem_write(addr, value);
        self.increment_vram_addr();
    }

    /// `$2000` bit 2 selects the post-access `v` increment: 0 => +1 (across
    /// a row), 1 => +32 (down a column) — nesdev.org/wiki/PPU_registers.
    fn increment_vram_addr(&mut self) {
        let step = if self.ctrl & 0x04 != 0 { 32 } else { 1 };
        self.v = self.v.wrapping_add(step) & 0x7FFF;
        // Ticket W2-03: the incremented `v` immediately becomes the new bus
        // address (same reasoning as `write_addr`'s hook above) --
        // `3-A12_clocking.s` tests 5/6 rely on THIS increment, not the
        // access that precedes it, to produce their rising edge (both set
        // up `v = $0FFF`, A12 already low, before the access).
        self.observe_ppu_bus_address(self.v & 0x3FFF);
    }

    /// nesdev.org/wiki/PPU_scrolling, verbatim:
    /// ```text
    /// if ((v & 0x001F) == 31)  // if coarse X == 31
    ///   v &= ~0x001F           // coarse X = 0
    ///   v ^= 0x0400            // switch horizontal nametable
    /// else
    ///   v += 1                 // increment coarse X
    /// ```
    pub(super) fn increment_coarse_x(&mut self) {
        if self.v & 0x001F == 31 {
            self.v &= !0x001F;
            self.v ^= 0x0400;
        } else {
            self.v += 1;
        }
    }

    /// nesdev.org/wiki/PPU_scrolling, verbatim:
    /// ```text
    /// if ((v & 0x7000) != 0x7000)        // if fine Y < 7
    ///   v += 0x1000                      // increment fine Y
    /// else
    ///   v &= ~0x7000                     // fine Y = 0
    ///   let y = (v & 0x03E0) >> 5        // let y = coarse Y
    ///   if (y == 29)
    ///     y = 0                          // coarse Y = 0, switch vertical nametable
    ///     v ^= 0x0800
    ///   elif (y == 31)
    ///     y = 0                          // coarse Y = 0, nametable not switched
    ///   else
    ///     y += 1                         // increment coarse Y
    ///   v = (v & ~0x03E0) | (y << 5)      // put coarse Y back into v
    /// ```
    pub(super) fn increment_y(&mut self) {
        if self.v & 0x7000 != 0x7000 {
            self.v += 0x1000;
        } else {
            self.v &= !0x7000;
            let mut y = (self.v & 0x03E0) >> 5;
            if y == 29 {
                y = 0;
                self.v ^= 0x0800;
            } else if y == 31 {
                y = 0;
            } else {
                y += 1;
            }
            self.v = (self.v & !0x03E0) | (y << 5);
        }
    }

    /// nesdev.org/wiki/PPU_rendering, dot 257: "v: ....A.. ...BCDEF <- t:
    /// ....A.. ...BCDEF" — copies the horizontal nametable bit (10) and
    /// coarse X (bits 0-4) from `t` into `v`.
    pub(super) fn copy_horizontal(&mut self) {
        self.v = (self.v & !0x041F) | (self.t & 0x041F);
    }

    /// nesdev.org/wiki/PPU_rendering, pre-render dots 280-304: "v: GHIA.BC
    /// DEF..... <- t: GHIA.BC DEF....." — copies fine Y (bits 12-14), the
    /// vertical nametable bit (11), and coarse Y (bits 5-9) from `t` into
    /// `v`.
    pub(super) fn copy_vertical(&mut self) {
        self.v = (self.v & !0x7BE0) | (self.t & 0x7BE0);
    }
}
