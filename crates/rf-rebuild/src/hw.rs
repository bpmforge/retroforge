//! What a CPU-bus address means on each console (ticket W36-09): the table
//! the understand pass classifies every logged access with.
//!
//! These are hardware facts, the same for every game (law 5). Sources:
//!
//! - NES: nesdev.org wiki "CPU memory map" (RAM `$0000-$07FF` mirrored to
//!   `$1FFF`; PPU registers `$2000-$2007` mirrored to `$3FFF`; APU and I/O
//!   `$4000-$4017`; cartridge space from `$4020`; PRG RAM `$6000-$7FFF`; PRG
//!   ROM from `$8000`, where a write reaches the mapper), "PPU registers",
//!   "APU registers", "OAM DMA" (`$4014`) and "Controller reading"
//!   (`$4016` strobe write, `$4016`/`$4017` serial reads; a write to `$4017`
//!   is the APU frame counter).
//! - SNES: fullsnes (problemkaputt.de) "SNES Memory Map" (system area at
//!   offsets `$0000-$1FFF` WRAM mirror, `$2100-$213F` PPU, `$2140-$217F`
//!   APU ports mirrored every four bytes, `$2180-$2183` WRAM port,
//!   `$4016/$4017` manual joypad, `$4200-$421F` CPU I/O, `$4300-$437F` DMA
//!   channels in banks `$00-$3F` and `$80-$BF`; WRAM in banks `$7E-$7F`),
//!   "SNES PPU Control", "SNES APU", "SNES DMA and HDMA", "SNES Maths
//!   Multiply/Divide", "SNES Timers and I/O" (`$4210` NMI flag, `$4212`
//!   status with the V-blank flag in bit 7, `$4218-$421F` auto-joypad).
//!
//! An access to ROM is ignored for classification: on both consoles every
//! instruction fetch and table read lands there, and none of it says what
//! the function does to the machine. A write to cartridge space on the NES
//! is a mapper write (nesdev "Mapper").

/// Which console's address map to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Platform {
    Nes,
    Snes,
}

/// A hardware block a register belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HwClass {
    /// Picture unit registers.
    Ppu,
    /// Sound: the NES APU, the SNES `$2140-$2143` ports to the sound CPU.
    Apu,
    /// Joypad strobe, serial reads, SNES auto-read results.
    Controller,
    /// NES `$4014` OAM DMA.
    OamDma,
    /// SNES DMA/HDMA enable and channel registers.
    Dma,
    /// NES mapper (a write to cartridge space).
    Mapper,
    /// SNES multiply/divide unit.
    Math,
    /// SNES interrupt enables, timers and status (`$4200-$4212`).
    CpuStatus,
    /// SNES `$2180-$2183` WRAM access port.
    WramPort,
    /// Cartridge coprocessor registers (SNES `$2200-$3FFF`).
    Coprocessor,
}

impl HwClass {
    /// Stable lower-case label (used in recipes, reports and the AI payload's
    /// closed vocabulary).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ppu => "ppu",
            Self::Apu => "apu",
            Self::Controller => "controller",
            Self::OamDma => "oam_dma",
            Self::Dma => "dma",
            Self::Mapper => "mapper",
            Self::Math => "math",
            Self::CpuStatus => "cpu_status",
            Self::WramPort => "wram_port",
            Self::Coprocessor => "coprocessor",
        }
    }
}

/// What one CPU-bus access touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// A hardware register, by its normalised 16-bit address (mirrors folded).
    Reg { class: HwClass, reg: u16 },
    /// Work or save RAM (`addr` is the folded address; SNES work RAM is
    /// `0x7E_0000 | offset`, whichever mirror was used).
    Ram { addr: u32 },
    /// The stack page (`$0100-$01FF`): push/pull traffic that says nothing
    /// about which data a function owns.
    Stack,
    /// ROM, open bus, unmapped: not classified.
    Ignore,
}

/// Classify an access of `addr` (`write` = a write).
#[must_use]
pub fn classify(platform: Platform, addr: u32, write: bool) -> Hit {
    match platform {
        Platform::Nes => classify_nes(addr as u16, write),
        Platform::Snes => classify_snes(addr),
    }
}

fn reg(class: HwClass, reg: u16) -> Hit {
    Hit::Reg { class, reg }
}

fn classify_nes(addr: u16, write: bool) -> Hit {
    match addr {
        0x0100..=0x01FF => Hit::Stack,
        0x0000..=0x1FFF => Hit::Ram {
            addr: u32::from(addr & 0x07FF),
        },
        0x2000..=0x3FFF => reg(HwClass::Ppu, 0x2000 | (addr & 7)),
        0x4014 => reg(HwClass::OamDma, 0x4014),
        0x4000..=0x4013 | 0x4015 => reg(HwClass::Apu, addr),
        // Writes to $4016 strobe the pads; reads of $4016/$4017 are the pads.
        // A write to $4017 is the APU frame counter (nesdev "APU Frame Counter").
        0x4016 => reg(HwClass::Controller, addr),
        0x4017 if write => reg(HwClass::Apu, addr),
        0x4017 => reg(HwClass::Controller, addr),
        0x4018..=0x401F => Hit::Ignore,
        0x4020..=0x5FFF if write => reg(HwClass::Mapper, addr),
        0x4020..=0x5FFF => Hit::Ignore,
        0x6000..=0x7FFF => Hit::Ram {
            addr: u32::from(addr),
        },
        0x8000..=0xFFFF if write => reg(HwClass::Mapper, addr),
        0x8000..=0xFFFF => Hit::Ignore,
    }
}

fn classify_snes(addr: u32) -> Hit {
    let bank = ((addr >> 16) & 0xFF) as u8;
    let off = (addr & 0xFFFF) as u16;
    match bank {
        0x7E | 0x7F => {
            if bank == 0x7E && (0x0100..=0x01FF).contains(&off) {
                Hit::Stack
            } else {
                Hit::Ram {
                    addr: 0x7E_0000 | (u32::from(bank - 0x7E) << 16) | u32::from(off),
                }
            }
        }
        0x00..=0x3F | 0x80..=0xBF => match off {
            0x0100..=0x01FF => Hit::Stack,
            0x0000..=0x1FFF => Hit::Ram {
                addr: 0x7E_0000 | u32::from(off),
            },
            0x2100..=0x213F => reg(HwClass::Ppu, off),
            0x2140..=0x217F => reg(HwClass::Apu, 0x2140 | (off & 3)),
            0x2180..=0x2183 => reg(HwClass::WramPort, off),
            0x2200..=0x3FFF => reg(HwClass::Coprocessor, off),
            0x4016 | 0x4017 => reg(HwClass::Controller, off),
            0x4202..=0x4206 | 0x4214..=0x4217 => reg(HwClass::Math, off),
            0x420B | 0x420C | 0x4300..=0x437F => reg(HwClass::Dma, off),
            0x4200..=0x4201 | 0x4207..=0x420A | 0x420D | 0x4210..=0x4212 => {
                reg(HwClass::CpuStatus, off)
            }
            0x4218..=0x421F => reg(HwClass::Controller, off),
            _ => Hit::Ignore,
        },
        // Cartridge SRAM in the banks LoROM puts it in.
        0x70..=0x7D if off < 0x8000 => Hit::Ram { addr },
        _ => Hit::Ignore,
    }
}

/// Bits of [`crate::profile::FunctionProfile::flags`]: facts that need the
/// value of an access, not just its address. All are about which hardware
/// resource was addressed, never about code bytes.
pub mod flag {
    /// NES: a write to `$2006` of a high byte in `$3F00-$3FFF` (palette).
    pub const PPU_ADDR_PALETTE: u32 = 1 << 0;
    /// NES: a write to `$2006` of a high byte in `$20-$3E` (nametables and
    /// their mirrors).
    pub const PPU_ADDR_NAMETABLE: u32 = 1 << 1;
    /// A read of the vblank/NMI status (NES `$2002`, SNES `$4210`/`$4212`)
    /// returned with bit 7 set.
    pub const STATUS_VBLANK_SEEN: u32 = 1 << 2;
    /// A read of NES `$2002` returned with bit 6 (sprite 0 hit) set.
    pub const STATUS_SPRITE0_SEEN: u32 = 1 << 3;
    /// SNES: a DMA channel's destination register `$43x1` was set to VRAM
    /// data (`$18`/`$19`).
    pub const DMA_TO_VRAM: u32 = 1 << 4;
    /// SNES: set to OAM data (`$04`).
    pub const DMA_TO_OAM: u32 = 1 << 5;
    /// SNES: set to CGRAM data (`$22`).
    pub const DMA_TO_CGRAM: u32 = 1 << 6;
}

/// Update `flags` for one register access (`value` is what was read or is
/// being written).
pub fn note_value(platform: Platform, flags: &mut u32, reg: u16, write: bool, value: u8) {
    match (platform, reg, write) {
        (Platform::Nes, 0x2006, true) => {
            if value == 0x3F {
                *flags |= flag::PPU_ADDR_PALETTE;
            } else if (0x20..0x3F).contains(&value) {
                *flags |= flag::PPU_ADDR_NAMETABLE;
            }
        }
        (Platform::Nes, 0x2002, false) => {
            if value & 0x80 != 0 {
                *flags |= flag::STATUS_VBLANK_SEEN;
            }
            if value & 0x40 != 0 {
                *flags |= flag::STATUS_SPRITE0_SEEN;
            }
        }
        (Platform::Snes, 0x4210 | 0x4212, false) => {
            if value & 0x80 != 0 {
                *flags |= flag::STATUS_VBLANK_SEEN;
            }
        }
        // $43x1 is the channel's B-bus destination (fullsnes "DMA Channel
        // Registers"): $18/$19 VMDATAL/H, $04 OAMDATA, $22 CGDATA.
        (Platform::Snes, 0x4300..=0x437F, true) if reg & 0xF == 1 => match value {
            0x18 | 0x19 => *flags |= flag::DMA_TO_VRAM,
            0x04 => *flags |= flag::DMA_TO_OAM,
            0x22 => *flags |= flag::DMA_TO_CGRAM,
            _ => {}
        },
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nes_registers_fold_mirrors_and_split_4017_by_direction() {
        let c = |a, w| classify(Platform::Nes, a, w);
        assert_eq!(
            c(0x3456, true),
            Hit::Reg {
                class: HwClass::Ppu,
                reg: 0x2006
            }
        );
        assert_eq!(c(0x0FFF, false), Hit::Ram { addr: 0x7FF });
        assert_eq!(c(0x01F0, true), Hit::Stack);
        assert!(matches!(
            c(0x4014, true),
            Hit::Reg {
                class: HwClass::OamDma,
                ..
            }
        ));
        assert!(matches!(
            c(0x4017, true),
            Hit::Reg {
                class: HwClass::Apu,
                ..
            }
        ));
        assert!(matches!(
            c(0x4017, false),
            Hit::Reg {
                class: HwClass::Controller,
                ..
            }
        ));
        assert!(matches!(
            c(0x8000, true),
            Hit::Reg {
                class: HwClass::Mapper,
                ..
            }
        ));
        assert_eq!(
            c(0x8000, false),
            Hit::Ignore,
            "ROM reads are not classified"
        );
        assert_eq!(c(0x6000, false), Hit::Ram { addr: 0x6000 });
    }

    #[test]
    fn snes_system_area_is_mirrored_across_banks_and_wram_is_folded() {
        let c = |a, w| classify(Platform::Snes, a, w);
        let ppu = Hit::Reg {
            class: HwClass::Ppu,
            reg: 0x2118,
        };
        assert_eq!(c(0x00_2118, true), ppu);
        assert_eq!(c(0x80_2118, true), ppu);
        assert_eq!(
            c(0x00_2147, true),
            Hit::Reg {
                class: HwClass::Apu,
                reg: 0x2143
            },
            "APU ports mirror every four bytes"
        );
        assert_eq!(c(0x00_0223, false), Hit::Ram { addr: 0x7E_0223 });
        assert_eq!(c(0x7E_0223, false), Hit::Ram { addr: 0x7E_0223 });
        assert_eq!(c(0x00_0010, false), Hit::Ram { addr: 0x7E_0010 });
        assert_eq!(c(0x7F_8000, true), Hit::Ram { addr: 0x7F_8000 });
        assert_eq!(c(0x00_01FF, true), Hit::Stack);
        assert!(matches!(
            c(0x00_420B, true),
            Hit::Reg {
                class: HwClass::Dma,
                ..
            }
        ));
        assert!(matches!(
            c(0x00_4218, false),
            Hit::Reg {
                class: HwClass::Controller,
                ..
            }
        ));
        assert_eq!(c(0x00_8000, false), Hit::Ignore);
        assert_eq!(c(0xC0_1234, false), Hit::Ignore);
    }

    #[test]
    fn values_set_only_the_flags_they_mean() {
        let mut f = 0;
        note_value(Platform::Nes, &mut f, 0x2006, true, 0x3F);
        assert_eq!(f, flag::PPU_ADDR_PALETTE);
        note_value(Platform::Nes, &mut f, 0x2006, true, 0x23);
        note_value(Platform::Nes, &mut f, 0x2002, false, 0x80);
        assert_eq!(
            f,
            flag::PPU_ADDR_PALETTE | flag::PPU_ADDR_NAMETABLE | flag::STATUS_VBLANK_SEEN
        );
        let mut g = 0;
        note_value(Platform::Snes, &mut g, 0x4301, true, 0x18);
        note_value(Platform::Snes, &mut g, 0x4302, true, 0x18);
        assert_eq!(g, flag::DMA_TO_VRAM, "only $43x1 selects a destination");
    }
}
