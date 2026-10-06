//! The IPL boot handshake, high-level-emulated (ticket W6-04b;
//! `docs/design/EMULATION_CORES.md` §3.4).
//!
//! ## This is what the W6-04a ruling chose
//!
//! Option (a) was "HLE the handshake: implement the boot protocol's
//! OBSERVABLE behaviour without the ROM's bytes". This module is that.
//! It is Rust, not SPC700 code, and it is deliberately explicit about the
//! consequence: **while the boot handshake is running, the SPC700 core
//! does not execute.** The uploaded program runs on the real core the
//! moment the handshake hands control over.
//!
//! ## The protocol
//!
//! Documented behaviour, reimplemented — not Nintendo's bytes.
//!
//! 1. **Ready.** The APU puts `$AA` in port 0 and `$BB` in port 1 and
//!    waits. A CPU that never sees this pair concludes there is no APU,
//!    which is why it is published before anything else.
//! 2. **Start.** The CPU puts a non-zero byte in port 1, the destination
//!    address in ports 2/3, and `$CC` in port 0. The APU echoes port 0
//!    back, which is the CPU's signal that it was heard.
//! 3. **Transfer.** For each byte the CPU writes the data to port 1 and
//!    an incrementing counter to port 0, **starting from 0**; the APU
//!    stores the byte and echoes the counter. The counter is how both
//!    sides stay in step without a clock — it is an acknowledgement, not
//!    an index.
//! 4. **Next block or run.** The CPU writes port 1 = 0 to finish (or
//!    non-zero to start another block), a new address in ports 2/3, and
//!    a counter advanced by **two or more** in port 0. The skip is what
//!    distinguishes "new block" from "next byte", and is the single
//!    easiest part of this protocol to get wrong.
//! 5. **Go.** The APU jumps to the last address supplied.
//!
//! ## WHEN each port is read is part of the protocol (ticket W14-48)
//!
//! Until 2026-10-06 this module decided each step from one snapshot of
//! all four ports, taken the instant port 0 changed. The real IPL is a
//! polling program: it reads port 0, *then* — some cycles later, in a
//! separate instruction — port 1, and later still ports 2/3. A CPU that
//! writes the counter BEFORE the data is relying on that gap, and real
//! uploaders do. Traced on Urban Strike (USA):
//!
//! ```text
//! IPLW t=14663 p0=00   (counter 0)
//! IPLW t=14665 p1=20   (its data byte, two SPC cycles later)
//! ```
//!
//! The snapshot model stored port 1's previous contents — the `$01`
//! "kind" byte — at the block's first address and every later byte one
//! slot late. `$01` is `TCALL 0`, which is how W14-48 came to believe the
//! driver opens by calling into the boot ROM (and W14-49 to build a trap
//! for it). The real first byte is `$20`, `CLRP`.
//!
//! So the HLE now walks the documented listing **instruction by
//! instruction**, each one occupying its listed cycle count and reading
//! or writing the ports when it completes. That is still not Nintendo's
//! bytes — there are no opcodes here, only the documented effect and
//! cost of each step — but it is the listing's timing, so every port is
//! sampled where the real ROM samples it. Source: fullsnes "SNES APU
//! Main CPU Communication Port" → "Boot ROM Disassembly" (instruction
//! sequence and branch structure) with each cost from this crate's
//! vector-verified `spc700::timing::CYCLES` (2 for a branch not taken, 4
//! taken).
//!
//! The same walk also fixes three smaller deviations the snapshot model
//! carried, each read straight off the listing:
//!
//! * **A block's first byte is counter 0.** After echoing `$CC` (or a
//!   block-ending skip) the ROM spins on `MOV Y,$F4 / BNE` until port 0
//!   reads zero. The snapshot model accepted any new value there, and a
//!   stale `$CC` one cycle after its own echo was misread as "block
//!   ended".
//! * **"Ahead" is signed.** A mismatch only ends a block when port 0 is
//!   1-128 counts AHEAD of the expected counter (`CMP Y,$F4` then `BPL`
//!   back to polling, checked twice); a value behind it is ignored.
//! * **The ROM leaves state behind.** The destination pointer lives at
//!   ARAM `$00`/`$01` (written by `MOVW $00,YA`, bumped by `INC $01`
//!   every 256 bytes), and the jump hands over with `A` = `X` = `Y` = 0
//!   (the zero "kind" byte, moved through `MOV A,Y / MOV X,A`), `SP` =
//!   `$EF` and `N`/`Z`/`C` as the last compare and move left them. A
//!   driver may read any of these.

use super::spc700::flags;

/// Where the handshake has got to — a coarse summary of the listing
/// position, for diagnostics and save states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootState {
    /// Ports 0/1 hold `$AA`/`$BB`; waiting for `$CC` in port 0.
    Ready,
    /// A block is in progress; the value is the counter expected next.
    Transferring(u8),
    /// Between blocks, or before the first byte: the value is the last
    /// echo, and the ROM is waiting for port 0 to read zero.
    AwaitingBlock(u8),
    /// The handshake handed control to the SPC700 core.
    Running,
    /// Re-entered at `$FFC0`: the boot ROM is clearing zero page and has
    /// not yet published `$AA`/`$BB` (ticket W14-10). The value is the
    /// SPC cycles left before it does.
    Initialising(u16),
}

/// SPC cycles the boot ROM spends before it publishes `$AA`/`$BB`.
///
/// fullsnes, "SNES APU Boot ROM": the ROM begins `MOV X,#$EF` / `MOV SP,X`
/// / `MOV A,#$00`, then a loop of `MOV (X),A` / `DEC X` / `BNE` that clears
/// `$01-$EF`, and only then `MOV $F4,#$AA` / `MOV $F5,#$BB`. With the
/// SPC700 cycle table (2 + 2 + 2, then 239 laps of 4 + 2 + 4 and a last
/// lap of 4 + 2 + 2) that is 2404 cycles, about 2.3 ms.
///
/// **This delay is load-bearing, not cosmetic.** A game that reboots the
/// APU by commanding a jump to `$FFC0` is still spinning on `CMP $2140`
/// for the echo of that jump's counter. Republishing `$AA` on the very
/// next SPC instruction overwrote the echo before the 65816 could read
/// it — Super Bonk waited for `$E3` for ever while the port held `$AA`.
pub const IPL_INIT_CYCLES: u16 = 2404;

/// SPC cycles from the compare that sees a block-ending counter to the
/// uploaded program's first instruction (the `$FFDA` compare included).
///
/// fullsnes "Boot ROM Disassembly", costs from `spc700::timing::CYCLES`:
///
/// ```text
/// $FFDA cmp Y,$F4     3   (the compare that observes the mismatch)
/// $FFDC jnz $FFE9     4   (taken)
/// $FFE9 jns $FFDA     2   (not taken: port 0 is ahead)
/// $FFEB cmp Y,$F4     3   (re-checks the same counter)
/// $FFED jns $FFDA     2   (not taken -> falls into `main`)
/// $FFEF movw YA,$F6   5   (load the destination/entry address)
/// $FFF1 movw $00,YA   5   (stash it at zero page for the JMP operand)
/// $FFF3 movw YA,$F4   5   (reload the kick/cmd pair)
/// $FFF5 mov $F4,A     4   (echo the kick byte — the CPU's spin ends here)
/// $FFF7 mov A,Y       2   (cmd into A)
/// $FFF8 mov X,A       2   (cmd into X, sets Z for cmd==0)
/// $FFF9 jnz $FFD6     2   (not taken: cmd==0 means "execute")
/// $FFFB jmp [$0000+X] 6   (X==0: the transfer/entry address just stashed)
/// ```
///
/// Total: 3+4+2+3+2+5+5+5+4+2+2+2+6 = 45. [`IplBoot`] spends exactly
/// these, one listed step at a time.
pub const RUN_HANDOFF_AFTER_TRANSFER_CYCLES: u16 = 45;

/// The same handoff when the CPU's very first `$CC` already carries a
/// zero "kind" byte in port 1 — "run immediately, nothing to transfer".
///
/// ```text
/// $FFCF cmp $F4,#$CC   5   (the compare that observes the go byte)
/// $FFD2 jnz $FFCF      2   (not taken)
/// $FFD4 jr main        4   (unconditional)
/// ```
/// then the same `$FFEF..$FFFB` tail as above (31 cycles).
///
/// Total: 5+2+4+31 = 42.
pub const RUN_HANDOFF_IMMEDIATE_CYCLES: u16 = 42;

/// SPC cycles the boot ROM spends per accepted upload byte, from the
/// counter match to being positioned to see the next one.
///
/// ```text
/// $FFDA cmp Y,$F4     3   (the compare that matches)
/// $FFDC jnz $FFE9     2   (not taken: Z set by the match)
/// $FFDE mov A,$F5     3   (fetch the data byte — 3 cycles AFTER the match)
/// $FFE0 mov $F4,Y     4   (echo the counter — the CPU's spin ends here)
/// $FFE2 mov [$00]+Y,A 7   (store the byte at the destination)
/// $FFE4 inc Y         2   (advance the counter)
/// $FFE5 jnz $FFDA     4   (taken: loop back to poll for the next byte)
/// ```
///
/// Total: 3+2+3+4+7+2+4 = 25 (31 on the 256th byte of a page, which
/// adds `inc $01` and a `bpl`).
pub const BYTE_HANDSHAKE_CYCLES: u16 = 25;

/// A step of the listing, named by the address fullsnes lists it at.
/// These are labels for documented instructions, not ROM contents.
type Label = u16;
/// `MOV X,#$EF / MOV SP,X` and the zero-page clear, collapsed into one
/// step of [`IPL_INIT_CYCLES`].
const INIT: Label = 0xFFC0;
/// `MOV $F4,#$AA / MOV $F5,#$BB`, collapsed into one step of
/// [`PUBLISH_CYCLES`].
const PUBLISH: Label = 0xFFC9;
/// Two `MOV dp,#imm` at 5 cycles each.
const PUBLISH_CYCLES: u16 = 10;
const WAIT_CC: Label = 0xFFCF;
const BNE_CC: Label = 0xFFD2;
const BRA_MAIN: Label = 0xFFD4;
const WAIT_ZERO: Label = 0xFFD6;
const BNE_ZERO: Label = 0xFFD8;
const CMP_COUNTER: Label = 0xFFDA;
const BNE_MISMATCH: Label = 0xFFDC;
const READ_DATA: Label = 0xFFDE;
const ECHO_COUNTER: Label = 0xFFE0;
const STORE_DATA: Label = 0xFFE2;
const INC_Y: Label = 0xFFE4;
const BNE_NEXT: Label = 0xFFE5;
const INC_PAGE: Label = 0xFFE7;
const BPL_WAIT: Label = 0xFFE9;
const RECHECK: Label = 0xFFEB;
const BPL_RECHECK: Label = 0xFFED;
const MAIN_ADDR: Label = 0xFFEF;
const STASH_ADDR: Label = 0xFFF1;
const READ_CMD: Label = 0xFFF3;
const ECHO_KICK: Label = 0xFFF5;
const CMD_TO_A: Label = 0xFFF7;
const CMD_TO_X: Label = 0xFFF8;
const BNE_BLOCK: Label = 0xFFF9;
const JUMP: Label = 0xFFFB;

/// The HLE boot handshake.
#[derive(Debug, Clone)]
pub struct IplBoot {
    pub state: BootState,
    /// Where the most recent byte was stored (`[$00]+Y`), for tests and
    /// probes.
    pub address: u16,
    /// Entry point the hand-over jumped to.
    pub entry: u16,
    /// Bytes transferred, so tests can assert the upload actually moved.
    pub transferred: usize,
    /// The listed instruction in flight, and the cycles before it
    /// completes. Effects land on completion, which is where an SPC700
    /// instruction's last access falls.
    pc: Label,
    busy: u16,
    a: u8,
    x: u8,
    y: u8,
    n: bool,
    z: bool,
    c: bool,
    /// The last value written to port 0, for [`BootState::AwaitingBlock`].
    last_echo: u8,
}

impl Default for IplBoot {
    fn default() -> Self {
        Self::new()
    }
}

impl IplBoot {
    /// Power-on: `$AA`/`$BB` already published (see [`Apu::reset`]), the
    /// ROM polling for `$CC`.
    ///
    /// [`Apu::reset`]: super::Apu::reset
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: BootState::Ready,
            address: 0,
            entry: 0,
            transferred: 0,
            pc: WAIT_CC,
            busy: step_cost(WAIT_CC, false, false),
            a: 0,
            x: 0,
            y: 0,
            n: false,
            z: false,
            c: false,
            last_echo: 0,
        }
    }

    /// A handshake re-entered at `$FFC0`: `Ready` only after the boot
    /// ROM's zero-page clear has run its documented course.
    #[must_use]
    pub fn rebooting() -> Self {
        Self::rebooting_at(0xFFC0)
    }

    /// A handshake re-entered wherever the program jumped (W14-48
    /// follow-up). **Where matters**: Champions - World Class Soccer's
    /// first stub jumps to `$FFC9`, past `MOV SP,X` and the zero-page
    /// clear, and the driver it then uploads is a lone `RET` at `$0050`
    /// that returns through the stub's own stack. Running the whole
    /// `$FFC0` init there wiped what it relied on. The listing is entered
    /// at the step containing `pc`: the init (before `$FFC9`), the
    /// `$AA`/`$BB` publish (`$FFC9-$FFCE`), or the `$CC` wait after it.
    #[must_use]
    pub fn rebooting_at(pc: u16) -> Self {
        let (pc, busy) = match pc {
            0xFFC0..=0xFFC8 => (INIT, IPL_INIT_CYCLES),
            0xFFC9..=0xFFCE => (PUBLISH, PUBLISH_CYCLES),
            _ => (WAIT_CC, step_cost(WAIT_CC, false, false)),
        };
        Self {
            state: if pc == WAIT_CC {
                BootState::Ready
            } else {
                BootState::Initialising(busy)
            },
            pc,
            busy,
            ..Self::new()
        }
    }

    #[must_use]
    pub fn is_running(&self) -> bool {
        self.state == BootState::Running
    }

    /// The `$AA`/`$BB` the CPU polls for.
    #[must_use]
    pub fn ready_ports() -> [u8; 4] {
        [0xAA, 0xBB, 0x00, 0x00]
    }
}

/// What the handshake wants done this cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootAction {
    /// Write this value to port 0 (`MOV $F4,…`).
    Echo(u8),
    /// Store `value` at `address` through the SPC700's write path.
    Store { address: u16, value: u8 },
    /// `MOVW $00,YA`: the destination pointer, little-endian at `$00`.
    SetPointer(u16),
    /// `MOV X,#$EF / MOV SP,X`, then `MOV (X),A / DEC X / BNE`: SP = `$EF`
    /// and `$01-$EF` cleared. Only on an entry at `$FFC0`.
    ClearZeroPage,
    /// `$AA`/`$BB` published on ports 0/1.
    Publish,
    /// Hand control to the SPC700 at `entry` with the register state the
    /// ROM leaves (`SP` is whatever it was — set to `$EF` only by
    /// [`BootAction::ClearZeroPage`]). `nzc` carries only the `N`/`Z`/`C`
    /// flags; the program's other PSW bits are untouched by the ROM.
    ///
    /// The kick echo has already gone out on an earlier cycle
    /// ([`BootAction::Echo`] at `$FFF5`) — the CPU is spinning on
    /// `CMP $2140 / BNE` for it (ticket W7-08), and it now arrives where
    /// the listing writes it rather than bundled with the jump.
    Run {
        entry: u16,
        a: u8,
        x: u8,
        y: u8,
        nzc: u8,
    },
    /// Nothing observable this cycle.
    None,
}

/// Listed cost of the step at `pc`, given the flags it will test.
const fn step_cost(pc: Label, n: bool, z: bool) -> u16 {
    // A conditional branch is 2 cycles not taken, 4 taken.
    const fn br(taken: bool) -> u16 {
        if taken {
            4
        } else {
            2
        }
    }
    match pc {
        INIT => IPL_INIT_CYCLES,
        PUBLISH => PUBLISH_CYCLES,
        WAIT_CC => 5,
        BNE_CC | BNE_ZERO | BNE_MISMATCH | BNE_NEXT | BNE_BLOCK => br(!z),
        BPL_WAIT | BPL_RECHECK => br(!n),
        BRA_MAIN | INC_PAGE | ECHO_COUNTER | ECHO_KICK => 4,
        WAIT_ZERO | CMP_COUNTER | READ_DATA | RECHECK => 3,
        STORE_DATA => 7,
        INC_Y | CMD_TO_A | CMD_TO_X => 2,
        MAIN_ADDR | STASH_ADDR | READ_CMD => 5,
        JUMP => 6,
        _ => 2,
    }
}

impl IplBoot {
    fn nz(&mut self, v: u8) {
        self.n = v & 0x80 != 0;
        self.z = v == 0;
    }

    /// `CMP` semantics: `reg - mem`, `C` = no borrow.
    fn cmp(&mut self, reg: u8, mem: u8) {
        self.nz(reg.wrapping_sub(mem));
        self.c = reg >= mem;
    }

    /// Advance one SPC cycle — **level-sampled on the APU's clock, not
    /// edge-triggered on the CPU's store.**
    ///
    /// The real IPL is a polling program, and this walks its listing: the
    /// instruction in flight finishes after its listed cycles, and only
    /// then reads or writes what it touches. Port 0 is therefore read at
    /// the compare, port 1 three cycles later, ports 2/3 later still —
    /// and a CPU that writes them in any order the ROM tolerates is seen
    /// the way the ROM sees it. That is also why a 16-bit `STA $2140`
    /// (port 0 first, port 1 one bus cycle later) is safe: the read of
    /// port 1 comes well after both halves have landed. `aram` is read
    /// for the `$00`/`$01` pointer, which the listing keeps in ARAM.
    pub fn poll(&mut self, ports_in: [u8; 4], aram: &[u8]) -> BootAction {
        if self.state == BootState::Running {
            return BootAction::None;
        }
        if self.busy > 1 {
            self.busy -= 1;
            if let BootState::Initialising(_) = self.state {
                self.state = BootState::Initialising(self.busy);
            }
            return BootAction::None;
        }
        let pointer = u16::from(aram[0]) | (u16::from(aram[1]) << 8);
        let (next, action) = match self.pc {
            INIT => {
                self.x = 0;
                (PUBLISH, BootAction::ClearZeroPage)
            }
            PUBLISH => (WAIT_CC, BootAction::Publish),
            WAIT_CC => {
                self.cmp(ports_in[0], 0xCC);
                (BNE_CC, BootAction::None)
            }
            BNE_CC => (if self.z { BRA_MAIN } else { WAIT_CC }, BootAction::None),
            BRA_MAIN => (MAIN_ADDR, BootAction::None),
            WAIT_ZERO => {
                self.y = ports_in[0];
                self.nz(self.y);
                (BNE_ZERO, BootAction::None)
            }
            BNE_ZERO => (
                if self.z { CMP_COUNTER } else { WAIT_ZERO },
                BootAction::None,
            ),
            CMP_COUNTER => {
                self.cmp(self.y, ports_in[0]);
                (BNE_MISMATCH, BootAction::None)
            }
            BNE_MISMATCH => (if self.z { READ_DATA } else { BPL_WAIT }, BootAction::None),
            READ_DATA => {
                self.a = ports_in[1];
                self.nz(self.a);
                (ECHO_COUNTER, BootAction::None)
            }
            ECHO_COUNTER => (STORE_DATA, BootAction::Echo(self.y)),
            STORE_DATA => {
                self.address = pointer.wrapping_add(u16::from(self.y));
                self.transferred += 1;
                (
                    INC_Y,
                    BootAction::Store {
                        address: self.address,
                        value: self.a,
                    },
                )
            }
            INC_Y => {
                self.y = self.y.wrapping_add(1);
                self.nz(self.y);
                (BNE_NEXT, BootAction::None)
            }
            BNE_NEXT => (
                if self.z { INC_PAGE } else { CMP_COUNTER },
                BootAction::None,
            ),
            INC_PAGE => {
                let page = aram[1].wrapping_add(1);
                self.nz(page);
                (
                    BPL_WAIT,
                    BootAction::Store {
                        address: 0x0001,
                        value: page,
                    },
                )
            }
            BPL_WAIT => (if self.n { RECHECK } else { CMP_COUNTER }, BootAction::None),
            RECHECK => {
                self.cmp(self.y, ports_in[0]);
                (BPL_RECHECK, BootAction::None)
            }
            BPL_RECHECK => (
                if self.n { MAIN_ADDR } else { CMP_COUNTER },
                BootAction::None,
            ),
            MAIN_ADDR => {
                self.a = ports_in[2];
                self.y = ports_in[3];
                self.n = self.y & 0x80 != 0;
                self.z = self.a == 0 && self.y == 0;
                (STASH_ADDR, BootAction::None)
            }
            STASH_ADDR => (
                READ_CMD,
                BootAction::SetPointer(u16::from(self.a) | (u16::from(self.y) << 8)),
            ),
            READ_CMD => {
                self.a = ports_in[0];
                self.y = ports_in[1];
                self.n = self.y & 0x80 != 0;
                self.z = self.a == 0 && self.y == 0;
                (ECHO_KICK, BootAction::None)
            }
            ECHO_KICK => (CMD_TO_A, BootAction::Echo(self.a)),
            CMD_TO_A => {
                self.a = self.y;
                self.nz(self.a);
                (CMD_TO_X, BootAction::None)
            }
            CMD_TO_X => {
                self.x = self.a;
                self.nz(self.x);
                (BNE_BLOCK, BootAction::None)
            }
            BNE_BLOCK => (if self.z { JUMP } else { WAIT_ZERO }, BootAction::None),
            JUMP => {
                // `jmp [$0000+X]` with X == 0: the word `MOVW $00,YA` left.
                self.entry = pointer;
                let nzc = if self.n { flags::N } else { 0 }
                    | if self.z { flags::Z } else { 0 }
                    | if self.c { flags::C } else { 0 };
                self.state = BootState::Running;
                return BootAction::Run {
                    entry: pointer,
                    a: self.a,
                    x: self.x,
                    y: self.y,
                    nzc,
                };
            }
            other => unreachable!("IPL HLE reached unlisted step {other:#06X}"),
        };
        if let BootAction::Echo(v) = action {
            self.last_echo = v;
        }
        self.pc = next;
        self.busy = step_cost(next, self.n, self.z);
        self.state = match next {
            PUBLISH => BootState::Initialising(self.busy),
            WAIT_CC | BNE_CC | BRA_MAIN => BootState::Ready,
            WAIT_ZERO | BNE_ZERO => BootState::AwaitingBlock(self.last_echo),
            CMP_COUNTER | BNE_MISMATCH | READ_DATA | ECHO_COUNTER | STORE_DATA => {
                BootState::Transferring(self.y)
            }
            INC_Y | BNE_NEXT | INC_PAGE | BPL_WAIT | RECHECK | BPL_RECHECK => {
                BootState::Transferring(self.y)
            }
            _ => match self.state {
                BootState::Ready | BootState::Initialising(_) => BootState::Ready,
                s => s,
            },
        };
        action
    }
}

impl IplBoot {
    /// Serialise the HLE boot handshake (ticket W7-09).
    ///
    /// The handshake is a multi-frame conversation between the CPU and the
    /// APU, so a state saved during boot must resume it rather than
    /// restart it — restarting would leave the CPU waiting on a `$BBAA`
    /// that never comes again. The listing position and the cycles left
    /// on the step in flight are what make it resumable mid-instruction.
    pub(crate) fn save(
        &self,
        o: &mut crate::state::StateOut,
    ) -> Result<(), rf_core_api::StateError> {
        let (tag, arg) = match self.state {
            BootState::Ready => (0u8, 0u16),
            BootState::Transferring(n) => (1, u16::from(n)),
            BootState::AwaitingBlock(n) => (2, u16::from(n)),
            BootState::Running => (3, 0),
            BootState::Initialising(n) => (4, n),
        };
        o.u8(tag)?;
        o.u16(arg)?;
        o.u16(self.address)?;
        o.u16(self.entry)?;
        o.usize(self.transferred)?;
        o.u16(self.pc)?;
        o.u16(self.busy)?;
        o.u8(self.a)?;
        o.u8(self.x)?;
        o.u8(self.y)?;
        o.u8(self.last_echo)?;
        o.u8(u8::from(self.n) | (u8::from(self.z) << 1) | (u8::from(self.c) << 2))
    }

    pub(crate) fn load(
        &mut self,
        i: &mut crate::state::StateIn,
    ) -> Result<(), rf_core_api::StateError> {
        let tag = i.u8()?;
        let arg = i.u16()?;
        self.state = match tag {
            0 => BootState::Ready,
            1 => BootState::Transferring(arg as u8),
            2 => BootState::AwaitingBlock(arg as u8),
            3 => BootState::Running,
            4 => BootState::Initialising(arg),
            other => {
                return Err(rf_core_api::StateError::Corrupt(format!(
                    "IPL boot state tag {other} is not one of ready/transferring/awaiting/running/initialising"
                )))
            }
        };
        self.address = i.u16()?;
        self.entry = i.u16()?;
        self.transferred = i.usize()?;
        let pc = i.u16()?;
        if !(INIT..=JUMP).contains(&pc) {
            return Err(rf_core_api::StateError::Corrupt(format!(
                "IPL boot listing position {pc:#06X} is outside $FFC0-$FFFB"
            )));
        }
        self.pc = pc;
        self.busy = i.u16()?;
        self.a = i.u8()?;
        self.x = i.u8()?;
        self.y = i.u8()?;
        self.last_echo = i.u8()?;
        let f = i.u8()?;
        self.n = f & 1 != 0;
        self.z = f & 2 != 0;
        self.c = f & 4 != 0;
        Ok(())
    }
}
