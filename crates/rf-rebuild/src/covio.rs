//! Byte form of a [`CoverageLog`] for the per-ROM cache (ticket W37-01).
//!
//! The atlas cannot be merged with new coverage: its blocks and functions
//! are derived, and the NES bank windows the builder resolves targets by
//! (`WindowIndex`) are not recoverable from it. So the cache keeps the merged
//! *recording* next to the atlas, and every rebuild is `cached log + what this
//! session saw`.
//!
//! Layout (little endian): magic `RFCV`, version `u8`, the two flag-array
//! lengths, three counters, both flag arrays as zero-friendly run lengths
//! (`value u8`, `run` LEB128: a 4 MiB ROM that executed 200 KiB is a few KiB),
//! then the edge, interrupt and window lists. Decoding checks every length
//! against the bytes actually present and refuses a run of zero, so a damaged
//! entry is a clean miss and never a loop or a giant allocation (law 8).

use rf_core_api::coverage::{
    CodeAddr, CoverageLog, IndirectEdge, IndirectKind, InterruptEntry, InterruptKind, WindowMap,
    WINDOW_PAGES,
};

/// Cache asset name of the stored recording (the key is built by the shell,
/// like the atlas's: this crate does not see the cache).
pub const COVERAGE_ASSET: &str = "code-coverage";
/// Format version in the cache key: another version is a clean miss.
pub const COVERAGE_SETTINGS: &str = "code-coverage-v1";

const MAGIC: [u8; 4] = *b"RFCV";
const VERSION: u8 = 1;
/// Largest flag array a stored log may claim (a 64 MiB cartridge is far
/// beyond any SNES/NES image).
const MAX_FLAGS: usize = 64 * 1024 * 1024;
/// Largest edge/interrupt/window list a stored log may claim.
const MAX_ITEMS: usize = 1 << 20;

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn put_rle(out: &mut Vec<u8>, data: &[u8]) {
    let mut it = data.iter().copied().peekable();
    while let Some(v) = it.next() {
        let mut run = 1u64;
        while it.next_if_eq(&v).is_some() {
            run += 1;
        }
        out.push(v);
        put_varint(out, run);
    }
}

fn put_addr(out: &mut Vec<u8>, a: &CodeAddr) {
    out.extend_from_slice(&a.cpu.to_le_bytes());
    out.push(a.pbr);
    out.push(a.mode);
    out.extend_from_slice(&a.rom.unwrap_or(u32::MAX).to_le_bytes());
}

fn kind_byte(k: IndirectKind) -> u8 {
    match k {
        IndirectKind::JmpIndirect => 0,
        IndirectKind::Rts => 1,
        IndirectKind::Rti => 2,
        IndirectKind::Brk => 3,
        IndirectKind::JsrIndirect => 4,
        IndirectKind::Rtl => 5,
        IndirectKind::Cop => 6,
    }
}

fn kind_of(b: u8) -> Option<IndirectKind> {
    Some(match b {
        0 => IndirectKind::JmpIndirect,
        1 => IndirectKind::Rts,
        2 => IndirectKind::Rti,
        3 => IndirectKind::Brk,
        4 => IndirectKind::JsrIndirect,
        5 => IndirectKind::Rtl,
        6 => IndirectKind::Cop,
        _ => return None,
    })
}

/// Serialise `log`.
#[must_use]
pub fn encode_coverage(log: &CoverageLog) -> Vec<u8> {
    let mut out = Vec::with_capacity(4096);
    out.extend_from_slice(&MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&(log.prg_flags.len() as u64).to_le_bytes());
    out.extend_from_slice(&(log.ram_exec.len() as u64).to_le_bytes());
    out.extend_from_slice(&log.coproc_exec.to_le_bytes());
    out.extend_from_slice(&log.other_exec.to_le_bytes());
    out.extend_from_slice(&log.instructions_recorded.to_le_bytes());
    put_rle(&mut out, &log.prg_flags);
    put_rle(&mut out, &log.ram_exec);
    out.extend_from_slice(&(log.indirect.len() as u32).to_le_bytes());
    for e in &log.indirect {
        put_addr(&mut out, &e.site);
        put_addr(&mut out, &e.target);
        out.push(kind_byte(e.kind));
    }
    out.extend_from_slice(&(log.interrupts.len() as u32).to_le_bytes());
    for i in &log.interrupts {
        out.push(match i.kind {
            InterruptKind::Reset => 0,
            InterruptKind::Nmi => 1,
            InterruptKind::Irq => 2,
        });
        put_addr(&mut out, &i.target);
    }
    out.extend_from_slice(&(log.windows.len() as u32).to_le_bytes());
    for w in &log.windows {
        for base in w.0 {
            out.extend_from_slice(&base.to_le_bytes());
        }
    }
    out
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let (head, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(head)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Option<u64> {
        let b = self.take(8)?;
        Some(u64::from_le_bytes(b.try_into().ok()?))
    }
    fn varint(&mut self) -> Option<u64> {
        let mut v = 0u64;
        // Ten bytes carry 64 bits; the loop is bounded by that, not the data.
        for shift in (0..70).step_by(7) {
            let b = self.u8()?;
            v |= u64::from(b & 0x7F).checked_shl(shift)?;
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }
    fn rle(&mut self, len: usize) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(len.min(self.0.len().saturating_mul(64)));
        // Every pass appends at least one byte (a zero run is refused), so
        // the length strictly grows toward `len` or the loop returns.
        while out.len() < len {
            let v = self.u8()?;
            let run = usize::try_from(self.varint()?).ok()?;
            if run == 0 || run > len - out.len() {
                return None;
            }
            out.resize(out.len() + run, v);
        }
        Some(out)
    }
    fn addr(&mut self) -> Option<CodeAddr> {
        let cpu = self.u16()?;
        let pbr = self.u8()?;
        let mode = self.u8()?;
        let rom = self.u32()?;
        Some(CodeAddr {
            cpu,
            pbr,
            mode,
            rom: (rom != u32::MAX).then_some(rom),
        })
    }
    fn count(&mut self) -> Option<usize> {
        let n = self.u32()? as usize;
        (n <= MAX_ITEMS).then_some(n)
    }
}

/// Parse [`encode_coverage`]'s output; `None` for anything else or damage.
#[must_use]
pub fn decode_coverage(bytes: &[u8]) -> Option<CoverageLog> {
    let mut r = Reader(bytes);
    if r.take(4)? != MAGIC || r.u8()? != VERSION {
        return None;
    }
    let prg_len = usize::try_from(r.u64()?).ok().filter(|&n| n <= MAX_FLAGS)?;
    let ram_len = usize::try_from(r.u64()?).ok().filter(|&n| n <= MAX_FLAGS)?;
    let coproc_exec = r.u64()?;
    let other_exec = r.u64()?;
    let instructions_recorded = r.u64()?;
    let prg_flags = r.rle(prg_len)?;
    let ram_exec = r.rle(ram_len)?;
    let mut indirect = Vec::new();
    for _ in 0..r.count()? {
        let site = r.addr()?;
        let target = r.addr()?;
        let kind = kind_of(r.u8()?)?;
        indirect.push(IndirectEdge { site, target, kind });
    }
    let mut interrupts = Vec::new();
    for _ in 0..r.count()? {
        let kind = match r.u8()? {
            0 => InterruptKind::Reset,
            1 => InterruptKind::Nmi,
            2 => InterruptKind::Irq,
            _ => return None,
        };
        interrupts.push(InterruptEntry {
            kind,
            target: r.addr()?,
        });
    }
    let mut windows = Vec::new();
    for _ in 0..r.count()? {
        let mut m = [0u32; WINDOW_PAGES];
        for slot in &mut m {
            *slot = r.u32()?;
        }
        windows.push(WindowMap(m));
    }
    r.0.is_empty().then_some(CoverageLog {
        prg_flags,
        ram_exec,
        coproc_exec,
        other_exec,
        indirect,
        interrupts,
        windows,
        instructions_recorded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_core_api::coverage::{mode_bit, OPCODE, OPERAND};

    fn sample() -> CoverageLog {
        let mut log = CoverageLog::with_volatile_len(300_000, 128 * 1024);
        log.mark_prg(0, OPCODE | mode_bit(2));
        log.mark_prg(1, OPERAND);
        log.mark_prg(299_999, OPCODE);
        log.mark_ram(0x1234, OPCODE);
        let a = |cpu, rom| CodeAddr {
            cpu,
            pbr: 1,
            mode: 3,
            rom,
        };
        log.note_indirect(IndirectEdge {
            site: a(0x8000, Some(0)),
            target: a(0x9000, None),
            kind: IndirectKind::JsrIndirect,
        });
        log.note_interrupt(InterruptEntry {
            kind: InterruptKind::Nmi,
            target: a(0x8100, Some(0x100)),
        });
        let mut w = WindowMap::EMPTY;
        w.0[7] = 0x6000;
        log.note_windows(w);
        log.instructions_recorded = 77;
        log.coproc_exec = 3;
        log
    }

    #[test]
    fn a_log_round_trips_and_a_mostly_empty_one_is_small() {
        let log = sample();
        let bytes = encode_coverage(&log);
        assert!(bytes.len() < 300, "{} bytes", bytes.len());
        assert_eq!(decode_coverage(&bytes), Some(log));
    }

    #[test]
    fn damage_is_a_miss_not_a_hang_or_a_panic() {
        let bytes = encode_coverage(&sample());
        for cut in 0..bytes.len() {
            assert_eq!(decode_coverage(&bytes[..cut]), None, "cut at {cut}");
        }
        let mut bad = bytes.clone();
        bad[0] = b'X';
        assert_eq!(decode_coverage(&bad), None);
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(decode_coverage(&trailing), None);
        // A run longer than the array, and a zero run.
        let mut over = Vec::new();
        over.extend_from_slice(&MAGIC);
        over.push(VERSION);
        over.extend_from_slice(&4u64.to_le_bytes());
        over.extend_from_slice(&0u64.to_le_bytes());
        over.extend_from_slice(&[0; 24]);
        over.extend_from_slice(&[0, 5]);
        assert_eq!(decode_coverage(&over), None);
        let n = over.len();
        over[n - 1] = 0;
        assert_eq!(decode_coverage(&over), None);
        // Every one-byte flip of a valid entry parses or is refused.
        for i in 0..bytes.len() {
            let mut flipped = bytes.clone();
            flipped[i] ^= 0xFF;
            let _ = decode_coverage(&flipped);
        }
    }
}
