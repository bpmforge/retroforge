//! NES code formats.
//!
//! # Game Genie
//!
//! Alphabet `APZLGITYEOXUKSVN` is the nibble values 0..=15 in order. With
//! letters as nibbles `n0..n7` the bit layout is (all of it identical in
//! three independent write-ups, which also agree on the worked vectors
//! below):
//!
//! ```text
//! address = $8000 | (n3&7)<<12 | (n5&7)<<8 | (n4&8)<<8 | (n2&7)<<4
//!                 | (n1&8)<<4  | (n4&7)    | (n3&8)
//! data    = (n1&7)<<4 | (n0&8)<<4 | (n0&7) | (n5&8)   6 letters
//!         = (n1&7)<<4 | (n0&8)<<4 | (n0&7) | (n7&8)   8 letters
//! compare = (n7&7)<<4 | (n6&8)<<4 | (n6&7) | (n5&8)   8 letters only
//! ```
//!
//! Sources: "NES Game Genie Technical Notes",
//! <https://tuxnes.sourceforge.net/gamegenie.html>; the same layout at
//! <https://notes.chipx86.com/Knowledge+Base/Games/Game+Genie+for+NES> and
//! <https://doc.kodewerx.org/hacking_nes.html>. The nesdev wiki page
//! (<https://www.nesdev.org/wiki/Game_Genie>) describes the device (address
//! bit 15 is fixed to 1, optional compare per code) but carries the code
//! layout only as a diagram, so it is the hardware reference, not the
//! source of the table. Vectors from tuxnes: `GOSSIP` is `$D1DD` value
//! `$14`; `ZEXPYGLA` is `$94A7` value `$02` compare `$03`.
//!
//! The high bit of the third letter (`n2`) does not take part in decoding.
//! This encoder sets it for 8-letter codes and clears it for 6-letter ones,
//! which is the convention published code lists follow (unverified against
//! a primary source; the decoder ignores it, so it cannot reject a code).
//!
//! # Pro Action Replay and raw codes
//!
//! Plain `AAAAVV` / `AAAA:VV` / `AAAA?CC:VV` hex. RAM-side addresses are the
//! 2 KiB work RAM and its mirrors `$0000-$1FFF` and cartridge RAM
//! `$6000-$7FFF` (nesdev CPU memory map,
//! <https://www.nesdev.org/wiki/CPU_memory_map>); `$2000-$5FFF` is I/O and is
//! refused. `$8000-$FFFF` is cartridge ROM, so a raw code there is a
//! [`RomOverride`].

use rf_core_api::{RamWrite, RomOverride};

use crate::error::CheatError;
use crate::{hex_field, split_raw, Cheat, CheatEffect, CodeFormat};

const ALPHABET: &[u8; 16] = b"APZLGITYEOXUKSVN";

fn letter_value(ch: char) -> Option<u32> {
    ALPHABET
        .iter()
        .position(|&b| char::from(b) == ch)
        .and_then(|p| u32::try_from(p).ok())
}

fn is_ram_addr(addr: u32) -> bool {
    addr <= 0x1FFF || (0x6000..=0x7FFF).contains(&addr)
}

fn check_ram(addr: u32) -> Result<(), CheatError> {
    if is_ram_addr(addr) {
        Ok(())
    } else if addr >= 0x8000 {
        Err(CheatError::AddressOutOfRange {
            addr,
            reason: "ROM address; use a Game Genie code or AAAA:VV",
        })
    } else {
        Err(CheatError::AddressOutOfRange {
            addr,
            reason: "I/O or unmapped, not RAM ($0000-$1FFF, $6000-$7FFF)",
        })
    }
}

pub(crate) fn parse(code: &str) -> Result<Cheat, CheatError> {
    if code.contains(':') {
        return parse_raw(code);
    }
    let chars: Vec<char> = code.chars().filter(|&c| c != '-').collect();
    let n = chars.len();
    if n != 6 && n != 8 {
        return Err(CheatError::BadCodeLength {
            expected: "6 or 8 letters (Game Genie), 6 hex digits (Action Replay) or AAAA:VV",
            got: n,
        });
    }
    // A 6-char string made only of hex digits that are not all Genie letters
    // is Action Replay (digits 0-9 and B-D, F are not Genie letters).
    let all_letters = chars.iter().all(|&c| letter_value(c).is_some());
    let all_hex = chars.iter().all(char::is_ascii_hexdigit);
    if n == 6 && all_hex && !all_letters {
        return parse_par(&chars);
    }
    decode_game_genie(&chars)
}

fn parse_par(chars: &[char]) -> Result<Cheat, CheatError> {
    let addr_s: String = chars.iter().take(4).collect();
    let val_s: String = chars.iter().skip(4).collect();
    let addr = hex_field("address", &addr_s, 4)?;
    let value = hex_field("value", &val_s, 2)?;
    check_ram(addr)?;
    Ok(Cheat {
        format: CodeFormat::NesActionReplay,
        effect: CheatEffect::Ram(RamWrite {
            addr,
            value: u8::try_from(value).unwrap_or(0),
            compare: None,
        }),
    })
}

fn parse_raw(code: &str) -> Result<Cheat, CheatError> {
    let (addr, compare, value) = split_raw(code, 4)?;
    let effect = if addr >= 0x8000 {
        CheatEffect::Rom(RomOverride {
            addr,
            value,
            compare,
        })
    } else {
        check_ram(addr)?;
        CheatEffect::Ram(RamWrite {
            addr,
            value,
            compare,
        })
    };
    Ok(Cheat {
        format: CodeFormat::Raw,
        effect,
    })
}

fn decode_game_genie(chars: &[char]) -> Result<Cheat, CheatError> {
    let nibbles = chars
        .iter()
        .enumerate()
        .map(|(index, &ch)| {
            letter_value(ch).ok_or(CheatError::BadChar {
                field: "Game Genie code",
                ch,
                index,
            })
        })
        .collect::<Result<Vec<u32>, _>>()?;
    let n = |i: usize| nibbles.get(i).copied().unwrap_or(0);
    let addr = 0x8000
        | ((n(3) & 7) << 12)
        | ((n(5) & 7) << 8)
        | ((n(4) & 8) << 8)
        | ((n(2) & 7) << 4)
        | ((n(1) & 8) << 4)
        | (n(4) & 7)
        | (n(3) & 8);
    let eight = nibbles.len() == 8;
    let data_bit3 = if eight { n(7) } else { n(5) } & 8;
    let value = ((n(1) & 7) << 4) | ((n(0) & 8) << 4) | (n(0) & 7) | data_bit3;
    let compare = eight.then(|| ((n(7) & 7) << 4) | ((n(6) & 8) << 4) | (n(6) & 7) | (n(5) & 8));
    Ok(Cheat {
        format: CodeFormat::NesGameGenie,
        effect: CheatEffect::Rom(RomOverride {
            addr,
            value: u8::try_from(value).unwrap_or(0),
            compare: compare.map(|c| u8::try_from(c).unwrap_or(0)),
        }),
    })
}

/// Encode as a Game Genie code: 6 letters without a compare, 8 with. Only
/// the low 15 bits of `o.addr` are encoded (bit 15 is implied by the
/// device).
#[must_use]
pub fn encode_nes_game_genie(o: RomOverride) -> String {
    let a = o.addr & 0x7FFF;
    let v = u32::from(o.value);
    let c = o.compare.map(u32::from);
    let flag = if c.is_some() { 8 } else { 0 };
    let n5_top = c.map_or(v, |c| c) & 8;
    let mut nibbles = vec![
        (v & 7) | ((v >> 4) & 8),
        ((v >> 4) & 7) | ((a >> 4) & 8),
        ((a >> 4) & 7) | flag,
        ((a >> 12) & 7) | (a & 8),
        (a & 7) | ((a >> 8) & 8),
        ((a >> 8) & 7) | n5_top,
    ];
    if let Some(c) = c {
        nibbles.push((c & 7) | ((c >> 4) & 8));
        nibbles.push(((c >> 4) & 7) | (v & 8));
    }
    nibbles
        .into_iter()
        .map(|x| {
            ALPHABET
                .get(usize::try_from(x & 15).unwrap_or(0))
                .map_or('A', |&b| char::from(b))
        })
        .collect()
}
