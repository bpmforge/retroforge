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
    /// Hand control to the SPC700 at `entry`.
    Run { entry: u16 },
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
    pub fn cpu_wrote(&mut self, index: usize, value: u8, ports_in: [u8; 4]) -> BootAction {
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
                        return BootAction::Run { entry: self.entry };
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
                if value == expected.wrapping_add(1) {
                    // Counter skipped by two from the last acknowledged
                    // byte: a new block (or the end).
                    self.address = u16::from(ports_in[2]) | (u16::from(ports_in[3]) << 8);
                    self.entry = self.address;
                    if ports_in[1] == 0 {
                        self.state = BootState::Running;
                        return BootAction::Run { entry: self.entry };
                    }
                    self.state = BootState::AwaitingBlock(value);
                    return BootAction::Echo(value);
                }
                BootAction::None
            }
            BootState::AwaitingBlock(ack) => {
                if index == 0 && value == ack.wrapping_add(1) {
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
