//! SNES code formats.
//!
//! # Game Genie (`DDAA-AAAA`)
//!
//! Eight "Genie hex" digits. Per fullsnes, section "SNES Cart Cheat Devices"
//! (<https://problemkaputt.de/fullsnes.htm>): first replace Genie digits
//! with normal hex digits,
//!
//! ```text
//! Genie hex:  D F 4 7 0 9 1 5 6 B C 8 A 2 3 E
//! Normal hex: 0 1 2 3 4 5 6 7 8 9 A B C D E F
//! ```
//!
//! then `DD` is the data byte and the 24-bit `AAAAAA` is shuffled:
//!
//! ```text
//! ijklqrst opabcduv wxefghmn   Genie address (i = bit 23 ... n = bit 0)
//! abcdefgh ijklmnop qrstuvwx   SNES address  (a = bit 23 ... x = bit 0)
//! ```
//!
//! fullsnes says the address may be ROM or SRAM and that WRAM is not
//! supported, so every Game Genie code is a [`RomOverride`] without a
//! compare. This table is single-sourced: the only other SNES page found
//! (<https://doc.kodewerx.org/hacking_snes.html>) says "use the encoder" and
//! gives no table, and the Genie letters-versus-hex mapping was not
//! cross-checked against a second document.
//!
//! # Pro Action Replay (`BBAAAAVV`)
//!
//! Raw 8 digits, 24-bit address then value (fullsnes, same section). The
//! device classification follows the fullsnes "SNES Memory Map" section:
//! banks `$7E-$7F` are WRAM; offsets `$0000-$1FFF` of banks `$00-$3F` and
//! `$80-$BF` mirror the first 8 KiB of WRAM; SRAM lives at `$70-$7D:0000-7FFF`
//! (LoROM; offsets `$8000+` of those banks are HiROM cartridge ROM). Those
//! are [`RamWrite`]; everything else is a [`RomOverride`].

use rf_core_api::{RamWrite, RomOverride};

use crate::error::CheatError;
use crate::{hex_field, split_raw, Cheat, CheatEffect, CodeFormat};

const GENIE_HEX: &[u8; 16] = b"DF4709156BC8A23E";

/// For each SNES address bit (index 0 = bit 23), the Genie address bit index
/// (0 = bit 23) it is stored in: `ijklqrstopabcduvwxefghmn` vs
/// `abcdefghijklmnopqrstuvwx`.
const SHUFFLE: [u32; 24] = [
    10, 11, 12, 13, 18, 19, 20, 21, 0, 1, 2, 3, 22, 23, 8, 9, 4, 5, 6, 7, 14, 15, 16, 17,
];

fn unshuffle(genie: u32) -> u32 {
    SHUFFLE
        .iter()
        .fold(0, |acc, &j| (acc << 1) | ((genie >> (23 - j)) & 1))
}

fn shuffle(snes: u32) -> u32 {
    SHUFFLE.iter().enumerate().fold(0, |acc, (k, &j)| {
        acc | (((snes >> (23 - k)) & 1) << (23 - j))
    })
}

/// True for addresses the cheat applies to as RAM (see module docs).
fn is_ram_addr(addr: u32) -> bool {
    let bank = (addr >> 16) & 0xFF;
    let off = addr & 0xFFFF;
    match bank {
        0x7E | 0x7F => true,
        0x70..=0x7D => off < 0x8000,
        0x00..=0x3F | 0x80..=0xBF => off <= 0x1FFF,
        _ => false,
    }
}

fn effect(addr: u32, value: u8, compare: Option<u8>) -> CheatEffect {
    if is_ram_addr(addr) {
        CheatEffect::Ram(RamWrite {
            addr,
            value,
            compare,
        })
    } else {
        CheatEffect::Rom(RomOverride {
            addr,
            value,
            compare,
        })
    }
}

pub(crate) fn parse(code: &str) -> Result<Cheat, CheatError> {
    if code.contains(':') {
        let (addr, compare, value) = split_raw(code, 6)?;
        return Ok(Cheat {
            format: CodeFormat::Raw,
            effect: effect(addr, value, compare),
        });
    }
    if code.contains('-') {
        return parse_game_genie(code);
    }
    let n = code.chars().count();
    if n != 8 {
        return Err(CheatError::BadCodeLength {
            expected: "XXXX-XXXX (Game Genie), 8 hex digits (Action Replay) or AAAAAA:VV",
            got: n,
        });
    }
    let all = hex_field("Action Replay code", code, 8)?;
    Ok(Cheat {
        format: CodeFormat::SnesActionReplay,
        effect: effect(all >> 8, (all & 0xFF) as u8, None),
    })
}

fn parse_game_genie(code: &str) -> Result<Cheat, CheatError> {
    let (left, right) = code.split_once('-').ok_or(CheatError::Malformed {
        reason: "expected XXXX-XXXX",
    })?;
    let (ln, rn) = (left.chars().count(), right.chars().count());
    if ln != 4 || rn != 4 || right.contains('-') {
        return Err(CheatError::BadCodeLength {
            expected: "XXXX-XXXX",
            got: ln + rn,
        });
    }
    let nibbles = left
        .chars()
        .chain(right.chars())
        .enumerate()
        .map(|(index, ch)| {
            GENIE_HEX
                .iter()
                .position(|&b| char::from(b) == ch)
                .and_then(|p| u32::try_from(p).ok())
                .ok_or(CheatError::BadChar {
                    field: "Game Genie code",
                    ch,
                    index,
                })
        })
        .collect::<Result<Vec<u32>, _>>()?;
    let all = nibbles.iter().fold(0u32, |acc, &d| (acc << 4) | d);
    let addr = unshuffle(all & 0xFF_FFFF);
    Ok(Cheat {
        format: CodeFormat::SnesGameGenie,
        effect: CheatEffect::Rom(RomOverride {
            addr,
            value: (all >> 24) as u8,
            compare: None,
        }),
    })
}

/// Encode as `DDAA-AAAA`. Only the low 24 bits of `addr` are encoded.
#[must_use]
pub fn encode_snes_game_genie(addr: u32, value: u8) -> String {
    let all = (u32::from(value) << 24) | shuffle(addr & 0xFF_FFFF);
    (0..8u32)
        .rev()
        .map(|i| {
            GENIE_HEX
                .get(((all >> (i * 4)) & 15) as usize)
                .map_or('D', |&b| char::from(b))
        })
        .enumerate()
        .flat_map(|(i, c)| (i == 4).then_some('-').into_iter().chain(Some(c)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shuffle_is_a_permutation() {
        let mut seen = SHUFFLE.to_vec();
        seen.sort_unstable();
        assert_eq!(seen, (0..24).collect::<Vec<u32>>());
    }

    #[test]
    fn shuffle_round_trips() {
        for a in (0..0x100_0000u32).step_by(0x1_0F1) {
            assert_eq!(unshuffle(shuffle(a)), a);
        }
    }

    #[test]
    fn shuffle_moves_documented_bits() {
        // SNES bit 23 ('a') sits at Genie index 10 -> Genie bit 13.
        assert_eq!(unshuffle(1 << 13), 1 << 23);
        // SNES bit 0 ('x') sits at Genie index 17 -> Genie bit 6.
        assert_eq!(unshuffle(1 << 6), 1);
    }

    #[test]
    fn ram_classification() {
        assert!(is_ram_addr(0x7E_0000));
        assert!(is_ram_addr(0x7F_FFFF));
        assert!(is_ram_addr(0x00_1FFF));
        assert!(is_ram_addr(0xBF_0000));
        assert!(is_ram_addr(0x70_7FFF));
        assert!(!is_ram_addr(0x70_8000));
        assert!(!is_ram_addr(0x00_8000));
        assert!(!is_ram_addr(0xC0_0000));
    }
}
