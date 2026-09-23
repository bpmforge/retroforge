//! ST010 (SETA D96050CW-012, an extended NEC uPD77C25) HLE, fullsnes "SNES
//! Cart DSP-n/ST010/ST011 (pre-programmed NEC uPD77C25 CPU)" (ticket
//! W19-04). One retail title: F1 Race of Champions / Exhaust Heat II
//! (1993, SETA Corp.).
//!
//! ## What the chapter actually documents (verified, not assumed)
//!
//! fullsnes's "BIOS Functions" subsection gives an "ST010 Commands" table
//! (lines 9265-9284 of the vendored `fullsnes.txt` this ticket reads from)
//! that promises: "See individual commands for input and output parameter
//! addresses." **That promise is never fulfilled in this chapter.** A
//! full-file grep for `ST010`, `Driver Placements`, `0010h]` and `Sort
//! Driver` turns up nothing between the command table (line ~9284, "the
//! only feature that is <really> used is the battery-backed on-chip RAM")
//! and the next unrelated hit at line 22770 (an oscillator part number).
//! `ST010 Commands` is immediately followed by `ST011 Commands` and then
//! the unrelated `ST018` chapter — there is no "individual commands"
//! subsection for this chip anywhere in the source text. So the only
//! command whose *effect* fullsnes states is `00h` ("Set RAM[0010h]=0000h");
//! every other command (`01h`-`08h`) is a bare name (or, for `01h`/`04h`,
//! explicitly "Unknown Command"), with no documented input, output,
//! address, width or sign rule. Per this ticket's own acceptance clause
//! ("whose inputs/outputs fullsnes specifies... clear busy for the rest"),
//! this module implements exactly one command's math and treats the other
//! eight as busy-clear-only, counted for visibility — **not** a partial
//! implementation of `06h` "Multiply", which would mean guessing RAM
//! addresses for the one feature (the battery RAM) that real software
//! actually depends on. `docs/design/EMULATION_CORES.md` §3.8 makes the
//! identical call for the CX4's 26 named-but-unaddressed functions.
//!
//! ## Memory model
//!
//! fullsnes ("NEC uPD77C25 - Registers & Flags & Overview", the per-board
//! table): "For ST010/ST011, the RAM is contained in the ST01n chip, and
//! is sized 2Kx16bit, whereas the SNES accesses it as 4Kx8bit (even
//! addresses accessing the LSB, odd ones the MSB of the 16bit words)."
//! [`ST010_RAM_LEN`] is that 4096-byte (2048-word) view — a single flat
//! buffer, not split DR/SR ports like DSP-1: this chip's command protocol
//! lives *inside* the RAM, not behind a separate register pair.
//!
//! "ST010 Commands": "Commands are executed on the ST-0010 by writing the
//! command to 0x0020 and setting bit7 of 0x0021. Bit7 of 0x0021 will stay
//! set until the Command has completed, at which time output data will be
//! available." Byte `$0020`/`$0021` is word index `$0010` of the RAM —
//! the SAME word `RAM[0010h]` command `00h` zeroes (see below), which this
//! module treats as one coherent fact rather than two coincidentally equal
//! numbers: the "command register" is not separate hardware, it is a
//! documented *convention* for using one particular RAM word.
//!
//! Busy is modelled as synchronous: a write to `$0021` with bit 7 set
//! dispatches the command immediately and the bit is stored already
//! cleared, the same call `docs/design/EMULATION_CORES.md` §3.8 makes for
//! the CX4's `$7F5E` bit 6 ("set, then immediately cleared within the same
//! write... rather than leaving it stuck set, which would hang any
//! title's poll loop") — this project has no real completion event to
//! model either chip's timing against, so leaving busy stuck set is a
//! worse guess than clearing it, not a more honest one.
//!
//! The generic per-board summary table earlier in the same fullsnes
//! chapter also lists a `60-6x:0000` (DR) / `60-6x:0001` (SR) pair for
//! this board — a *different*, smaller window than the RAM's own
//! `68-6F:0000-0FFF`. Since the chapter's own concrete protocol text
//! ("ST010 Commands") routes command/busy through the RAM instead, this
//! module maps that separate DR/SR pair too (so it does not silently
//! become open bus) but treats it as inert: reads return a fixed value,
//! writes are counted and dropped. No documented behaviour distinguishes
//! it from the RAM-hosted protocol above, and no command is reachable
//! through it.
//!
//! ## `RAM[0010h]`: byte or word index?
//!
//! Command `00h`'s "Set RAM[0010h]=0000h" is read here as a **byte**
//! offset (zeroing bytes `$0010`-`$0011`), matching every other RAM
//! address this chapter states in byte terms (`0x0020`, `0x0021`,
//! `0000h-0FFFh`). The alternative reading — `RAM[0010h]` as the 16-bit
//! *word* at index `$10`, i.e. bytes `$0020`-`$0021`, which would make
//! `00h` self-clearing on the very word it was issued through — is
//! equally plausible from the prose alone and would not affect rendering
//! either way (nothing in the one census title this ticket covers
//! observably depends on which byte range clears). The byte-offset
//! reading is chosen for consistency with the chapter's own units
//! elsewhere; [`tests::command_00_zeroes_bytes_0010_0011_not_the_command_word`]
//! pins the choice down explicitly so it stays visible rather than buried.
//!
//! ## Save state / battery
//!
//! The header reports `battery = true` for chipset `$F6` (fullsnes: "plus
//! battery; for the on-chip RAM"). This build has no separate `.sav`
//! persistence path for any coprocessor's battery-backed memory (SRAM
//! included) — everything rides in the ordinary save-state
//! (`StateRegion::Cart`), the same precedent OBC1/CX4/S-DD1 already set.
//! [`St010::save`]/[`St010::load`] persist the whole RAM blob; the
//! diagnostic counters below do not round-trip, the same "diagnostic, not
//! machine state" contract `Obc1Regs::unknown_reg_other_writes` and
//! `Dsp1::unknown_opcode_hist` already use.

use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};

/// The on-chip RAM's SNES-visible size: 2048 x 16-bit words, addressed as
/// 4096 x 8-bit bytes (see the module doc's memory-model section).
pub const ST010_RAM_LEN: usize = 4096;

/// Byte offset of the command register — the LSB of word `$0010`
/// (fullsnes "ST010 Commands": "writing the command to 0x0020").
const CMD_OFFSET: usize = 0x0020;
/// Byte offset of the busy/status byte — the MSB of word `$0010`
/// (fullsnes: "setting bit7 of 0x0021").
const BUSY_OFFSET: usize = 0x0021;
/// Busy flag, bit 7 of [`BUSY_OFFSET`].
const BUSY_BIT: u8 = 0x80;
/// Byte offset command `00h` zeroes ("Set RAM[0010h]=0000h"), read as a
/// byte index — see the module doc's "byte or word index?" section.
const CMD_00_TARGET: usize = 0x0010;

/// The ST010 HLE: its battery-backed RAM plus diagnostic counters. See the
/// module doc for the command protocol and what is/isn't documented.
pub struct St010 {
    /// The full 4096-byte RAM, including the command/busy word at
    /// `$0020`/`$0021` — see the module doc's memory-model section.
    pub ram: Vec<u8>,
    /// Diagnostic: how many times each of the nine distinct command
    /// values (`00h`..`08h`, after folding the documented `09h..0Fh` and
    /// `10h..FFh` mirrors — fullsnes "ST010 Commands") has been issued.
    /// Not part of save state — see the module doc.
    pub command_counts: [u32; 9],
    /// Diagnostic: accesses to the inert `$60-$67:0000/0001` DR/SR pair
    /// (module doc). Not part of save state.
    pub inert_register_accesses: u32,
}

impl Default for St010 {
    fn default() -> Self {
        Self::new()
    }
}

impl St010 {
    #[must_use]
    pub fn new() -> Self {
        Self {
            ram: vec![0u8; ST010_RAM_LEN],
            command_counts: [0; 9],
            inert_register_accesses: 0,
        }
    }

    /// A plain read of the RAM window, offset already reduced into
    /// `0..ST010_RAM_LEN` by `crate::mapping::st010_target`. No side
    /// effect — reading the command/busy byte does not perturb it
    /// (fullsnes states no read-side behaviour for either).
    #[must_use]
    pub fn read(&self, offset: usize) -> u8 {
        self.ram[offset]
    }

    /// A write to the RAM window. A write to the busy byte
    /// ([`BUSY_OFFSET`]) that sets bit 7 dispatches the command already
    /// sitting in [`CMD_OFFSET`] synchronously (module doc's "Busy is
    /// modelled as synchronous" section) and stores the byte with bit 7
    /// already cleared; every other write (including a write to the busy
    /// byte that leaves bit 7 clear) is a plain memory store.
    pub fn write(&mut self, offset: usize, value: u8) {
        if offset == BUSY_OFFSET && value & BUSY_BIT != 0 {
            let cmd = self.ram[CMD_OFFSET];
            self.ram[offset] = value & !BUSY_BIT;
            self.execute(cmd);
        } else {
            self.ram[offset] = value;
        }
    }

    /// Run one command. `raw` is the byte written to [`CMD_OFFSET]` at the
    /// moment busy was set.
    fn execute(&mut self, raw: u8) {
        // fullsnes "ST010 Commands": "09h..0Fh Mirrors of 01h..07h" and
        // "10h..FFh Mirrors of 00h..0Fh".
        let folded = raw % 0x10;
        let cmd = if folded >= 0x09 { folded - 8 } else { folded };
        self.command_counts[usize::from(cmd)] += 1;
        if cmd == 0x00 {
            // "00h Set RAM[0010h]=0000h" — the only command whose effect
            // fullsnes states outright (module doc). Note this zeroes
            // BYTES 0x0010-0x0011, a different pair from the command word
            // itself (0x0020-0x0021) under the byte-offset reading this
            // module takes — see the module doc's "byte or word index?"
            // section.
            self.ram[CMD_00_TARGET] = 0;
            self.ram[CMD_00_TARGET + 1] = 0;
        }
        // cmd 1..=8: fullsnes names 02h/03h/05h/06h/07h/08h and marks
        // 01h/04h "Unknown Command", but documents no input/output
        // address for any of the eight (module doc's opening section) —
        // the busy-clear this fn's caller already performed, plus the
        // counter above, are this build's entire documented-honest
        // effect. Implementing a guessed "Multiply" for 06h here would
        // write results into invented RAM offsets inside the very buffer
        // fullsnes calls the chip's one real feature — worse than doing
        // nothing.
    }

    pub(crate) fn save(&self, o: &mut StateOut) -> Result<(), StateError> {
        o.blob(&self.ram)
    }

    pub(crate) fn load(&mut self, i: &mut StateIn) -> Result<(), StateError> {
        i.blob_into(&mut self.ram, "ST010 RAM")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_ram_is_all_zero_and_busy_clear() {
        let chip = St010::new();
        assert!(chip.ram.iter().all(|&b| b == 0));
        assert_eq!(chip.read(BUSY_OFFSET) & BUSY_BIT, 0);
    }

    #[test]
    fn plain_ram_writes_outside_the_busy_byte_never_trigger_a_command() {
        let mut chip = St010::new();
        chip.write(CMD_OFFSET, 0x06); // stage a command byte
        chip.write(0x0000, 0xFF); // unrelated byte
        chip.write(BUSY_OFFSET, 0x00); // busy write with bit 7 CLEAR
        assert_eq!(chip.command_counts, [0; 9]);
        assert_eq!(chip.read(0x0000), 0xFF);
        assert_eq!(chip.read(BUSY_OFFSET), 0x00);
    }

    #[test]
    fn setting_busy_bit_dispatches_the_staged_command_and_clears_busy_synchronously() {
        let mut chip = St010::new();
        chip.write(CMD_OFFSET, 0x02); // "Sort Driver Placements"
        chip.write(BUSY_OFFSET, 0x80);
        assert_eq!(
            chip.read(BUSY_OFFSET) & BUSY_BIT,
            0,
            "busy clears within the same write"
        );
        assert_eq!(chip.command_counts[2], 1);
    }

    #[test]
    fn command_00_zeroes_bytes_0010_0011_not_the_command_word() {
        let mut chip = St010::new();
        chip.ram[0x0010] = 0xAA;
        chip.ram[0x0011] = 0xBB;
        chip.write(CMD_OFFSET, 0x00);
        chip.write(BUSY_OFFSET, 0x80);
        assert_eq!(chip.ram[0x0010], 0x00);
        assert_eq!(chip.ram[0x0011], 0x00);
        // The command word itself (0x0020/0x0021) is left at "busy
        // cleared, command byte unchanged" — command 00h's documented
        // target is a DIFFERENT pair of bytes under this module's
        // byte-offset reading (module doc).
        assert_eq!(chip.ram[CMD_OFFSET], 0x00);
        assert_eq!(chip.ram[BUSY_OFFSET] & BUSY_BIT, 0);
    }

    #[test]
    fn commands_01_and_04_are_named_unknown_and_only_clear_busy() {
        let mut chip = St010::new();
        for cmd in [0x01u8, 0x04] {
            chip.ram.fill(0x11); // poison the whole buffer
            chip.write(CMD_OFFSET, cmd);
            chip.write(BUSY_OFFSET, 0x80);
            assert_eq!(chip.read(BUSY_OFFSET) & BUSY_BIT, 0);
            // No RAM byte other than the command/busy word this write
            // itself touched (poisoned 0x11 everywhere else) changes.
            for (i, &b) in chip.ram.iter().enumerate() {
                if i == CMD_OFFSET || i == BUSY_OFFSET {
                    continue;
                }
                assert_eq!(b, 0x11, "byte {i:#06x} was touched by cmd {cmd:#04x}");
            }
        }
    }

    #[test]
    fn named_but_undocumented_commands_02_03_05_06_07_08_only_clear_busy() {
        for cmd in [0x02u8, 0x03, 0x05, 0x06, 0x07, 0x08] {
            let mut chip = St010::new();
            chip.ram.fill(0x22);
            chip.write(CMD_OFFSET, cmd);
            chip.write(BUSY_OFFSET, 0x80);
            assert_eq!(chip.read(BUSY_OFFSET) & BUSY_BIT, 0);
            for (i, &b) in chip.ram.iter().enumerate() {
                if i == CMD_OFFSET || i == BUSY_OFFSET {
                    continue;
                }
                assert_eq!(b, 0x22, "byte {i:#06x} was touched by cmd {cmd:#04x}");
            }
        }
    }

    #[test]
    fn commands_09_through_0f_mirror_01_through_07() {
        let mut chip = St010::new();
        for (mirrored, canonical) in (0x09u8..=0x0F).zip(0x01u8..=0x07) {
            chip.write(CMD_OFFSET, mirrored);
            chip.write(BUSY_OFFSET, 0x80);
            assert_eq!(
                chip.command_counts[usize::from(canonical)],
                1,
                "cmd {mirrored:#04x} should count as {canonical:#04x}"
            );
        }
    }

    #[test]
    fn commands_10_through_ff_mirror_00_through_0f() {
        let mut chip = St010::new();
        chip.write(CMD_OFFSET, 0x36); // 0x36 % 0x10 == 0x06
        chip.write(BUSY_OFFSET, 0x80);
        assert_eq!(chip.command_counts[6], 1);
    }

    #[test]
    fn save_load_round_trips_the_ram_but_not_diagnostic_counters() {
        let mut chip = St010::new();
        chip.ram[0x0500] = 0x42;
        chip.write(CMD_OFFSET, 0x06);
        chip.write(BUSY_OFFSET, 0x80);
        assert_eq!(chip.command_counts[6], 1);

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

        let mut stream = MemStream {
            buf: Vec::new(),
            at: 0,
        };
        chip.save(&mut StateOut::new(&mut stream)).expect("save");
        let mut restored = St010::new();
        restored.load(&mut StateIn::new(&mut stream)).expect("load");

        assert_eq!(restored.ram, chip.ram);
        assert_eq!(restored.ram[0x0500], 0x42);
        // Diagnostic counters are not part of the payload — a freshly
        // constructed chip's zeroed counters survive the load untouched.
        assert_eq!(restored.command_counts, [0; 9]);
    }
}
