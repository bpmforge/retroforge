//! The PPU's own memory bus: pattern tables (CHR), nametables (with mirror
//! mapping), and palette RAM — `$0000-$3FFF` as seen through `v`/`$2007`,
//! entirely separate from the CPU-side `$0000-$FFFF` bus
//! ([`crate::system::NesBus`]).
//!
//! ## CHR banking: the materialize/push design (ticket W2-02)
//!
//! `Ppu::chr` is still a flat `Vec<u8>` addressed directly by `chr_read`/
//! `chr_write` below, completely unchanged from ticket W1-02 (mapper 0
//! has no CHR banking) — CNROM (mapper 3) and MMC1 (mapper 1) DO bank
//! CHR, but that banking is not implemented in this file, or anywhere in
//! `crate::ppu`, at all. Instead, [`crate::mappers::Mapper::chr_window`]
//! exposes each banked mapper's *current* 8 KiB CHR view, and
//! [`crate::system::NesBus`] copies it into `self.chr` — via
//! [`Ppu::set_chr_window`] below — after every `$8000-$FFFF` write that
//! could have changed the selected bank. `chr_read`/`chr_write`'s
//! addressing math never needed to change; they just always see whatever
//! bank was pushed most recently.
//!
//! This indirection exists specifically because of ticket W2-02's
//! write_scope: `docs/design/EMULATION_CORES.md` §2.4 sketches a
//! `Mapper::ppu_read`/`ppu_write` pair that would let a mapper answer
//! every individual PPU-bus access directly, but the only call sites
//! that could invoke them (`Ppu::tick`'s background/sprite fetch
//! pipeline) live in `ppu/mod.rs`/`background.rs`/`sprites.rs`/
//! `scroll.rs` — all outside that ticket's write_scope (only this file
//! was in scope then). There was nowhere to thread a `&mut dyn Mapper`
//! parameter through, so the mapper couldn't be asked per-access; it could
//! only be *pushed* into this file's existing storage. See `crate::mappers`
//! module doc for the full reasoning.
//!
//! ## A12 rising-edge detection (ticket W2-03) — computed here, pulled by
//! `NesBus`, never pushed a mapper reference
//!
//! MMC3's scanline IRQ counter needs genuine per-access interception (every
//! time the PPU address bus changes, not just after a `$8000-$FFFF` CPU
//! write) — exactly the gap this file's "CHR banking" section above
//! predicted. Rather than threading `&mut dyn Mapper` through the render
//! pipeline (which W2-02 correctly identified as unreachable without
//! inverting this crate's layering), [`Ppu::mem_read`]/[`Ppu::mem_write`]
//! (below) and the two `$2006`/`$2007`-driven `v`-changes
//! ([`super::scroll`]'s `write_addr` second write and
//! `increment_vram_addr`) all funnel through [`Ppu::observe_ppu_bus_address`],
//! which is mapper-agnostic (A12 is a PPU-bus-electrical fact, not an MMC3
//! fact) and only *records* filtered rising edges into a drainable counter
//! ([`Ppu::take_a12_edges`], `pub(crate)`). `crate::system::NesBus` is what
//! polls that counter and forwards each edge into
//! [`crate::mappers::Mapper::clock_irq_counter`] — see that trait's and
//! `crate::mappers`' module docs for the full push/pull design. `Ppu` never
//! holds or calls into a `Mapper`.
//!
//! ### The filter itself
//!
//! nesdev.org/wiki/MMC3, verbatim: "The MMC3 scanline counter is based
//! entirely on PPU A12, triggered on a rising edge after the line has
//! remained low for three falling edges of M2" (M2 = the CPU clock). This
//! crate has no CPU-cycle counter reachable from `Ppu` (by design — see
//! `crate::system` module doc's master-clock seam), so the filter is
//! expressed in PPU dots instead: [`Ppu::dot_clock`] increments exactly
//! once per [`Ppu::tick`] call, and since `NesBus::tick_master` guarantees
//! **exactly 3 dots per CPU cycle always** (the same lock-step invariant
//! nestest's byte-exact canary depends on), "3 CPU cycles" translates
//! losslessly to **9 PPU dots**. nesdev's own text gives no exact number;
//! the quantitative `>= 3` (CPU cycles) threshold is sourced from
//! [Mesen2's `MMC3::IsA12RisingEdge`](https://github.com/SourMesen/Mesen2/blob/master/Core/NES/Mappers/Nintendo/MMC3.h)
//! (`_console->GetMasterClock()`, which that codebase's `NesConsole::GetMasterClock`
//! confirms is a CPU-cycle count, not a PPU-dot count), a reference
//! implementation independently verified against this exact oracle suite.
//!
//! Margin, not fine-tuning: tracing `mmc3_test_2/source/3-A12_clocking.s`
//! by hand (the actual asm, cached from the pinned
//! christopherpow/nes-test-roms commit) shows every "should count" rising
//! edge separated by >= 12 dots (a full `STA $2006`/`STX $2006`/`STY $2006`
//! instruction's worth of cycles between the low sample and the high one),
//! and every "should NOT count" case inside the render pipeline (one BG
//! tile's NT/AT-low-period before its own PT-high fetch) separated by only
//! 2-4 dots. The 7-11 dot boundary this threshold sits inside is never
//! exercised by any oracle this ticket runs — 9 is not finely tuned to a
//! razor's-edge test case, just Mesen's sourced value translated to this
//! crate's units.
//!
//! ## CHR RAM is not banked by this design (documented gap, not a bug)
//!
//! [`Mapper::chr_window`](crate::mappers::Mapper::chr_window) returns
//! `None` for any CHR-RAM cartridge, so [`Ppu::set_chr_window`] is simply
//! never called for one. If it *were* called for CHR RAM, a PPU-side
//! `$2007` write to `self.chr` (below) would only ever land in the
//! current push's copy — the next push (from an unrelated register
//! write elsewhere in the same mapper) would overwrite it with a stale
//! read of the mapper's own backing buffer, silently losing the write.
//! Refusing to push CHR-RAM banks at all avoids that silent-loss failure
//! mode entirely, at the cost of CHR-RAM bank switching simply not
//! working — an honest, narrow gap (no licensed CNROM/MMC1 game in
//! `docs/design/EMULATION_CORES.md`'s launch-set table uses banked CHR
//! RAM) rather than a silently wrong one.
use super::Ppu;
use rf_cart::Mirroring;

/// The A12 low-period filter threshold, in PPU dots — 3 CPU cycles x 3
/// dots/cycle. See this module's doc "A12 rising-edge detection" section
/// for the full derivation and sourcing.
const A12_FILTER_DOTS: u64 = 9;

impl Ppu {
    /// One PPU-bus read at `addr & 0x3FFF`: `$0000-$1FFF` pattern tables
    /// (CHR), `$2000-$3EFF` nametables (mirrored per `mirroring`),
    /// `$3F00-$3FFF` palette RAM. `&mut self` (ticket W2-03, widened from
    /// `&self`) because every real access now also feeds
    /// [`Ppu::observe_ppu_bus_address`] — see this module's doc.
    pub(super) fn mem_read(&mut self, addr: u16) -> u8 {
        let addr = addr & 0x3FFF;
        self.observe_ppu_bus_address(addr);
        match addr {
            0x0000..=0x1FFF => self.chr_read(addr),
            0x2000..=0x3EFF => self.vram[self.nametable_offset(addr)],
            0x3F00..=0x3FFF => self.palette_read(addr),
            _ => unreachable!("addr masked to 14 bits above"),
        }
    }

    /// One PPU-bus write at `addr & 0x3FFF` — same ranges as [`Ppu::mem_read`].
    pub(super) fn mem_write(&mut self, addr: u16, value: u8) {
        let addr = addr & 0x3FFF;
        self.observe_ppu_bus_address(addr);
        match addr {
            0x0000..=0x1FFF => self.chr_write(addr, value),
            0x2000..=0x3EFF => {
                let offset = self.nametable_offset(addr);
                self.vram[offset] = value;
            }
            0x3F00..=0x3FFF => self.palette_write(addr, value),
            _ => unreachable!("addr masked to 14 bits above"),
        }
    }

    /// Record one PPU-address-bus sample (ticket W2-03) — called for every
    /// real bus access ([`Ppu::mem_read`]/[`Ppu::mem_write`], above) AND
    /// the two places `v` changes without an accompanying access
    /// (`$2006`'s second write, `$2007`'s post-access auto-increment — both
    /// in `super::scroll`): on real hardware the address bus continuously
    /// reflects `v` whenever the PPU isn't actively fetching, so those two
    /// `v`-only changes are genuine bus-address changes too. Verified
    /// against `mmc3_test_2/source/3-A12_clocking.s` tests 5/6 ("Should be
    /// clocked when A12 changes to 1 via PPUDATA read/write"): both set up
    /// `v = $0FFF` (A12 low, no transition) and rely ENTIRELY on the
    /// post-access increment to `$1000` (A12 high) to produce the rising
    /// edge — the access itself never does.
    ///
    /// Implements nesdev's filter (module doc): `addr`'s bit 12 rising from
    /// low to high counts as a clock-worthy edge only if the low period
    /// that preceded it lasted at least [`A12_FILTER_DOTS`]. Mirrors
    /// [Mesen2's `MMC3::IsA12RisingEdge`](https://github.com/SourMesen/Mesen2/blob/master/Core/NES/Mappers/Nintendo/MMC3.h)
    /// structurally: `a12_low_since` records the dot A12 was FIRST observed
    /// low (not re-armed by further low samples while already tracking),
    /// and is always consumed (cleared) the next time A12 is observed high,
    /// whether or not that edge cleared the threshold — so a too-soon rise
    /// doesn't leave a stale timestamp behind to wrongly credit a LATER,
    /// genuinely-separated rise.
    pub(super) fn observe_ppu_bus_address(&mut self, addr: u16) {
        if addr & 0x1000 != 0 {
            if let Some(low_since) = self.a12_low_since {
                if self.dot_clock.saturating_sub(low_since) >= A12_FILTER_DOTS {
                    self.pending_a12_edges += 1;
                }
            }
            self.a12_low_since = None;
        } else if self.a12_low_since.is_none() {
            self.a12_low_since = Some(self.dot_clock);
        }
    }

    /// Drain every filtered A12 rising edge recorded since the last call
    /// (ticket W2-03) — [`crate::system::NesBus::tick_master`] polls this
    /// once per PPU dot and forwards each pulse into
    /// [`crate::mappers::Mapper::clock_irq_counter`]. `pub(crate)`, never
    /// `pub`: this is `crate::system`'s integration seam, not a public API.
    pub(crate) fn take_a12_edges(&mut self) -> u32 {
        std::mem::take(&mut self.pending_a12_edges)
    }

    fn chr_read(&self, addr: u16) -> u8 {
        if self.chr.is_empty() {
            return 0;
        }
        self.chr[addr as usize % self.chr.len()]
    }

    /// CHR pattern-table byte, side-effect-free (ticket W3-05a) — for the
    /// sprite-limit-bypass overlay's pattern fetch (`sprites.rs`'s
    /// `record_overlay_sprites`), which must NOT go through [`Ppu::mem_read`].
    /// `mem_read` unconditionally feeds [`Ppu::observe_ppu_bus_address`],
    /// the A12-rising-edge filter MMC3's scanline IRQ counter depends on
    /// (this module's doc, "A12 rising-edge detection" section) — an
    /// overlay pattern fetch through `mem_read` would perturb real A12 edge
    /// timing and, through it, MMC3 IRQ delivery, exactly the kind of
    /// simulation perturbation the ticket's mode invariant forbids. This is
    /// the same masked-`chr_read` path `mem_read` itself uses, just without
    /// the bus-address observation call.
    pub(super) fn chr_peek(&self, addr: u16) -> u8 {
        self.chr_read(addr & 0x1FFF)
    }

    /// CHR ROM writes have no effect (no mapper registers exist for mapper
    /// 0, and real ROM can't be written); CHR RAM cartridges (`chr_is_ram`)
    /// accept them.
    fn chr_write(&mut self, addr: u16, value: u8) {
        if !self.chr_is_ram || self.chr.is_empty() {
            return;
        }
        let len = self.chr.len();
        self.chr[addr as usize % len] = value;
    }

    /// Replace `self.chr`'s bytes with `window` (ticket W2-02) — see this
    /// module's doc for the full materialize/push design. Called by
    /// [`crate::system::NesBus`] after any `$8000-$FFFF` write that could
    /// have changed a banked mapper's selected CHR bank.
    ///
    /// `window.len()` must equal `self.chr.len()`: every mapper this
    /// crate constructs a [`Ppu`] from seeds `self.chr` (at construction,
    /// in [`crate::system::NesBus::new`]) from the exact same
    /// [`crate::mappers::Mapper::chr_window`] call this method's callers
    /// use afterward, so the lengths can never legitimately diverge — a
    /// mismatch here is a caller bug (e.g. a mapper whose window size
    /// isn't a fixed 8 KiB), not a recoverable runtime condition, so this
    /// asserts rather than silently truncating or panicking on an
    /// out-of-bounds copy later.
    pub(crate) fn set_chr_window(&mut self, window: &[u8]) {
        assert_eq!(
            window.len(),
            self.chr.len(),
            "CHR window size must match the buffer NesBus::new seeded"
        );
        self.chr.copy_from_slice(window);
    }

    /// Update the nametable mirroring a mapper's own register controls
    /// (MMC1; ticket W2-02) — called by [`crate::system::NesBus`] after
    /// every `$8000-$FFFF` write, the same "always push, cheap no-op for
    /// static-mirroring mappers" convention [`Ppu::set_chr_window`] uses.
    /// `mirroring` is private to `crate::ppu` (unlike `chr`, which is
    /// `pub(super)`), so this accessor is `crate::system`'s only way to
    /// reach it.
    pub(crate) fn set_mirroring(&mut self, mirroring: Mirroring) {
        self.mirroring = mirroring;
    }

    /// Physical offset into `vram` (4 KiB, one 1 KiB bank per logical
    /// nametable) for a PPU-bus address in `$2000-$3EFF`. `$3000-$3EFF`
    /// mirrors `$2000-$2EFF` exactly (both fold into the same 4 KiB modulo
    /// below), matching nesdev.org/wiki/PPU_memory_map.
    ///
    /// Horizontal mirroring: nametables 0/1 share physical bank 0, 2/3
    /// share bank 1 (`logical_bank / 2`). Vertical: 0/2 share bank 0, 1/3
    /// share bank 1 (`logical_bank % 2`). Four-screen: each of the 4
    /// logical nametables gets its own physical bank (`vram` is sized for
    /// exactly this case). One-screen (ticket W2-02, MMC1 control values
    /// 0/1 — nesdev.org/wiki/MMC1): every logical nametable aliases the
    /// single physical bank 0 (`Lower`) or bank 1 (`Upper`).
    fn nametable_offset(&self, addr: u16) -> usize {
        let logical_offset = (addr - 0x2000) % 0x1000;
        let logical_bank = (logical_offset / 0x400) as usize;
        let within_bank = (logical_offset % 0x400) as usize;
        let physical_bank = match self.mirroring {
            Mirroring::Horizontal => logical_bank / 2,
            Mirroring::Vertical => logical_bank % 2,
            Mirroring::FourScreen => logical_bank,
            Mirroring::OneScreenLower => 0,
            Mirroring::OneScreenUpper => 1,
        };
        physical_bank * 0x400 + within_bank
    }

    /// Palette RAM index (0-31) for a PPU-bus address in `$3F00-$3FFF`:
    /// mirrored every 32 bytes, with `$3F10`/`$3F14`/`$3F18`/`$3F1C`
    /// aliasing `$3F00`/`$3F04`/`$3F08`/`$3F0C` (nesdev.org/wiki/PPU_palettes:
    /// "entry 0 of each palette is also unique in that its color value is
    /// shared between the background and sprite palettes... the backdrop
    /// color can be written through both $3F00 and $3F10" — the same
    /// shared-storage mechanism applies symmetrically to the other three
    /// sprite-palette entry-0s).
    fn palette_index(addr: u16) -> usize {
        let mut index = (addr & 0x1F) as usize;
        if index >= 0x10 && index.is_multiple_of(4) {
            index -= 0x10;
        }
        index
    }

    pub(super) fn palette_read(&self, addr: u16) -> u8 {
        self.palette[Self::palette_index(addr)]
    }

    fn palette_write(&mut self, addr: u16, value: u8) {
        let index = Self::palette_index(addr);
        self.palette[index] = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ppu::Ppu;

    fn ppu_with_mirroring(mirroring: Mirroring) -> Ppu {
        Ppu::new(vec![0u8; 0x2000], false, mirroring)
    }

    #[test]
    fn horizontal_mirroring_aliases_nt0_with_nt1_and_nt2_with_nt3() {
        let mut ppu = ppu_with_mirroring(Mirroring::Horizontal);
        ppu.mem_write(0x2000, 0xAB); // NT0
        assert_eq!(ppu.mem_read(0x2400), 0xAB, "NT1 aliases NT0");
        ppu.mem_write(0x2800, 0xCD); // NT2
        assert_eq!(ppu.mem_read(0x2C00), 0xCD, "NT3 aliases NT2");
        assert_ne!(
            ppu.mem_read(0x2000),
            ppu.mem_read(0x2800),
            "NT0/NT1 bank must be distinct from NT2/NT3 bank"
        );
    }

    #[test]
    fn vertical_mirroring_aliases_nt0_with_nt2_and_nt1_with_nt3() {
        let mut ppu = ppu_with_mirroring(Mirroring::Vertical);
        ppu.mem_write(0x2000, 0x11); // NT0
        assert_eq!(ppu.mem_read(0x2800), 0x11, "NT2 aliases NT0");
        ppu.mem_write(0x2400, 0x22); // NT1
        assert_eq!(ppu.mem_read(0x2C00), 0x22, "NT3 aliases NT1");
    }

    #[test]
    fn nametable_3000_mirrors_2000() {
        let mut ppu = ppu_with_mirroring(Mirroring::Horizontal);
        ppu.mem_write(0x2001, 0x55);
        assert_eq!(ppu.mem_read(0x3001), 0x55);
    }

    #[test]
    fn four_screen_mirroring_keeps_all_four_nametables_distinct() {
        let mut ppu = ppu_with_mirroring(Mirroring::FourScreen);
        ppu.mem_write(0x2000, 1);
        ppu.mem_write(0x2400, 2);
        ppu.mem_write(0x2800, 3);
        ppu.mem_write(0x2C00, 4);
        assert_eq!(ppu.mem_read(0x2000), 1);
        assert_eq!(ppu.mem_read(0x2400), 2);
        assert_eq!(ppu.mem_read(0x2800), 3);
        assert_eq!(ppu.mem_read(0x2C00), 4);
    }

    #[test]
    fn palette_mirrors_every_32_bytes() {
        let mut ppu = ppu_with_mirroring(Mirroring::Horizontal);
        ppu.mem_write(0x3F05, 0x2A);
        assert_eq!(ppu.mem_read(0x3F25), 0x2A);
        assert_eq!(ppu.mem_read(0x3F45), 0x2A);
    }

    #[test]
    fn sprite_palette_entry_zeros_alias_background_entry_zeros() {
        let mut ppu = ppu_with_mirroring(Mirroring::Horizontal);
        ppu.mem_write(0x3F00, 0x01);
        assert_eq!(ppu.mem_read(0x3F10), 0x01);
        ppu.mem_write(0x3F14, 0x02);
        assert_eq!(ppu.mem_read(0x3F04), 0x02);
        ppu.mem_write(0x3F18, 0x03);
        assert_eq!(ppu.mem_read(0x3F08), 0x03);
        ppu.mem_write(0x3F1C, 0x04);
        assert_eq!(ppu.mem_read(0x3F0C), 0x04);
    }

    #[test]
    fn non_entry_zero_sprite_palette_slots_are_independent() {
        let mut ppu = ppu_with_mirroring(Mirroring::Horizontal);
        ppu.mem_write(0x3F01, 0x11);
        ppu.mem_write(0x3F11, 0x22);
        assert_eq!(ppu.mem_read(0x3F01), 0x11, "not aliased, unlike entry 0");
        assert_eq!(ppu.mem_read(0x3F11), 0x22);
    }

    #[test]
    fn chr_rom_writes_are_ignored() {
        let mut ppu = ppu_with_mirroring(Mirroring::Horizontal);
        let before = ppu.mem_read(0x0000);
        ppu.mem_write(0x0000, !before);
        assert_eq!(ppu.mem_read(0x0000), before, "CHR ROM is not writable");
    }

    #[test]
    fn chr_ram_writes_take_effect() {
        let mut ppu = Ppu::new(vec![0u8; 0x2000], true, Mirroring::Horizontal);
        ppu.mem_write(0x0010, 0x99);
        assert_eq!(ppu.mem_read(0x0010), 0x99);
    }

    #[test]
    fn set_chr_window_replaces_the_visible_bank() {
        let mut ppu = ppu_with_mirroring(Mirroring::Horizontal);
        assert_eq!(
            ppu.mem_read(0x0000),
            0,
            "bank 0 (all zeroes) at construction"
        );
        let bank1 = [0x42u8; 0x2000];
        ppu.set_chr_window(&bank1);
        assert_eq!(
            ppu.mem_read(0x0000),
            0x42,
            "NesBus's push must have replaced the entire visible CHR window -- a no-op \
             push would still read the original bank's bytes here"
        );
        assert_eq!(
            ppu.mem_read(0x1FFF),
            0x42,
            "and the whole window, not just the start"
        );
    }

    #[test]
    #[should_panic(expected = "CHR window size must match")]
    fn set_chr_window_rejects_a_mismatched_length() {
        let mut ppu = ppu_with_mirroring(Mirroring::Horizontal);
        ppu.set_chr_window(&[0u8; 4]);
    }

    #[test]
    fn set_mirroring_changes_nametable_routing_live() {
        let mut ppu = ppu_with_mirroring(Mirroring::Horizontal);
        ppu.mem_write(0x2000, 0xAB); // NT0
        assert_eq!(
            ppu.mem_read(0x2800),
            0x00,
            "under Horizontal, NT2 does not alias NT0"
        );
        ppu.set_mirroring(Mirroring::Vertical);
        assert_eq!(
            ppu.mem_read(0x2800),
            0xAB,
            "after switching to Vertical, NT2 must alias NT0 -- a no-op set_mirroring \
             would still read 0 here"
        );
    }

    #[test]
    fn one_screen_modes_alias_all_four_nametables_to_a_single_bank() {
        let mut ppu = ppu_with_mirroring(Mirroring::OneScreenLower);
        ppu.mem_write(0x2000, 0x11);
        assert_eq!(ppu.mem_read(0x2400), 0x11);
        assert_eq!(ppu.mem_read(0x2800), 0x11);
        assert_eq!(ppu.mem_read(0x2C00), 0x11);
    }

    #[test]
    fn one_screen_lower_and_upper_are_genuinely_different_physical_banks() {
        // Not just "all four logical nametables alias each other" (the
        // previous test) -- Lower and Upper must alias *different*
        // physical pages, or a mapper's MMC1-driven mode switch between
        // them (nesdev's control values 0/1) would be a no-op in practice.
        let mut ppu = ppu_with_mirroring(Mirroring::OneScreenLower);
        ppu.mem_write(0x2000, 0x11);
        ppu.set_mirroring(Mirroring::OneScreenUpper);
        assert_eq!(
            ppu.mem_read(0x2000),
            0x00,
            "switching to OneScreenUpper must expose a bank that never saw the earlier \
             write -- a mapper that treated both one-screen modes identically would \
             still read 0x11 here"
        );
    }
}
