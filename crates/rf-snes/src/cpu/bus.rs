//! The 24-bit bus the 65C816 talks to (ticket W6-01a).
//!
//! Separate from the CPU for the same reason `rf_nes::cpu::bus` is: the
//! CPU's tests need a bus they can assert against, and the real system
//! bus (W6-02a) needs to exist without the CPU knowing anything about
//! cartridge mapping. Every access carries a full 24-bit address, so the
//! CPU never has to know which bank register produced it.

/// One byte of the 16 MiB address space.
pub trait CpuBus {
    /// Read one byte. Ticks the machine's master clock by one CPU cycle.
    fn read(&mut self, addr: u32) -> u8;

    /// Write one byte. Ticks the machine's master clock by one CPU cycle.
    fn write(&mut self, addr: u32, value: u8);

    /// Read without side effects, for the debugger and for tests.
    ///
    /// Default: falls back to `read`, which is WRONG for any bus with
    /// read-triggered side effects and is why W6-02a must override it —
    /// the same contract `rf_nes::cpu::CpuBus::peek` sets, and the same
    /// trap (a trace logger that used `read` would perturb the machine it
    /// was tracing).
    fn peek(&self, addr: u32) -> u8;
}

/// A flat 16 MiB bus, for unit tests.
///
/// Deliberately not sparse: a test that has to declare which banks exist
/// spends its assertions on plumbing, and the whole point of the tests in
/// this module is the CPU's own behaviour.
pub struct FlatBus {
    pub mem: Vec<u8>,
    /// Every access in order, so a test can assert the CPU touched the
    /// addresses it should have — the property that separates a correct
    /// addressing mode from one that reads the right value by luck.
    pub log: Vec<Access>,
}

/// One logged bus access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read(u32),
    Write(u32, u8),
}

impl Default for FlatBus {
    fn default() -> Self {
        Self::new()
    }
}

impl FlatBus {
    #[must_use]
    pub fn new() -> Self {
        Self {
            mem: vec![0; 0x100_0000],
            log: Vec::new(),
        }
    }

    /// Load `bytes` at `addr` without logging — test setup, not a CPU
    /// access.
    pub fn load(&mut self, addr: u32, bytes: &[u8]) {
        let start = addr as usize;
        self.mem[start..start + bytes.len()].copy_from_slice(bytes);
    }

    /// Addresses read, in order.
    #[must_use]
    pub fn reads(&self) -> Vec<u32> {
        self.log
            .iter()
            .filter_map(|a| match a {
                Access::Read(addr) => Some(*addr),
                Access::Write(..) => None,
            })
            .collect()
    }

    /// Writes, in order.
    #[must_use]
    pub fn writes(&self) -> Vec<(u32, u8)> {
        self.log
            .iter()
            .filter_map(|a| match a {
                Access::Write(addr, v) => Some((*addr, *v)),
                Access::Read(_) => None,
            })
            .collect()
    }
}

impl CpuBus for FlatBus {
    fn read(&mut self, addr: u32) -> u8 {
        let v = self.mem[addr as usize];
        self.log.push(Access::Read(addr));
        v
    }

    fn write(&mut self, addr: u32, value: u8) {
        self.mem[addr as usize] = value;
        self.log.push(Access::Write(addr, value));
    }

    fn peek(&self, addr: u32) -> u8 {
        self.mem[addr as usize]
    }
}
