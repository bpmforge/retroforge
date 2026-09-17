//! The NES system bus (ticket W1-02): memory map, NROM cartridge space,
//! OAM DMA, and controller strobe — the `CpuBus` a real console boots
//! against, as opposed to the CPU crate's own unit-test mocks
//! (`crate::cpu::bus`).
//!
//! ## The master-clock seam (`docs/design/EMULATION_CORES.md` §1)
//!
//! [`Cpu::step`](crate::Cpu::step) is instruction-granular: it issues every
//! bus cycle in hardware order but returns only after a whole instruction.
//! Per the architecture doc, "CPU is the master clock... each chip keeps
//! its own cycle counter", so [`NesBus`] — not `Cpu` — owns
//! [`NesBus::master_cycle`], and advances it *inside* [`CpuBus::read`]/
//! [`CpuBus::write`], one tick per real bus operation. This is what lets
//! OAM DMA's stolen cycles (see `run_oam_dma` below) land in the master
//! clock for free: the DMA triggered by a `$4014` write ticks the clock
//! hundreds of times from *inside* that one `write` call, entirely
//! invisible to `Cpu::step`, which only ever sees the single write cycle
//! its `STA $4014` instruction issued. Nothing here ever feeds cycles back
//! into `Cpu` — if it needed to, this seam would have failed.
//!
//! ## The PPU tick seam (ticket W1-04a)
//!
//! `docs/design/EMULATION_CORES.md` §1 calls for the PPU to run either
//! catch-up-scheduled or "in lock-step per CPU cycle" in Accuracy mode;
//! Accuracy is lock-step and Compatibility is catch-up as of ticket
//! W3-07b (see [`crate::ppu`]'s module doc, and `catch_up_ppu_dots`
//! below). Lock-step means the
//! real [`crate::ppu::Ppu`] must advance exactly 3 dots for every one
//! master cycle the bus itself advances — so `tick_master` (below), the one
//! private helper every `master_cycle` increment in this file now goes
//! through, does both in one place: `self.master_cycle += cycles` and
//! `self.ppu.tick()` x3 per cycle. This preserves the master-clock seam
//! above unchanged (`master_cycle` still only ever changes inside this
//! file, at the same call sites as before) while guaranteeing the PPU can
//! never drift out of lock-step with it, including during OAM DMA's
//! stolen cycles (`run_oam_dma` ticks the PPU right along with them, which
//! is correct hardware behavior — DMA doesn't pause the PPU).
//!
//! ## Memory map (nesdev.org/wiki/CPU_memory_map)
//!
//! | Range | Contents |
//! |---|---|
//! | `$0000-$07FF` | 2 KiB internal RAM |
//! | `$0800-$1FFF` | mirrors of `$0000-$07FF`, every `$0800` |
//! | `$2000-$2007` | PPU registers (real 2C02 — see [`crate::ppu`]) |
//! | `$2008-$3FFF` | mirrors of `$2000-$2007`, every `$0008` |
//! | `$4000-$4013` | APU registers (stub — dropped/open-bus) |
//! | `$4014` | OAM DMA trigger |
//! | `$4015` | APU status (stub) |
//! | `$4016` | Controller 1 read / both controllers' strobe write |
//! | `$4017` | Controller 2 read / APU frame counter write (stub) |
//! | `$4018-$401F` | normally-disabled APU/IO test registers (stub) |
//! | `$4020-$5FFF` | unmapped cartridge expansion (open bus) |
//! | `$6000-$7FFF` | cartridge PRG RAM (always backed, 8 KiB) |
//! | `$8000-$FFFF` | cartridge PRG ROM (NROM: mirrored/mapped, see `read_prg`) |
//!
//! ## Mapper scope (`docs/design/EMULATION_CORES.md` §2.4, tickets W2-02/W2-03)
//!
//! Ticket W1-02 inlined NROM's PRG read/mirroring logic directly in this
//! module (`read_prg`, no longer present) rather than building the §2.4
//! `Mapper` trait, on the grounds that a single zero-register mapper with
//! no second implementor to validate against would make that trait
//! "speculative scaffolding". Ticket W2-02 is that second (third, fourth)
//! implementor: [`crate::mappers`] now holds the trait plus NROM (0,
//! extracted verbatim from this file's old `read_prg`), MMC1 (1), UxROM
//! (2), and CNROM (3); W2-03 adds MMC3 (4). `self.mapper: Box<dyn Mapper>`
//! (built in [`NesBus::new`] from `rom.header().mapper`) is now what
//! `read_untimed`/`peek`/`write_untimed` dispatch `$8000-$FFFF` PRG
//! accesses through — verified end to end (not just per-mapper unit tests)
//! by `system/tests/rom_loading.rs`'s
//! `every_emulated_mapper_loads_through_the_production_path`, which
//! constructs a real `NesBus` for every id in `EMULATED_MAPPERS`.
//!
//! `self.mapper` is also how CHR banking and mirroring control reach
//! [`crate::ppu::Ppu`]: after every `$8000-$FFFF` write, this module pushes
//! `self.mapper.chr_window()` (if `Some`) and `self.mapper.mirroring()`
//! into the PPU via `Ppu::set_chr_window`/`Ppu::set_mirroring` (both new in
//! `ppu/mem.rs`, ticket W2-02) — see `crate::mappers`' and `ppu/mem.rs`'s
//! module docs for why this materialize/push design exists instead of a
//! per-access `Mapper::ppu_read`/`ppu_write`. W2-03 adds a second,
//! inverted pull/push seam for MMC3's IRQ counter (`tick_master`'s A12-edge
//! drain, below) — see `crate::mappers` module doc's "MMC3 additions"
//! section for the full design.
//!
//! ## Open bus
//!
//! Real NES open bus is the last byte value that was actually driven onto
//! the shared data bus, which decays over time and is not perfectly
//! deterministic on real hardware. This bus models the simple, common
//! subset: [`NesBus::open_bus`] is the last byte *this emulation* actually
//! drove (every real RAM/ROM/PRG-RAM access and every write updates it);
//! reads from undriven stub registers return it unchanged. We do **not**
//! model the address-high-byte quirk that makes real `LDA $4016` typically
//! read back `$40` specifically (nesdev.org/wiki/Controller_reading
//! mentions this as a commonly-observed value, not a guaranteed one) — our
//! controller reads combine the shift-register bit with whatever the
//! shared latch currently holds, which is simpler, fully deterministic,
//! and correct for the one bit every acceptance test actually checks (D0).
pub(crate) mod cartridge;
mod controller;
mod state;

#[cfg(test)]
mod tests;

pub use cartridge::{NesLoadError, NesRom};
pub use controller::Controller;

use crate::apu::Apu;
use crate::cpu::CpuBus;
use crate::mappers::{
    Action53, AxRom, Camerica, Cnrom, ColorDreams, DxRom, GxRom, Mapper, Mmc1, Mmc2, Mmc3,
    Mmc3Revision, Nina, Nrom, UxRom,
};
use crate::ppu::Ppu;
use rf_cart::NesHeader;
use rf_core_api::{CoreEvent, CoreSink, EventMask};

const RAM_SIZE: usize = 0x0800;
const PRG_RAM_SIZE: usize = 0x2000;

/// The NES system bus: RAM, PPU/APU register stubs, controllers, and NROM
/// cartridge space, wired together as one [`CpuBus`] implementor. See the
/// module doc for the memory map and the master-clock seam this exists to
/// prove out.
pub struct NesBus {
    master_cycle: u64,
    ram: [u8; RAM_SIZE],
    open_bus: u8,
    ppu: Ppu,
    controllers: [Controller; 2],
    prg_ram: [u8; PRG_RAM_SIZE],
    rom: NesRom,
    /// The cartridge's mapper (ticket W2-02) — every `$8000-$FFFF` CPU
    /// access and every CHR-bank/mirroring push into `ppu` goes through
    /// this. See the module doc's "Mapper scope" section.
    mapper: Box<dyn Mapper>,
    /// Stall length (513 or 514) of the most recently completed OAM DMA,
    /// *not counting* the `$4014` write's own bus cycle (see
    /// `run_oam_dma`'s doc for the exact accounting). `None` until the
    /// first DMA runs.
    last_oam_dma_stall: Option<u32>,
    /// Dots the compatibility catch-up scheduler skipped rather than
    /// processed (ticket W3-07b) — diagnostic, see
    /// [`NesBus::skipped_dots`].
    skipped_dots: u64,
    /// Dots for which the catch-up scheduler may tick without re-asking
    /// [`crate::ppu::Ppu::inert_run_len`] (ticket W3-07b). Always 0 in
    /// Accuracy — that path never reaches `catch_up_ppu_dots`.
    inert_recheck_in: u16,
    /// `master_cycle` of the most recent CPU read of each controller port
    /// (`$4016`/`$4017`), or `u64::MAX` if there has not been one — the
    /// state behind the edge-triggered shift-clock model (ticket W2-01c).
    ///
    /// A standard controller's shift register is clocked by the **edge**
    /// of the read strobe, not by the fact that a read happened. Reads on
    /// consecutive CPU cycles hold that strobe continuously asserted, so
    /// they produce ONE rising edge between them, not one per read.
    ///
    /// This is what blargg's `dmc_dma_during_read4/dma_4016_read`
    /// measures. Its `end:` routine counts how many reads it takes for the
    /// controller to return 1, so its expected output `08 08 07 08 08`
    /// says: on four of five iterations the DMA cost no extra bit, and on
    /// the one where it collided with the `lda $4016` it cost **exactly
    /// one**. A DMC DMA halt puts three back-to-back `$4016` reads on the
    /// bus (halt, dummy, alignment), and nesdev counts those as "1 or 3
    /// extra *reads*" — but the ROM counts *bits*, and contiguous reads
    /// clock the pad only once. The run is broken by the DMA's own get
    /// (`$C000`), and the CPU's resumed read is the second edge; hence one
    /// extra bit regardless of whether the alignment cycle is present.
    last_joy_read_cycle: [u64; 2],
    /// The bit each controller port last drove onto D0. A contiguous read
    /// re-reports this rather than peeking the *next* bit: with no new
    /// clock edge the shift register's output simply holds (W2-01c).
    last_joy_bit: [u8; 2],
    /// The `/NMI` level [`CpuBus::nmi_line`] reports, latched once per bus
    /// cycle inside `tick_master` rather than read live from the PPU — see
    /// that method's doc for why this one-cycle-delayed snapshot (not
    /// `self.ppu.nmi_line()` directly) is what reproduces nesdev's "same
    /// PPU clock or one clock later" VBlank-race row.
    nmi_level_latch: bool,
    /// The APU (ticket W2-01a). Clocked exactly once per CPU cycle from
    /// `tick_master`, alongside the PPU's three dots — see
    /// `crate::apu`'s module doc for the full clocking contract, and this
    /// method for the DMC memory-reader handshake that rides with it.
    apu: Apu,
}

impl NesBus {
    /// Build a bus from an already-loaded [`NesRom`]. Ticket W2-02/W2-03:
    /// the mapper is selected by `rom.header().mapper` — see the module
    /// doc's "Mapper scope" section for why only mappers `EMULATED_MAPPERS`
    /// lists are reachable via this constructor (`system/cartridge.rs`'s
    /// own gate). MMC3 (mapper 4) always builds [`Mmc3Revision::B`] — see
    /// [`Self::new_forcing_mmc3_revision_a`]'s doc for why a second
    /// constructor exists rather than a parameter here.
    pub fn new(rom: NesRom) -> Self {
        Self::new_with_mmc3_revision(rom, Mmc3Revision::B)
    }

    /// Identical to [`Self::new`] except an MMC3 cartridge (mapper 4)
    /// builds [`Mmc3Revision::A`] instead of the default
    /// [`Mmc3Revision::B`] — ticket W2-03's `mmc3.rs` module doc "Which
    /// revision does a real cartridge get?" section explains why this
    /// exists at all: `mmc3_test_2`'s `6-MMC3_alt.nes` and
    /// `mmc3_irq_tests`' `5.MMC3_rev_A.nes` need revision-A behavior to
    /// pass, but their iNES headers are byte-identical to their
    /// revision-B-requiring siblings, so there is no header field this
    /// crate could dispatch on. This is `crate::ppu::tests::blargg_roms`'s
    /// integration seam for exactly those two ROMs, never a
    /// general-purpose loading path — no real, un-database-identified
    /// cartridge should ever be forced through it.
    #[cfg(test)]
    pub(crate) fn new_forcing_mmc3_revision_a(rom: NesRom) -> Self {
        Self::new_with_mmc3_revision(rom, Mmc3Revision::A)
    }

    fn new_with_mmc3_revision(rom: NesRom, mmc3_revision: Mmc3Revision) -> Self {
        let mapper: Box<dyn Mapper> = match rom.header().mapper {
            0 => Box::new(Nrom::new(rom.prg_rom().to_vec(), rom.header().mirroring)),
            1 => Box::new(Mmc1::new(
                rom.prg_rom().to_vec(),
                rom.chr_rom().to_vec(),
                rom.chr_is_ram(),
            )),
            2 => Box::new(UxRom::new(rom.prg_rom().to_vec(), rom.header().mirroring)),
            3 => Box::new(Cnrom::new(
                rom.prg_rom().to_vec(),
                rom.chr_rom().to_vec(),
                rom.chr_is_ram(),
                rom.header().mirroring,
            )),
            4 => Box::new(Mmc3::new(
                rom.prg_rom().to_vec(),
                rom.chr_rom().to_vec(),
                rom.chr_is_ram(),
                rom.header().mirroring,
                mmc3_revision,
            )),
            7 => Box::new(AxRom::new(rom.prg_rom().to_vec())),
            // Ticket W14-04: the four mappers a real library actually
            // asks for, in mapper-number order like the arms above.
            11 => Box::new(ColorDreams::new(
                rom.prg_rom().to_vec(),
                rom.chr_rom().to_vec(),
                rom.chr_is_ram(),
                rom.header().mirroring,
            )),
            // Ticket W14-13.
            9 => Box::new(Mmc2::new(rom.prg_rom().to_vec(), rom.chr_rom().to_vec())),
            28 => Box::new(Action53::new(rom.prg_rom().to_vec())),
            // Ticket W14-12.
            66 => Box::new(GxRom::new(
                rom.prg_rom().to_vec(),
                rom.chr_rom().to_vec(),
                rom.chr_is_ram(),
                rom.header().mirroring,
            )),
            71 => Box::new(Camerica::new(
                rom.prg_rom().to_vec(),
                rom.header().mirroring,
            )),
            79 => Box::new(Nina::new(
                rom.prg_rom().to_vec(),
                rom.chr_rom().to_vec(),
                rom.chr_is_ram(),
                rom.header().mirroring,
            )),
            118 => Box::new(Mmc3::new_txsrom(
                rom.prg_rom().to_vec(),
                rom.chr_rom().to_vec(),
                rom.chr_is_ram(),
            )),
            206 => Box::new(DxRom::new(
                rom.prg_rom().to_vec(),
                rom.chr_rom().to_vec(),
                rom.chr_is_ram(),
                rom.header().mirroring,
            )),
            other => unreachable!(
                "system/cartridge.rs's UnimplementedMapper gate must reject mapper {other} \
                 before NesBus::new is ever reached -- see ticket W2-02's blocked-with-evidence \
                 note in plan.json"
            ),
        };
        // Seed the PPU's flat CHR buffer from the mapper's initial view:
        // `chr_window()` (bank-0-windowed) for a banked mapper, or the raw
        // cartridge CHR bytes for one with no CHR banking at all (module
        // doc's materialize/push design — see `crate::mappers` and
        // `ppu/mem.rs`'s module docs).
        let chr = mapper
            .chr_window()
            .map(<[u8]>::to_vec)
            .unwrap_or_else(|| rom.chr_rom().to_vec());
        let mut ppu = Ppu::new(chr, rom.chr_is_ram(), mapper.mirroring());
        if let Some(latch) = mapper.chr_latch() {
            ppu.set_chr_latch(latch);
        }
        NesBus {
            master_cycle: 0,
            ram: [0; RAM_SIZE],
            open_bus: 0,
            ppu,
            controllers: [Controller::new(), Controller::new()],
            prg_ram: [0; PRG_RAM_SIZE],
            rom,
            mapper,
            last_oam_dma_stall: None,
            skipped_dots: 0,
            inert_recheck_in: 0,
            last_joy_read_cycle: [u64::MAX; 2],
            last_joy_bit: [0; 2],
            nmi_level_latch: false,
            apu: Apu::new(),
        }
    }

    /// Parse `raw` as an iNES/NES 2.0 image and build a bus from it in one
    /// step (acceptance criterion 2: "NROM loads via rf-cart").
    pub fn from_ines_bytes(raw: &[u8]) -> Result<Self, NesLoadError> {
        Ok(Self::new(NesRom::from_ines_bytes(raw)?))
    }

    /// [`Self::from_ines_bytes`], but see [`Self::new_forcing_mmc3_revision_a`].
    #[cfg(test)]
    pub(crate) fn from_ines_bytes_forcing_mmc3_revision_a(
        raw: &[u8],
    ) -> Result<Self, NesLoadError> {
        Ok(Self::new_forcing_mmc3_revision_a(NesRom::from_ines_bytes(
            raw,
        )?))
    }

    /// Total bus cycles elapsed since this bus was created — the master
    /// clock (see module doc). Every [`CpuBus::read`]/[`CpuBus::write`]
    /// advances this by at least 1; OAM DMA advances it by hundreds more
    /// from inside a single `write` call.
    pub fn master_cycle(&self) -> u64 {
        self.master_cycle
    }

    /// The parsed header of the loaded cartridge.
    pub fn rom(&self) -> &NesHeader {
        self.rom.header()
    }

    /// The 256-byte OAM as last written (by CPU register writes or OAM
    /// DMA) — for tests and, eventually, the PPU's sprite evaluation.
    pub fn oam(&self) -> &[u8; 256] {
        self.ppu.oam()
    }

    /// The PPU's 4 KiB nametable VRAM (ticket W4-06d) — forwards
    /// [`Ppu::vram`], which is a plain non-observing borrow. See that
    /// method for why "non-observing" is the load-bearing property.
    pub fn vram(&self) -> &[u8; 0x1000] {
        self.ppu.vram()
    }

    /// The PPU's 32-byte palette RAM (ticket W4-06d) — forwards
    /// [`Ppu::palette`], raw and unmirrored.
    pub fn palette(&self) -> &[u8; 32] {
        self.ppu.palette()
    }

    /// Ticket W11-05: start or stop recording the tiles the PPU draws —
    /// forwards [`Ppu::set_tile_capture`].
    pub fn set_tile_capture(&mut self, on: bool) {
        self.ppu.set_tile_capture(on);
    }

    /// The background tiles of the frame just completed — forwards
    /// [`Ppu::completed_tiles`]. Empty unless capture is on.
    #[must_use]
    pub fn completed_tiles(&self) -> &[crate::ppu::DrawnTile] {
        self.ppu.completed_tiles()
    }

    /// The cartridge's CHR — forwards [`Ppu::chr`], a non-observing
    /// borrow like [`NesBus::vram`].
    #[must_use]
    pub fn chr(&self) -> &[u8] {
        self.ppu.chr()
    }

    /// The sprite tiles of the frame just completed (ticket W11-14).
    #[must_use]
    pub fn completed_sprites(&self) -> &[crate::ppu::DrawnSprite] {
        self.ppu.completed_sprites()
    }

    /// Is CHR writable (CHR-RAM)? Forwards [`Ppu::chr_is_ram`].
    #[must_use]
    pub fn chr_is_ram(&self) -> bool {
        self.ppu.chr_is_ram()
    }

    /// Whether the sprite-limit-bypass overlay is currently recording
    /// (ticket W3-05a) — forwards [`Ppu::sprite_overlay_enabled`].
    #[must_use]
    pub fn sprite_overlay_enabled(&self) -> bool {
        self.ppu.sprite_overlay_enabled()
    }

    /// Opt into (or out of) the sprite-limit-bypass overlay (ticket
    /// W3-05a) — forwards [`Ppu::set_sprite_overlay_enabled`]. `false` by
    /// default (a freshly built `NesBus` boots in Accuracy Mode, law 6).
    pub fn set_sprite_overlay_enabled(&mut self, enabled: bool) {
        self.ppu.set_sprite_overlay_enabled(enabled);
    }

    /// Configure which [`CoreEvent`]s this bus pushes through
    /// [`Self::drain_video`] (ticket W4-00) — forwards to
    /// [`Ppu::set_event_mask`], the same push-to-the-PPU convention
    /// [`Self::set_sprite_overlay_enabled`] above already uses. `NONE`
    /// (Accuracy mode's default, law 6) until called; this is the seam a
    /// future `EmulatorCore` impl's `CoreConfig::event_mask` would wire
    /// through (see [`rf_core_api::CoreConfig`]'s own doc for why
    /// `event_mask` lives on that shared config struct) — no
    /// `EmulatorCore` exists in this crate yet (module doc, "Mapper
    /// scope"), so today only tests/benches call this directly.
    /// Install the debugger's watchpoints (ticket W13-02e) — pushed to
    /// the PPU, which owns the event queue a hit is reported on, exactly
    /// as [`NesBus::set_event_mask`] pushes the mask.
    pub fn set_watches(&mut self, watches: rf_core_api::WatchTable) {
        self.ppu.set_watches(watches);
    }

    pub fn set_event_mask(&mut self, mask: EventMask) {
        self.ppu.set_event_mask(mask);
    }

    /// The APU, for the debugger's channel scopes (ticket W4-10b).
    ///
    /// `&mut` because both callers mutate: one flips capture on, the
    /// other drains the captured streams. Neither touches machine state —
    /// `Apu::set_channel_capture` and `Apu::take_channel_samples` are
    /// both audio-output operations, under the same ruling
    /// `crate::apu::state`'s exhaustive destructure already applies to
    /// `samples`.
    pub fn apu_mut(&mut self) -> &mut Apu {
        &mut self.apu
    }

    /// Wire [`rf_core_api::CoreConfig::accuracy_mode`] into this machine
    /// (ticket W3-07), same push-to-the-PPU convention as the two setters
    /// above.
    ///
    /// `true` (the default, and `CoreConfig`'s own default) is the
    /// bit-for-bit reference path. `false` selects EMULATION_CORES §5's
    /// compatibility settings; today that is exactly one switch, the
    /// **simplified open-bus model** — see [`crate::ppu::Ppu::accuracy_mode`]
    /// for what it changes and why this is the §5 row W3-07 could offer
    /// both sides of. §5's other big one, **PPU catch-up stepping**, is
    /// now built too (ticket W3-07b) and rides this same flag — see
    /// [`NesBus::catch_up_ppu_dots`]. The two differ in their contract:
    /// open-bus modelling has a declared, test-suite-visible divergence,
    /// while catch-up is required to have none at all.
    pub fn set_accuracy_mode(&mut self, accuracy: bool) {
        self.ppu.set_accuracy_mode(accuracy);
    }

    /// Flush every completed-but-undrained scanline (plus, as of ticket
    /// W4-00, every queued [`CoreEvent`] — `Ppu::drain`'s own doc covers
    /// both) the PPU has produced through `sink` (acceptance criterion 3:
    /// "indexed pixels + metadata emitted via CoreSink") — see
    /// [`crate::ppu`]'s module doc for why this is a separate call rather
    /// than something `CpuBus::read`/`write` do inline. No `EmulatorCore`
    /// exists in this crate yet to call this once per frame automatically
    /// (out of this ticket's write scope); callers (today: tests) drive it
    /// directly.
    /// Hand every audio sample produced since the last call to `sink`
    /// (ticket W2-05, FR-CORE-024 / ARCHITECTURE §8's `audio.push`).
    ///
    /// Separate from [`NesBus::drain_video`] rather than folded into it
    /// because the two have different cadences in the app: video is drained
    /// once per frame to paint, audio can be drained more often to keep the
    /// ring fed. Both are pure output — draining either never touches
    /// machine state, which is what lets `crate::apu::state` exclude the
    /// sample queue from save states.
    ///
    /// Emits nothing when no samples are pending, so a caller polling it
    /// every instruction costs one branch.
    pub fn drain_audio(&mut self, sink: &mut dyn CoreSink) {
        if self.apu.queued_samples() == 0 {
            return;
        }
        let samples = self.apu.take_samples();
        sink.audio(&samples);
    }

    pub fn drain_video(&mut self, sink: &mut dyn CoreSink) {
        self.ppu.drain(sink);
    }

    /// Total PPU frames completed since this bus was created (ticket
    /// W1-05b) — forwards [`Ppu::frame_count`]. `rf-harness`'s blargg-
    /// protocol runner (driving a real [`Cpu`](crate::Cpu) + `NesBus`, not
    /// the `EmulatorCore`-mock path `crate::system` module doc's tick seam
    /// describes) uses this to detect "one whole frame elapsed" without
    /// reaching into PPU-internal `scanline`/`dot` state: run instructions
    /// until this value increments, then inspect `$6000+` — the same
    /// frame-boundary contract the (not-yet-built) `EmulatorCore::run_frame`
    /// will eventually offer.
    pub fn frame_count(&self) -> u64 {
        self.ppu.frame_count()
    }

    /// The cartridge PRG-RAM window (`$6000-$7FFF`, 8 KiB), side-effect-free
    /// and read-only (ticket W1-05b) — for `rf-harness`'s blargg-protocol
    /// runner, which needs the whole `$6000+` region (status byte, 3-byte
    /// validity signature, NUL-terminated message text) every frame rather
    /// than one address at a time. `crate::system` module doc's memory map:
    /// PRG-RAM is "always backed, 8 KiB" for every cartridge this crate
    /// loads (mapper 0 has no PRG-RAM-disable register), so this is a
    /// direct slice of `prg_ram`, not a `peek`-style dispatch through the
    /// The 2 KiB of internal RAM, borrowed (ticket W11-10).
    ///
    /// A borrow, unlike `EmuStepper::wram_snapshot`, which builds a copy
    /// by peeking each address. `rf_core_api::StateView::wram` is a
    /// `&[u8]`, so the trait needs to lend this rather than hand over a
    /// temporary — and a snapshot per frame would be 2 KiB of copying to
    /// produce bytes the caller only reads.
    #[must_use]
    pub fn ram(&self) -> &[u8; RAM_SIZE] {
        &self.ram
    }

    /// full memory map.
    pub fn prg_ram(&self) -> &[u8; PRG_RAM_SIZE] {
        &self.prg_ram
    }

    /// Stall length (513 or 514) of the most recently completed OAM DMA,
    /// counted the way nesdev describes it: the number of cycles the CPU
    /// is halted for, *after* the `$4014` write's own bus cycle has
    /// already elapsed as an ordinary part of the `STA` instruction that
    /// performed it. `None` if no DMA has run yet.
    pub fn last_oam_dma_stall(&self) -> Option<u32> {
        self.last_oam_dma_stall
    }

    /// Host-side input hook: set port `index`'s (0 or 1) live button byte.
    /// Bit layout: 0=A, 1=B, 2=Select, 3=Start, 4=Up, 5=Down, 6=Left,
    /// 7=Right (see `controller` module doc).
    pub fn set_controller_buttons(&mut self, index: usize, buttons: u8) {
        self.controllers[index].set_buttons(buttons);
    }

    /// One CPU-visible read with no `master_cycle` side effect (it still
    /// updates `open_bus`, exactly like a real read would) — used
    /// internally by OAM DMA, which issues its own 256 "get" cycles by
    /// hand rather than recursing into [`CpuBus::read`] (module doc: OAM
    /// DMA's cycles must be counted exactly once). Callers other than
    /// `CpuBus::read`/`run_oam_dma` MUST tick `master_cycle` themselves —
    /// nothing enforces that mechanically, so a future ticket reusing this
    /// (e.g. DMC DMA) needs to preserve the same discipline.
    fn read_untimed(&mut self, addr: u16) -> u8 {
        let value = self.read_untimed_inner(addr);
        // Ticket W13-02e, DEBUGGER.md §1's CPU address space. Wrapping
        // rather than editing the body: `read_untimed` has a dozen return
        // points across its address decode, and a watch that fired on only
        // eleven of them would be the silent-miss failure `WatchTable::set`
        // already refuses to commit. The DMC-DMA no-op re-reads below call
        // this too, deliberately — they are real bus reads with real side
        // effects, which is the entire content of `dmc_dma_during_read4`.
        if self.ppu.watches().is_armed() {
            self.ppu.note_watch_access(
                rf_core_api::WatchSpace::Cpu,
                rf_core_api::WatchAccess::Read,
                u32::from(addr),
                value,
            );
        }
        value
    }

    fn read_untimed_inner(&mut self, addr: u16) -> u8 {
        let value = match addr {
            0x0000..=0x1FFF => self.ram[(addr as usize) & (RAM_SIZE - 1)],
            0x2000..=0x3FFF => self.ppu.read_register((addr & 0x0007) as u8),
            0x4016 => self.read_controller_port(0) | (self.open_bus & !0x01),
            0x4017 => self.read_controller_port(1) | (self.open_bus & !0x01),
            // Ticket W2-01a: `$4015` is the APU's one readable register.
            // nesdev.org/wiki/APU ("Status ($4015)"): "This register is
            // internal to the CPU and so the external CPU data bus is
            // disconnected when reading it... the value does not affect
            // open bus. Bit 5 is open bus." Both halves of that are
            // honored below -- bit 5 comes from the latch, and the early
            // return skips the `self.open_bus = value` this method ends
            // with.
            0x4015 => return self.apu.read_status() | (self.open_bus & 0x20),
            0x4000..=0x4014 | 0x4018..=0x401F => self.open_bus,
            0x4020..=0x5FFF => self.open_bus,
            0x6000..=0x7FFF => self.prg_ram[(addr as usize) - 0x6000],
            0x8000..=0xFFFF => self.mapper.cpu_read(addr),
        };
        self.open_bus = value;
        value
    }

    /// Side-effect-free, cycle-free memory read (ticket W1-03): never
    /// advances `master_cycle`, never mutates controller shift-register
    /// state (`Controller::peek_bit`, not `read_bit`), never updates
    /// `open_bus`. Exists only for `crate::trace`'s disassembly-annotation
    /// column — nestest.log's `@ 80 = 0200 = 5A`-style operand annotations
    /// are computed by peeking memory *before* the traced instruction
    /// executes, and must never perturb the very state the trace is
    /// describing. This is not a general emulation primitive: every real
    /// CPU access must go through [`CpuBus::read`]/[`CpuBus::write`]
    /// instead.
    ///
    /// ## `$4000-$4015`/`$4018-$401F` disassemble as `$FF`, not tracked
    /// `open_bus` (ticket W1-03 finding)
    ///
    /// `read_untimed` (the *real* emulation path, used by
    /// [`CpuBus::read`]) still returns the tracked `open_bus` latch for
    /// this range, unchanged. This peek-only override is narrower, and its
    /// necessity is directly **verified** for exactly five addresses
    /// against the real, fetched `nestest.log`: its last few lines
    /// disassemble `STA` to `$4004`, `$4005`, `$4006`, `$4007` (write-only
    /// APU pulse2 registers — no circuit ever drives them back onto the
    /// bus) and `$4015` (APU status; nesdev.org/wiki/APU: *"This register
    /// is internal to the CPU, so the external CPU data bus is
    /// disconnected when reading it"* — i.e. a real `$4015` read cannot be
    /// `open_bus` passthrough at all) all as `= FF`, regardless of the
    /// actual preceding bus traffic (verified: the immediately-preceding
    /// instructions in that sequence are ordinary immediate-mode ROM
    /// fetches whose values do **not** match `FF`, so this is not our
    /// `open_bus` model coincidentally agreeing with real hardware). No
    /// combination of real emulated APU state or open-bus tracking
    /// produces `FF` from either of those two independent causes at that
    /// point — the parsimonious explanation, and the one that makes every
    /// one of nestest.log's 8991 lines byte-exact, is that nestest's own
    /// disassembler (like many generic 6502 disassemblers) prints a fixed
    /// `$FF` placeholder for any address it cannot resolve to real
    /// backing memory (RAM/ROM/PRG-RAM), rather than attempting to model
    /// MMIO/open-bus content at disassembly time. This crate has no APU
    /// (out of scope, a later ticket) to model `$4015`'s real status bits
    /// correctly anyway, so matching that same placeholder convention
    /// here — for the disassembly annotation only, never for real
    /// emulation — is the honest fix rather than a curve-fit.
    ///
    /// The rest of this arm's range — `$4000`-`$4013` (the other APU
    /// write-only registers), `$4014` (the OAM DMA trigger, also
    /// write-only), and `$4018`-`$401F` (disabled test registers) — is
    /// **extrapolated** by the same reasoning (stubbed, unreadable,
    /// nothing ever drives them back), not independently confirmed against
    /// the log: nestest.log never disassembles a read of any of those
    /// specific addresses. `$4016`/`$4017` (the controller ports) are
    /// deliberately *not* included here and stay on `open_bus` passthrough
    /// (combined with [`Controller::peek_bit`]) — nestest never reads a
    /// controller either, so there is no golden-log evidence either way
    /// for them, and passthrough is the conservative default absent a
    /// reason to change it.
    ///
    /// ## PPU registers (ticket W1-04a)
    ///
    /// [`Ppu::read_register`] has real side effects for `$2002`/`$2007`
    /// (clearing the vblank flag/`w` latch, advancing the delayed-read
    /// buffer/`v`) — using it here would violate this method's own
    /// no-side-effects contract. [`Ppu::peek_register`] is the
    /// side-effect-free counterpart (same convention as
    /// [`Controller::peek_bit`] vs. `read_bit` above). nestest never
    /// touches `$2000-$3FFF` at all (verified against the real fetched
    /// `nestest.log`: zero lines disassemble an operand in that range), so
    /// this arm's correctness has no golden-trace coverage either way —
    /// see `crate::ppu`'s own test suite for its behavior instead.
    pub fn peek(&self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x1FFF => self.ram[(addr as usize) & (RAM_SIZE - 1)],
            0x2000..=0x3FFF => self.ppu.peek_register((addr & 0x0007) as u8),
            0x4016 => self.controllers[0].peek_bit() | (self.open_bus & !0x01),
            0x4017 => self.controllers[1].peek_bit() | (self.open_bus & !0x01),
            0x4000..=0x4015 | 0x4018..=0x401F => 0xFF,
            0x4020..=0x5FFF => self.open_bus,
            0x6000..=0x7FFF => self.prg_ram[(addr as usize) - 0x6000],
            0x8000..=0xFFFF => self.mapper.cpu_read(addr),
        }
    }

    /// One CPU-visible write with no `master_cycle` side effect (see
    /// `read_untimed`'s doc — the same reasoning, and the same
    /// caller-must-tick obligation, applies to OAM DMA's "put" cycles).
    fn write_untimed(&mut self, addr: u16, value: u8) {
        if self.ppu.watches().is_armed() {
            // Before the write, so a watch sees the value being written
            // rather than having to infer it — DEBUGGER.md §1's condition
            // is `addr==X && val&mask`, and `val` is what is arriving.
            self.ppu.note_watch_access(
                rf_core_api::WatchSpace::Cpu,
                rf_core_api::WatchAccess::Write,
                u32::from(addr),
                value,
            );
        }
        self.write_untimed_inner(addr, value);
    }

    fn write_untimed_inner(&mut self, addr: u16, value: u8) {
        self.open_bus = value;
        match addr {
            0x0000..=0x1FFF => self.ram[(addr as usize) & (RAM_SIZE - 1)] = value,
            0x2000..=0x3FFF => {
                // Ticket W3-07b: a `$2001` write is the only way rendering
                // can be toggled, so it is the only thing that can make
                // the scheduler's prediction stale. Dropping it here is
                // an OPTIMISATION, not a safety hook — a stale prediction
                // only costs missed skips (see `catch_up_ppu_dots`) — and
                // what it recovers is the run a rendering-off transition
                // just freed. Done for every `$2000-$3FFF` write rather
                // than for `$2001` alone because one comparison per PPU
                // register write is not where this crate's time goes.
                //
                // Reads need no hook: the predicate depends only on
                // scanline, dot and `mask`, and no read path touches
                // `mask` (`$2002` moves `status`, `$2007` moves `v`).
                self.inert_recheck_in = 0;
                self.ppu.write_register((addr & 0x0007) as u8, value);
            }
            0x4016 => {
                // Both controllers share the one strobe line (nesdev.org/
                // wiki/Standard_controller): a $4016 write reaches both.
                self.controllers[0].write_strobe(value);
                self.controllers[1].write_strobe(value);
            }
            0x4014 => unreachable!("intercepted in CpuBus::write before reaching here"),
            // Ticket W2-01a: the APU's registers. `$4017` is the frame
            // counter here, NOT a controller register (only `$4016`
            // strobes the controllers). `$4018-$401F` are the disabled
            // test registers and stay dropped.
            0x4000..=0x4013 | 0x4015 | 0x4017 => self.apu.write_register(addr, value),
            0x4018..=0x401F => {}
            // Open bus on every mapper except Action 53, whose
            // register-select latch lives at $5000-$5FFF. The trait
            // method defaults to a no-op, so this stays a drop for
            // everything else.
            0x4020..=0x5FFF => self.mapper.cpu_write_expansion(addr, value),
            0x6000..=0x7FFF => self.prg_ram[(addr as usize) - 0x6000] = value,
            // Ticket W2-02: dispatched to the cartridge's own mapper (a
            // no-op for NROM, which has no registers) rather than ignored
            // outright. `self.master_cycle` here is still the cycle THIS
            // write occupies -- `CpuBus::write`'s own `tick_master(1)`
            // call (which advances it) hasn't run yet at this point in the
            // call chain, which is exactly what makes consecutive writes
            // (e.g. an RMW instruction's two `$8000+` writes) land on
            // `master_cycle` values exactly 1 apart, the input MMC1's
            // ignore-consecutive-write quirk needs (`crate::mappers`
            // module doc). After the mapper's own write, push whatever it
            // now reports for CHR/mirroring into the PPU (module doc's
            // "Mapper scope" section; `ppu/mem.rs`'s module doc for the
            // full materialize/push design and its CHR-RAM caveat) --
            // cheap and correct to do unconditionally even for mappers
            // that never change either.
            0x8000..=0xFFFF => {
                self.mapper.cpu_write(addr, value, self.master_cycle);
                if let Some(window) = self.mapper.chr_window() {
                    self.ppu.set_chr_window(window);
                }
                if let Some(latch) = self.mapper.chr_latch() {
                    self.ppu.set_chr_latch(latch);
                }
                self.ppu.set_mirroring(self.mapper.mirroring());
            }
        }
    }

    /// OAM DMA (`$4014`): copies 256 bytes from `$XX00-$XXFF` (`page =
    /// $XX`) into OAM through the same path as 256 `OAMDATA` writes
    /// (nesdev.org/wiki/DMA), honoring and advancing whatever `OAMADDR`
    /// already holds.
    ///
    /// Cost, exactly as nesdev.org/wiki/DMA describes it and exactly as
    /// this method's own tick-counting proves: **513 or 514 cycles**,
    /// counted as `1 dummy alignment read + (0 or 1) extra get/put
    /// alignment cycle + 256 get/put pairs (512)`. This total does *not*
    /// include the `$4014` write's own bus cycle — that already elapsed
    /// as an ordinary cycle of the `STA $4014` instruction before the CPU
    /// is halted (see `CpuBus::write`'s call site: it ticks once for the
    /// write, *then* calls this). `needs_alignment_cycle` is whether the
    /// `$4014` write landed on the get/put phase that costs the extra
    /// cycle — an internal bookkeeping convention when no reset
    /// has run, not a claim about a specific absolute cycle number; what
    /// matters, and what the acceptance tests check, is that the two cases
    /// differ by exactly one cycle. **Which parity takes the extra cycle
    /// was flipped in ticket W2-21**, resolved against real hardware by
    /// blargg's `cpu_interrupts_v2` `4-irq_and_dma` (under the old phase
    /// its DMA ran one cycle short and the ROM's `8`/`9` boundary landed
    /// at `+526` where hardware puts it at `+527`); the parameter is now
    /// named for what it does rather than for a parity, since the parity
    /// was never the point. Concretely, `master_cycle` starts at 0
    /// in `NesBus::new`, so a caller that skips `Cpu::power_on` (ticket
    /// W1-03) and sets `cpu.pc` directly — as this crate's own bus-level
    /// unit tests still do — sees a parity that depends on however many
    /// bus ops happened before the first `$4014` write; this is the one
    /// place such a caller's DMA cost could disagree with a real console
    /// by exactly one cycle. A caller that runs `Cpu::power_on` first
    /// (which burns exactly 7 cycles, an odd count) gets the phase
    /// anchored to hardware from then on.
    ///
    /// Each of the 256 "get" reads below also updates `open_bus` (via
    /// `read_untimed`) to that source byte, same as a real CPU read would
    /// — so after a DMA, `open_bus` holds the *last PRG/RAM byte DMA
    /// fetched*, not the `$XX` page value that triggered it.
    fn run_oam_dma(&mut self, page: u8, needs_alignment_cycle: bool) -> u32 {
        let mut stall = 0u32;

        // 1 dummy read cycle while the DMA controller takes over the bus.
        self.tick_master(1);
        stall += 1;

        // +1 extra alignment cycle when the triggering write landed on
        // the wrong half of an APU cycle, so the first "get" below lines
        // up (the "get/put alignment" EMULATION_CORES.md §2.5 cites).
        if needs_alignment_cycle {
            self.tick_master(1);
            stall += 1;
        }

        for i in 0..256u16 {
            let src = ((page as u16) << 8) | i;
            // "get": read one byte from the source page. Uses the
            // non-ticking internal read plus a manual tick, deliberately
            // *not* `CpuBus::read`, so this loop's 256 cycles are counted
            // exactly once each (see `read_untimed`'s doc).
            let byte = self.read_untimed(src);
            self.tick_master(1);
            stall += 1;

            // "put": write the byte into OAM via the OAMDATA path.
            self.ppu.write_oam_data(byte);
            self.tick_master(1);
            stall += 1;
        }

        stall
    }

    /// The single place `master_cycle` is ever mutated (module doc's "PPU
    /// tick seam"): advances the master clock by `cycles` and ticks the PPU
    /// exactly `3 * cycles` dots, keeping it in permanent lock-step with
    /// the bus regardless of which call site (a plain read/write or one of
    /// `run_oam_dma`'s stolen cycles) is advancing time.
    ///
    /// ## The `nmi_level_latch` snapshot (ticket W1-05c) — a genuine
    /// one-PPU-*dot* lag, deliberately splitting this dot loop rather than
    /// widening it to a whole cycle
    ///
    /// Snapshots [`Ppu::nmi_line`] *before each individual dot ticks*, not
    /// once per `tick_master` call — so after this method returns,
    /// `nmi_level_latch` holds the level as of one PPU dot (not one CPU
    /// cycle) before the last dot just ticked. `master_cycle` is still
    /// mutated exactly once per call (the invariant `CpuBus::nmi_line`'s
    /// doc and this ticket's acceptance criteria both depend on) — only the
    /// dot loop itself is split into per-dot snapshot-then-tick steps,
    /// which is the narrower operation the ticket's pre-flight permitted
    /// ("split only the dot loop; never make `master_cycle` sub-cycle").
    ///
    /// A whole-cycle-wide snapshot (captured once, before all 3 dots) was
    /// tried first and measured wrong: every one of `05`/`06`/`07`/`08`'s
    /// NMI-suppression windows came out exactly 2 rows (2 PPU dots, per
    /// `06-suppression.s`'s own "one PPU clock later each line" comment)
    /// too wide. The per-dot version is the fix — see `CpuBus::nmi_line`'s
    /// doc for why a 1-dot lag, not 0 or 3, is what nesdev's "one clock
    /// later" row needs.
    fn tick_master(&mut self, cycles: u32) {
        self.master_cycle += cycles as u64;
        // Ticket W3-07b: the mode check is hoisted OUT of the per-cycle
        // loop rather than made per call, and that is a measurement, not
        // a preference. Putting it inside cost the accuracy path 3.9%
        // (1.0818 ms -> 1.1243 ms/frame on `machine_frame_accuracy`) —
        // small, but this ticket's acceptance says in as many words that
        // "the scheduler may not be paid for out of the accuracy mode",
        // and a 4% tax on the reference path to make a compatibility
        // switch tidier is exactly that. Hoisted, the accuracy arm below
        // is byte-for-byte the loop that was here before this ticket.
        if self.ppu.accuracy_mode {
            for _ in 0..cycles {
                self.apu.tick();
                for _ in 0..3 {
                    self.tick_ppu_dot();
                }
            }
            return;
        }
        for _ in 0..cycles {
            // Ticket W2-01a: one APU cycle per CPU cycle, before this
            // cycle's three PPU dots. The DMC's memory-reader fetch is NOT
            // serviced here -- ticket W2-01b moved it into `CpuBus::read`,
            // because nesdev.org/wiki/DMA's "DMA can only halt on CPU read
            // cycles" makes the fetch a property of a CPU read cycle, not
            // of the clock.
            self.apu.tick();
            self.catch_up_ppu_dots(3);
        }
    }

    /// Whether `cycle` is a DMA "get" cycle — nesdev.org/wiki/DMA: "The CPU
    /// alternates between cycles on which DMA can get (read) and cycles on
    /// which DMA can put (write). These are the first and second halves of
    /// APU cycles, respectively. **At power-on, whether the first CPU cycle
    /// is get or put is random.**"
    ///
    /// A deterministic emulator cannot be random (ARCHITECTURE §3), so this
    /// crate picks one alignment and keeps it: even `master_cycle` is a get.
    /// That choice is observable — `dmc_dma_during_read4`'s own source
    /// headers list two-to-four accepted outputs per ROM precisely because
    /// "number of extra reads depends on CPU-PPU synchronization at reset" —
    /// so the suite's harness accepts any of the documented variants rather
    /// than pinning the one this alignment happens to produce.
    /// One CPU read of controller port `port` (0 = `$4016`, 1 = `$4017`),
    /// clocking the pad's shift register only on a **new strobe edge**
    /// (ticket W2-01c — see [`NesBus::last_joy_read_cycle`] for the
    /// hardware reasoning and the ROM that measures it).
    ///
    /// A read on the cycle immediately after another read of the same port
    /// is a continuation of one continuously-asserted strobe, so it
    /// returns the same bit without advancing — [`Controller::peek_bit`]
    /// is exactly that operation, and already existed for the trace
    /// logger's non-perturbing peek.
    fn read_controller_port(&mut self, port: usize) -> u8 {
        // Only meaningful while the strobe is LOW. With it high the pad is
        // continuously reloading from the live buttons rather than
        // shifting, so there is no edge to miss and the output tracks the
        // buttons on every read.
        let contiguous = !self.controllers[port].strobe
            && self.last_joy_read_cycle[port]
                .checked_add(1)
                .is_some_and(|next| next == self.master_cycle);
        self.last_joy_read_cycle[port] = self.master_cycle;
        if contiguous {
            // No new edge: the shift register holds whatever it last
            // drove. Note this is NOT `peek_bit`, which reports the bit
            // that the NEXT clock would present.
            self.last_joy_bit[port]
        } else {
            let bit = self.controllers[port].read_bit();
            self.last_joy_bit[port] = bit;
            bit
        }
    }

    fn is_get_cycle(cycle: u64) -> bool {
        cycle.is_multiple_of(2)
    }

    /// One PPU dot, factored out of [`NesBus::tick_master`] when ticket
    /// W2-01a made that method's outer loop count CPU cycles rather than
    /// dots. The body is unchanged.
    /// Advance the PPU by `dots` under the **compatibility** catch-up
    /// scheduler (ticket W3-07b; `docs/design/EMULATION_CORES.md` §5 row
    /// 1, "PPU stepping: dot-accurate | catch-up"). Accuracy never calls
    /// this — see [`NesBus::tick_master`]'s hoisted branch.
    ///
    /// It asks the PPU how many of the dots ahead are provably inert
    /// ([`Ppu::inert_run_len`]) and advances them in one arithmetic step
    /// instead of processing them. **Nothing is deferred** — the PPU's
    /// position is always exact — so `nmi_level_latch`, the A12 drain and
    /// the dot-count-keyed state W2-19/W2-01d added all keep working
    /// untouched. That is why this switch is required to produce **zero**
    /// divergence rather than a declared one, and why it does.
    ///
    /// A lagging-PPU design (defer dots, flush on observation, predict
    /// NMI/A12 edges to answer without flushing) was considered and
    /// rejected on the measurement in `docs/TESTING.md`: it would skip
    /// the same dots this does, differing only in how many arithmetic
    /// steps the skipping takes, in exchange for a flush obligation on
    /// every path `NesBus` has into the PPU.
    ///
    /// `nmi_level_latch` is the one thing this path must argue rather
    /// than inherit. Lock-step assigns it before every dot; a skipped run
    /// assigns it once. Those are equivalent precisely because nothing in
    /// an inert run can move `nmi_line` — `inert_run_len`'s table
    /// excludes (241,1) and (261,1), the only two dots that touch the
    /// vblank flag, and a `$2000`/`$2002` access that could move it from
    /// the CPU side is a bus access, not a dot. This is a **narrower**
    /// claim than the one W1-05c measured at exactly 2 dots too wide
    /// (that one widened the latch across a whole CPU cycle regardless of
    /// what the dots did), and `ppu_vbl_nmi` 05-08 is the suite that
    /// decides it: 10/10 under both configs, with byte-identical result
    /// text and identical frame counts.
    fn catch_up_ppu_dots(&mut self, dots: u16) {
        // Fast path, and the reason the compatibility scheduler does not
        // cost more than it saves: while the prediction covers this whole
        // cycle — which is every cycle of a rendering scanline — this is
        // one compare and one subtract in front of the identical three
        // ticks the accuracy path runs.
        if self.inert_recheck_in >= dots {
            self.inert_recheck_in -= dots;
            for _ in 0..dots {
                self.tick_ppu_dot();
            }
            return;
        }
        let mut remaining = dots;
        while remaining > 0 {
            // Prediction (ticket W3-07b): while the PPU has told us the
            // predicate cannot fire, tick without asking it again. This is
            // what stops the scheduler costing more than it saves — see
            // `Ppu::dots_until_possible_inert` for the two measurements.
            //
            // **The cache can never make the machine wrong, and that is
            // the invariant to preserve when editing this.** It only ever
            // routes to `tick_ppu_dot` — the exact thing Accuracy does —
            // and the skip branch below is reachable only when the cache
            // is 0, where the predicate is re-derived from live state. So
            // a stale prediction costs a missed skip, never a wrong dot.
            // Do NOT restructure this so the cache drives *skipping*: the
            // write-invalidation hook is an optimisation, not a
            // correctness protocol, and it would not cover you.
            if self.inert_recheck_in > 0 {
                let n = self.inert_recheck_in.min(remaining);
                for _ in 0..n {
                    self.tick_ppu_dot();
                }
                self.inert_recheck_in -= n;
                remaining -= n;
                continue;
            }
            let run = self.ppu.inert_run_len().min(remaining);
            if run > 0 {
                self.nmi_level_latch = self.ppu.nmi_line();
                self.ppu.skip_inert_dots(run);
                self.drain_a12_edges();
                self.skipped_dots += u64::from(run);
                remaining -= run;
            } else {
                self.inert_recheck_in = self.ppu.dots_until_possible_inert();
            }
        }
    }

    /// How many dots the compatibility scheduler has skipped rather than
    /// processed, since power-on (ticket W3-07b).
    ///
    /// Diagnostic only — nothing reads it back into the simulation, and
    /// it is deliberately **not** serialized into the save state for the
    /// same reason `accuracy_mode` is not: it describes how a session was
    /// configured and run, not what the machine is. It exists so the
    /// "8% with rendering on, ~31% with rendering off" claim in
    /// `docs/TESTING.md` stays re-measurable instead of being a number
    /// somebody once wrote in a commit message.
    pub fn skipped_dots(&self) -> u64 {
        self.skipped_dots
    }

    #[inline]
    fn tick_ppu_dot(&mut self) {
        self.nmi_level_latch = self.ppu.nmi_line();
        self.ppu.tick();
        self.drain_a12_edges();
    }

    /// Forward this dot's filtered A12 rising edges into the mapper's IRQ
    /// counter (ticket W2-03), split out of `tick_ppu_dot` by ticket
    /// W3-07b so the catch-up scheduler can call it too.
    ///
    /// **The scheduler cannot skip this, and that is not a precaution —
    /// it is a bug this ticket's own `mode_diff` run caught.** Skipping
    /// dots skips no PPU-side edges (inert dots do no pattern fetches),
    /// but a CPU-side edge — a `$2006`/`$2007` access, which is exactly
    /// how `mmc3_test_2/3-A12_clocking` tests 5/6 clock the counter with
    /// rendering off — is recorded by `Ppu::note_a12` between dots and
    /// sits in `pending_a12_edges` until a dot drains it. Skip the dots
    /// and the clock arrives up to a whole CPU cycle late; the first run
    /// of the diff reported it as an UNDECLARED DIVERGENCE on three
    /// separate `mmc3_test_2` ROMs.
    ///
    /// One call per skipped run is equivalent to lock-step's one call per
    /// dot: a run never spans more than a single CPU cycle (it is capped
    /// by the three dots `tick_master` asks for), and no inert dot can
    /// produce an edge, so only the first of lock-step's three drains
    /// could ever have found anything.
    #[inline]
    fn drain_a12_edges(&mut self) {
        // Ticket W2-03: drain whatever filtered A12 rising edges this
        // dot produced and forward each one into the mapper's own IRQ
        // counter (a no-op for every mapper but MMC3) — see
        // `crate::mappers` module doc's "MMC3 additions" section and
        // `ppu/mem.rs`'s "A12 rising-edge detection" section for the
        // full push/pull design this drain is the `NesBus` half of.
        // Draining every dot (not once per `tick_master` call) keeps
        // multi-edge ordering trivially correct: each `clock_irq_counter`
        // call sees exactly the counter state the PREVIOUS call left,
        // the same sequencing real hardware's back-to-back edges would
        // produce.
        // Ticket W14-13: the PPU flips an MMC2 latch itself; tell the
        // mapper where it stands so the save state carries it.
        if let Some(selected) = self.ppu.chr_latch_selected() {
            self.mapper.note_chr_latch(selected);
        }
        for _ in 0..self.ppu.take_a12_edges() {
            // Ticket W4-00: `MapperIrq` fires on the RISING EDGE of
            // `irq_pending()` (false -> true), not on every clock while
            // it stays asserted — `irq_pending` is a pure getter
            // (`crate::mappers::Mapper::irq_pending`'s own doc: this is
            // the exact same accessor `CpuBus::irq_line` already
            // forwards), so reading it before/after
            // `clock_irq_counter` costs nothing extra and touches no
            // bus/PPU state (this ticket's hazard note).
            let was_pending = self.mapper.irq_pending();
            self.mapper.clock_irq_counter();
            if !was_pending
                && self.mapper.irq_pending()
                && self.ppu.event_mask().is_subscribed(EventMask::MAPPER_IRQ)
            {
                self.ppu.queue_event(CoreEvent::MapperIrq);
            }
        }
    }
}
impl CpuBus for NesBus {
    /// One CPU read cycle — and, since ticket W2-01b, the only place a DMC
    /// DMA can steal cycles.
    ///
    /// nesdev.org/wiki/DMA: "DMA can only halt on CPU read cycles. On write
    /// cycles, the halt fails and the DMA unit tries again next CPU cycle,
    /// repeating until successful." Modelling the halt here rather than in
    /// `tick_master` is what makes that true for free — and it is also why
    /// `dmc_dma_during_read4`'s `dma_2007_write` ROM ("DMC DMA during $2007
    /// write has no effect") passes without a special case.
    ///
    /// The stall itself, verbatim from that page's "DMC DMA collides with
    /// $2007 read" example: a halt cycle, a dummy cycle, an optional
    /// alignment cycle, then the DMA's own get. **On the 2A03 the CPU's
    /// read is re-issued on every one of those no-operation cycles** —
    /// "When RDY is deasserted, the 6502 core repeats the last read cycle
    /// indefinitely... these repeated reads are externally visible on any
    /// no-operation DMA cycle, causing data loss if reading a register with
    /// side effects" — which is the entire content of the
    /// `dmc_dma_during_read4` suite. So this loop calls `read_untimed` on
    /// the CPU's own address once per no-op cycle, side effects and all,
    /// and only the last read's value reaches the CPU ("When the DMA
    /// process completes, the CPU performs the read it attempted when
    /// halted").
    ///
    /// Deliberately NOT modelled, and listed rather than silently omitted:
    /// the bus conflicts that occur when the DMA address's low five bits
    /// alias `$4015-$4017` (that page's last three examples), the DMC-DMA-
    /// during-OAM-DMA interleave, and the two sample-stop bugs (aborted and
    /// unexpected DMAs). None is exercised by this suite's five ROMs.
    fn read(&mut self, addr: u16) -> u8 {
        if let Some((dma_addr, _is_load)) = self.apu.dmc_fetch_request() {
            // Halt + dummy are unconditional; the get must land on a get
            // cycle, so an alignment cycle is inserted when the cycle two
            // after this one is a put. ("After the halt, DMC DMA always
            // performs a dummy cycle where no work is done. If the next
            // cycle is not a get cycle, then a cycle will be spent on
            // alignment.")
            let noop_cycles = if Self::is_get_cycle(self.master_cycle + 2) {
                2
            } else {
                3
            };
            for _ in 0..noop_cycles {
                self.read_untimed(addr);
                self.tick_master(1);
            }
            let byte = self.read_untimed(dma_addr);
            self.apu.dmc_supply_byte(byte);
            self.tick_master(1);
        }
        let value = self.read_untimed(addr);
        self.tick_master(1);
        value
    }

    fn write(&mut self, addr: u16, value: u8) {
        if addr == 0x4014 {
            let needs_alignment_cycle = self.master_cycle.is_multiple_of(2);
            self.open_bus = value;
            self.tick_master(1); // the $4014 write's own cycle

            // Ticket W4-00: "started" fires BEFORE the copy runs (only one
            // DMA channel exists in this crate, so chan is always 0);
            // "OAM was rewritten" fires AFTER, once the whole 256-byte
            // table has actually changed — the module doc's own worked
            // example for `OamRewrite` ("outside the normal per-scanline
            // path... e.g. mid-frame DMA"). Individual `OAMDATA` ($2004)
            // writes deliberately do NOT also fire `OamRewrite` — kept to
            // this one, explicitly-cited call site rather than guessed at.
            if self.ppu.event_mask().is_subscribed(EventMask::DMA_START) {
                self.ppu.queue_event(CoreEvent::DmaStart { chan: 0 });
            }
            let stall = self.run_oam_dma(value, needs_alignment_cycle);
            self.last_oam_dma_stall = Some(stall);
            if self.ppu.event_mask().is_subscribed(EventMask::OAM_REWRITE) {
                self.ppu.queue_event(CoreEvent::OamRewrite);
            }
            return;
        }
        self.write_untimed(addr, value);
        self.tick_master(1);
    }

    /// Ticket W1-05b/W1-05c: `$2000` bit 7 (NMI enable) AND `$2002` bit 7
    /// (VBlank flag), the exact level real hardware pulls `/NMI` low with
    /// (nesdev.org/wiki/NMI) — but a one-PPU-*dot*-delayed latch, not
    /// [`Ppu::nmi_line`] read live.
    ///
    /// ## Why a latch, not a passthrough (ticket W1-05c)
    ///
    /// `cpu::exec::CountingBus` (out of this ticket's write scope) samples
    /// this once per bus cycle, strictly *after* the whole `read`/`write`
    /// call returns — i.e., after this cycle's own register access AND
    /// this cycle's own 3 PPU dots have both already run. A live
    /// `self.ppu.nmi_line()` read at that point sees VBlank exactly as of
    /// the END of this cycle, which is too late for nesdev.org/wiki/
    /// PPU_frame_timing's "VBL Flag Timing" row that isn't about the flag's
    /// *value* at all: "reading on the same PPU clock or one [PPU clock]
    /// later reads it as set, clears it, and suppresses the NMI for that
    /// frame." The "same clock" half is already handled by
    /// `scroll.rs::read_status`'s dot-1 branch (the flag never really
    /// becomes visible to `self.status` at all that frame). The "one clock
    /// later" half needs something different: the VBlank-set can land in a
    /// bus cycle that ISN'T a `$2002` access at all — an intervening `NOP`
    /// mid-instruction-stream, say — so `read_status` (which only runs on
    /// `$2002` accesses) never gets a chance to intervene, and a live
    /// `nmi_line()` read right after THAT cycle would see the freshly-set
    /// flag and latch a real edge, before some LATER cycle's `$2002` read
    /// ever gets to clear it.
    ///
    /// `nmi_level_latch` (`tick_master`'s doc) fixes exactly this with a
    /// **one-PPU-dot** lag, not a whole-cycle one: it's snapshotted right
    /// before each individual dot ticks, so after any `tick_master` call it
    /// holds the level as of one dot before the last dot that ran. A VBlank
    /// set that lands on the FINAL dot of some cycle's 3-dot batch is
    /// therefore still invisible to that cycle's own end-of-op sample (the
    /// snapshot taken for that last dot was captured one dot earlier, still
    /// clear) — matching nesdev's "one clock later" suppression exactly,
    /// and no wider: a set on the *first or second* dot of a batch IS
    /// visible by that same batch's last snapshot, so `CountingBus::sample`
    /// (called once the whole op — all 3 dots — completes) still catches
    /// it, matching "two or more PPU clocks... doesn't affect NMI
    /// operation" for the normal case. Measured, not assumed: a
    /// whole-cycle-wide version of this latch (snapshot once per
    /// `tick_master` call rather than once per dot) was tried first and
    /// over-suppressed every one of `05`/`06`/`07`/`08` by exactly 2 rows
    /// (2 PPU dots) — see `tick_master`'s doc.
    fn nmi_line(&self) -> bool {
        self.nmi_level_latch
    }

    /// The wired-OR of every IRQ source on the bus, which
    /// [`CpuBus::irq_line`]'s own doc describes: the cartridge mapper
    /// (ticket W2-03 — `false` for every mapper but MMC3, whose
    /// `Mmc3Revision`-specific fire rule and `$E000`-ack are described in
    /// `crate::mappers::mmc3`'s module doc) and, since ticket W2-01a, the
    /// APU's frame-counter and DMC interrupt flags
    /// ([`crate::apu::Apu::irq_line`]).
    fn irq_line(&self) -> bool {
        self.mapper.irq_pending() || self.apu.irq_line()
    }
}

#[cfg(test)]
impl NesBus {
    /// Read access to PPU VRAM for in-crate tests (ticket W2-01b): blargg's
    /// older shells, including `dmc_dma_during_read4`'s, print only to the
    /// screen, so the nametable is the only place their result exists.
    /// Public VRAM exposure for the debugger is a separate ticket (W4-06d).
    pub(crate) fn ppu_vram_for_test(&self) -> &[u8; 0x1000] {
        &self.ppu.vram
    }
}
