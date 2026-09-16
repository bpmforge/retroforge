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
}

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

/// What the handshake wants done after a port write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootAction {
    /// Echo this value back on port 0.
    Echo(u8),
    /// Store `value` at `address`, then echo.
    Store { address: u16, value: u8, echo: u8 },
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
        self.cpu_wrote(0, ports_in[0], ports_in)
    }

    pub(crate) fn cpu_wrote(&mut self, index: usize, value: u8, ports_in: [u8; 4]) -> BootAction {
        match self.state {
            BootState::Running => BootAction::None,
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
            BootState::Ready => (0u8, 0u8),
            BootState::Transferring(n) => (1, n),
            BootState::AwaitingBlock(n) => (2, n),
            BootState::Running => (3, 0),
        };
        o.u8(tag)?;
        o.u8(arg)?;
        o.u16(self.address)?;
        o.u16(self.entry)?;
        o.usize(self.transferred)
    }

    pub(crate) fn load(
        &mut self,
        i: &mut crate::state::StateIn,
    ) -> Result<(), rf_core_api::StateError> {
        let tag = i.u8()?;
        let arg = i.u8()?;
        self.state = match tag {
            0 => BootState::Ready,
            1 => BootState::Transferring(arg),
            2 => BootState::AwaitingBlock(arg),
            3 => BootState::Running,
            other => {
                return Err(rf_core_api::StateError::Corrupt(format!(
                    "IPL boot state tag {other} is not one of ready/transferring/awaiting/running"
                )))
            }
        };
        self.address = i.u16()?;
        self.entry = i.u16()?;
        self.transferred = i.usize()?;
        Ok(())
    }
}
