//! Cheats in the app (ticket W41-03; `docs/design/CHEATS_ACHIEVEMENTS.md`
//! sections 2-3).
//!
//! The player's stored codes ([`crate::game_settings::CheatEntry`]) become a
//! [`CheatSet`]: ROM-read overrides the core answers CPU reads with (Game
//! Genie style) and RAM writes the core thread applies at the start of every
//! frame. Cheats are mods (NON_GOALS 13): off by default per game, listed in
//! the `PROF` save-state chunk by [`mod_id`], and every RAM write that
//! changes a byte goes in a [`CheatLedger`] the way a hook script's does.
//!
//! The set is plain data, built on the UI thread and handed to the core
//! thread whole (`CoreCommand::SetCheats`), so nothing here touches core
//! state except [`CheatSet::apply_ram`], which the core thread calls.

use rf_cheats::{CheatEffect, Console};
use rf_core_api::{RamWrite, RomOverride, MAX_ROM_OVERRIDES};
use rf_plugin_sdk::LedgerEntry;

use crate::game_settings::CheatEntry;
use crate::stepper::EmuStepper;

/// Prefix of a cheat's id in the `PROF` mods list and of its ledger name.
pub const LEDGER_PREFIX: &str = "cheat:";

/// Most ledger entries kept (a cheat that fights the game every frame would
/// otherwise grow without bound). Past it the writes still happen and are
/// counted in [`CheatLedger::dropped`].
pub const MAX_LEDGER: usize = 4096;

/// The console a UI label ("NES" / "SNES") names.
#[must_use]
pub fn console_of(label: &str) -> Option<Console> {
    match label {
        "NES" => Some(Console::Nes),
        "SNES" => Some(Console::Snes),
        _ => None,
    }
}

/// A code as compared and recorded: upper case, no whitespace.
#[must_use]
pub fn normalise(code: &str) -> String {
    code.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_uppercase)
        .collect()
}

/// The id a cheat has in the `PROF` mods list.
#[must_use]
pub fn mod_id(code: &str) -> String {
    format!("{LEDGER_PREFIX}{}", normalise(code))
}

/// Whether a RAM cheat may write `addr`: the console's work RAM (and, on the
/// NES, cartridge RAM at `$6000-$7FFF`). A "RAM" code aimed at a register or
/// the cartridge's code space is refused rather than poked.
#[must_use]
pub fn ram_writable(console: Console, addr: u32) -> bool {
    match console {
        Console::Nes => addr < 0x2000 || (0x6000..0x8000).contains(&addr),
        Console::Snes => addr < 0x2000 || (0x7E_0000..=0x7F_FFFF).contains(&addr),
    }
}

/// Most codes stored per game (a sanity bound; at most 16 of them can be ROM codes in force).
pub const MAX_ENTRIES: usize = 64;

/// What the player sees for a code's notation.
#[must_use]
pub fn format_name(code: &str, console: Console) -> &'static str {
    use rf_cheats::CodeFormat;
    match rf_cheats::parse(console, code).map(|c| c.format) {
        Ok(CodeFormat::NesGameGenie | CodeFormat::SnesGameGenie) => "Game Genie",
        Ok(CodeFormat::NesActionReplay | CodeFormat::SnesActionReplay) => "Action Replay",
        Ok(CodeFormat::Raw) => "Raw",
        Err(_) => "Not valid for this console",
    }
}

/// The outcome of pasting text into the Cheats box.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pasted {
    /// Valid, new codes, stored switched OFF.
    pub added: Vec<CheatEntry>,
    /// One line per rejected code: where it was and why.
    pub errors: Vec<String>,
}

/// Parse pasted text (`rf_cheats::parse_list`'s rules: one code per line, `+` joins codes,
/// blank lines and `#` comments skipped). Only valid codes the game does not already have are
/// added, always switched off: the player turns each on.
#[must_use]
pub fn paste(console: Console, existing: &[CheatEntry], text: &str) -> Pasted {
    // The parts in the order `parse_list` yields its results (same filter, same split).
    let parts: Vec<(usize, &str)> = text
        .lines()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with('#')
        })
        .flat_map(|(i, l)| l.split('+').map(move |part| (i + 1, part.trim())))
        .collect();
    let results = rf_cheats::parse_list(console, text);
    let mut out = Pasted::default();
    for ((line, part), result) in parts.into_iter().zip(results) {
        if let Err(e) = result {
            out.errors.push(format!("{e}: {part}"));
            continue;
        }
        let id = normalise(part);
        let known = existing
            .iter()
            .chain(&out.added)
            .any(|e| normalise(&e.code) == id);
        if known {
            out.errors
                .push(format!("line {line}: already in the list: {part}"));
        } else if existing.len() + out.added.len() >= MAX_ENTRIES {
            out.errors
                .push(format!("line {line}: at most {MAX_ENTRIES} codes per game"));
        } else {
            out.added.push(CheatEntry {
                code: part.to_string(),
                label: String::new(),
                enabled: false,
            });
        }
    }
    out
}

/// One RAM write and the cheat it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RamCheat {
    /// Position of this cheat's id in [`CheatSet::ram_ids`].
    pub id_index: usize,
    pub write: RamWrite,
}

/// Everything the core thread needs to apply the player's enabled cheats.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheatSet {
    /// Answered by the core on CPU ROM reads.
    pub rom: Vec<RomOverride>,
    /// Written at the start of every frame.
    pub ram: Vec<RamCheat>,
    /// `cheat:<code>` of each RAM cheat, indexed by [`RamCheat::id_index`].
    pub ram_ids: Vec<String>,
    /// The `PROF` mod ids of every cheat in force, sorted and unique.
    pub ids: Vec<String>,
    /// Enabled entries that were not applied, each with the reason.
    pub skipped: Vec<String>,
}

impl CheatSet {
    /// Parse the enabled entries for `console`.
    #[must_use]
    pub fn build(console: Console, entries: &[CheatEntry]) -> Self {
        let mut set = Self::default();
        for entry in entries.iter().filter(|e| e.enabled) {
            let cheat = match rf_cheats::parse(console, &entry.code) {
                Ok(c) => c,
                Err(e) => {
                    set.skipped.push(format!("{}: {e}", entry.code.trim()));
                    continue;
                }
            };
            let id = mod_id(&entry.code);
            match cheat.effect {
                CheatEffect::Rom(o) => {
                    if set.rom.len() >= MAX_ROM_OVERRIDES {
                        set.skipped.push(format!(
                            "{}: at most {MAX_ROM_OVERRIDES} ROM codes at once",
                            entry.code.trim()
                        ));
                        continue;
                    }
                    set.rom.push(o);
                }
                CheatEffect::Ram(w) => {
                    if !ram_writable(console, w.addr) {
                        set.skipped.push(format!(
                            "{}: ${:X} is not work RAM",
                            entry.code.trim(),
                            w.addr
                        ));
                        continue;
                    }
                    set.ram_ids.push(id.clone());
                    set.ram.push(RamCheat {
                        id_index: set.ram_ids.len() - 1,
                        write: w,
                    });
                }
            }
            set.ids.push(id);
        }
        set.ids.sort();
        set.ids.dedup();
        set
    }

    /// No cheat in force at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Whether the code the CPU runs differs from the cartridge's.
    #[must_use]
    pub fn rom_active(&self) -> bool {
        !self.rom.is_empty()
    }

    /// Apply the RAM writes to `stepper` through the same out-of-band door the
    /// memory editor uses ([`EmuStepper::poke_ram`]). A write with a compare
    /// byte happens only while the current byte equals it. Writes that change
    /// a byte go in `ledger`. Returns whether any byte changed.
    pub fn apply_ram(
        &self,
        stepper: &mut EmuStepper,
        mut ledger: Option<&mut CheatLedger>,
    ) -> bool {
        let mut changed = false;
        for c in &self.ram {
            let w = c.write;
            let current = stepper.peek_bus(w.addr);
            if current == w.value || w.compare.is_some_and(|want| want != current) {
                continue;
            }
            stepper.poke_ram(w.addr, w.value);
            changed = true;
            if let Some(l) = ledger.as_deref_mut() {
                let name = self.ram_ids.get(c.id_index).map_or("", String::as_str);
                l.record(stepper.frame_count(), w.addr, w.value, name);
            }
        }
        changed
    }
}

/// The cheat writes made this session, for the `PROF` chunk.
#[derive(Debug, Clone, Default)]
pub struct CheatLedger {
    entries: Vec<LedgerEntry>,
    /// Writes made after [`MAX_LEDGER`] was reached.
    pub dropped: u64,
}

impl CheatLedger {
    fn record(&mut self, frame: u64, addr: u32, value: u8, plugin: &str) {
        if self.entries.len() >= MAX_LEDGER {
            self.dropped += 1;
            return;
        }
        self.entries.push(LedgerEntry {
            frame,
            addr,
            value,
            plugin: plugin.to_string(),
        });
    }

    #[must_use]
    pub fn entries(&self) -> &[LedgerEntry] {
        &self.entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(code: &str, enabled: bool) -> CheatEntry {
        CheatEntry {
            code: code.into(),
            label: String::new(),
            enabled,
        }
    }

    #[test]
    fn only_enabled_valid_codes_are_applied_and_named() {
        let set = CheatSet::build(
            Console::Nes,
            &[
                entry("SXIOPO", true),
                entry("0075:09", true),
                entry("0076:01", false),
                entry("hello!", true),
                entry("0000 : 05", true),
            ],
        );
        assert_eq!(set.rom.len(), 1);
        assert_eq!(set.ram.len(), 1);
        // "hello!" and the code with spaces inside it are not codes.
        assert_eq!(set.skipped.len(), 2, "{:?}", set.skipped);
        assert_eq!(set.ids, vec!["cheat:0075:09", "cheat:SXIOPO"]);
        assert!(set.rom_active());
        assert!(CheatSet::build(Console::Nes, &[entry("SXIOPO", false)]).is_empty());
    }

    #[test]
    fn a_ram_code_aimed_at_a_register_is_refused() {
        let set = CheatSet::build(Console::Nes, &[entry("2001:00", true)]);
        assert!(set.is_empty());
        assert_eq!(set.skipped.len(), 1);
        assert!(ram_writable(Console::Snes, 0x7E_0010));
        assert!(!ram_writable(Console::Snes, 0x21_0000));
    }

    #[test]
    fn at_most_sixteen_rom_codes() {
        let entries: Vec<_> = (0..=MAX_ROM_OVERRIDES)
            .map(|i| entry(&format!("{:04X}?00:11", 0x8000 + i), true))
            .collect();
        let set = CheatSet::build(Console::Nes, &entries);
        assert_eq!(set.rom.len(), MAX_ROM_OVERRIDES);
        assert_eq!(set.skipped.len(), 1);
    }

    #[test]
    fn pasting_keeps_the_valid_codes_off_and_explains_the_rest() {
        let have = [entry("0075:09", true)];
        let p = paste(
            Console::Nes,
            &have,
            "# lives\nSXIOPO\n0075:09\nZZZ\n0010:01 + 0011:02\n",
        );
        let codes: Vec<_> = p.added.iter().map(|e| e.code.as_str()).collect();
        assert_eq!(codes, ["SXIOPO", "0010:01", "0011:02"]);
        assert!(p.added.iter().all(|e| !e.enabled), "nothing starts on");
        assert_eq!(p.errors.len(), 2, "{:?}", p.errors);
        assert!(p.errors[0].contains("already"), "{:?}", p.errors);
        assert!(p.errors[1].starts_with("line 4: "), "{:?}", p.errors);
    }

    #[test]
    fn ids_ignore_case_and_spacing() {
        assert_eq!(mod_id(" sxiopo "), mod_id("SXIOPO"));
        assert_eq!(mod_id("00 75:09"), "cheat:0075:09");
    }
}
