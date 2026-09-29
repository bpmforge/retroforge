//! The IPL boot handshake, high-level-emulated (ticket W6-04b;
//! `docs/design/EMULATION_CORES.md` §3.4).
//!
//! ## This is what the W6-04a ruling chose
//!
//! Option (a) was "HLE the handshake: implement the boot protocol's
//! OBSERVABLE behaviour without the ROM's bytes". This module is that.
//! It is a state machine, not SPC700 code, and it is deliberately
//! explicit about the consequence: **while the boot handshake is
//! running, the SPC700 core does not execute.** The uploaded program
//! runs on the real core the moment the handshake hands control over.
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
//!    an incrementing counter to port 0; the APU stores the byte and
//!    echoes the counter. The counter is how both sides stay in step
//!    without a clock — it is an acknowledgement, not an index.
//! 4. **Next block or run.** The CPU writes port 1 = 0 to finish (or
//!    non-zero to start another block), a new address in ports 2/3, and
//!    a counter advanced by **two** in port 0. The skip is what
//!    distinguishes "new block" from "next byte", and is the single
//!    easiest part of this protocol to get wrong.
//! 5. **Go.** The APU jumps to the last address supplied.

/// Where the handshake has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootState {
    /// Ports 0/1 hold `$AA`/`$BB`; waiting for `$CC` in port 0.
    Ready,
    /// A block is in progress; the value is the counter expected next.
    Transferring(u8),
    /// Between blocks: the CPU has signalled a new address.
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
/// `$00-$EF`, and only then `MOV $F4,#$AA` / `MOV $F5,#$BB`. With the
/// SPC700 cycle table (2 + 2 + 2, then 239 laps of 4 + 2 + 4 and a last
/// lap of 4 + 2 + 2) that is 2404 cycles, about 2.3 ms.
///
/// **This delay is load-bearing, not cosmetic.** A game that reboots the
/// APU by commanding a jump to `$FFC0` is still spinning on `CMP $2140`
/// for the echo of that jump's counter. Republishing `$AA` on the very
/// next SPC instruction overwrote the echo before the 65816 could read
/// it — Super Bonk waited for `$E3` for ever while the port held `$AA`.
pub const IPL_INIT_CYCLES: u16 = 2404;

/// SPC cycles between the boot ROM detecting the CPU's final "go" write
/// and the uploaded program's first instruction executing, for the
/// **post-transfer** run (the CPU's counter jumped by 2+, ending a
/// block) — the shape every census title in this ticket's brief uses.
///
/// fullsnes "SNES APU Main CPU Communication Port" -> "Boot ROM
/// Disassembly" (clean-room disassembly of the documented protocol, not
/// Nintendo's bytes — law 5). Cycle costs from this crate's own
/// vector-verified `spc700::timing::CYCLES`:
///
/// ```text
/// $FFDA cmp Y,$F4     3   (the compare that observes the mismatch)
/// $FFDC jnz $FFE9     4   (taken: 2 base + 2 BRANCH_TAKEN_EXTRA)
/// $FFE9 jns $FFDA     2   (not taken)
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
/// Total: 3+4+2+3+2+5+5+5+4+2+2+2+6 = 45.
pub const RUN_HANDOFF_AFTER_TRANSFER_CYCLES: u16 = 45;

/// The same handoff when the CPU's very first `$CC` already carries a
/// zero "kind" byte in port 1 — "run immediately, nothing to transfer".
/// The boot ROM takes the `jr main` fast path instead of the two-compare
/// wait-loop tail above.
///
/// ```text
/// $FFCF cmp $F4,#$CC   5   (the compare that observes the go byte)
/// $FFD2 jnz $FFCF      2   (not taken)
/// $FFD4 jr main        4   (unconditional)
/// ```
/// then the same `$FFEF..$FFFB` tail as above (31 cycles: 5+5+5+4+2+2+2+6).
///
/// Total: 5+2+4+31 = 42.
pub const RUN_HANDOFF_IMMEDIATE_CYCLES: u16 = 42;

/// SPC cycles the boot ROM spends per accepted upload byte, from the
/// counter match to being positioned to see the next one.
///
/// fullsnes "Boot ROM Disassembly":
///
/// ```text
/// $FFDA cmp Y,$F4     3   (the compare that matches)
/// $FFDC jnz $FFE9     2   (not taken: Z set by the match)
/// $FFDE mov A,$F5     3   (fetch the data byte)
/// $FFE0 mov $F4,Y     4   (echo the counter — the CPU's spin ends here)
/// $FFE2 mov [$00]+Y,A 7   (store the byte at the destination)
/// $FFE4 inc Y         2   (advance the counter)
/// $FFE5 jnz $FFDA     4   (taken: loop back to poll for the next byte)
/// ```
///
/// Total: 3+2+3+4+7+2+4 = 25.
pub const BYTE_HANDSHAKE_CYCLES: u16 = 25;

/// SPC cycles from the boot ROM's observation of the new counter on `$F4`
/// to its read of the data byte on `$F5`.
///
/// fullsnes "Boot ROM Disassembly": the matching `$FFDA cmp Y,$F4` (3
/// cycles, `$F4` sampled on its last), `$FFDC jnz` (2), then `$FFDE mov
/// A,$F5` (3, `$F5` sampled on its last): about 5 cycles after the
/// `$F4` sample, which is why an uploader that writes `$2141` a few CPU
/// cycles after `$2140` still has its byte stored.
pub const BYTE_DATA_FETCH_CYCLES: u16 = 5;

/// The HLE boot handshake.
#[derive(Debug, Clone)]
pub struct IplBoot {
    pub state: BootState,
    /// Where the next transferred byte goes.
    pub address: u16,
    /// Entry point, taken from the last address the CPU supplied.
    pub entry: u16,
    /// Bytes transferred, so tests can assert the upload actually moved.
    pub transferred: usize,
    /// A [`BootAction`] the protocol already decided on, held back for
    /// the boot ROM's own documented instruction cost (ticket W14-37)
    /// before it becomes observable — `(cycles left, the action)`.
    ///
    /// `cpu_wrote` is the pure protocol decision and stays instantaneous
    /// (so it is still directly testable without a clock around it); this
    /// is what makes [`IplBoot::poll`], which is the entry point the
    /// machine's clock actually calls, delay the OUTPUT by the listing's
    /// cycle counts instead. While this is `Some`, `poll` does not
    /// re-examine the ports at all — matching real hardware, which is not
    /// polling `$F4` while it is mid-way through this fixed instruction
    /// tail.
    pending: Option<(u16, BootAction)>,
}

impl Default for IplBoot {
    fn default() -> Self {
        Self::new()
    }
}

impl IplBoot {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: BootState::Ready,
            address: 0,
            entry: 0,
            transferred: 0,
            pending: None,
        }
    }

    /// A handshake re-entered at `$FFC0`: `Ready` only after the boot
    /// ROM's zero-page clear has run its documented course.
    #[must_use]
    pub fn rebooting() -> Self {
        Self {
            state: BootState::Initialising(IPL_INIT_CYCLES),
            ..Self::new()
        }
    }

    #[must_use]
    pub fn is_running(&self) -> bool {
        // `cpu_wrote` sets `state` to `Running` the instant it DECIDES to
        // hand over — see `poll`'s doc comment — but the SPC700 core is
        // not actually free to run until the pending `Run` action's delay
        // (ticket W14-37) has been paid out. Reporting `Running` early
        // would make `SnesBus::catch_up_apu`/`Apu::poll_boot` stop calling
        // `poll` altogether, which is exactly what would strand the
        // pending action mid-countdown forever.
        self.state == BootState::Running && self.pending.is_none()
    }

    /// The `$AA`/`$BB` the CPU polls for.
    #[must_use]
    pub fn ready_ports() -> [u8; 4] {
        [0xAA, 0xBB, 0x00, 0x00]
    }
}

/// What the handshake wants done after a port write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootAction {
    /// Echo this value back on port 0.
    Echo(u8),
    /// Store `value` at `address`, then echo.
    Store { address: u16, value: u8, echo: u8 },
    /// The boot ROM's init finished: publish `$AA`/`$BB` on ports 0/1.
    Publish,
    /// Hand control to the SPC700 at `entry`, **after echoing `echo` on
    /// port 0**.
    ///
    /// The echo is not optional and not cosmetic (ticket W7-08). The CPU's
    /// upload loop writes the counter to `$2140` and then SPINS on
    /// `CMP $2140 / BNE` until the IPL sends it back — the jump signal is
    /// no exception. Handing over without the echo leaves the 65816
    /// polling a value that will never arrive while the SPC700 runs the
    /// program it was just given: two live processors, each waiting on the
    /// other, from a handshake that had otherwise completed perfectly.
    Run { entry: u16, echo: u8 },
    /// Nothing to do.
    None,
}

impl IplBoot {
    /// The CPU wrote `value` to port `index` (`$2140 + index`).
    ///
    /// Returns what the APU side should do. Kept as a pure decision
    /// function returning an action, rather than reaching into ARAM
    /// directly, so the protocol can be tested without a machine around
    /// it — and so the one place that mutates memory stays visible.
    /// Sample the ports and advance the handshake — **level-triggered on
    /// the APU's clock, not edge-triggered on the CPU's store.**
    ///
    /// This distinction is the whole reason the function is shaped this
    /// way, and getting it wrong cost blargg's entire SPC suite (W7-08).
    /// The real IPL is a *polling program*: it reads `$F4`-`$F7` when it
    /// runs, so it can never observe a half-finished write. A 16-bit
    /// `STA $2140` — which is how blargg's uploader starts a transfer —
    /// lands as TWO byte writes, port 0 first:
    ///
    /// ```text
    /// idx=0 val=CC  ports_in before [00, 00, 00, 04]
    /// idx=1 val=01  ports_in before [CC, 00, 00, 04]
    /// ```
    ///
    /// Reacting to the port-0 write reads the "kind" byte one instruction
    /// too early, sees `0`, and takes the "transfer nothing, just run"
    /// branch — jumping to an address nothing was uploaded to. The SPC700
    /// then NOP-slides through empty ARAM forever while the 65816 spins
    /// waiting for a byte counter that is never echoed.
    ///
    /// Polling is also naturally **idempotent**, which is what makes it
    /// safe to call every APU step: every accepted value advances the
    /// value this is waiting for, so re-reading an unchanged port 0 never
    /// matches twice.
    pub fn poll(&mut self, ports_in: [u8; 4]) -> BootAction {
        // A `Run` or `Store` this function already returned once is
        // sitting in `pending`, ticking down the boot ROM's own
        // instruction cost (ticket W14-37) before it is delivered. Real
        // hardware is not polling the ports during that window either —
        // it is mid-way through the fixed tail those cycles model — so
        // this deliberately does not fall through to `cpu_wrote` here.
        if let Some((left, action)) = self.pending {
            if left > 1 {
                let left = left - 1;
                // The boot ROM fetches the data byte from `$F5` (`$FFDE
                // mov A,$F5`) [`BYTE_DATA_FETCH_CYCLES`] SPC cycles AFTER
                // the compare that observed the counter on `$F4`, so a
                // byte the CPU wrote to `$2141` after its `$2140` counter
                // write (the `STA $2140 / XBA / STA $2141` order, ~2.7
                // SPC cycles apart) is the one stored; the value the
                // port held when the counter changed is stale.
                let action = match action {
                    BootAction::Store { address, echo, .. }
                        if left == BYTE_HANDSHAKE_CYCLES - BYTE_DATA_FETCH_CYCLES =>
                    {
                        BootAction::Store {
                            address,
                            value: ports_in[1],
                            echo,
                        }
                    }
                    other => other,
                };
                self.pending = Some((left, action));
                return BootAction::None;
            }
            self.pending = None;
            return action;
        }
        if let BootState::Initialising(left) = self.state {
            // One poll per SPC cycle while the HLE owns the machine —
            // see `SnesBus::catch_up_apu`.
            if left > 1 {
                self.state = BootState::Initialising(left - 1);
                return BootAction::None;
            }
            self.state = BootState::Ready;
            return BootAction::Publish;
        }
        // Captured before `cpu_wrote` runs: it decides the protocol
        // outcome (and, for a `Run`, already advances `self.state` to
        // `Running`) in one instantaneous step, so the state that
        // determines WHICH handoff path the real ROM took has to be read
        // beforehand.
        let was_ready = self.state == BootState::Ready;
        let action = self.cpu_wrote(0, ports_in[0], ports_in);
        match action {
            BootAction::Run { .. } => {
                let delay = if was_ready {
                    RUN_HANDOFF_IMMEDIATE_CYCLES
                } else {
                    RUN_HANDOFF_AFTER_TRANSFER_CYCLES
                };
                self.pending = Some((delay, action));
                BootAction::None
            }
            BootAction::Store { .. } => {
                self.pending = Some((BYTE_HANDSHAKE_CYCLES, action));
                BootAction::None
            }
            other => other,
        }
    }

    pub(crate) fn cpu_wrote(&mut self, index: usize, value: u8, ports_in: [u8; 4]) -> BootAction {
        match self.state {
            BootState::Running | BootState::Initialising(_) => BootAction::None,
            BootState::Ready => {
                if index == 0 && value == 0xCC {
                    self.address = u16::from(ports_in[2]) | (u16::from(ports_in[3]) << 8);
                    self.entry = self.address;
                    if ports_in[1] == 0 {
                        // A zero "kind" byte with the very first $CC means
                        // "do not transfer anything, just run".
                        self.state = BootState::Running;
                        return BootAction::Run {
                            entry: self.entry,
                            echo: 0xCC,
                        };
                    }
                    self.state = BootState::Transferring(0);
                    return BootAction::Echo(0xCC);
                }
                BootAction::None
            }
            BootState::Transferring(expected) => {
                if index != 0 {
                    // Data arrives on port 1 and is latched by the port-0
                    // write that follows; nothing to do yet.
                    return BootAction::None;
                }
                if value == expected {
                    let address = self.address;
                    self.address = self.address.wrapping_add(1);
                    self.transferred += 1;
                    self.state = BootState::Transferring(expected.wrapping_add(1));
                    return BootAction::Store {
                        address,
                        value: ports_in[1],
                        echo: value,
                    };
                }
                // The last counter this handler acknowledged. `poll` feeds
                // the CURRENT port-0 value in on a timer, so the value
                // just consumed arrives again and again until the CPU
                // writes the next one; seeing it is "nothing new", not a
                // mismatch. Getting this wrong ends every block after its
                // first byte.
                if value == expected.wrapping_sub(1) {
                    return BootAction::None;
                }
                // Any OTHER value that is not the expected counter ends the
                // block. This accepted only `expected + 1` until ticket
                // W14-06, and that over-specific rule is why most
                // commercial titles never finished booting: the IPL does
                // not check for a particular skip, it checks for a
                // MISMATCH, and different uploaders jump by different
                // amounts. Super Mario World's jumps by four — traced:
                // it acknowledged counter $3D, wrote the new block's
                // address to ports 2-3 and a non-zero kind to port 1,
                // then wrote $41. Under the old rule that matched
                // neither arm, the handler did nothing, and the game
                // spun on `CMP $2140 / BNE` for ever with the screen
                // still in forced blank.
                self.address = u16::from(ports_in[2]) | (u16::from(ports_in[3]) << 8);
                self.entry = self.address;
                if ports_in[1] == 0 {
                    self.state = BootState::Running;
                    return BootAction::Run {
                        entry: self.entry,
                        echo: value,
                    };
                }
                self.state = BootState::AwaitingBlock(value);
                BootAction::Echo(value)
            }
            BootState::AwaitingBlock(ack) => {
                // The new block's first byte arrives under whatever
                // counter the uploader is now using — which is not
                // necessarily `ack + 1`, for the same reason the mismatch
                // above is not necessarily `expected + 1`. The one value
                // that cannot start it is a repeat of the acknowledgement
                // itself, which is the CPU still waiting for its echo.
                if index == 0 && value != ack {
                    self.state = BootState::Transferring(value.wrapping_add(1));
                    let address = self.address;
                    self.address = self.address.wrapping_add(1);
                    self.transferred += 1;
                    return BootAction::Store {
                        address,
                        value: ports_in[1],
                        echo: value,
                    };
                }
                BootAction::None
            }
        }
    }
}

impl IplBoot {
    /// Serialise the HLE boot handshake (ticket W7-09).
    ///
    /// The handshake is a multi-frame conversation between the CPU and the
    /// APU, so a state saved during boot must resume it rather than
    /// restart it — restarting would leave the CPU waiting on a `$BBAA`
    /// that never comes again.
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
        // W14-37: a handoff or byte-store queued behind the boot ROM's own
        // instruction cost. Must round-trip — a save mid-countdown that
        // silently dropped this would resume with the CPU still spinning
        // on an echo that already "happened" from the protocol's point of
        // view (`state` already moved on) but was never written to the
        // port, hanging the boot forever.
        match self.pending {
            None => o.bool(false),
            Some((left, BootAction::Run { entry, echo })) => {
                o.bool(true)?;
                o.u8(0)?; // pending action kind: Run
                o.u16(left)?;
                o.u16(entry)?;
                o.u8(echo)
            }
            Some((
                left,
                BootAction::Store {
                    address,
                    value,
                    echo,
                },
            )) => {
                o.bool(true)?;
                o.u8(1)?; // pending action kind: Store
                o.u16(left)?;
                o.u16(address)?;
                o.u8(value)?;
                o.u8(echo)
            }
            Some((_, other)) => Err(rf_core_api::StateError::Corrupt(format!(
                "IPL boot has an unexpected pending action {other:?} — only Run/Store are ever queued"
            ))),
        }
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
        self.pending = if i.bool()? {
            let kind = i.u8()?;
            let left = i.u16()?;
            Some((
                left,
                match kind {
                    0 => BootAction::Run {
                        entry: i.u16()?,
                        echo: i.u8()?,
                    },
                    1 => BootAction::Store {
                        address: i.u16()?,
                        value: i.u8()?,
                        echo: i.u8()?,
                    },
                    other => {
                        return Err(rf_core_api::StateError::Corrupt(format!(
                            "IPL boot pending-action kind {other} is not one of Run(0)/Store(1)"
                        )))
                    }
                },
            ))
        } else {
            None
        };
        Ok(())
    }
}
