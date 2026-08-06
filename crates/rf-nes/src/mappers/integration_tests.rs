//! End-to-end, hand-assembled-6502-program fixtures for UxROM/CNROM/MMC1
//! (ticket W2-02 acceptance criterion 2) — the same technique
//! `crates/retroforge/tests/determinism.rs`/`crates/rf-nes/src/system/
//! tests/integration.rs` use: a real [`Cpu`] executes real machine code
//! that writes a mapper's own bank-select register(s), and the test
//! asserts on what became visible afterward.
//!
//! ## Why a local [`TestBus`], not the real [`crate::system::NesBus`]
//!
//! `NesBus::from_ines_bytes`/`NesRom::from_ines_bytes` can only ever
//! construct a mapper-0 (NROM) cartridge today: `system/cartridge.rs`'s
//! `if header.mapper != 0` gate rejects everything else, and that file is
//! outside this ticket's write_scope (`plan.json`'s W2-02 entry lists
//! `system/mod.rs`, not `system/cartridge.rs`) — see this ticket's own
//! notes for the full evidence. So there is no way to load a synthetic
//! MMC1/UxROM/CNROM `.nes` image through the production path yet; this
//! module runs the real [`Cpu`] and the real [`super::Mapper`]
//! implementations against the smallest bus that can, entirely within
//! this ticket's write_scope (`mappers/**`). This is deliberately NOT a
//! second iNES-parsing/loading path — there is no header parsing or
//! PRG/CHR slicing here at all, just RAM plus a mapper wired straight to
//! `$8000-$FFFF` — so it does not duplicate anything `system/cartridge.rs`
//! owns, and it does not exercise (or claim to exercise)
//! `NesBus::new`'s mapper-dispatch `match`, which has no test coverage
//! anywhere until that gate is fixed.
use crate::cpu::{Cpu, CpuBus};
use rf_cart::Mirroring;

use super::{Cnrom, Mapper, Mmc1, UxRom};

/// The smallest possible [`CpuBus`]: 2 KiB RAM at `$0000-$1FFF`, a
/// [`Mapper`] wired straight to `$8000-$FFFF`, everything else open
/// bus/dropped (no PPU/APU/controllers — none of this module's programs
/// touch them). `cycle` free-runs the same way
/// [`crate::system::NesBus::master_cycle`] does (incremented once per
/// bus access), which is what lets [`Mmc1`]'s consecutive-write-ignore
/// quirk see real, meaningful cycle numbers.
struct TestBus<M: Mapper> {
    cycle: u64,
    ram: [u8; 0x0800],
    mapper: M,
}

impl<M: Mapper> TestBus<M> {
    fn new(mapper: M) -> Self {
        TestBus {
            cycle: 0,
            ram: [0; 0x0800],
            mapper,
        }
    }
}

impl<M: Mapper> CpuBus for TestBus<M> {
    fn read(&mut self, addr: u16) -> u8 {
        let value = match addr {
            0x0000..=0x1FFF => self.ram[(addr as usize) & 0x07FF],
            0x8000..=0xFFFF => self.mapper.cpu_read(addr),
            _ => 0,
        };
        self.cycle += 1;
        value
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x1FFF => self.ram[(addr as usize) & 0x07FF] = value,
            0x8000..=0xFFFF => self.mapper.cpu_write(addr, value, self.cycle),
            _ => {}
        }
        self.cycle += 1;
    }
}

const PRG_BANK: usize = 16 * 1024;
const CHR_BANK: usize = 8 * 1024;

/// `banks` 16 KiB PRG banks, each marked with its own bank index at byte
/// 0, with `program` overlaid at the start of the LAST bank (`$C000` on
/// both UxROM and MMC1's default PRG mode 3 — both fix the last bank
/// there, so code living there is reachable no matter what the
/// switchable half is currently showing, the same convention real
/// UxROM/MMC1 games use).
fn prg_with_program_in_last_bank(banks: u8, program: &[u8]) -> Vec<u8> {
    let mut prg = vec![0u8; banks as usize * PRG_BANK];
    for n in 0..banks {
        prg[n as usize * PRG_BANK] = n;
    }
    let last = (banks as usize - 1) * PRG_BANK;
    prg[last..last + program.len()].copy_from_slice(program);
    prg
}

/// Runs `cpu` for exactly `steps` instructions against `bus`.
fn run(cpu: &mut Cpu, bus: &mut dyn CpuBus, steps: usize) {
    for _ in 0..steps {
        cpu.step(bus);
    }
}

#[test]
fn uxrom_program_selects_two_different_prg_banks_and_reads_each_back() {
    // LDA #$02 ; STA $8000 ; LDA $8000 ; STA $0010   -- select+read bank 2
    // LDA #$01 ; STA $8000 ; LDA $8000 ; STA $0011   -- select+read bank 1
    #[rustfmt::skip]
    let program: [u8; 22] = [
        0xA9, 0x02,             // LDA #$02
        0x8D, 0x00, 0x80,       // STA $8000
        0xAD, 0x00, 0x80,       // LDA $8000
        0x8D, 0x10, 0x00,       // STA $0010
        0xA9, 0x01,             // LDA #$01
        0x8D, 0x00, 0x80,       // STA $8000
        0xAD, 0x00, 0x80,       // LDA $8000
        0x8D, 0x11, 0x00,       // STA $0011
    ];
    let mapper = UxRom::new(
        prg_with_program_in_last_bank(4, &program),
        Mirroring::Horizontal,
    );
    let mut bus = TestBus::new(mapper);
    let mut cpu = Cpu::new();
    cpu.pc = 0xC000;
    run(&mut cpu, &mut bus, 8);

    assert_eq!(
        bus.ram[0x10], 2,
        "the program's own STA $8000 must have selected bank 2 -- a no-op mapper \
         (always bank 0) would leave this at 0"
    );
    assert_eq!(
        bus.ram[0x11], 1,
        "and re-selecting bank 1 must show bank 1's marker"
    );
}

#[test]
fn cnrom_program_writes_its_own_chr_bank_register_and_the_window_updates() {
    // PRG is fixed/unbanked on CNROM, so the program can live at $8000
    // directly with no bank-fixedness concerns.
    #[rustfmt::skip]
    let program: [u8; 5] = [
        0xA9, 0x02,       // LDA #$02
        0x8D, 0x00, 0x80, // STA $8000
    ];
    let mut prg = vec![0xEAu8; PRG_BANK]; // NOP-filled, program overlaid at 0
    prg[0..program.len()].copy_from_slice(&program);

    let mut chr = vec![0u8; 4 * CHR_BANK];
    for n in 0..4u8 {
        chr[n as usize * CHR_BANK] = 0x10 + n;
    }
    let mapper = Cnrom::new(prg, chr, false, Mirroring::Vertical);
    let mut bus = TestBus::new(mapper);
    let mut cpu = Cpu::new();
    cpu.pc = 0x8000;
    run(&mut cpu, &mut bus, 2);

    // CHR is not CPU-addressable on real hardware either -- the program
    // itself has no way to read back its own bank selection, so the
    // assertion inspects the mapper's materialized view directly, the
    // same value `crate::system::NesBus` would push into
    // `crate::ppu::Ppu`'s CHR buffer for the PPU to render from.
    assert_eq!(
        bus.mapper.chr_window().unwrap()[0],
        0x12,
        "the program's own STA $8000 must have selected CHR bank 2 -- a no-op mapper \
         (always bank 0) would still show bank 0's marker (0x10) here"
    );
}

#[test]
fn mmc1_program_selects_a_prg_bank_via_five_real_writes_and_reads_it_back() {
    // Five real STA $E000 writes (LDA #imm; STA $E000, x5), each pair
    // separated by the other pair's own cycles -- never two writes on
    // consecutive cycles -- exactly how real MMC1 software is written to
    // avoid the RMW ignore-quirk this ticket's mmc1.rs unit tests cover
    // directly. Bits (LSB first): 1,0,0,0,0 -> commits PRG Bank = 1.
    // PRG mode defaults to 3 (fixed last bank at $C000, switch $8000),
    // so a final LDA $8000 must read bank 1's marker.
    #[rustfmt::skip]
    let program: [u8; 31] = [
        0xA9, 0x01,       // LDA #$01
        0x8D, 0x00, 0xE0, // STA $E000
        0xA9, 0x00,       // LDA #$00
        0x8D, 0x00, 0xE0, // STA $E000
        0xA9, 0x00,       // LDA #$00
        0x8D, 0x00, 0xE0, // STA $E000
        0xA9, 0x00,       // LDA #$00
        0x8D, 0x00, 0xE0, // STA $E000
        0xA9, 0x00,       // LDA #$00
        0x8D, 0x00, 0xE0, // STA $E000
        0xAD, 0x00, 0x80, // LDA $8000
        0x8D, 0x10, 0x00, // STA $0010
    ];
    // 5 x (LDA #imm + STA abs) + (LDA abs + STA abs) = 5*2 + 2 = 12 instructions.
    const STEPS: usize = 12;

    let chr = vec![0u8; 2 * (4 * 1024)];
    let mapper = Mmc1::new(prg_with_program_in_last_bank(4, &program), chr, false);
    let mut bus = TestBus::new(mapper);
    let mut cpu = Cpu::new();
    cpu.pc = 0xC000;
    run(&mut cpu, &mut bus, STEPS);

    assert_eq!(
        bus.ram[0x10], 1,
        "five real writes to $E000 must have committed PRG Bank = 1 and $8000 must show \
         its marker -- a no-op mapper (always bank 0) would leave this at 0"
    );
}
