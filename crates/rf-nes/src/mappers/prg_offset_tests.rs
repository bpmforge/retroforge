//! Ticket W36-01: [`Mapper::prg_rom_offset`] must index the very byte
//! [`Mapper::cpu_read`] returns, under every bank configuration a game can
//! reach. The PRG image is scrambled so a wrong offset that lands on a
//! different bank matches by chance only 1 time in 256 per address, and
//! the check sweeps all 32 KiB of `$8000-$FFFF` after each bank change.
use rf_cart::Mirroring;

use super::{
    Action53, AxRom, Bnrom, Camerica, Cnrom, ColorDreams, Cprom, DxRom, Fme7, GxRom, JalecoJf,
    Mapper, Mmc1, Mmc2, Mmc3, Mmc3Revision, Mmc5, Nina, Nrom, Quattro, Rambo1, Sachen, Ss88006,
    Unrom512, UxRom,
};

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

// ---- W37-02: the remaining boards --------------------------------------

#[test]
fn camerica_unrom512_quattro_follow_bank_switch() {
    let prg = scrambled(16 * 16 * 1024);
    let mut c = Camerica::new(prg.clone(), Mirroring::Vertical);
    let mut u = Unrom512::new(prg.clone(), Mirroring::Horizontal);
    let mut q = Quattro::new(prg.clone(), Mirroring::Vertical);
    for bank in [0u8, 1, 5, 9, 15] {
        c.cpu_write(0xC000, bank, 0);
        u.cpu_write(0x8000, bank, 0);
        check(&c, &prg, "camerica");
        check(&u, &prg, "unrom512");
    }
    for block in 0..4u8 {
        for bank in 0..4u8 {
            q.cpu_write(0x8000, block << 3, 0);
            q.cpu_write(0xC000, bank, 0);
            check(&q, &prg, "quattro");
        }
    }
}

#[test]
fn whole_window_boards_follow_their_registers() {
    let prg = scrambled(4 * 32 * 1024);
    let mut cd = ColorDreams::new(prg.clone(), vec![0; 8192], false, Mirroring::Vertical);
    let mut dr =
        ColorDreams::new_death_race(prg.clone(), vec![0; 8192], false, Mirroring::Vertical);
    let mut nina = Nina::new(prg.clone(), vec![0; 8192], false, Mirroring::Vertical);
    for v in [0u8, 1, 2, 3, 0x13, 0xF2] {
        cd.cpu_write(0x8000, v, 0);
        dr.cpu_write(0x8000, v, 0);
        nina.cpu_write_expansion(0x4100, v);
        check(&cd, &prg, "color dreams");
        check(&dr, &prg, "death race");
        check(&nina, &prg, "nina");
    }
    let mut sa = Sachen::new(prg.clone(), vec![0; 8192], false, Mirroring::Vertical);
    for v in [0u8, 8, 0xFF] {
        sa.cpu_write(0x8000, v, 0);
        check(&sa, &prg, "sachen");
    }
    let jf = JalecoJf::new(prg.clone(), vec![0; 8192], false, Mirroring::Vertical);
    check(&jf, &prg, "jaleco jf");
}

#[test]
fn eight_k_window_boards_follow_their_registers() {
    let prg = scrambled(32 * 8 * 1024);
    let mut mmc2 = Mmc2::new(prg.clone(), vec![0; 32 * 4096]);
    let mut dx = DxRom::new(prg.clone(), vec![0; 64 * 1024], false, Mirroring::Vertical);
    let mut fme = Fme7::new(prg.clone(), vec![0; 64 * 1024], false);
    let mut ss = Ss88006::new(prg.clone(), vec![0; 64 * 1024], false);
    let mut rb = Rambo1::new(prg.clone(), vec![0; 64 * 1024], false);
    for (a, b, c) in [(0u8, 1u8, 2u8), (3, 7, 9), (15, 22, 31), (30, 17, 5)] {
        mmc2.cpu_write(0xA000, a & 0x0F, 0);
        check(&mmc2, &prg, "mmc2");
        for (reg, v) in [(6u8, a), (7, b)] {
            dx.cpu_write(0x8000, reg, 0);
            dx.cpu_write(0x8001, v, 0);
        }
        check(&dx, &prg, "dxrom");
        for (cmd, v) in [(9u8, a), (10, b), (11, c)] {
            fme.cpu_write(0x8000, cmd, 0);
            fme.cpu_write(0xA000, v, 0);
        }
        check(&fme, &prg, "fme7");
        for (lo, hi, v) in [
            (0x8000u16, 0x8001u16, a),
            (0x8002, 0x8003, b),
            (0x9000, 0x9001, c),
        ] {
            ss.cpu_write(lo, v & 0x0F, 0);
            ss.cpu_write(hi, v >> 4, 0);
        }
        check(&ss, &prg, "ss88006");
        for mode in [0x00u8, 0x40] {
            for (reg, v) in [(6u8, a), (7, b), (15, c)] {
                rb.cpu_write(0x8000, mode | reg, 0);
                rb.cpu_write(0x8001, v, 0);
            }
            check(&rb, &prg, "rambo1");
        }
    }
}

#[test]
fn action53_every_prg_mode_and_outer_size() {
    let prg = scrambled(32 * 16 * 1024);
    let mut m = Action53::new(prg.clone());
    let mut reg = |m: &mut Action53, select: u8, value: u8| {
        m.cpu_write_expansion(0x5000, select);
        m.cpu_write(0x8000, value, 0);
    };
    // `$80` mode (PRG mode bits 3-2, outer size bits 5-4), `$81` outer,
    // `$01` inner.
    for mode in 0..0x40u8 {
        reg(&mut m, 0x80, mode);
        for outer in [0u8, 1, 2, 7, 0x3F] {
            for inner in [0u8, 1, 6, 0xF] {
                reg(&mut m, 0x81, outer);
                reg(&mut m, 0x01, inner);
                check(&m, &prg, "action53");
            }
        }
    }
}

#[test]
fn mmc5_all_prg_modes_and_ram_windows() {
    let prg = scrambled(64 * 8 * 1024);
    let mut m = Mmc5::new(prg.clone(), vec![0; 64 * 1024], false);
    for mode in 0..4u8 {
        m.cpu_write_expansion(0x5100, mode);
        for regs in [
            [0x80u8, 0x81, 0x82, 0x83],
            [0x84, 0x8B, 0xA2, 0xBF],
            [0x00, 0x91, 0xC2, 0xFF],
        ] {
            for (i, r) in regs.into_iter().enumerate() {
                m.cpu_write_expansion(0x5114 + i as u16, r);
            }
            for addr in 0x8000..=0xFFFFu16 {
                match m.prg_ram_window(addr) {
                    // RAM selected: not ROM, the bus never asks, and the
                    // mapper does not claim a ROM offset for it.
                    Some(_) => assert_eq!(m.prg_rom_offset(addr), None, "mmc5 ram {addr:04X}"),
                    None => {
                        let off = m.prg_rom_offset(addr).expect("a ROM window has an offset");
                        assert_eq!(prg[off], m.cpu_read(addr), "mmc5 m{mode} {addr:04X}");
                    }
                }
            }
        }
    }
}
