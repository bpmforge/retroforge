//! Recipe RAM rows to and from cheats and RetroAchievements references
//! (ticket W38-10; `docs/design/REBUILD_LAB.md` sections 8-9, D-030, D-031).
//!
//! A recipe's RAM row is an address, a width in bytes and a name
//! ([`rf_rebuild::RamMeaning`]). Players already hold the same facts in two
//! other notations, and this module moves them between all three without
//! loss:
//!
//! * **Raw RAM cheat codes** (`rf-cheats`): `AAAA:VV` on the NES,
//!   `AAAAAA:VV` on the SNES. A cheat writes one byte, so a row of width `w`
//!   is `w` codes, little-endian, and the label of each carries the row name
//!   plus ` [i/w]` so the group is recognised again. [`export_cheat_lines`] /
//!   [`import_cheat_lines`]; the text form is [`cheat_text`] /
//!   [`parse_cheat_text`] (`code # label`).
//! * **RetroAchievements memory references** in RA's address space:
//!   `0xH` 8-bit, `0x` 16-bit, `0xW` 24-bit, `0xX` 32-bit (little-endian),
//!   an optional `=value`, and a trailing `// name`. The address mapping is
//!   `rf_achievements` ([`ra_address`](rf_achievements::ra_address)
//!   out, [`translate_with`](rf_achievements::translate_with) in).
//!   [`export_ra_refs`] / [`import_ra_refs`].
//!
//! Rows carry an optional `value` (cheats and RA conditions have one, a
//! recipe row does not); it survives the round trip.
//!
//! ## Provenance (D-021, D-031)
//!
//! Rows made by an import are always [`Grade::Candidate`] with a source that
//! names where they came from ([`SOURCE_USER_CHEAT`], [`SOURCE_RA`]).
//! [`RamMeaning::is_hub_eligible`] is false for them, so they cannot be
//! submitted to or promoted on the recipe hub: a cheat list or an RA
//! reference is somebody's documentation, not an observation on the player's
//! own copy. Rows from [`crate::cheat_search`] are `observed: cheat search`.
//!
//! What does not survive, by design: a cheat's compare byte, mirrors (NES
//! `$0800-$1FFF` and SNES low-bank WRAM fold to their canonical address on
//! RA export), and RA's non-plain references (delta, prior, bit, big-endian).
//! Those are refused with a reason, never guessed.

use rf_achievements::{ra_address, translate_with, Console as RaConsole};
use rf_cheats::{CheatEffect, Console};
use rf_rebuild::{Grade, RamMeaning};

pub use rf_achievements::CartMap;

/// Source of a row made from a user's cheat list.
pub const SOURCE_USER_CHEAT: &str = "imported: user cheat";
/// Source of a row made from a pasted RetroAchievements reference.
pub const SOURCE_RA: &str = "imported: RetroAchievements";
/// Source of a row made by [`crate::cheat_search`].
pub const SOURCE_SEARCH: &str = "observed: cheat search";

/// Widest row the exchange handles (a 32-bit value).
pub const MAX_WIDTH: u8 = 4;

/// A recipe RAM row plus the value it was seen with, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RamRow {
    pub meaning: RamMeaning,
    /// Little-endian value of `width` bytes. `None` exports as 0.
    pub value: Option<u32>,
}

impl RamRow {
    /// A candidate row with the given source.
    #[must_use]
    pub fn candidate(addr: u32, width: u8, name: &str, source: &str, value: Option<u32>) -> Self {
        Self {
            meaning: RamMeaning {
                addr,
                width,
                name: name.to_string(),
                source: source.to_string(),
                grade: Grade::Candidate,
            },
            value,
        }
    }
}

/// One cheat as the app stores it: the code text and a label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheatLine {
    pub code: String,
    pub label: String,
}

/// An input that was not converted, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    /// Position in the input list (rows, lines or reference lines).
    pub index: usize,
    pub reason: String,
}

/// The result of an export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export<T> {
    pub items: Vec<T>,
    pub skipped: Vec<Skipped>,
}

/// The result of an import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub rows: Vec<RamRow>,
    pub skipped: Vec<Skipped>,
}

fn skip(index: usize, reason: impl Into<String>) -> Skipped {
    Skipped {
        index,
        reason: reason.into(),
    }
}

fn bad_row(row: &RamRow) -> Option<String> {
    let m = &row.meaning;
    if !(1..=MAX_WIDTH).contains(&m.width) {
        return Some(format!("width {} is not 1 to {MAX_WIDTH} bytes", m.width));
    }
    if m.name.chars().any(char::is_control) {
        return Some("the name has a control character".into());
    }
    if let Some(v) = row.value {
        if m.width < 4 && v >> (8 * u32::from(m.width)) != 0 {
            return Some(format!("value {v} does not fit {} byte(s)", m.width));
        }
    }
    None
}

fn le_bytes(value: u32, width: u8) -> impl Iterator<Item = u8> {
    (0..u32::from(width)).map(move |i| ((value >> (8 * i)) & 0xFF) as u8)
}

fn default_name(prefix: &str, addr: u32) -> String {
    format!("{prefix}_{addr:X}")
}

// ---------------------------------------------------------------- cheats --

fn raw_code(console: Console, addr: u32, byte: u8) -> Option<String> {
    match console {
        Console::Nes if addr <= 0xFFFF => Some(format!("{addr:04X}:{byte:02X}")),
        Console::Snes if addr <= 0xFF_FFFF => Some(format!("{addr:06X}:{byte:02X}")),
        _ => None,
    }
}

fn label_for(name: &str, i: u8, width: u8) -> String {
    if width == 1 {
        name.to_string()
    } else {
        format!("{name} [{}/{width}]", i + 1)
    }
}

/// Split `"name [i/w]"` into its parts.
fn split_group(label: &str) -> (&str, Option<(u8, u8)>) {
    let group = label.strip_suffix(']').and_then(|l| {
        let (name, tail) = l.rsplit_once(" [")?;
        let (i, w) = tail.split_once('/')?;
        Some((name, (i.parse().ok()?, w.parse().ok()?)))
    });
    match group {
        Some((name, g)) => (name, Some(g)),
        None => (label, None),
    }
}

/// Rows to raw RAM cheat codes: one [`CheatLine`] per byte, so each can be a
/// `CheatEntry`. A row that no RAM cheat can reach (a register, cartridge
/// code space) is skipped whole, never partly exported. Every code is parsed
/// back with `rf_cheats::parse` before it is returned.
#[must_use]
pub fn export_cheat_lines(console: Console, rows: &[RamRow]) -> Export<CheatLine> {
    let mut out = Export {
        items: Vec::new(),
        skipped: Vec::new(),
    };
    'rows: for (index, row) in rows.iter().enumerate() {
        if let Some(why) = bad_row(row) {
            out.skipped.push(skip(index, why));
            continue;
        }
        let m = &row.meaning;
        let mut lines = Vec::new();
        for (i, byte) in le_bytes(row.value.unwrap_or(0), m.width).enumerate() {
            let i = i as u8;
            let addr = m.addr.wrapping_add(u32::from(i));
            let Some(code) = raw_code(console, addr, byte) else {
                out.skipped
                    .push(skip(index, format!("address {addr:#X} is out of range")));
                continue 'rows;
            };
            let is_ram = matches!(
                rf_cheats::parse(console, &code),
                Ok(c) if matches!(c.effect, CheatEffect::Ram(w) if w.addr == addr && w.value == byte)
            );
            if !is_ram {
                out.skipped.push(skip(
                    index,
                    format!("{addr:#X} is not RAM a cheat can write"),
                ));
                continue 'rows;
            }
            lines.push(CheatLine {
                code,
                label: label_for(&m.name, i, m.width),
            });
        }
        out.items.extend(lines);
    }
    out
}

/// Lines as text, one `code # label` per line (`code` alone without a label).
#[must_use]
pub fn cheat_text(lines: &[CheatLine]) -> String {
    lines
        .iter()
        .map(|l| {
            if l.label.is_empty() {
                format!("{}\n", l.code)
            } else {
                format!("{} # {}\n", l.code, l.label)
            }
        })
        .collect()
}

/// Read text in the [`cheat_text`] form, or any rf-cheats list: one code per
/// line, `+` joins codes (each gets the line's label), blank lines and lines
/// starting with `#` are skipped, `#` after a code starts its label.
#[must_use]
pub fn parse_cheat_text(text: &str) -> Vec<CheatLine> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .flat_map(|l| {
            let (codes, label) = l.split_once('#').unwrap_or((l, ""));
            let label = label.trim().to_string();
            codes
                .split('+')
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(|c| CheatLine {
                    code: c.to_string(),
                    label: label.clone(),
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

struct Byte {
    index: usize,
    addr: u32,
    value: u8,
    name: String,
    group: Option<(u8, u8)>,
}

/// A user's cheat list to candidate rows (source [`SOURCE_USER_CHEAT`]).
/// Only RAM codes become rows; Game Genie ROM codes and unparsable lines are
/// skipped with a reason. Consecutive bytes whose labels say ` [i/w]` for one
/// name regroup into a row of width `w` with the little-endian value; every
/// other code is a width-1 row, named by its label or `cheat_<addr>`.
#[must_use]
pub fn import_cheat_lines(console: Console, lines: &[CheatLine]) -> Import {
    let mut skipped = Vec::new();
    let mut bytes = Vec::new();
    for (index, l) in lines.iter().enumerate() {
        match rf_cheats::parse(console, &l.code) {
            Ok(c) => match c.effect {
                CheatEffect::Ram(w) => {
                    let (name, group) = split_group(l.label.trim());
                    bytes.push(Byte {
                        index,
                        addr: w.addr,
                        value: w.value,
                        name: name.to_string(),
                        group,
                    });
                }
                CheatEffect::Rom(_) => {
                    skipped.push(skip(index, "a ROM code, not a RAM address"));
                }
            },
            Err(e) => skipped.push(skip(index, e.to_string())),
        }
    }
    let mut rows = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        // Every path below adds at least 1 to `i`, so the walk ends.
        let head = &bytes[i];
        let mut take = 1;
        if let Some((1, w)) = head.group {
            let w = usize::from(w);
            let whole = (2..=usize::from(MAX_WIDTH)).contains(&w)
                && bytes.get(i..i + w).is_some_and(|run| {
                    run.iter().enumerate().all(|(k, b)| {
                        b.group == Some((k as u8 + 1, w as u8))
                            && b.name == head.name
                            && b.addr == head.addr.wrapping_add(k as u32)
                            && b.index == head.index + k
                    })
                });
            if whole {
                take = w;
            }
        }
        let run = &bytes[i..i + take];
        let value = run
            .iter()
            .rev()
            .fold(0u32, |acc, b| (acc << 8) | u32::from(b.value));
        let name = if head.name.is_empty() {
            default_name("cheat", head.addr)
        } else {
            head.name.clone()
        };
        rows.push(RamRow::candidate(
            head.addr,
            take as u8,
            &name,
            SOURCE_USER_CHEAT,
            Some(value),
        ));
        i += take;
    }
    Import { rows, skipped }
}

// ------------------------------------------------------------------ RA ---

fn ra_console(c: Console) -> RaConsole {
    match c {
        Console::Nes => RaConsole::Nes,
        Console::Snes => RaConsole::Snes,
    }
}

fn size_prefix(width: u8) -> &'static str {
    match width {
        1 => "0xH",
        2 => "0x",
        3 => "0xW",
        _ => "0xX",
    }
}

/// Rows to RA memory references, `0xH000010=5 // name`, in RA's address
/// space. A row RA cannot address (a register, cartridge RAM without `cart`,
/// bytes that are not contiguous in RA's space) is skipped with a reason. The
/// `=value` part is written only when the row has a value.
#[must_use]
pub fn export_ra_refs(console: Console, cart: CartMap, rows: &[RamRow]) -> Export<String> {
    let mut out = Export {
        items: Vec::new(),
        skipped: Vec::new(),
    };
    for (index, row) in rows.iter().enumerate() {
        if let Some(why) = bad_row(row) {
            out.skipped.push(skip(index, why));
            continue;
        }
        let m = &row.meaning;
        let ra = ra_address(ra_console(console), m.addr, cart);
        let last = ra_address(
            ra_console(console),
            m.addr.wrapping_add(u32::from(m.width) - 1),
            cart,
        );
        let Some(ra) = ra.filter(|a| last == Some(a + u32::from(m.width) - 1)) else {
            out.skipped.push(skip(
                index,
                format!("{:#X} has no contiguous RetroAchievements address", m.addr),
            ));
            continue;
        };
        let mut s = format!("{}{ra:06X}", size_prefix(m.width));
        if let Some(v) = row.value {
            s.push_str(&format!("={v}"));
        }
        if !m.name.is_empty() {
            s.push_str(" // ");
            s.push_str(&m.name);
        }
        out.items.push(s);
    }
    out
}

fn parse_ra_ref(text: &str) -> Result<(u32, u8, Option<u32>, String), String> {
    let (body, name) = match text.split_once("//") {
        Some((b, n)) => (b.trim(), n.trim().to_string()),
        None => (text.trim(), String::new()),
    };
    let lower = body.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("0x")
        .ok_or("not a plain `0x...` memory reference")?;
    let (width, rest) = match rest.chars().next() {
        Some('h') => (1u8, &rest[1..]),
        Some('w') => (3, &rest[1..]),
        Some('x') => (4, &rest[1..]),
        Some(c) if c.is_ascii_hexdigit() => (2, rest),
        _ => return Err("unsupported RetroAchievements size".into()),
    };
    let (addr_s, value_s) = match rest.split_once('=') {
        Some((a, v)) => (a, Some(v.trim())),
        None => (rest, None),
    };
    if addr_s.is_empty() || addr_s.len() > 8 || !addr_s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("the address is not 1 to 8 hex digits".into());
    }
    let addr = u32::from_str_radix(addr_s, 16).map_err(|e| e.to_string())?;
    let value = match value_s {
        None => None,
        Some(v) => {
            let parsed = match v.strip_prefix("0x") {
                Some(h) => u32::from_str_radix(h, 16),
                None => v.parse::<u32>(),
            };
            let n = parsed.map_err(|_| format!("`{v}` is not a number"))?;
            if width < 4 && n >> (8 * u32::from(width)) != 0 {
                return Err(format!("value {n} does not fit {width} byte(s)"));
            }
            Some(n)
        }
    };
    Ok((addr, width, value, name))
}

/// Pasted RA memory references (one per line; blank lines and lines starting
/// `//` or `#` are skipped) to candidate rows (source [`SOURCE_RA`]) in bus
/// addresses via `memmap::translate_with`. An address the console cannot
/// serve, a multi-byte run that is not contiguous on the bus, or a reference
/// that is not a plain one is skipped with a reason. `index` in
/// [`Skipped`] counts the non-skipped-blank input lines from 0.
#[must_use]
pub fn import_ra_refs(console: Console, cart: CartMap, text: &str) -> Import {
    let mut out = Import {
        rows: Vec::new(),
        skipped: Vec::new(),
    };
    let lines = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//") && !l.starts_with('#'));
    for (index, line) in lines.enumerate() {
        let (ra, width, value, name) = match parse_ra_ref(line) {
            Ok(p) => p,
            Err(e) => {
                out.skipped.push(skip(index, e));
                continue;
            }
        };
        let rac = ra_console(console);
        let bus = translate_with(rac, ra, cart);
        let end = translate_with(rac, ra.wrapping_add(u32::from(width) - 1), cart);
        let Some(bus) = bus.filter(|b| end == Some(b + u32::from(width) - 1)) else {
            out.skipped.push(skip(
                index,
                format!("RA address {ra:#X} is not served by this console/cartridge"),
            ));
            continue;
        };
        let name = if name.is_empty() {
            default_name("ra", ra)
        } else {
            name
        };
        out.rows
            .push(RamRow::candidate(bus, width, &name, SOURCE_RA, value));
    }
    out
}

/// Add imported or found rows to a recipe's RAM list. A row whose address and
/// width, or whose name, is already there is left out: existing facts (and
/// their grade) are never overwritten. Returns how many were added.
pub fn add_candidates(ram: &mut Vec<RamMeaning>, rows: &[RamRow]) -> usize {
    let mut added = 0;
    for r in rows {
        let m = &r.meaning;
        let dup = ram
            .iter()
            .any(|e| (e.addr == m.addr && e.width == m.width) || e.name == m.name);
        if !dup {
            ram.push(m.clone());
            added += 1;
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic addresses only (D-021): nothing here is a real game's map.
    fn row(addr: u32, width: u8, name: &str, value: Option<u32>) -> RamRow {
        RamRow {
            meaning: RamMeaning {
                addr,
                width,
                name: name.into(),
                source: "observed: test".into(),
                grade: Grade::Observed,
            },
            value,
        }
    }

    fn key(r: &RamRow) -> (u32, u8, String, Option<u32>) {
        (
            r.meaning.addr,
            r.meaning.width,
            r.meaning.name.clone(),
            r.value,
        )
    }

    fn nes_rows() -> Vec<RamRow> {
        vec![
            row(0x0010, 1, "alpha", Some(0xAB)),
            row(0x0123, 2, "beta", Some(0x1234)),
            row(0x07FE, 1, "edge", Some(0)),
            row(0x6000, 3, "sram", Some(0x00_FF01)),
            row(0x0300, 4, "wide", Some(0xDEAD_BEEF)),
        ]
    }

    fn snes_rows() -> Vec<RamRow> {
        vec![
            row(0x7E_0010, 1, "low", Some(7)),
            row(0x7E_FFFF, 2, "bank_cross", Some(0xBEEF)),
            row(0x7F_0000, 1, "bank7f", Some(255)),
            row(0x7F_FFFF, 1, "last", Some(1)),
            row(0x7E_1234, 4, "wide", Some(0x0102_0304)),
            row(0x7E_2000, 3, "tri", Some(0x0A0B0C)),
        ]
    }

    fn assert_same(a: &[RamRow], b: &[RamRow]) {
        assert_eq!(
            a.iter().map(key).collect::<Vec<_>>(),
            b.iter().map(key).collect::<Vec<_>>()
        );
    }

    #[test]
    fn cheats_round_trip_nes_and_snes() {
        for (console, rows) in [(Console::Nes, nes_rows()), (Console::Snes, snes_rows())] {
            let ex = export_cheat_lines(console, &rows);
            assert!(ex.skipped.is_empty(), "{:?}", ex.skipped);
            assert_eq!(
                ex.items.len(),
                rows.iter().map(|r| r.meaning.width as usize).sum::<usize>()
            );
            // Through the text form as well as the struct form.
            let text = cheat_text(&ex.items);
            let lines = parse_cheat_text(&text);
            assert_eq!(lines, ex.items);
            let back = import_cheat_lines(console, &lines);
            assert!(back.skipped.is_empty());
            assert_same(&rows, &back.rows);
            for r in &back.rows {
                assert_eq!(r.meaning.grade, Grade::Candidate);
                assert_eq!(r.meaning.source, SOURCE_USER_CHEAT);
                assert!(!r.meaning.is_hub_eligible());
            }
        }
    }

    #[test]
    fn cheat_codes_are_the_rf_cheats_raw_form() {
        let ex = export_cheat_lines(Console::Snes, &[row(0x7E_0010, 2, "p", Some(0x0201))]);
        let codes: Vec<_> = ex.items.iter().map(|l| l.code.as_str()).collect();
        assert_eq!(codes, ["7E0010:01", "7E0011:02"]);
        let ex = export_cheat_lines(Console::Nes, &[row(0x0010, 1, "p", Some(5))]);
        assert_eq!(ex.items[0].code, "0010:05");
    }

    #[test]
    fn unreachable_rows_are_skipped_whole() {
        // Last byte of the row lands in a register / ROM space.
        let rows = vec![
            row(0x07FF, 2, "ok_mirror_gap", Some(1)), // 0x0800 is still RAM mirror
            row(0x1FFF, 2, "cross", Some(1)),         // 0x2000 is a PPU register
            row(0x8000, 1, "rom", Some(1)),
            row(0x0010, 5, "toowide", Some(1)),
            row(0x0010, 1, "big", Some(0x100)),
            row(0x0010, 1, "nl\nname", Some(1)),
        ];
        let ex = export_cheat_lines(Console::Nes, &rows);
        let skipped: Vec<_> = ex.skipped.iter().map(|s| s.index).collect();
        assert_eq!(skipped, [1, 2, 3, 4, 5]);
        assert_eq!(ex.items.len(), 2);
    }

    #[test]
    fn imported_cheats_without_labels_get_names_and_rom_codes_are_skipped() {
        let text = "0010:05\n# comment\n0020:01+0021:02 # pair\nAAAA-BBB\nSXIOPO\n";
        let lines = parse_cheat_text(text);
        let imp = import_cheat_lines(Console::Nes, &lines);
        let names: Vec<_> = imp.rows.iter().map(|r| r.meaning.name.as_str()).collect();
        // `pair` labels both bytes but carries no [i/w] group: two width-1 rows.
        assert_eq!(names, ["cheat_10", "pair", "pair"]);
        assert!(imp.rows.iter().all(|r| r.meaning.width == 1));
        assert_eq!(imp.skipped.len(), 2); // SXIOPO is a ROM Game Genie code; AAAA-BBB bad
    }

    #[test]
    fn broken_group_labels_do_not_regroup() {
        let lines = vec![
            CheatLine {
                code: "0010:01".into(),
                label: "x [1/2]".into(),
            },
            CheatLine {
                code: "0020:02".into(),
                label: "x [2/2]".into(),
            }, // not adjacent
            CheatLine {
                code: "0030:03".into(),
                label: "y [1/3]".into(),
            }, // run too short
        ];
        let imp = import_cheat_lines(Console::Nes, &lines);
        assert_eq!(imp.rows.len(), 3);
        assert!(imp.rows.iter().all(|r| r.meaning.width == 1));
    }

    #[test]
    fn ra_refs_round_trip_nes_and_snes() {
        let cases = [
            (Console::Nes, CartMap::None, nes_rows()),
            (Console::Snes, CartMap::None, snes_rows()),
        ];
        for (console, cart, rows) in cases {
            let ex = export_ra_refs(console, cart, &rows);
            assert!(ex.skipped.is_empty(), "{:?}", ex.skipped);
            let back = import_ra_refs(console, cart, &ex.items.join("\n"));
            assert!(back.skipped.is_empty(), "{:?}", back.skipped);
            assert_same(&rows, &back.rows);
            for r in &back.rows {
                assert_eq!(r.meaning.grade, Grade::Candidate);
                assert_eq!(r.meaning.source, SOURCE_RA);
                assert!(!r.meaning.is_hub_eligible());
            }
        }
    }

    #[test]
    fn ra_text_uses_ra_address_space() {
        let rows = [
            row(0x7E_0010, 1, "a", Some(5)),
            row(0x7F_0002, 2, "b", None),
            row(0x7E_0100, 3, "c", Some(1)),
            row(0x7E_0200, 4, "d", None),
        ];
        let ex = export_ra_refs(Console::Snes, CartMap::None, &rows);
        assert_eq!(
            ex.items,
            [
                "0xH000010=5 // a",
                "0x010002 // b",
                "0xW000100=1 // c",
                "0xX000200 // d"
            ]
        );
        // NES mirrors fold; the SNES low-bank WRAM mirror folds too.
        let nes = export_ra_refs(Console::Nes, CartMap::None, &[row(0x0810, 1, "m", None)]);
        assert_eq!(nes.items, ["0xH000010 // m"]);
    }

    #[test]
    fn snes_cartridge_ram_round_trips_with_its_layout() {
        for cart in [
            CartMap::LoRom { ram: 0x8000 * 2 },
            CartMap::HiRom { ram: 0x2000 * 4 },
            CartMap::Sa1 { bwram: 0x2_0000 },
        ] {
            let ra_cases = [0x02_0000u32, 0x02_0042, 0x02_1F00];
            let text: String = ra_cases
                .iter()
                .map(|a| format!("0xH{a:06X}=9 // c\n"))
                .collect();
            let imp = import_ra_refs(Console::Snes, cart, &text);
            assert_eq!(imp.rows.len(), 3, "{cart:?} {:?}", imp.skipped);
            let ex = export_ra_refs(Console::Snes, cart, &imp.rows);
            let again = import_ra_refs(Console::Snes, cart, &ex.items.join("\n"));
            assert_same(&imp.rows, &again.rows);
        }
        // Without a layout, cartridge RAM is refused, not guessed.
        let imp = import_ra_refs(Console::Snes, CartMap::None, "0xH020000");
        assert!(imp.rows.is_empty() && imp.skipped.len() == 1);
    }

    #[test]
    fn ra_refusals_are_explained() {
        let text = "0xH1_0000\nd0xH0010\n0xM0010\n0xH0010=300\n0xH0010=abc\n0xHzz\n0xH00000010=1\n0x0010 // ok";
        let imp = import_ra_refs(Console::Nes, CartMap::None, text);
        assert_eq!(imp.rows.len(), 2); // leading zeros and `0x0010` are fine
        assert!(imp.rows.iter().all(|r| r.meaning.addr == 0x10));
        assert_eq!(imp.skipped.len(), 6);
        // Bytes that straddle a mapping edge are not a row.
        let imp = import_ra_refs(Console::Snes, CartMap::None, "0x01FFFF\n0xH01FFFF");
        assert_eq!((imp.rows.len(), imp.skipped.len()), (1, 1));
        let ex = export_ra_refs(Console::Nes, CartMap::None, &[row(0x1FFF, 2, "x", None)]);
        assert_eq!(ex.skipped.len(), 1);
    }

    #[test]
    fn imported_rows_never_pass_the_hub_check() {
        let c = import_cheat_lines(
            Console::Nes,
            &[CheatLine {
                code: "0010:01".into(),
                label: "observed: x".into(),
            }],
        );
        let r = import_ra_refs(Console::Nes, CartMap::None, "0xH0010 // observed: fine");
        for row in c.rows.iter().chain(&r.rows) {
            assert_eq!(row.meaning.grade, Grade::Candidate);
            assert!(!row.meaning.is_hub_eligible());
        }
    }

    #[test]
    fn add_candidates_never_overwrites() {
        let mut ram = vec![row(0x10, 1, "lives", None).meaning];
        let new = [
            row(0x10, 1, "other_name", None),
            row(0x20, 1, "lives", None),
            row(0x30, 1, "fresh", None),
        ];
        assert_eq!(add_candidates(&mut ram, &new), 1);
        assert_eq!(ram.len(), 2);
        assert_eq!(ram[0].grade, Grade::Observed);
    }
}
