//! The 65816 [`Isa`] for the code atlas (ticket W36-08).
//!
//! ## Width model
//!
//! Instruction length is not a property of the opcode: `LDA #imm` takes one
//! or two operand bytes by `M`, `LDX/LDY/CPX/CPY #imm` by `X`, and emulation
//! mode (`E = 1`) forces both to 8 bits. The five reachable states are the
//! decode *modes* ([`crate::coverage::mode_index`]): 0 = emulation, 1..=4 =
//! native with `1 + m8 + 2 * x8`. Lengths come from the same opcode table
//! the disassembler uses ([`crate::trace::opcode_info`], transcribed from the
//! W65C816S datasheet and fullsnes), so there is one table in the crate; the
//! unit tests check it against what [`crate::cpu::Cpu::step`] actually
//! advances `PC` by, for all 256 opcodes in all five modes.
//!
//! ## REP / SEP / XCE / PLP
//!
//! Within straight-line code the width after an instruction is static for
//! `REP #imm` / `SEP #imm` (native mode: bit 5 is `M`, bit 4 is `X`;
//! emulation mode ignores both, fullsnes "SNES CPU 65XX Microprocessor") and
//! unknowable from the bytes for `XCE` (depends on carry) and `PLP` (depends
//! on the stack). Those two report [`ModeAfter::Unknown`]; the atlas builder
//! then follows the code after them only in modes the recording saw it run
//! in. `RTI` returns to the width its caller had; the same rule applies.
//!
//! ## Addresses
//!
//! `Branch`/`Jump`/`Call` targets are 16-bit and stay in the program bank of
//! the site (branches wrap inside the bank); `JumpLong`/`CallLong` carry the
//! full 24-bit address. [`Wdc65816::rom_offset`] resolves either through the
//! LoROM/HiROM cartridge map, which is fixed, so no bank-switch recording is
//! needed; boards that switch ROM banks (SA-1, S-DD1) are outside this map and place nothing statically in their switched banks (W37-02).

use rf_cart::SnesMapMode;
use rf_core_api::coverage::CoverageLog;
pub use rf_rebuild::{
    build_atlas as build_generic_atlas, entry, Atlas, Provenance, CACHE_ASSET, CACHE_PRODUCER,
    CACHE_SETTINGS,
};
use rf_rebuild::{DecodeMode, Decoded, Flow, Isa, ModeAfter};

use crate::mapping;
use crate::trace::{opcode_info, Mode};

/// The atlas of a LoROM/HiROM cartridge image from a coverage recording: the
/// generic builder with the 65816 ISA. `rom` is the cartridge ROM with any
/// copier header already stripped (what the bus maps).
#[must_use]
pub fn build_atlas(rom: &[u8], map_mode: SnesMapMode, log: &CoverageLog) -> Atlas {
    build_generic_atlas(&Wdc65816::new(map_mode, rom.len()), rom, log)
}

/// [`build_atlas`] for a cartridge with `coprocessor` (see
/// [`Wdc65816::for_board`]).
#[must_use]
pub fn build_atlas_on(
    rom: &[u8],
    map_mode: SnesMapMode,
    coprocessor: rf_cart::Coprocessor,
    log: &CoverageLog,
) -> Atlas {
    build_generic_atlas(
        &Wdc65816::for_board(map_mode, rom.len(), coprocessor),
        rom,
        log,
    )
}

/// [`build_atlas`] for a raw cartridge image as a player has it: the map mode
/// and any 512-byte copier header come from the cartridge header, exactly as
/// [`crate::SnesSystem::load`] reads them, so the ROM offsets in the atlas
/// are the offsets in the recording.
///
/// # Errors
/// Whatever `rf-cart` rejects (not a SNES image, unparseable header).
pub fn build_atlas_from_image(raw: &[u8], log: &CoverageLog) -> Result<Atlas, rf_cart::CartError> {
    let rf_cart::Cartridge::Snes { header, .. } = rf_cart::Cartridge::load(raw)? else {
        return Err(rf_cart::CartError::InvalidHeader(
            "not a SNES cartridge (an iNES header was found)".to_string(),
        ));
    };
    let rom = if header.had_copier_header {
        &raw[512..]
    } else {
        raw
    };
    Ok(build_atlas_on(
        rom,
        header.map_mode,
        header.coprocessor,
        log,
    ))
}

/// [`build_atlas_from_image`] with the hardware vectors added as seeds
/// (ticket W37-01); see [`add_vector_seeds`]. An empty `log`
/// ([`CoverageLog::new`] of length 0) gives the vectors-only atlas.
///
/// # Errors
/// Whatever `rf-cart` rejects.
pub fn build_seeded_atlas_from_image(
    raw: &[u8],
    log: &CoverageLog,
) -> Result<Atlas, rf_cart::CartError> {
    let rf_cart::Cartridge::Snes { header, .. } = rf_cart::Cartridge::load(raw)? else {
        return Err(rf_cart::CartError::InvalidHeader(
            "not a SNES cartridge (an iNES header was found)".to_string(),
        ));
    };
    let rom = if header.had_copier_header {
        &raw[512..]
    } else {
        raw
    };
    let mut seeded = log.clone();
    add_vector_seeds(rom, header.map_mode, &mut seeded);
    Ok(build_atlas_on(
        rom,
        header.map_mode,
        header.coprocessor,
        &seeded,
    ))
}

/// Add the interrupt vectors of `rom` to `log` as entries (fullsnes, "SNES
/// Memory Map": the vectors are the last 32 bytes of bank `$00`).
///
/// RESET always starts in emulation mode (mode 0). The emulation-mode NMI and
/// IRQ/BRK vectors are seeded in mode 0. The native-mode NMI and IRQ vectors
/// are seeded in all four native width states: the handler's first
/// instruction is usually the `REP`/`SEP` that sets them, but nothing static
/// says which state an interrupt arrives in, and an unexecuted decode in the
/// wrong state is harmless (it is never entered) while a missing one is a
/// fallback. COP and the native BRK vector are not seeded.
pub fn add_vector_seeds(rom: &[u8], map_mode: SnesMapMode, log: &mut CoverageLog) {
    use rf_core_api::coverage::{CodeAddr, InterruptEntry, InterruptKind};
    let isa = Wdc65816::new(map_mode, rom.len());
    let word = |addr: u32| -> Option<u16> {
        let at = isa.rom_offset(addr)? as usize;
        Some(u16::from_le_bytes([*rom.get(at)?, *rom.get(at + 1)?]))
    };
    // (vector address, kind, modes it is entered in)
    let table: [(u32, InterruptKind, &[u8]); 5] = [
        (0xFFFC, InterruptKind::Reset, &[0]),
        (0xFFFA, InterruptKind::Nmi, &[0]),
        (0xFFFE, InterruptKind::Irq, &[0]),
        (0xFFEA, InterruptKind::Nmi, &[1, 2, 3, 4]),
        (0xFFEE, InterruptKind::Irq, &[1, 2, 3, 4]),
    ];
    for (vector, kind, modes) in table {
        let Some(cpu) = word(vector).filter(|&c| c != 0 && c != 0xFFFF) else {
            continue;
        };
        let Some(rom_off) = isa.rom_offset(u32::from(cpu)) else {
            continue;
        };
        for &mode in modes {
            log.note_interrupt(InterruptEntry {
                kind,
                target: CodeAddr {
                    cpu,
                    pbr: 0,
                    mode,
                    rom: Some(rom_off),
                },
            });
        }
    }
}

/// The accumulator and index widths (`m8`, `x8`) of a decode mode. Mode 0 is
/// emulation (both 8-bit); an index past the last mode reads as emulation.
#[must_use]
pub const fn widths(mode: u8) -> (bool, bool) {
    match mode {
        1..=4 => ((mode - 1) & 1 != 0, (mode - 1) & 2 != 0),
        _ => (true, true),
    }
}

/// The 5A22's 65816 instruction set over a LoROM or HiROM cartridge.
#[derive(Debug, Clone, Copy)]
pub struct Wdc65816 {
    map: SnesMapMode,
    rom_len: usize,
    /// A Super FX board: its `$40-$5F` ROM window is linear 64 KiB banks and
    /// `$70-$71` is the GSU's RAM, neither of which plain LoROM says
    /// (`mapping::gsu_target`; W37-02).
    super_fx: bool,
}

impl Wdc65816 {
    /// `rom_len` is the (header-stripped) cartridge ROM size: the map mirrors
    /// smaller images exactly as the bus does.
    #[must_use]
    pub const fn new(map: SnesMapMode, rom_len: usize) -> Self {
        Self {
            map,
            rom_len,
            super_fx: false,
        }
    }

    /// The ISA for a cartridge whose header names `coprocessor`: the boards
    /// whose fixed ROM windows differ from plain LoROM/HiROM place addresses
    /// as the bus does (Super FX), and the ones whose ROM banks switch place
    /// nothing in the switched banks (S-DD1, SA-1; see [`Self::rom_offset`]).
    #[must_use]
    pub const fn for_board(
        map: SnesMapMode,
        rom_len: usize,
        coprocessor: rf_cart::Coprocessor,
    ) -> Self {
        Self {
            map,
            rom_len,
            super_fx: matches!(coprocessor, rf_cart::Coprocessor::SuperFx { .. }),
        }
    }
}

impl Isa for Wdc65816 {
    fn max_len(&self) -> usize {
        4
    }

    /// The program bank never changes by falling through or branching.
    fn addr_mask(&self) -> u32 {
        0xFFFF
    }

    fn mode_count(&self) -> usize {
        5
    }

    fn fixed_mapping(&self) -> bool {
        true
    }

    fn rom_offset(&self, cpu: u32) -> Option<u32> {
        let bank = ((cpu >> 16) & 0xFF) as u8;
        // An S-DD1 cartridge's `$C0-$FF` is a bank-register-selected view of
        // the ROM; the plain LoROM arithmetic would place it on `$40-$7F`
        // (W37-02). Unresolved here, so static targets there stay unresolved
        // and only recorded code in those banks gets nodes, at the offsets
        // the bus really read. SA-1 maps nothing statically (`map` answers
        // open bus for it).
        if self.map == SnesMapMode::Sdd1 && bank >= 0xC0 {
            return None;
        }
        // LoROM save RAM: `$70-$7D:0000-7FFF` is SRAM on a board that has
        // any and a ROM mirror on one that has none; the ISA does not know
        // which, so it places nothing there rather than guess.
        if matches!(self.map, SnesMapMode::LoRom | SnesMapMode::Sdd1)
            && (0x70..0x7E).contains(&bank)
            && cpu & 0xFFFF < 0x8000
        {
            return None;
        }
        if self.super_fx {
            match bank {
                // `gsu_target`: `$40-$5F` is 64 KiB linear ROM banks.
                0x40..=0x5F if self.rom_len > 0 => {
                    let index = (usize::from(bank - 0x40) << 16) | (cpu & 0xFFFF) as usize;
                    return u32::try_from(index % self.rom_len).ok();
                }
                // The GSU's own RAM, whole banks.
                0x70..=0x71 => return None,
                _ => {}
            }
        }
        mapping::rom_offset(self.map, bank, (cpu & 0xFFFF) as u16, self.rom_len, 0)
            .and_then(|o| u32::try_from(o).ok())
    }

    /// The upper-bank alias: `$80+` for LoROM (never a work-RAM bank),
    /// `$C0+` for HiROM, where the ROM is linear.
    fn cpu_for_rom(&self, rom: u32) -> u32 {
        match self.map {
            SnesMapMode::HiRom => ((0xC0 | ((rom >> 16) & 0x3F)) << 16) | (rom & 0xFFFF),
            _ => ((0x80 | ((rom >> 15) & 0x7F)) << 16) | 0x8000 | (rom & 0x7FFF),
        }
    }

    fn decode(&self, bytes: &[u8], cpu: u32, mode: DecodeMode) -> Option<Decoded> {
        let op = *bytes.first()?;
        let (m8, x8) = widths(mode.0);
        let (_, addressing) = opcode_info(op);
        let len = 1 + addressing.operand_len(m8, x8);
        if bytes.len() < len {
            return None;
        }
        let word = |i: usize| u32::from(u16::from_le_bytes([bytes[i], bytes[i + 1]]));
        let pc = cpu & 0xFFFF;
        let rel8 = || (pc as i32 + 2 + i32::from(bytes[1] as i8)) as u32 & 0xFFFF;
        let flow = match op {
            // BPL BMI BVC BVS BCC BCS BNE BEQ.
            0x10 | 0x30 | 0x50 | 0x70 | 0x90 | 0xB0 | 0xD0 | 0xF0 => {
                Flow::Branch { target: rel8() }
            }
            // BRA, then BRL: a signed 16-bit displacement from the next
            // instruction, wrapping inside the bank.
            0x80 => Flow::Jump { target: rel8() },
            0x82 => Flow::Jump {
                target: pc.wrapping_add(3).wrapping_add(word(1)) & 0xFFFF,
            },
            0x4C => Flow::Jump { target: word(1) },
            0x5C => Flow::JumpLong {
                target: word(1) | (u32::from(bytes[3]) << 16),
            },
            0x20 => Flow::Call { target: word(1) },
            0x22 => Flow::CallLong {
                target: word(1) | (u32::from(bytes[3]) << 16),
            },
            // JMP (abs), JMP (abs,X), JML [abs].
            0x6C | 0x7C | 0xDC => Flow::IndirectJump,
            0xFC => Flow::IndirectCall,
            0x60 => Flow::Return,
            0x6B => Flow::ReturnLong,
            0x40 => Flow::ReturnInterrupt,
            // BRK and COP both go through a vector.
            0x00 | 0x02 => Flow::Break,
            // STP stops the clock until reset. (WAI resumes at the next
            // instruction, so it falls through.)
            0xDB => Flow::Halt,
            _ => Flow::Fall,
        };
        let after = match op {
            // In emulation mode REP/SEP cannot move M and X.
            0xC2 | 0xE2 if mode.0 != 0 => {
                let (mut m8, mut x8) = (m8, x8);
                let set = op == 0xE2;
                if bytes[1] & 0x20 != 0 {
                    m8 = set;
                }
                if bytes[1] & 0x10 != 0 {
                    x8 = set;
                }
                ModeAfter::Set(DecodeMode(1 + u8::from(m8) + 2 * u8::from(x8)))
            }
            // XCE flips E from carry; PLP pulls P. Neither is in the bytes.
            // (Emulation mode keeps E, and PLP cannot change M/X there.)
            0xFB => ModeAfter::Unknown,
            0x28 if mode.0 != 0 => ModeAfter::Unknown,
            _ => ModeAfter::Same,
        };
        Some(Decoded {
            len: len as u8,
            flow,
            after,
        })
    }

    /// The shape-hash rule: operands naming an absolute address are masked,
    /// that is the 16-bit absolute family (`abs`, `abs,X`, `abs,Y`, `(abs)`,
    /// `(abs,X)`, `[abs]`) and the 24-bit long family (`long`, `long,X`).
    /// Immediates of either width, direct page, stack-relative, relative
    /// branches, `PEA` and the `MVN`/`MVP` bank pair stay: they are the
    /// game's own constants or move with the code.
    fn masked_operand(&self, insn: &[u8], _mode: DecodeMode) -> std::ops::Range<usize> {
        let Some(&op) = insn.first() else {
            return 0..0;
        };
        let end = match opcode_info(op).1 {
            Mode::Abs
            | Mode::AbsX
            | Mode::AbsY
            | Mode::AbsInd
            | Mode::AbsIndX
            | Mode::AbsIndLong => 3,
            Mode::Long | Mode::LongX => 4,
            _ => return 0..0,
        };
        if insn.len() >= end {
            1..end
        } else {
            0..0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::{flags, Cpu, FlatBus};

    const LO: Wdc65816 = Wdc65816::new(SnesMapMode::LoRom, 0x10_0000);
    const HI: Wdc65816 = Wdc65816::new(SnesMapMode::HiRom, 0x10_0000);

    fn decode(bytes: &[u8], mode: u8) -> Decoded {
        LO.decode(bytes, 0x00_8000, DecodeMode(mode))
            .expect("decodes")
    }

    /// A CPU in decode mode `mode` (the inverse of `mode_index`).
    fn cpu_in(mode: u8) -> Cpu {
        let mut cpu = Cpu::new();
        let (m8, x8) = widths(mode);
        if mode == 0 {
            cpu.set_emulation(true);
        } else {
            cpu.set_emulation(false);
            cpu.set_flag(flags::M, m8);
            cpu.set_flag(flags::X, x8);
        }
        cpu.pc = 0x8000;
        cpu.pbr = 0;
        cpu
    }

    #[test]
    fn mode_index_and_widths_are_inverses_over_all_five_states() {
        use crate::coverage::mode_index;
        assert_eq!(mode_index(true, 0x00), 0);
        assert_eq!(mode_index(true, 0x30), 0);
        let mut seen = [false; 5];
        for p in [0x00u8, flags::M, flags::X, flags::M | flags::X] {
            let m = mode_index(false, p);
            seen[usize::from(m)] = true;
            assert_eq!(
                widths(m),
                (p & flags::M != 0, p & flags::X != 0),
                "p={p:#04x}"
            );
        }
        assert_eq!(seen, [false, true, true, true, true]);
        assert_eq!(widths(0), (true, true));
    }

    /// The decoded length is what `Cpu::step` really advances `PC` by, for
    /// every straight-line opcode in every width mode. (A check against the
    /// trace table alone would be circular: the decoder is built on it.)
    #[test]
    fn lengths_match_what_the_cpu_executes_for_all_256_opcodes_in_all_five_modes() {
        // Not straight-line: control transfers; and MVN/MVP, which repeat in
        // place until the count runs out.
        let moves = [0x44u8, 0x54];
        let mut checked = 0;
        for mode in 0..5u8 {
            for op in 0..=255u8 {
                let bytes = [op, 0x12, 0x34, 0x56];
                let d = decode(&bytes, mode);
                if d.flow != Flow::Fall || moves.contains(&op) {
                    continue;
                }
                let mut cpu = cpu_in(mode);
                let mut bus = FlatBus::new();
                bus.load(0x8000, &bytes);
                cpu.step(&mut bus).expect("implemented");
                assert_eq!(
                    cpu.pc.wrapping_sub(0x8000),
                    u16::from(d.len),
                    "opcode {op:#04x} in mode {mode}"
                );
                checked += 1;
            }
        }
        // 256 opcodes x 5 modes, less the control transfers and block moves.
        assert!(checked > 5 * 200, "only {checked} cases ran");
    }

    #[test]
    fn immediates_follow_m_and_x_and_emulation_forces_both() {
        // LDA #imm ($A9): M. LDX #imm ($A2): X.
        for (mode, lda, ldx) in [(0u8, 2u8, 2u8), (1, 3, 3), (2, 2, 3), (3, 3, 2), (4, 2, 2)] {
            assert_eq!(decode(&[0xA9, 1, 2], mode).len, lda, "LDA mode {mode}");
            assert_eq!(decode(&[0xA2, 1, 2], mode).len, ldx, "LDX mode {mode}");
        }
        // Width-independent immediates.
        for mode in 0..5 {
            assert_eq!(decode(&[0xC2, 0x30], mode).len, 2, "REP");
            assert_eq!(decode(&[0xF4, 1, 2], mode).len, 3, "PEA");
            assert_eq!(decode(&[0x00, 0], mode).len, 2, "BRK");
        }
        // Truncated input is None, not a guess.
        assert!(LO.decode(&[0xA9, 1], 0, DecodeMode(1)).is_none());
        assert!(LO.decode(&[0xAF, 1, 2], 0, DecodeMode(1)).is_none());
        assert!(LO.decode(&[], 0, DecodeMode(1)).is_none());
    }

    #[test]
    fn flow_classes_match_the_mnemonic_for_all_256() {
        for op in 0..=255u8 {
            let (m, _) = opcode_info(op);
            let d = decode(&[op, 0x10, 0x20, 0x01], 1);
            let want = match m {
                "BPL" | "BMI" | "BVC" | "BVS" | "BCC" | "BCS" | "BNE" | "BEQ" => "branch",
                "BRA" | "BRL" => "jump",
                "JMP" if op == 0x4C => "jump",
                "JMP" => "indirect",
                "JML" if op == 0x5C => "jumplong",
                "JML" => "indirect",
                "JSR" if op == 0x20 => "call",
                "JSR" => "indcall",
                "JSL" => "calllong",
                "RTS" => "return",
                "RTL" => "returnlong",
                "RTI" => "rti",
                "BRK" | "COP" => "break",
                "STP" => "halt",
                _ => "fall",
            };
            let got = match d.flow {
                Flow::Branch { .. } => "branch",
                Flow::Jump { .. } => "jump",
                Flow::JumpLong { .. } => "jumplong",
                Flow::Call { .. } => "call",
                Flow::CallLong { .. } => "calllong",
                Flow::IndirectCall => "indcall",
                Flow::IndirectJump => "indirect",
                Flow::Return => "return",
                Flow::ReturnLong => "returnlong",
                Flow::ReturnInterrupt => "rti",
                Flow::Break => "break",
                Flow::Halt => "halt",
                Flow::Fall => "fall",
            };
            assert_eq!(got, want, "opcode {op:#04x} ({m})");
        }
    }

    #[test]
    fn targets_stay_in_the_bank_and_long_ones_carry_all_24_bits() {
        let at = |cpu: u32, bytes: &[u8]| LO.decode(bytes, cpu, DecodeMode(1)).unwrap().flow;
        assert_eq!(
            at(0x80_8000, &[0xD0, 0x05]),
            Flow::Branch { target: 0x8007 }
        );
        assert_eq!(
            at(0x80_8000, &[0xD0, 0xFE]),
            Flow::Branch { target: 0x8000 }
        );
        // Wraps inside the bank, never into the next one.
        assert_eq!(
            at(0x81_FFF0, &[0xD0, 0x7F]),
            Flow::Branch { target: 0x0071 }
        );
        assert_eq!(at(0x00_8000, &[0x80, 0x10]), Flow::Jump { target: 0x8012 });
        // BRL: displacement from the next instruction, 16-bit wrap.
        assert_eq!(
            at(0x00_8000, &[0x82, 0xFD, 0xFF]),
            Flow::Jump { target: 0x8000 }
        );
        assert_eq!(
            at(0x02_FFFE, &[0x82, 0x05, 0x00]),
            Flow::Jump { target: 0x0006 }
        );
        assert_eq!(at(0, &[0x4C, 0x34, 0x92]), Flow::Jump { target: 0x9234 });
        assert_eq!(
            at(0, &[0x5C, 0x56, 0x34, 0x12]),
            Flow::JumpLong { target: 0x12_3456 }
        );
        assert_eq!(
            at(0, &[0x22, 0x00, 0x80, 0x84]),
            Flow::CallLong { target: 0x84_8000 }
        );
        assert_eq!(at(0, &[0x20, 0x00, 0x90]), Flow::Call { target: 0x9000 });
    }

    #[test]
    fn rep_and_sep_change_the_mode_after_and_xce_plp_say_unknown() {
        let after = |bytes: &[u8], mode: u8| decode(bytes, mode).after;
        // Native: REP/SEP act on bit 5 (M) and bit 4 (X).
        assert_eq!(after(&[0xC2, 0x30], 4), ModeAfter::Set(DecodeMode(1)));
        assert_eq!(after(&[0xC2, 0x20], 4), ModeAfter::Set(DecodeMode(3)));
        assert_eq!(after(&[0xC2, 0x10], 4), ModeAfter::Set(DecodeMode(2)));
        assert_eq!(after(&[0xE2, 0x30], 1), ModeAfter::Set(DecodeMode(4)));
        assert_eq!(after(&[0xE2, 0x20], 1), ModeAfter::Set(DecodeMode(2)));
        // Other bits of the immediate leave the widths alone.
        assert_eq!(after(&[0xC2, 0x03], 2), ModeAfter::Set(DecodeMode(2)));
        // Emulation mode: nothing to change.
        assert_eq!(after(&[0xC2, 0x30], 0), ModeAfter::Same);
        assert_eq!(after(&[0xE2, 0x30], 0), ModeAfter::Same);
        // XCE depends on carry, PLP on the stack.
        assert_eq!(after(&[0xFB], 0), ModeAfter::Unknown);
        assert_eq!(after(&[0xFB], 3), ModeAfter::Unknown);
        assert_eq!(after(&[0x28], 2), ModeAfter::Unknown);
        assert_eq!(after(&[0x28], 0), ModeAfter::Same);
        assert_eq!(after(&[0xEA], 2), ModeAfter::Same);
    }

    /// The static mode after REP/SEP is what the CPU really ends up in.
    #[test]
    fn rep_sep_prediction_agrees_with_the_cpu() {
        use crate::coverage::mode_index;
        for mode in 1..5u8 {
            for op in [0xC2u8, 0xE2] {
                for imm in [0x00u8, 0x10, 0x20, 0x30, 0xFF, 0x03] {
                    let bytes = [op, imm];
                    let ModeAfter::Set(want) = decode(&bytes, mode).after else {
                        panic!("REP/SEP in native mode must be Set");
                    };
                    let mut cpu = cpu_in(mode);
                    let mut bus = FlatBus::new();
                    bus.load(0x8000, &bytes);
                    cpu.step(&mut bus).unwrap();
                    assert_eq!(
                        mode_index(cpu.e, cpu.p),
                        want.0,
                        "op {op:#04x} #{imm:#04x} from mode {mode}"
                    );
                }
            }
        }
    }

    #[test]
    fn shape_hash_mask_covers_three_and_four_byte_absolute_operands_only() {
        let mask = |bytes: &[u8], mode: u8| LO.masked_operand(bytes, DecodeMode(mode));
        // abs (3), abs,X, abs,Y, (abs), (abs,X), [abs]; JMP/JSR forms too.
        for op in [
            0xAD_u8, 0xBD, 0xB9, 0x6C, 0x7C, 0xDC, 0x4C, 0x20, 0xFC, 0x8D, 0x0E,
        ] {
            assert_eq!(mask(&[op, 1, 2], 1), 1..3, "{op:#04x}");
        }
        // long (4) and long,X.
        for op in [0xAF_u8, 0xBF, 0x5C, 0x22, 0x8F] {
            assert_eq!(mask(&[op, 1, 2, 3], 1), 1..4, "{op:#04x}");
        }
        // Kept: immediates of both widths, direct page, relative, stack,
        // PEA, block moves, implied.
        assert_eq!(mask(&[0xA9, 1, 2], 1), 0..0, "LDA #imm16");
        assert_eq!(mask(&[0xA9, 1], 2), 0..0, "LDA #imm8");
        assert_eq!(mask(&[0xA2, 1, 2], 1), 0..0, "LDX #imm16");
        assert_eq!(mask(&[0xA5, 1], 1), 0..0, "LDA dp");
        assert_eq!(mask(&[0xB5, 1], 1), 0..0, "LDA dp,X");
        assert_eq!(mask(&[0xA3, 1], 1), 0..0, "LDA sr,S");
        assert_eq!(mask(&[0xD0, 1], 1), 0..0, "BNE");
        assert_eq!(mask(&[0x82, 1, 2], 1), 0..0, "BRL");
        assert_eq!(mask(&[0xF4, 1, 2], 1), 0..0, "PEA");
        assert_eq!(mask(&[0x54, 1, 2], 1), 0..0, "MVN");
        assert_eq!(mask(&[0xEA], 1), 0..0, "NOP");
        assert_eq!(mask(&[], 1), 0..0);
        // A truncated instruction masks nothing rather than overrunning.
        assert_eq!(mask(&[0xAF, 1, 2], 1), 0..0);
    }

    /// Every opcode that takes an absolute or long operand in the table is
    /// covered by the mask rule, none other.
    #[test]
    fn mask_rule_follows_the_addressing_mode_table_for_all_256() {
        for op in 0..=255u8 {
            let (_, m) = opcode_info(op);
            let insn = [op, 1, 2, 3];
            let len = 1 + m.operand_len(true, true);
            let got = LO.masked_operand(&insn[..len], DecodeMode(0));
            let want = match m {
                Mode::Abs
                | Mode::AbsX
                | Mode::AbsY
                | Mode::AbsInd
                | Mode::AbsIndX
                | Mode::AbsIndLong => 1..3,
                Mode::Long | Mode::LongX => 1..4,
                _ => 0..0,
            };
            assert_eq!(got, want, "{op:#04x}");
        }
    }

    #[test]
    fn canonical_cpu_address_round_trips_for_lorom_and_hirom() {
        for (isa, len) in [(LO, 0x10_0000usize), (HI, 0x10_0000), (LO, 0x8000)] {
            for rom in (0..len as u32).step_by(0x1F7) {
                let cpu = isa.cpu_for_rom(rom);
                assert_eq!(isa.rom_offset(cpu), Some(rom), "{rom:#x} via {cpu:#08x}");
            }
        }
    }

    #[test]
    fn switched_boards_place_nothing_statically_in_their_switched_banks() {
        let sdd1 = Wdc65816::new(SnesMapMode::Sdd1, 0x10_0000);
        assert_eq!(sdd1.rom_offset(0x00_8000), Some(0));
        assert_eq!(sdd1.rom_offset(0x80_8000), Some(0));
        for cpu in [0xC0_0000u32, 0xC0_8000, 0xDF_FFFF, 0xFF_8000] {
            assert_eq!(sdd1.rom_offset(cpu), None, "{cpu:#08x}");
        }
        let sa1 = Wdc65816::new(SnesMapMode::Sa1, 0x10_0000);
        for cpu in [0x00_8000u32, 0x80_8000, 0xC0_0000, 0xFF_FFFF] {
            assert_eq!(sa1.rom_offset(cpu), None, "{cpu:#08x}");
        }
    }

    #[test]
    fn rom_offset_is_none_for_ram_and_registers() {
        for cpu in [0x7E_0000u32, 0x7F_FFFF, 0x00_2100, 0x00_4200, 0x00_0000] {
            assert_eq!(LO.rom_offset(cpu), None, "{cpu:#08x}");
        }
        // The documented aliases.
        assert_eq!(LO.rom_offset(0x00_8000), LO.rom_offset(0x80_8000));
        assert_eq!(HI.rom_offset(0x40_8000), HI.rom_offset(0xC0_8000));
        assert_eq!(LO.rom_offset(0x01_8000), Some(0x8000));
    }

    /// Ticket W37-01: a LoROM with only its vectors set seeds RESET in
    /// emulation mode and the native NMI in all four width states, every
    /// node `Inferred`.
    #[test]
    fn the_vectors_alone_seed_an_all_inferred_atlas() {
        let mut rom = vec![0u8; 0x8000];
        rom[0x7FD5] = 0x20;
        rom[0x7FD7] = 0x08;
        rom[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes());
        rom[0x7FEA..0x7FEC].copy_from_slice(&0x8010u16.to_le_bytes());
        rom[0..2].copy_from_slice(&[0x80, 0xFE]); // BRA *
        rom[0x10..0x15].copy_from_slice(&[0xC2, 0x30, 0xE6, 0x30, 0x40]); // REP #$30 ; INC $30 ; RTI
        let a = build_seeded_atlas_from_image(&rom, &CoverageLog::new(0)).expect("builds");
        assert_eq!(a.stats.observed_insns, 0);
        assert!(a
            .insns()
            .all(|i| i.prov == rf_rebuild::Provenance::Inferred));
        let modes = |rom_off: u32| -> Vec<u8> {
            let mut m: Vec<u8> = a
                .functions
                .iter()
                .filter(|f| f.entry_rom == rom_off)
                .map(|f| f.entry_mode)
                .collect();
            m.sort_unstable();
            m
        };
        assert_eq!(modes(0), vec![0], "reset runs in emulation mode");
        assert_eq!(modes(0x10), vec![1, 2, 3, 4], "native NMI: every width");
        // An image with no vectors seeds nothing and still builds.
        let blank = vec![0u8; 0x8000];
        let mut b = blank.clone();
        b[0x7FD5] = 0x20;
        b[0x7FD7] = 0x08;
        let none = build_seeded_atlas_from_image(&b, &CoverageLog::new(0)).expect("builds");
        assert!(none.functions.is_empty());
    }
}
