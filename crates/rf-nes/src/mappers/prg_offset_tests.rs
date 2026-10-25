//! Ticket W36-01: [`Mapper::prg_rom_offset`] must index the very byte
//! [`Mapper::cpu_read`] returns, under every bank configuration a game can
//! reach. The PRG image is scrambled so a wrong offset that lands on a
//! different bank matches by chance only 1 time in 256 per address, and
//! the check sweeps all 32 KiB of `$8000-$FFFF` after each bank change.
use rf_cart::Mirroring;

use super::{AxRom, Bnrom, Cnrom, Cprom, GxRom, Mapper, Mmc1, Mmc3, Mmc3Revision, Nrom, UxRom};

/// `len` bytes of deterministic noise (xorshift32), so banks differ.
fn scrambled(len: usize) -> Vec<u8> {
    let mut x: u32 = 0x1234_5678;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 11) as u8
        })
        .collect()
}

fn check(m: &dyn Mapper, prg: &[u8], what: &str) {
    for addr in 0x8000..=0xFFFFu16 {
        let off = m
            .prg_rom_offset(addr)
            .unwrap_or_else(|| panic!("{what}: no offset for {addr:04X}"));
        assert_eq!(prg[off], m.cpu_read(addr), "{what}: addr {addr:04X}");
    }
}

#[test]
fn nrom_16k_and_32k() {
    for len in [16 * 1024, 32 * 1024] {
        let prg = scrambled(len);
        let m = Nrom::new(prg.clone(), Mirroring::Vertical);
        check(&m, &prg, "nrom");
    }
    let prg = scrambled(16 * 1024);
    let m = Nrom::new(prg.clone(), Mirroring::Vertical);
    assert_eq!(m.prg_rom_offset(0xC000), m.prg_rom_offset(0x8000));
}

#[test]
fn cnrom_cprom_fixed_prg() {
    let prg = scrambled(32 * 1024);
    let m = Cnrom::new(prg.clone(), vec![0; 8192], false, Mirroring::Vertical);
    check(&m, &prg, "cnrom");
    let mut m = Cprom::new(prg.clone(), Mirroring::Vertical);
    check(&m, &prg, "cprom");
    m.cpu_write(0x8000, 3, 0);
    check(&m, &prg, "cprom after write");
}

#[test]
fn gxrom_follows_bank_switch() {
    let prg = scrambled(4 * 32 * 1024);
    let mut m = GxRom::new(prg.clone(), vec![0; 4 * 8192], false, Mirroring::Vertical);
    for bank in 0..4u8 {
        m.cpu_write(0x8000, bank << 4, 0);
        check(&m, &prg, "gxrom");
        assert_eq!(m.prg_rom_offset(0x8000), Some(usize::from(bank) * 0x8000));
    }
}

#[test]
fn uxrom_follows_bank_switch() {
    let prg = scrambled(8 * 16 * 1024);
    let mut m = UxRom::new(prg.clone(), Mirroring::Vertical);
    for bank in 0..8u8 {
        m.cpu_write(0x8000, bank, 0);
        check(&m, &prg, "uxrom");
        assert_eq!(m.prg_rom_offset(0x8000), Some(usize::from(bank) * 0x4000));
        assert_eq!(m.prg_rom_offset(0xC000), Some(7 * 0x4000));
    }
}

#[test]
fn axrom_and_bnrom_follow_bank_switch() {
    let prg = scrambled(4 * 32 * 1024);
    let mut a = AxRom::new(prg.clone());
    let mut b = Bnrom::new(prg.clone(), Vec::new(), true, Mirroring::Vertical);
    for bank in 0..4u8 {
        a.cpu_write(0x8000, bank, 0);
        b.cpu_write(0x8000, bank, 0);
        check(&a, &prg, "axrom");
        check(&b, &prg, "bnrom");
        assert_eq!(a.prg_rom_offset(0x8000), Some(usize::from(bank) * 0x8000));
        assert_eq!(b.prg_rom_offset(0x8000), Some(usize::from(bank) * 0x8000));
    }
}

/// One MMC1 serial-port write: five LSB-first bits, cycles spaced so the
/// consecutive-write filter never swallows one.
fn mmc1_write(m: &mut Mmc1, addr: u16, value: u8, base: u64) {
    for i in 0..5u64 {
        m.cpu_write(addr, (value >> i) & 1, base + i * 2);
    }
}

#[test]
fn mmc1_all_prg_modes_and_surom_half() {
    let prg = scrambled(32 * 16 * 1024); // 512 KiB: SUROM-shaped
    let mut m = Mmc1::new(prg.clone(), vec![0; 8 * 4096], false);
    let mut t = 100u64;
    for control in [0x00u8, 0x04, 0x08, 0x0C] {
        mmc1_write(&mut m, 0x8000, control, t);
        t += 100;
        for bank in [0u8, 1, 2, 5, 15] {
            for chr0 in [0u8, 0x10] {
                mmc1_write(&mut m, 0xA000, chr0, t);
                t += 100;
                mmc1_write(&mut m, 0xE000, bank, t);
                t += 100;
                check(&m, &prg, "mmc1");
            }
        }
    }
    // The upper half really is reachable.
    mmc1_write(&mut m, 0xA000, 0x10, t);
    assert!(m.prg_rom_offset(0xC000).unwrap() >= 256 * 1024);
}

#[test]
fn mmc3_both_prg_modes() {
    let prg = scrambled(16 * 8 * 1024);
    let mut m = Mmc3::new(
        prg.clone(),
        vec![0; 8 * 1024],
        false,
        Mirroring::Vertical,
        Mmc3Revision::B,
    );
    for mode in [0x00u8, 0x40] {
        for (r6, r7) in [(0u8, 1u8), (3, 9), (14, 2), (15, 15)] {
            m.cpu_write(0x8000, mode | 6, 0);
            m.cpu_write(0x8001, r6, 0);
            m.cpu_write(0x8000, mode | 7, 0);
            m.cpu_write(0x8001, r7, 0);
            check(&m, &prg, "mmc3");
        }
    }
}
