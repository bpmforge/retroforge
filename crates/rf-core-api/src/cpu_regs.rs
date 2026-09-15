//! Typed CPU register files for [`crate::StateView`] (ticket W13-02i,
//! ruling D-6).
//!
//! # Why an enum of typed register files, and not bytes
//!
//! `StateView::cpu_regs` was an untyped `&[u8]` with a "core-defined
//! field order". Nobody ever filled it, and both cores said why in the
//! same words: publishing a byte layout means every consumer decodes it
//! per console, which is how one shell ends up knowing two consoles'
//! private formats — the half-console shape W11-07 was split to avoid.
//! The register *set* of a CPU is not private: it is the instruction-set
//! architecture, documented on nesdev and in the W65C816S datasheet, and
//! a debugger cannot show `A` without knowing there is an `A`. So the
//! contract names the register files and the shell matches on them.
//!
//! [`CpuRegs::None`] exists for cores that have no CPU to report — the
//! scripted harness cores — and is a truthful answer, not a stub: a
//! consumer that receives it draws nothing rather than decoding garbage.
//!
//! # Values, not borrows
//!
//! Every field is a plain integer copied out at `state_view()` time. A
//! register file is a dozen bytes; borrowing it would pin a core-private
//! `Cpu` type into the contract's lifetime for no saving. Copying also
//! keeps [`crate::StateView`] `Copy`, which it already was.
//!
//! # What this decides for the other two trait gaps
//!
//! W11-10's close note listed three things the trait could not express:
//! typed CPU registers (this), an out-of-band bus **write** for the
//! memory editor (W13-02d delivered it shell-side and deliberately did
//! not promote it), and APU access for channel capture. The rule this
//! module sets is: **a trait expression is added when a second core has
//! a consumer for it, and it is typed, never a byte layout.** The bus
//! write has one consumer and one console today and stays where it is.
//! APU access has no consumer through the trait at all — the SNES DSP
//! voice view reaches `rf_snes::debug` directly — and stays that way
//! until a viewer wants both APUs through one path.

/// The 6502 register file as the NES's Ricoh 2A03 exposes it
/// (nesdev "CPU registers").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mos6502Regs {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    /// Stack pointer: the offset into page `$01xx`.
    pub s: u8,
    pub pc: u16,
    /// Processor status, `NV-BDIZC`. Bit 5 always reads 1 and bit 4
    /// (`B`) always reads 0 in the live register; only pushed copies
    /// carry `B`.
    pub p: u8,
}

impl Mos6502Regs {
    /// The status byte as nestest prints it: a letter for a set flag, a
    /// dot for a clear one, in `NV-BDIZC` order (bit 5 is shown as `-`).
    #[must_use]
    pub fn flags(&self) -> String {
        flag_string(self.p, b"NV-BDIZC")
    }
}

/// The 65C816 register file and mode state as the SNES's CPU exposes it
/// (W65C816S datasheet §2; fullsnes "SNES CPU Registers").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Wdc65816Regs {
    /// Full 16-bit accumulator. In 8-bit mode (`M` set, or emulation)
    /// the high half `B` is hidden from most operations but preserved.
    pub a: u16,
    pub x: u16,
    pub y: u16,
    /// Stack pointer. In emulation mode the high byte is pinned to `$01`.
    pub sp: u16,
    /// Direct page base.
    pub d: u16,
    /// Data bank register.
    pub dbr: u8,
    /// Program bank register: the bank `pc` is fetched from.
    pub pbr: u8,
    pub pc: u16,
    /// Processor status, `NVMXDIZC` in native mode. In emulation mode bit
    /// 5 is the `M`-position and reads as 1, and bit 4 is `B`.
    pub p: u8,
    /// Emulation mode (`E`). Not a bit of `p`: it is the hidden flag
    /// `XCE` swaps with carry, and it changes what `p`'s bits 4 and 5
    /// mean.
    pub e: bool,
}

impl Wdc65816Regs {
    /// The 24-bit address the next instruction is fetched from,
    /// `pbr:pc`, which is what a bsnes-style trace prints first.
    #[must_use]
    pub fn full_pc(&self) -> u32 {
        (u32::from(self.pbr) << 16) | u32::from(self.pc)
    }

    /// The status byte as bsnes prints it: `NVMXDIZC` in native mode,
    /// `NV1BDIZC` in emulation mode (bit 5 is fixed at 1 there and bit 4
    /// is `B`), a letter for a set flag and a dot for a clear one.
    #[must_use]
    pub fn flags(&self) -> String {
        if self.e {
            flag_string(self.p, b"NV1BDIZC")
        } else {
            flag_string(self.p, b"NVMXDIZC")
        }
    }
}

/// The CPU register file a core reports through [`crate::StateView`].
///
/// One variant per CPU family the workspace emulates. A consumer that
/// wants to serve every console matches on this; a consumer that only
/// understands one CPU matches one arm and draws nothing for the rest,
/// which is the honest answer and costs nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CpuRegs {
    /// This core has no CPU register file to report (a scripted or
    /// mock core). Never the right answer for a real console.
    #[default]
    None,
    /// A 6502-family CPU (NES).
    Mos6502(Mos6502Regs),
    /// A 65C816 (SNES).
    Wdc65816(Wdc65816Regs),
}

impl CpuRegs {
    /// The address the next instruction will be fetched from, as a flat
    /// number: 16-bit on a 6502, `pbr:pc` on a 65C816, `None` when there
    /// is no CPU. This is the one register every consumer wants without
    /// caring which CPU it is (a "run to cursor", a breakpoint readout).
    #[must_use]
    pub fn pc(&self) -> Option<u32> {
        match self {
            CpuRegs::None => None,
            CpuRegs::Mos6502(r) => Some(u32::from(r.pc)),
            CpuRegs::Wdc65816(r) => Some(r.full_pc()),
        }
    }
}

/// Render `p` against an 8-letter template, MSB first: the template
/// letter when the bit is set, `.` when clear. A template position of
/// `-` or `1` is copied through unchanged — those bits have a fixed
/// meaning and showing them as set/clear would be noise.
fn flag_string(p: u8, template: &[u8; 8]) -> String {
    template
        .iter()
        .enumerate()
        .map(|(i, &letter)| {
            let bit = 7 - i;
            let fixed = letter == b'-' || letter == b'1';
            if fixed || p & (1 << bit) != 0 {
                char::from(letter)
            } else {
                '.'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mos6502_flags_read_in_nestest_order_with_bit5_fixed() {
        let r = Mos6502Regs {
            p: 0b1010_0101,
            ..Default::default()
        };
        // N set, V clear, bit5 shown as '-', B clear, D clear, I set,
        // Z clear, C set.
        assert_eq!(r.flags(), "N.-..I.C");
        let all = Mos6502Regs {
            p: 0xFF,
            ..Default::default()
        };
        assert_eq!(all.flags(), "NV-BDIZC");
        let none = Mos6502Regs::default();
        assert_eq!(none.flags(), "..-.....");
    }

    #[test]
    fn wdc65816_flags_depend_on_emulation_mode() {
        let native = Wdc65816Regs {
            p: 0b0011_0000,
            e: false,
            ..Default::default()
        };
        assert_eq!(native.flags(), "..MX....");
        let emu = Wdc65816Regs {
            p: 0b0011_0000,
            e: true,
            ..Default::default()
        };
        // Same byte, different meaning: bit 5 is fixed and bit 4 is B.
        assert_eq!(emu.flags(), "..1B....");
    }

    #[test]
    fn full_pc_joins_bank_and_offset() {
        let r = Wdc65816Regs {
            pbr: 0x80,
            pc: 0x8123,
            ..Default::default()
        };
        assert_eq!(r.full_pc(), 0x80_8123);
        assert_eq!(CpuRegs::Wdc65816(r).pc(), Some(0x80_8123));
    }

    #[test]
    fn pc_is_the_one_register_every_consumer_can_read_blind() {
        assert_eq!(CpuRegs::None.pc(), None);
        let r = Mos6502Regs {
            pc: 0xC000,
            ..Default::default()
        };
        assert_eq!(CpuRegs::Mos6502(r).pc(), Some(0xC000));
    }

    #[test]
    fn default_is_none_so_a_forgotten_field_is_visibly_empty() {
        assert_eq!(CpuRegs::default(), CpuRegs::None);
    }
}
