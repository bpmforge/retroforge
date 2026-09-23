//! Capcom CX4 (Hitachi HG51B169) SNES-side register window, data RAM, DMA
//! transfer ports, and the internal CX4ROM math tables (ticket W19-02;
//! fullsnes "SNES Cart Capcom CX4 (programmable RISC CPU) (Mega Man X 2-3)"
//! and its three sub-chapters, "...I/O Ports", "...Opcodes", "...Functions").
//!
//! ## What this models, and what it deliberately does not
//!
//! The CX4 is a general-purpose RISC CPU running a program that lives in
//! the *cartridge's own SNES ROM* (fullsnes: `"Program ROM Base"`, e.g.
//! `028000h` in Mega Man — a plain LoROM address, not an undisclosed
//! internal mask ROM). That is architecturally the same situation as
//! [`crate::sa1`]/[`crate::gsu`]: the running program is the game's own
//! copyrighted data, not something this project would embed, so an
//! instruction-level (LLE) core executing it would be ordinary clean-room
//! hardware emulation — no different in kind from the main 65816 core
//! executing ROM bytes.
//!
//! **This ticket does not build that LLE core**, and the acceptance's own
//! escape clause is why: fullsnes's "...Opcodes" chapter documents the
//! encoding of every opcode, but explicitly leaves the pieces an
//! interpreter needs to run real code unresolved —
//! - flag effects are `???`/unstated for most opcode *forms* (only the
//!   `<op>`-vs-`<imm>` operand-order variants that happen to set `NZC`
//!   are given a concrete letter combination; the rest are blank),
//! - the byte-read sequence every ROM/vertex fetch uses is stated as
//!   literally unknown: *"The exact meaning of the above opcodes is
//!   unknown (which one does what part?)"* (`612Eh`/`4000h`/`1C00h`,
//!   fullsnes "CX4 CPU Misc"),
//! - two of the eight `skip<cond>` conditions are themselves `?`,
//! - a dozen opcodes are reserved (`-`) with no stated effect,
//! - `$7F48`/`$7F4C`/`$7F50-51`/`$7F52` are marked "Unknown", and `$7F48`'s
//!   own doc entry says its documented guess "doesn't match up with how
//!   it's used by the existing games",
//! - all timings are "100% unknown".
//!
//! An interpreter guessing any one of those diverges from the real program
//! on its first affected branch or ROM read — which is not "clean-room HLE
//! of a documented command", it is guessing relocated to the opcode level.
//! Per the ticket's acceptance ("if the chapter documents only the
//! interface and not each command's algorithm... stop there"), the CX4's
//! **26 named game-facing functions** (`build_oam`, `draw_wireframe_*`,
//! `propulsion`, `transform_coordinates`, `scale_rotate1/2`, `pythagorean`,
//! `arc_tan`, `wave`, `disintergrate`, etc. — fullsnes "...Functions") are
//! each given ONLY a name and an entry address, no register-level
//! input/output semantics or algorithm — so they are not implemented here.
//! [`docs/design/EMULATION_CORES.md`] §3.8 carries the full documented/
//! undocumented split.
//!
//! ## What IS documented well enough to implement
//!
//! - The full `$6000-$7FFF` SNES-side window (banks `$00-$3F`/`$80-$BF`):
//!   3 KiB CX4RAM, the DMA transfer ports, the program-ROM base/page/
//!   pointer registers, the busy flag, the NMI/IRQ vector shadows, and the
//!   sixteen 24-bit general registers R0-R15 — fullsnes "...I/O Ports".
//! - The DMA transfer itself (SNES ROM → CX4RAM, source/length/dest,
//!   triggered by writing `$00` to `$7F47`) — fully specified.
//! - The CX4ROM (3 KiB, 1024 24-bit values): six documented closed-form
//!   tables (Div/Sqrt/Sin/Asin/Tan/Cos) fullsnes gives complete formulas
//!   for, including the Div(0)/Cos(0) overflow-truncation rule.
//!
//! ## The busy flag: documented interface, undocumented timing
//!
//! fullsnes says bit 6 of `$7F5E` is set by a write to `$7F47`/`$7F48`/
//! `$7F4F` and "will stay set until the command has completed" — but with
//! no program execution model (see above), there is no real completion
//! event to wait for. Modelling the flag as staying set forever would hang
//! any title's poll loop; this project instead treats the transition as
//! synchronous — set, then immediately cleared, within the same write —
//! which is a stated stub for undocumented timing, not a guess at real
//! hardware behaviour, and matches the same "immediate" reading fullsnes's
//! own GSU chapter documents *as* real GSU LOZ/stop-flag behaviour is
//! never used here to claim CX4 timing accuracy.

use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};

/// CX4RAM size: 3 KiB (fullsnes "CX4 I/O Map": `"6000h..6BFFh R/W CX4RAM
/// (3Kbytes)"`).
pub const CX4_RAM_LEN: usize = 0x0C00;

/// CX4ROM entry count: 1024 24-bit values (fullsnes "CX4ROM (3Kbytes)
/// (1024 values of 24bit each)").
pub const CX4ROM_LEN: usize = 1024;

/// Build the CX4's internal data ROM from fullsnes's documented closed-form
/// tables (fullsnes "CX4ROM (3Kbytes)"). Six ranges, each transcribed
/// straight from the chapter's own `Entry = Table Contents = Formula`
/// line:
///
/// - `000h-0FFh` **Div**: `N = 0x800000 / n` for `n` in `0..=0xFF`.
///   `Div(0)` is undefined; fullsnes: "Overflows on Div(0) and Cos(0) are
///   truncated to FFFFFFh" — so `n = 0` reads `0xFFFFFF`.
/// - `100h-1FFh` **Sqrt**: `N = truncate(0x100000 * sqrt(n))` for `n` in
///   `0..=0xFF`.
/// - `200h-27Fh` **Sin**: `N = truncate(0x1000000 * sin(deg))`, `deg = n *
///   90/128` for `n` in `0..=0x7F` (fullsnes: "Sin/Asin/Tan/Cos are
///   spanning only 90' out of 360' degrees (aka 80h out of 200h
///   degrees)" — the SNES's 512-unit circle, so one table step is
///   `360/512 = 0.703125` degrees).
/// - `280h-2FFh` **Asin**: `N = truncate((0x800000 / 90) * asin_deg(n /
///   128.0))` for `n` in `0..=0x7F` (fullsnes gives the argument range as
///   "Asin(0..0.99)", i.e. `n/128` reaching `0x7F/0x80 = 0.9921875`; the
///   `800000h/90'` factor expresses the arcsine result, itself in
///   degrees, as a fraction of a right angle).
/// - `300h-37Fh` **Tan**: `N = truncate(0x10000 * tan(deg))`, same `deg`
///   mapping as Sin/Cos.
/// - `380h-3FFh` **Cos**: `N = truncate(0x1000000 * cos(deg))`, same `deg`
///   mapping; `Cos(0) = 1.0` overflows 24 bits and is truncated to
///   `0xFFFFFF` per the same overflow rule as Div(0).
///
/// Every value is masked to 24 bits (fullsnes: "all... are using full
/// 24bits"); the six ranges are deterministic pure functions of the
/// index, so this table needs no save-state entry — [`Cx4::rom`] rebuilds
/// it identically on every load.
#[must_use]
pub fn build_cx4rom() -> [u32; CX4ROM_LEN] {
    let mut rom = [0u32; CX4ROM_LEN];
    for n in 0u32..=0xFF {
        // Div.
        rom[n as usize] = if n == 0 {
            0x00FF_FFFF
        } else {
            (0x0080_0000u32 / n) & 0x00FF_FFFF
        };
        // Sqrt.
        let sqrt_val = (f64::from(0x0010_0000u32) * f64::from(n).sqrt()).trunc();
        rom[0x100 + n as usize] = (sqrt_val as u32) & 0x00FF_FFFF;
    }
    for n in 0u32..=0x7F {
        let deg = f64::from(n) * 90.0 / 128.0;
        let rad = deg.to_radians();
        let sin_val = (f64::from(0x0100_0000u32) * rad.sin()).trunc() as i64;
        rom[0x200 + n as usize] = (sin_val as u32) & 0x00FF_FFFF;

        let asin_ratio = f64::from(n) / 128.0;
        let asin_deg = asin_ratio.clamp(-1.0, 1.0).asin().to_degrees();
        let asin_val = ((f64::from(0x0080_0000u32) / 90.0) * asin_deg).trunc() as i64;
        rom[0x280 + n as usize] = (asin_val as u32) & 0x00FF_FFFF;

        let tan_val = (f64::from(0x0001_0000u32) * rad.tan()).trunc() as i64;
        rom[0x300 + n as usize] = (tan_val as u32) & 0x00FF_FFFF;

        let cos_val = f64::from(0x0100_0000u32) * rad.cos();
        // Cos(0) = 1.0 * 0x1000000 overflows 24 bits; truncated to
        // 0xFFFFFF per the documented Div(0)/Cos(0) rule.
        rom[0x380 + n as usize] = if n == 0 {
            0x00FF_FFFF
        } else {
            (cos_val.trunc() as i64 as u32) & 0x00FF_FFFF
        };
    }
    rom
}

fn byte_of(v: u32, i: u16) -> u8 {
    (v >> (8 * i)) as u8
}

fn set_byte(v: &mut u32, i: u16, b: u8) {
    let shift = 8 * i;
    *v = (*v & !(0xFF << shift)) | (u32::from(b) << shift);
}

fn byte_of16(v: u16, i: u16) -> u8 {
    (v >> (8 * i)) as u8
}

fn set_byte16(v: &mut u16, i: u16, b: u8) {
    let shift = 8 * i;
    *v = (*v & !(0xFF << shift)) | (u16::from(b) << shift);
}

/// The CX4's SNES-visible state: CX4RAM, the documented I/O ports, and the
/// sixteen general registers. See the module doc for what is and is not
/// modelled.
#[derive(Debug, Clone)]
pub struct Cx4 {
    /// `$6000-$6BFF`, 3 KiB (fullsnes "CX4 I/O Map").
    pub ram: Vec<u8>,
    /// `$7F40-$7F42`, 24-bit SNES LoROM address (fullsnes: "DMA source,
    /// 24bit SNES LoROM address").
    pub dma_source: u32,
    /// `$7F43-$7F44`, byte count (fullsnes: "DMA length, 16bit, in
    /// bytes").
    pub dma_length: u16,
    /// `$7F45-$7F46`, offset into CX4RAM, `$6000` = first byte (fullsnes:
    /// "DMA destination, 16bit in CX4RAM").
    pub dma_dest: u16,
    /// `$7F48`, documented only as "Unknown 'toggle'" — stored verbatim,
    /// no behaviour attached.
    pub cache_toggle: u8,
    /// `$7F49-$7F4B`, 24-bit LoROM address of the running program
    /// (fullsnes: "Program ROM Base, 24bit LoROM addr (028000h in Mega
    /// Man)"). Stored for completeness of the register window; nothing
    /// reads through it since no program executes (module doc).
    pub rom_base: u32,
    /// `$7F4C`, documented only as "Unknown (set to 00h or 01h)".
    pub soft_reset: u8,
    /// `$7F4D-$7F4E`, Program ROM Instruction Page.
    pub rom_page: u16,
    /// `$7F4F`, Program ROM Instruction Pointer; a write here is one of
    /// the three documented busy-flag triggers (module doc).
    pub instruction_pointer: u8,
    /// `$7F50-$7F51`, documented only as "Unknown, set to 0144h (maybe
    /// config flags or waitstates?)" — no reset default is claimed here;
    /// this is a plain read/write register with whatever software last
    /// wrote.
    pub unknown_7f50_51: u16,
    /// `$7F52`, documented only as "Unknown (set to 00h)".
    pub unknown_7f52: u8,
    /// Status `$7F5E` bit 6 — see the module doc's "busy flag" section.
    pub busy: bool,
    /// `$7F6A-$7F6B`, SNES NMI vector shadow (`[FFEA..FFEB]`).
    pub nmi_vector: u16,
    /// `$7F6E-$7F6F`, SNES IRQ vector shadow (`[FFEE..FFEF]`).
    pub irq_vector: u16,
    /// `$7F80-$7FAF`, sixteen 24-bit general registers R0-R15 (masked to
    /// 24 bits on every write).
    pub regs: [u32; 16],
}

impl Default for Cx4 {
    fn default() -> Self {
        Self::new()
    }
}

impl Cx4 {
    #[must_use]
    pub fn new() -> Self {
        Self {
            ram: vec![0u8; CX4_RAM_LEN],
            dma_source: 0,
            dma_length: 0,
            dma_dest: 0,
            cache_toggle: 0,
            rom_base: 0,
            soft_reset: 0,
            rom_page: 0,
            instruction_pointer: 0,
            unknown_7f50_51: 0,
            unknown_7f52: 0,
            busy: false,
            nmi_vector: 0,
            irq_vector: 0,
            regs: [0; 16],
        }
    }

    /// Read a register-window byte at its raw bus offset (`$7F40..=$7FAF`
    /// plus the vector-shadow/status addresses — see [`crate::mapping::cx4_target`]
    /// for exactly which offsets reach here). Side-effect-free, so
    /// [`crate::bus::SnesBus::peek`] can reuse it directly.
    #[must_use]
    pub fn read(&self, offset: u16) -> u8 {
        match offset {
            0x7F49..=0x7F4B => byte_of(self.rom_base, offset - 0x7F49),
            0x7F4C => self.soft_reset,
            0x7F4D..=0x7F4E => byte_of16(self.rom_page, offset - 0x7F4D),
            0x7F4F => self.instruction_pointer,
            0x7F50..=0x7F51 => byte_of16(self.unknown_7f50_51, offset - 0x7F50),
            0x7F52 => self.unknown_7f52,
            // Bit 6 = busy; every other bit is fullsnes-undocumented, read
            // as 0 rather than guessed.
            0x7F5E => u8::from(self.busy) << 6,
            0x7F6A..=0x7F6B => byte_of16(self.nmi_vector, offset - 0x7F6A),
            0x7F6E..=0x7F6F => byte_of16(self.irq_vector, offset - 0x7F6E),
            0x7F80..=0x7FAF => {
                let rel = offset - 0x7F80;
                let n = usize::from(rel / 3);
                byte_of(self.regs[n], rel % 3)
            }
            // $7F40-$7F46 (DMA ports) are documented "?/W" — write-only,
            // no read behaviour stated — and $7F48 likewise; read as 0
            // rather than echoing the last write, since fullsnes states
            // no such behaviour.
            _ => 0,
        }
    }

    /// Write a register-window byte. DMA start (`$7F47` write `$00`) is
    /// handled by the caller ([`crate::bus::SnesBus::write`]) since it
    /// needs the cartridge ROM this struct does not hold — see
    /// [`dma_transfer`].
    pub fn write(&mut self, offset: u16, value: u8) {
        match offset {
            0x7F40..=0x7F42 => set_byte(&mut self.dma_source, offset - 0x7F40, value),
            0x7F43..=0x7F44 => set_byte16(&mut self.dma_length, offset - 0x7F43, value),
            0x7F45..=0x7F46 => set_byte16(&mut self.dma_dest, offset - 0x7F45, value),
            // "set upon [7F47],[7F48],[7F4F] writes" — modelled as
            // synchronous (module doc's "busy flag" section): the flag
            // never observably holds `true` for a poller, since there is
            // no program execution to wait for.
            0x7F47 => {}
            0x7F48 => self.cache_toggle = value,
            0x7F49..=0x7F4B => set_byte(&mut self.rom_base, offset - 0x7F49, value),
            0x7F4C => self.soft_reset = value,
            0x7F4D..=0x7F4E => set_byte16(&mut self.rom_page, offset - 0x7F4D, value),
            0x7F4F => self.instruction_pointer = value,
            0x7F50..=0x7F51 => set_byte16(&mut self.unknown_7f50_51, offset - 0x7F50, value),
            0x7F52 => self.unknown_7f52 = value,
            0x7F6A..=0x7F6B => set_byte16(&mut self.nmi_vector, offset - 0x7F6A, value),
            0x7F6E..=0x7F6F => set_byte16(&mut self.irq_vector, offset - 0x7F6E, value),
            0x7F80..=0x7FAF => {
                let rel = offset - 0x7F80;
                let n = usize::from(rel / 3);
                set_byte(&mut self.regs[n], rel % 3, value);
                self.regs[n] &= 0x00FF_FFFF;
            }
            _ => {}
        }
    }

    pub(crate) fn save(&self, o: &mut StateOut) -> Result<(), StateError> {
        o.blob(&self.ram)?;
        o.u32(self.dma_source)?;
        o.u16(self.dma_length)?;
        o.u16(self.dma_dest)?;
        o.u8(self.cache_toggle)?;
        o.u32(self.rom_base)?;
        o.u8(self.soft_reset)?;
        o.u16(self.rom_page)?;
        o.u8(self.instruction_pointer)?;
        o.u16(self.unknown_7f50_51)?;
        o.u8(self.unknown_7f52)?;
        o.bool(self.busy)?;
        o.u16(self.nmi_vector)?;
        o.u16(self.irq_vector)?;
        for r in self.regs {
            o.u32(r)?;
        }
        Ok(())
    }

    pub(crate) fn load(&mut self, i: &mut StateIn) -> Result<(), StateError> {
        i.blob_into(&mut self.ram, "CX4 RAM")?;
        self.dma_source = i.u32()?;
        self.dma_length = i.u16()?;
        self.dma_dest = i.u16()?;
        self.cache_toggle = i.u8()?;
        self.rom_base = i.u32()?;
        self.soft_reset = i.u8()?;
        self.rom_page = i.u16()?;
        self.instruction_pointer = i.u8()?;
        self.unknown_7f50_51 = i.u16()?;
        self.unknown_7f52 = i.u8()?;
        self.busy = i.bool()?;
        self.nmi_vector = i.u16()?;
        self.irq_vector = i.u16()?;
        for r in &mut self.regs {
            *r = i.u32()?;
        }
        Ok(())
    }
}

/// Perform the one documented DMA transfer direction (fullsnes "CX4 I/O
/// Map": `"7F47h ?/W DMA start (write 00h to transfer direction
/// SNES-to-CX4)"`) — `dma_length` bytes from the LoROM address
/// `dma_source` into CX4RAM starting at `dma_dest`. Any other write value
/// is a documented no-op (fullsnes names no other direction/encoding, and
/// this project does not guess one).
///
/// `rom` is resolved through the ordinary LoROM mapping
/// ([`crate::mapping::map`]) a Cx4 cartridge's own header always declares
/// (fullsnes CX4 cartridge header: `"[FFD5]=20h ;Slow LoROM"`); an address
/// [`crate::mapping::map`] does not resolve to [`crate::mapping::Target::Rom`]
/// (e.g. `rom` shorter than the header claims) reads as `0`, the same
/// "never guess, never panic" rule the rest of this bus uses for an
/// unbacked read.
pub(crate) fn dma_transfer(cx4: &mut Cx4, mode: rf_cart::SnesMapMode, rom: &[u8], value: u8) {
    if value != 0x00 {
        return;
    }
    let length = usize::from(cx4.dma_length);
    for i in 0..length {
        let addr = cx4.dma_source.wrapping_add(i as u32) & 0x00FF_FFFF;
        let bank = (addr >> 16) as u8;
        let offset = addr as u16;
        let byte = match crate::mapping::map(mode, bank, offset, rom.len(), 0) {
            crate::mapping::Target::Rom(idx) => rom.get(idx).copied().unwrap_or(0),
            _ => 0,
        };
        let dest = (usize::from(cx4.dma_dest) + i) % CX4_RAM_LEN;
        cx4.ram[dest] = byte;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- CX4ROM tables, hand-computed against fullsnes's own formulas ----

    #[test]
    fn div_table_matches_formula_and_documents_zero_overflow() {
        let rom = build_cx4rom();
        // Div(0) truncates to FFFFFFh (documented overflow rule).
        assert_eq!(rom[0], 0x00FF_FFFF);
        // Div(1) = 800000h / 1 = 800000h.
        assert_eq!(rom[1], 0x0080_0000);
        // Div(FFh) = 800000h / 255, fullsnes's own last table entry.
        assert_eq!(rom[0xFF], 0x0080_0000 / 0xFF);
        assert_eq!(rom[0xFF], 0x0000_8080);
    }

    #[test]
    fn sqrt_table_matches_formula_and_documented_endpoints() {
        let rom = build_cx4rom();
        // Sqrt(0) = 0 (fullsnes: "N[0..FFh] = 000000h..FF7FDFh").
        assert_eq!(rom[0x100], 0);
        // Sqrt(255) = 100000h * sqrt(255) ~= FF7FDFh (fullsnes's own
        // documented last entry).
        assert_eq!(rom[0x100 + 0xFF], 0x00FF_7FDF);
    }

    #[test]
    fn sin_table_zero_and_documented_last_entry() {
        let rom = build_cx4rom();
        // Sin(0 degrees) = 0.
        assert_eq!(rom[0x200], 0);
        // fullsnes's own documented last entry for the Sin table.
        assert_eq!(rom[0x200 + 0x7F], 0x00FF_FB10);
    }

    #[test]
    fn cos_table_zero_overflows_and_documented_last_entry() {
        let rom = build_cx4rom();
        // Cos(0) = 1.0 * 1000000h overflows 24 bits, truncated to FFFFFFh
        // (documented overflow rule, same as Div(0)).
        assert_eq!(rom[0x380], 0x00FF_FFFF);
        // fullsnes's own documented last entry for the Cos table.
        assert_eq!(rom[0x380 + 0x7F], 0x0003_243A);
    }

    #[test]
    fn asin_table_endpoints_within_documented_range() {
        let rom = build_cx4rom();
        assert_eq!(rom[0x280], 0);
        // fullsnes documents the last entry as 75CEB4h; the chapter's own
        // domain note ("Asin(0..0.99)") is an approximation of the true
        // 0x7F/0x80 ratio, so this checks the formula lands within a
        // small tolerance of the documented value rather than bit-exact.
        let last = rom[0x280 + 0x7F];
        let documented = 0x0075_CEB4i64;
        assert!(
            (i64::from(last) - documented).abs() < 0x0001_0000,
            "got {last:#08X}, documented ~{documented:#08X}"
        );
    }

    #[test]
    fn tan_table_zero_and_within_documented_range() {
        let rom = build_cx4rom();
        assert_eq!(rom[0x300], 0);
        let last = rom[0x300 + 0x7F];
        let documented = 0x0051_7BB5i64;
        assert!(
            (i64::from(last) - documented).abs() < 0x0001_0000,
            "got {last:#08X}, documented ~{documented:#08X}"
        );
    }

    #[test]
    fn every_rom_entry_fits_in_24_bits() {
        for v in build_cx4rom() {
            assert_eq!(v & !0x00FF_FFFF, 0);
        }
    }

    // ---- register window read/write semantics ----

    #[test]
    fn dma_ports_are_write_only_per_documented_io_map() {
        let mut cx4 = Cx4::new();
        cx4.write(0x7F40, 0xAB);
        // Fullsnes marks 7F40-7F46 "?/W" — no read behaviour documented,
        // so a read reports 0 rather than echoing the write.
        assert_eq!(cx4.read(0x7F40), 0);
    }

    #[test]
    fn rom_base_page_pointer_round_trip() {
        let mut cx4 = Cx4::new();
        cx4.write(0x7F49, 0x00);
        cx4.write(0x7F4A, 0x80);
        cx4.write(0x7F4B, 0x02);
        assert_eq!(cx4.rom_base, 0x0002_8000);
        assert_eq!(cx4.read(0x7F49), 0x00);
        assert_eq!(cx4.read(0x7F4A), 0x80);
        assert_eq!(cx4.read(0x7F4B), 0x02);

        cx4.write(0x7F4D, 0x03);
        cx4.write(0x7F4E, 0x00);
        assert_eq!(cx4.rom_page, 0x0003);

        cx4.write(0x7F4F, 0x42);
        assert_eq!(cx4.instruction_pointer, 0x42);
        assert_eq!(cx4.read(0x7F4F), 0x42);
    }

    #[test]
    fn general_registers_are_24_bit_little_endian() {
        let mut cx4 = Cx4::new();
        // R3 lives at 7F80 + 3*3 = 7F89.
        cx4.write(0x7F89, 0x11);
        cx4.write(0x7F8A, 0x22);
        cx4.write(0x7F8B, 0x33);
        assert_eq!(cx4.regs[3], 0x0033_2211);
        assert_eq!(cx4.read(0x7F89), 0x11);
        assert_eq!(cx4.read(0x7F8A), 0x22);
        assert_eq!(cx4.read(0x7F8B), 0x33);
        // A fourth byte's worth of value written to the top byte is
        // masked away — the register is only ever 24 bits.
        cx4.write(0x7F8B, 0xFF);
        assert_eq!(cx4.regs[3] & !0x00FF_FFFF, 0);
    }

    #[test]
    fn nmi_and_irq_vector_shadows_round_trip() {
        let mut cx4 = Cx4::new();
        cx4.write(0x7F6A, 0x34);
        cx4.write(0x7F6B, 0x12);
        assert_eq!(cx4.nmi_vector, 0x1234);
        cx4.write(0x7F6E, 0x78);
        cx4.write(0x7F6F, 0x56);
        assert_eq!(cx4.irq_vector, 0x5678);
    }

    #[test]
    fn status_busy_bit_defaults_clear_and_only_reflects_bit6() {
        let cx4 = Cx4::new();
        assert_eq!(cx4.read(0x7F5E), 0x00);
    }

    // ---- DMA transfer ----

    #[test]
    fn dma_transfer_copies_lorom_bytes_into_cx4ram() {
        let mut cx4 = Cx4::new();
        let mut rom = vec![0u8; 0x8000 * 2];
        // LoROM bank 0, offset 8000-8003 -> rom index 0-3 (bank 0 quarter).
        rom[0] = 0xDE;
        rom[1] = 0xAD;
        rom[2] = 0xBE;
        rom[3] = 0xEF;
        cx4.dma_source = 0x00_8000;
        cx4.dma_length = 4;
        cx4.dma_dest = 0x0000;
        dma_transfer(&mut cx4, rf_cart::SnesMapMode::LoRom, &rom, 0x00);
        assert_eq!(&cx4.ram[0..4], &[0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn dma_transfer_ignores_any_value_other_than_zero() {
        let mut cx4 = Cx4::new();
        cx4.ram[0] = 0x99;
        let rom = vec![0x11u8; 0x8000];
        cx4.dma_source = 0x00_8000;
        cx4.dma_length = 1;
        dma_transfer(&mut cx4, rf_cart::SnesMapMode::LoRom, &rom, 0x01);
        assert_eq!(cx4.ram[0], 0x99, "only write 00h documents a transfer");
    }

    #[test]
    fn dma_transfer_wraps_destination_within_cx4ram() {
        let mut cx4 = Cx4::new();
        let mut rom = vec![0u8; 0x8000];
        rom[0] = 1;
        rom[1] = 2;
        cx4.dma_source = 0x00_8000;
        cx4.dma_length = 2;
        cx4.dma_dest = (CX4_RAM_LEN - 1) as u16;
        dma_transfer(&mut cx4, rf_cart::SnesMapMode::LoRom, &rom, 0x00);
        assert_eq!(cx4.ram[CX4_RAM_LEN - 1], 1);
        assert_eq!(cx4.ram[0], 2);
    }

    // ---- determinism / save-load ----

    #[test]
    fn save_load_round_trip_preserves_every_field() {
        struct MemStream {
            buf: Vec<u8>,
            at: usize,
        }
        impl rf_core_api::StateWriter for MemStream {
            fn write_all(&mut self, bytes: &[u8]) -> Result<(), StateError> {
                self.buf.extend_from_slice(bytes);
                Ok(())
            }
        }
        impl rf_core_api::StateReader for MemStream {
            fn read_exact(&mut self, out: &mut [u8]) -> Result<(), StateError> {
                let end = self.at + out.len();
                out.copy_from_slice(&self.buf[self.at..end]);
                self.at = end;
                Ok(())
            }
        }

        let mut cx4 = Cx4::new();
        cx4.write(0x7F40, 0x01);
        cx4.write(0x7F41, 0x02);
        cx4.write(0x7F42, 0x03);
        cx4.write(0x7F43, 0x10);
        cx4.write(0x7F44, 0x00);
        cx4.write(0x7F45, 0x20);
        cx4.write(0x7F46, 0x00);
        cx4.write(0x7F48, 0x01);
        cx4.write(0x7F49, 0x00);
        cx4.write(0x7F4A, 0x80);
        cx4.write(0x7F4B, 0x02);
        cx4.write(0x7F4C, 0x01);
        cx4.write(0x7F4D, 0x03);
        cx4.write(0x7F4E, 0x00);
        cx4.write(0x7F4F, 0x42);
        cx4.write(0x7F50, 0x44);
        cx4.write(0x7F51, 0x01);
        cx4.write(0x7F52, 0x01);
        cx4.write(0x7F6A, 0x34);
        cx4.write(0x7F6B, 0x12);
        cx4.write(0x7F6E, 0x78);
        cx4.write(0x7F6F, 0x56);
        cx4.write(0x7F80, 0xAA);
        cx4.write(0x7F81, 0xBB);
        cx4.write(0x7F82, 0xCC);
        cx4.ram[0] = 0x77;
        cx4.busy = true;

        let mut stream = MemStream {
            buf: Vec::new(),
            at: 0,
        };
        cx4.save(&mut StateOut::new(&mut stream)).unwrap();

        let mut restored = Cx4::new();
        restored.load(&mut StateIn::new(&mut stream)).unwrap();

        assert_eq!(restored.dma_source, cx4.dma_source);
        assert_eq!(restored.dma_length, cx4.dma_length);
        assert_eq!(restored.dma_dest, cx4.dma_dest);
        assert_eq!(restored.cache_toggle, cx4.cache_toggle);
        assert_eq!(restored.rom_base, cx4.rom_base);
        assert_eq!(restored.soft_reset, cx4.soft_reset);
        assert_eq!(restored.rom_page, cx4.rom_page);
        assert_eq!(restored.instruction_pointer, cx4.instruction_pointer);
        assert_eq!(restored.unknown_7f50_51, cx4.unknown_7f50_51);
        assert_eq!(restored.unknown_7f52, cx4.unknown_7f52);
        assert_eq!(restored.busy, cx4.busy);
        assert_eq!(restored.nmi_vector, cx4.nmi_vector);
        assert_eq!(restored.irq_vector, cx4.irq_vector);
        assert_eq!(restored.regs, cx4.regs);
        assert_eq!(restored.ram, cx4.ram);
    }
}
