//! CPU<->APU ports, the boot handshake, and the S-DSP skeleton
//! (ticket W6-04b).

use crate::apu::boot::{
    BootState, BYTE_HANDSHAKE_CYCLES, IPL_INIT_CYCLES, RUN_HANDOFF_AFTER_TRANSFER_CYCLES,
    RUN_HANDOFF_IMMEDIATE_CYCLES,
};
use crate::apu::dsp::{decode_brr, Dsp};
use crate::apu::spc700::flags;
use crate::apu::Apu;
// `read`/`write` are ApuBus methods; the trait must be in scope to call them.
use crate::apu::spc700::ApuBus;
use crate::cpu::CpuBus;
use crate::system::SnesSystem;
use rf_cart::SnesMapMode;

/// Write a port, then let the APU look.
///
/// **The handshake advances on the APU's clock, not on the CPU's store**
/// (see `IplBoot::poll`), so a test that writes a port and asserts
/// immediately is asserting against a machine that has not run yet. The
/// separation is not pedantry: it is what lets a 16-bit `STA $2140` — two
/// byte writes, port 0 first — be seen as one settled state rather than
/// evaluated half-done, which is what broke blargg's entire SPC suite.
fn write_port(apu: &mut Apu, index: usize, value: u8) {
    apu.cpu_write_port(index, value);
    apu.poll_boot();
    // The HLE walks the boot ROM's listing at its documented cycle
    // costs (W14-37, W14-48), so a write is only acted on when the ROM's
    // next read of that port completes. Drain `SETTLE_CYCLES` so the
    // helper keeps its "assert against a machine that has run" contract.
    for _ in 0..SETTLE_CYCLES {
        apu.poll_boot();
    }
}

/// Enough SPC cycles for any single step of the handshake to settle: the
/// longest listed tail (`RUN_HANDOFF_AFTER_TRANSFER_CYCLES`, 45) plus one
/// lap of the slowest polling loop the CPU's write can land in (the
/// `$FFDA` wait, 11 cycles), rounded up. Since W14-48 the HLE walks the
/// listing, so a write is only seen when the ROM's next compare reads it.
const SETTLE_CYCLES: u16 = 64;

// ---------------------------------------------------------------------
// The handshake
// ---------------------------------------------------------------------

/// Step 1: the APU publishes `$AA`/`$BB` before anything else.
///
/// A CPU that never sees this pair concludes there is no APU, so this is
/// the one part of the protocol that must be true at reset with no
/// prompting.
#[test]
fn the_apu_publishes_aa_bb_at_reset() {
    let apu = Apu::new();
    assert_eq!(apu.cpu_read_port(0), 0xAA);
    assert_eq!(apu.cpu_read_port(1), 0xBB);
}

/// Drive the documented sequence directly and assert the upload lands.
#[test]
fn the_handshake_transfers_a_block_and_runs_it() {
    let mut apu = Apu::new();
    let program: [u8; 4] = [0x11, 0x22, 0x33, 0x44];
    let dest: u16 = 0x0200;

    // Step 2: kind != 0, destination in ports 2/3, then $CC in port 0.
    write_port(&mut apu, 1, 0x01);
    write_port(&mut apu, 2, dest as u8);
    write_port(&mut apu, 3, (dest >> 8) as u8);
    write_port(&mut apu, 0, 0xCC);
    assert_eq!(apu.cpu_read_port(0), 0xCC, "the APU echoes $CC");

    // Step 3: data on port 1, incrementing counter on port 0.
    for (i, b) in program.iter().enumerate() {
        write_port(&mut apu, 1, *b);
        write_port(&mut apu, 0, i as u8);
        assert_eq!(
            apu.cpu_read_port(0),
            i as u8,
            "the counter echo is the acknowledgement both sides step on"
        );
    }
    assert_eq!(apu.boot.transferred, 4);
    assert_eq!(&apu.aram[0x0200..0x0204], &program);

    // Step 4/5: port 1 = 0 finishes, entry in ports 2/3, counter skips
    // by two.
    let entry: u16 = 0x0200;
    write_port(&mut apu, 1, 0x00);
    write_port(&mut apu, 2, entry as u8);
    write_port(&mut apu, 3, (entry >> 8) as u8);
    write_port(&mut apu, 0, 4 + 1);

    assert!(apu.boot.is_running(), "control handed to the SPC700");
    assert_eq!(apu.cpu.pc, entry, "...at the address the CPU supplied");
    // The IPL stays banked in: hardware leaves `$F1` bit 7 set until the
    // program clears it (ticket W14-10 — this used to assert the
    // opposite, and that is what broke every title that reboots the APU
    // by jumping to `$FFC0`).
    assert!(
        apu.ipl_enabled,
        "the hand-over must not bank the IPL out; only a $F1 write does"
    );
}

/// **A commanded jump to `$FFC0` reboots into the handshake** (ticket
/// W14-10). Wild Guns uploads a block and then, instead of running it,
/// jumps to the boot ROM's entry to start over; the IPL republishes
/// `$AA`/`$BB` and the 65816 uploads the real driver.
///
/// Shaped like the trace: the jump arrives as a block-ending counter
/// (not a fresh `$CC`), so the re-armed handshake waits rather than
/// re-running the same command — exactly as the real IPL, which polls
/// for `$CC`, would.
#[test]
fn a_jump_to_ffc0_re_enters_the_boot_handshake() {
    let mut apu = Apu::new();
    write_port(&mut apu, 1, 0x01);
    write_port(&mut apu, 2, 0x00);
    write_port(&mut apu, 3, 0x02);
    write_port(&mut apu, 0, 0xCC);
    write_port(&mut apu, 1, 0x5A);
    write_port(&mut apu, 0, 0x00); // one byte to $0200
                                   // Kind 0, entry $FFC0, counter skip: "run" — at the boot ROM.
    write_port(&mut apu, 1, 0x00);
    write_port(&mut apu, 2, 0xC0);
    write_port(&mut apu, 3, 0xFF);
    write_port(&mut apu, 0, 0x02);
    assert!(apu.boot.is_running());
    assert_eq!(apu.cpu.pc, 0xFFC0);
    assert!(apu.ipl_enabled, "the window is still mapped at the jump");
    // One SPC700 step lands in the IPL window and re-arms the handshake.
    let _ = apu.step_counted();
    assert!(
        !apu.boot.is_running(),
        "the handshake owns the machine again"
    );
    // But the boot ROM is clearing zero page: the jump's echo is still
    // on port 0 for the CPU to read, and $AA/$BB come only after the
    // documented init.
    assert_eq!(apu.cpu_read_port(0), 0x02, "the echo survives re-entry");
    for _ in 0..100 {
        apu.poll_boot();
    }
    assert_eq!(
        apu.cpu_read_port(0),
        0x02,
        "and is still there 100 cycles in"
    );
    // The init, then the two `MOV dp,#imm` that publish (10 cycles).
    for _ in 0..IPL_INIT_CYCLES + 10 {
        apu.poll_boot();
    }
    assert_eq!(apu.boot.state, BootState::Ready);
    assert_eq!(apu.cpu_read_port(0), 0xAA);
    assert_eq!(apu.cpu_read_port(1), 0xBB);
}

/// Start a one-block upload to `dest` and send `bytes` the documented way
/// (data on port 1, then counter on port 0, from 0).
fn upload(apu: &mut Apu, dest: u16, bytes: &[u8]) {
    write_port(apu, 1, 0x01);
    write_port(apu, 2, dest as u8);
    write_port(apu, 3, (dest >> 8) as u8);
    write_port(apu, 0, 0xCC);
    for (i, b) in bytes.iter().enumerate() {
        write_port(apu, 1, *b);
        write_port(apu, 0, i as u8);
    }
}

/// The counter SKIP is what distinguishes "new block" from "next byte" —
/// the single easiest part of this protocol to get wrong.
#[test]
fn the_counter_distinguishes_the_next_byte_from_a_new_block() {
    let mut apu = Apu::new();
    upload(&mut apu, 0x0300, &[0xAB, 0xCD]);
    assert_eq!(&apu.aram[0x0300..0x0302], &[0xAB, 0xCD]);
    assert_eq!(apu.boot.state, BootState::Transferring(2));

    // Counter skipping to 3 (expected is 2) = a new block at ports 2/3.
    write_port(&mut apu, 1, 0x01);
    write_port(&mut apu, 2, 0x00);
    write_port(&mut apu, 3, 0x08);
    write_port(&mut apu, 0, 3);
    assert_eq!(apu.cpu_read_port(0), 3, "the kick is echoed");
    assert_eq!(apu.boot.state, BootState::AwaitingBlock(3));
    assert_eq!(
        &apu.aram[0x0000..0x0002],
        &[0x00, 0x08],
        "the listing keeps the new block's address at $00/$01 (MOVW $00,YA)"
    );
    // The new block starts again from counter 0.
    write_port(&mut apu, 1, 0xEE);
    write_port(&mut apu, 0, 0);
    assert_eq!(apu.aram[0x0800], 0xEE);
}

/// Ticket W14-06: the skip is a MISMATCH, not a particular distance.
/// Super Mario World acknowledged counter `$3D` and then wrote **`$41`**
/// — four past it. The listing's rule (`CMP Y,$F4` then `BPL` back to
/// polling, twice) is signed: a block ends when port 0 is 1-128 counts
/// AHEAD of the expected counter, and only then (W14-48).
#[test]
fn any_counter_ahead_of_the_expected_one_starts_a_new_block() {
    for jump in [2u8, 3, 4, 9, 64, 128, 129] {
        let mut apu = Apu::new();
        upload(&mut apu, 0x0300, &[0xAB]);
        assert_eq!(apu.boot.state, BootState::Transferring(1));

        write_port(&mut apu, 1, 0x01);
        write_port(&mut apu, 2, 0x70);
        write_port(&mut apu, 3, 0x55);
        write_port(&mut apu, 0, jump);
        assert_eq!(
            apu.boot.state,
            BootState::AwaitingBlock(jump),
            "a jump to {jump} must start a new block"
        );
        write_port(&mut apu, 1, 0xCD);
        write_port(&mut apu, 0, 0);
        assert_eq!(
            apu.aram[0x5570], 0xCD,
            "jump {jump}: new block's first byte"
        );
    }
}

/// The other half of the signed rule: a counter BEHIND the expected one
/// is not a block boundary. That includes the value just acknowledged,
/// which `poll` sees again and again until the CPU writes the next one —
/// treating it as a mismatch would end every block after its first byte.
#[test]
fn a_counter_behind_the_expected_one_is_not_a_block_boundary() {
    let mut apu = Apu::new();
    upload(&mut apu, 0x0300, &[0xAB]);
    for behind in [0u8, 0xF0, 0x82] {
        write_port(&mut apu, 1, 0x00);
        write_port(&mut apu, 0, behind);
        assert_eq!(
            apu.boot.state,
            BootState::Transferring(1),
            "port 0 = {behind:#04X} is behind counter 1 and must be ignored"
        );
    }
    // And the real next byte still lands.
    write_port(&mut apu, 1, 0x77);
    write_port(&mut apu, 0, 1);
    assert_eq!(apu.aram[0x0301], 0x77);
}

/// **Port 1 is read three cycles after the counter matches, not with it**
/// (ticket W14-48). Shaped like Urban Strike's uploader (synthetic bytes,
/// law 5): the counter goes to `$2140` FIRST and its data byte to `$2141`
/// two SPC cycles later. The real ROM's `MOV A,$F5` comes after the
/// matching `CMP Y,$F4` and a `BNE`, so it sees the new byte. Sampling all
/// ports at the compare stored the PREVIOUS port 1 — the kind byte `$01`
/// first — and shifted the whole upload by one.
#[test]
fn a_counter_written_before_its_data_byte_still_stores_the_data() {
    let mut apu = Apu::new();
    write_port(&mut apu, 1, 0x01);
    write_port(&mut apu, 2, 0x60);
    write_port(&mut apu, 3, 0x04);
    write_port(&mut apu, 0, 0xCC);
    let program = [0x20u8, 0xCD, 0xFF, 0xBD, 0xE8, 0x00];
    for (i, b) in program.iter().enumerate() {
        apu.cpu_write_port(0, i as u8);
        apu.poll_boot();
        apu.poll_boot();
        apu.cpu_write_port(1, *b);
        for _ in 0..SETTLE_CYCLES {
            apu.poll_boot();
        }
    }
    assert_eq!(
        &apu.aram[0x0460..0x0466],
        &program,
        "every byte at its own address, none shifted by the stale port 1"
    );
}

/// A block's first byte is counter **0**: after echoing `$CC` the ROM
/// spins on `MOV Y,$F4 / BNE` until port 0 reads zero. The snapshot model
/// read the `$CC` still sitting in port 0 one cycle after its own echo as
/// a block-ending mismatch (W14-48 trace: `Echo(CC) st=AwaitingBlock(CC)`
/// one poll after `Transferring(0)`).
#[test]
fn a_stale_cc_after_its_echo_is_waited_out_not_treated_as_a_block_end() {
    let mut apu = Apu::new();
    write_port(&mut apu, 1, 0x01);
    write_port(&mut apu, 2, 0x00);
    write_port(&mut apu, 3, 0x02);
    write_port(&mut apu, 0, 0xCC);
    assert_eq!(apu.cpu_read_port(0), 0xCC);
    for _ in 0..500 {
        apu.poll_boot();
    }
    assert_eq!(apu.boot.state, BootState::AwaitingBlock(0xCC));
    assert_eq!(
        apu.boot.transferred, 0,
        "nothing stored while port 0 is $CC"
    );
    write_port(&mut apu, 1, 0x5A);
    write_port(&mut apu, 0, 0);
    assert_eq!(apu.aram[0x0200], 0x5A);
}

/// The hand-over leaves the register state the listing does: `A` = `X`
/// = `Y` = 0 (the zero "kind", via `MOV A,Y / MOV X,A`), `SP` = `$EF` (from
/// power-on, not forced at the jump), `Z` set by
/// `MOV X,A`, and the entry address at `$00`/`$01`.
#[test]
fn the_hand_over_leaves_the_roms_register_state() {
    let mut apu = Apu::new();
    upload(&mut apu, 0x0400, &[0x00, 0x00]);
    write_port(&mut apu, 1, 0x00);
    write_port(&mut apu, 2, 0x00);
    write_port(&mut apu, 3, 0x04);
    write_port(&mut apu, 0, 0x04);
    assert!(apu.boot.is_running());
    assert_eq!(apu.cpu.pc, 0x0400);
    assert_eq!(
        (apu.cpu.a, apu.cpu.x, apu.cpu.y, apu.cpu.sp),
        (0, 0, 0, 0xEF),
        "A = Y = the zero kind (`MOV A,Y`), X = A (`MOV X,A`), SP from power-on"
    );
    assert!(apu.cpu.flag(flags::Z) && !apu.cpu.flag(flags::N));
    assert_eq!(&apu.aram[0x0000..0x0002], &[0x00, 0x04]);
    assert_eq!(apu.cpu_read_port(0), 0x04, "the kick was echoed");
}

/// A zero "kind" byte with the very first `$CC` means run immediately,
/// with nothing transferred.
#[test]
fn a_zero_kind_byte_runs_without_transferring() {
    let mut apu = Apu::new();
    write_port(&mut apu, 1, 0x00);
    write_port(&mut apu, 2, 0x00);
    write_port(&mut apu, 3, 0x04);
    write_port(&mut apu, 0, 0xCC);
    assert!(apu.boot.is_running());
    assert_eq!(apu.cpu.pc, 0x0400);
    assert_eq!(apu.boot.transferred, 0);
}

/// Once the handshake hands over, port writes are plain data — the boot
/// state machine must not keep interpreting them.
#[test]
fn port_writes_after_boot_are_not_reinterpreted_as_protocol() {
    let mut apu = Apu::new();
    write_port(&mut apu, 1, 0x00);
    write_port(&mut apu, 0, 0xCC);
    assert!(apu.boot.is_running());
    let pc = apu.cpu.pc;
    write_port(&mut apu, 0, 0xCC);
    write_port(&mut apu, 1, 0x01);
    assert_eq!(apu.cpu.pc, pc, "a later $CC must not restart anything");
}

// ---------------------------------------------------------------------
// Catch-up sync
// ---------------------------------------------------------------------

fn system() -> SnesSystem {
    SnesSystem::from_rom(vec![0xEA; 32 * 1024], SnesMapMode::LoRom, 0)
}

/// **Never free-running.** A port access settles the APU's debt first, so
/// the CPU can only observe a state the APU actually reached.
#[test]
fn a_port_access_catches_the_apu_up_first() {
    let mut s = system();
    s.bus.apu_debt = 21 * 40;
    let _ = s.bus.read(0x00_2140);
    assert!(
        s.bus.apu_debt < 21,
        "the debt must be settled by the access, leaving only a partial cycle"
    );
}

/// `peek` must NOT catch up — a debugger reading a port would otherwise
/// advance the APU and change the machine it is inspecting.
#[test]
fn peeking_a_port_does_not_advance_the_apu() {
    let mut s = system();
    s.bus.apu_debt = 21 * 40;
    let before = s.bus.apu_debt;
    let _ = s.bus.peek(0x00_2140);
    assert_eq!(s.bus.apu_debt, before);
}

/// Real 65816 code performing a commercial-style init must complete the
/// handshake.
#[test]
fn real_65816_code_completes_the_boot_handshake() {
    // Poll $2140 for $AA, then drive the sequence. Written as machine
    // code for the same reason the DMA wired-path test is: the claim
    // being made is about what happens when a GAME does this, not about
    // what happens when a unit test calls the API.
    let mut rom = vec![0xEAu8; 32 * 1024];
    let program: &[u8] = &[
        // wait: LDA $2140 : CMP #$AA : BNE wait
        0xAD, 0x40, 0x21, 0xC9, 0xAA, 0xD0, 0xF9,
        // LDA #$01 : STA $2141      (kind != 0)
        0xA9, 0x01, 0x8D, 0x41, 0x21, // LDA #$00 : STA $2142      (dest low)
        0xA9, 0x00, 0x8D, 0x42, 0x21, // LDA #$02 : STA $2143      (dest high -> $0200)
        0xA9, 0x02, 0x8D, 0x43, 0x21, // LDA #$CC : STA $2140      (start)
        0xA9, 0xCC, 0x8D, 0x40, 0x21,
        // wait: LDA $2140 : CMP #$CC : BNE wait   (the echo — W14-48: the
        // HLE walks the listing now, so a CPU that overwrites port 0
        // before the ROM's next compare reads it loses the write, exactly
        // as on hardware; real uploaders always wait here)
        0xAD, 0x40, 0x21, 0xC9, 0xCC, 0xD0, 0xF9, // LDA #$5A : STA $2141      (data)
        0xA9, 0x5A, 0x8D, 0x41, 0x21, // LDA #$00 : STA $2140      (counter 0 -> store)
        0xA9, 0x00, 0x8D, 0x40, 0x21, // wait: LDA $2140 : CMP #$00 : BNE wait
        0xAD, 0x40, 0x21, 0xC9, 0x00, 0xD0, 0xF9,
        // LDA #$00 : STA $2141      (kind 0 = finish)
        0xA9, 0x00, 0x8D, 0x41, 0x21, // LDA #$02 : STA $2143      (entry high -> $0200)
        0xA9, 0x02, 0x8D, 0x43, 0x21,
        // LDA #$02 : STA $2140      (counter SKIP -> run)
        //
        // Two, not one. After the byte acknowledged with counter 0 the
        // APU expects 1 for the next byte, so finishing means skipping
        // to 2. Writing 1 here stores another byte instead and the
        // handshake never ends — which is exactly what this test caught
        // on its first run.
        0xA9, 0x02, 0x8D, 0x40, 0x21,
        // LDA $2140                 (let the APU actually run)
        //
        // NOT padding. The handshake advances on the APU's CLOCK now, not
        // on the CPU's store (`IplBoot::poll`), so the CPU has to give the
        // APU at least one tick after the final write for it to be seen.
        // A port access settles the APU's debt, which is why a read does
        // it. Real init code always goes on to do something; only a test
        // writes the last byte and halts on the next instruction.
        0xAD, 0x40, 0x21, 0xDB, // STP
    ];
    rom[..program.len()].copy_from_slice(program);
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;

    let mut s = SnesSystem::from_rom(rom, SnesMapMode::LoRom, 0);
    s.run_until(10_000, None).expect("implemented");

    assert!(s.cpu.stopped, "the init routine reached its STP");
    // W14-37/W14-48: the final counter write does not resolve the
    // instant it lands. The ROM finishes its per-byte loop
    // (`BYTE_HANDSHAKE_CYCLES`, 25) before its next compare reads port 0,
    // and THEN runs its post-detection tail
    // (`RUN_HANDOFF_AFTER_TRANSFER_CYCLES`, 45).
    // `run_until` stops the INSTANT the CPU hits its `STP`, well before
    // that many real cycles accrue from the handful of instructions
    // between the final write and the `STP`, and a stopped 65816 does
    // not drive the master clock forward on its own — so hand the APU
    // clock the cycles no CPU instruction produced, the same technique
    // `apu_debt_past_the_per_call_bound_is_carried_not_dropped` already
    // uses. One cycle of debt per call, not one big lump: `catch_up_apu`
    // pays down any already-overspent debt from the CPU's own
    // instructions first, so a single large injection can be partly
    // absorbed by that instead of turning into new forward progress.
    let settle_budget =
        u64::from(BYTE_HANDSHAKE_CYCLES) + u64::from(RUN_HANDOFF_AFTER_TRANSFER_CYCLES) + 10;
    for _ in 0..settle_budget {
        if s.bus.apu.boot.is_running() {
            break;
        }
        s.bus.apu_debt += 21;
        s.bus.catch_up_apu();
    }
    assert!(
        s.bus.apu.boot.is_running(),
        "a game-shaped init sequence must complete the handshake"
    );
    assert_eq!(s.bus.apu.aram[0x0200], 0x5A, "the uploaded byte landed");
    // The SPC700 was handed `$0200` and has been RUNNING since (ticket
    // W14-09: the APU advances on the master clock, so by the time the
    // CPU reaches its STP the SPC has executed a few bytes of what it
    // was given). Before that ticket this asserted `pc == $0200`, which
    // only held because a CPU that stopped touching the ports stopped
    // the APU with it.
    assert!(
        (0x0200..0x0210).contains(&s.bus.apu.cpu.pc),
        "the APU started at $0200 and is executing from there, pc={:04X}",
        s.bus.apu.cpu.pc
    );
}

/// **The APU runs on the master clock, not on port traffic** (ticket
/// W14-09).
///
/// Upload a four-byte SPC program — `INCW $10 : BRA -4` — through the
/// real handshake, then have the 65816 spin on `NOP : BRA` and never
/// touch `$2140-$2143` again. The counter at `$0010` must keep climbing,
/// and at the hardware ratio: 10 SPC cycles per lap (INCW 6, BRA 4) is
/// 210 master cycles.
///
/// Before the fix this counter stayed at zero: the APU was advanced only
/// from the port arms, so a CPU that stopped talking to it stopped it.
#[test]
fn the_apu_keeps_running_while_the_cpu_never_touches_a_port() {
    let mut rom = vec![0xEAu8; 32 * 1024];
    let mut program: Vec<u8> = vec![
        // wait: LDA $2140 : CMP #$AA : BNE wait
        0xAD, 0x40, 0x21, 0xC9, 0xAA, 0xD0, 0xF9, // kind 1, dest $0200, start
        0xA9, 0x01, 0x8D, 0x41, 0x21, 0xA9, 0x00, 0x8D, 0x42, 0x21, 0xA9, 0x02, 0x8D, 0x43, 0x21,
        0xA9, 0xCC, 0x8D, 0x40, 0x21,
        // wait: LDA $2140 : CMP #$CC : BNE wait   (the start echo)
        0xAD, 0x40, 0x21, 0xC9, 0xCC, 0xD0, 0xF9,
    ];
    for (counter, byte) in [0x3Au8, 0x10, 0x2F, 0xFC].into_iter().enumerate() {
        let counter = counter as u8;
        // LDA #byte : STA $2141 : LDA #counter : STA $2140
        program.extend([
            0xA9, byte, 0x8D, 0x41, 0x21, 0xA9, counter, 0x8D, 0x40, 0x21,
        ]);
        // wait: LDA $2140 : CMP #counter : BNE wait
        program.extend([0xAD, 0x40, 0x21, 0xC9, counter, 0xD0, 0xF9]);
    }
    program.extend([
        // kind 0, entry $0200, counter skip to 5 -> run
        0xA9, 0x00, 0x8D, 0x41, 0x21, 0xA9, 0x00, 0x8D, 0x42, 0x21, 0xA9, 0x02, 0x8D, 0x43, 0x21,
        0xA9, 0x05, 0x8D, 0x40, 0x21, // loop: NOP : BRA loop   (no port access ever again)
        0xEA, 0x80, 0xFD,
    ]);
    let loop_pc = 0x8000 + program.len() as u16 - 3;
    rom[..program.len()].copy_from_slice(&program);
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;

    let mut s = SnesSystem::from_rom(rom, SnesMapMode::LoRom, 0);
    s.run_until(50_000, Some(loop_pc)).expect("implemented");
    assert_eq!(
        s.cpu.pc, loop_pc,
        "the upload must complete and reach the silent loop"
    );
    // W14-37: the CPU reaches its silent `NOP : BRA` loop only a handful
    // of its own instructions after the final counter write, which is not
    // enough real time for the boot ROM's documented post-detection tail
    // (`RUN_HANDOFF_AFTER_TRANSFER_CYCLES`, 45 SPC cycles) to have run
    // out yet. Unlike the STP tests, the 65816 is still very much running
    // here, so let it keep looping a little longer — cheap, and it is
    // exactly the settling the two `run_until` calls below already do at
    // a larger scale.
    s.run_until(500, None).expect("implemented");
    assert!(s.bus.apu.boot.is_running());
    assert!(
        (0x0200..0x0204).contains(&s.bus.apu.cpu.pc),
        "the SPC runs the uploaded program"
    );

    let laps =
        |s: &SnesSystem| u32::from(s.bus.apu.aram[0x10]) | (u32::from(s.bus.apu.aram[0x11]) << 8);
    // Let the SPC settle into the loop, then measure a span.
    s.run_until(2_000, None).expect("implemented");
    let (laps0, master0) = (laps(&s), s.master_cycles);
    s.run_until(5_000, None).expect("implemented");
    let (laps1, master1) = (laps(&s), s.master_cycles);
    let expected = (master1 - master0) / 210;
    let got = u64::from(laps1 - laps0);
    assert!(got > 0, "the SPC made no progress without port traffic");
    assert!(
        got >= expected * 3 / 4 && got <= expected * 5 / 4,
        "SPC ran {got} laps over {} master cycles; the hardware ratio predicts {expected}",
        master1 - master0
    );
}

/// **Debt past the per-call bound is carried, never dropped** (ticket
/// W14-09). The old `min(64)` subtracted the whole debt and then ran 64
/// cycles of it, which is how a two-vblank wait cost the APU nothing.
#[test]
fn apu_debt_past_the_per_call_bound_is_carried_not_dropped() {
    let mut s = system();
    let bound: u64 = 1 << 16;
    s.bus.apu_debt = 21 * (bound + 1000);
    s.bus.catch_up_apu();
    assert!(
        s.bus.apu_debt >= 21 * 1000 - 21 * 16,
        "the excess must survive the call (within one overspent instruction), got {}",
        s.bus.apu_debt
    );
    s.bus.catch_up_apu();
    assert!(s.bus.apu_debt < 21, "and be settled by the next one");
}

/// **A large post-hand-over catch-up burst must not let the freshly-run
/// program clobber its own `Run` echo** (ticket W14-39 follow-up; traced
/// against Tommy Moe's Winter Extreme in `docs/TESTING.md`).
///
/// `BootAction::Run`'s doc calls the echo it publishes on port 0 "not
/// optional and not cosmetic" — the 65816 is spinning on `CMP $2140`
/// waiting for it — and `IPL_INIT_CYCLES` already exists to stop the
/// SPC700's own next instructions from overwriting it for a re-entry at
/// `$FFC0`. That guard never covered the FIRST hand-off to an arbitrary
/// uploaded entry point: before this fix, `catch_up_apu`'s loop kept
/// spending the SAME call's leftover budget on the just-woken SPC700
/// once `poll_boot` fired `Run`, so a large enough debt ran the
/// uploaded program's own first instruction (here, `MOV $F4,#$F1` —
/// exactly Tommy Moe's driver's entry code) before the 65816's next
/// instruction could ever read the echo. Before this ticket a plain
/// `STA $2140`'s access-only debt was rarely big enough to trigger it;
/// W14-39's corrected (larger) per-instruction charge makes it routine.
#[test]
fn a_large_catch_up_burst_does_not_let_the_freshly_run_program_clobber_its_echo() {
    let mut s = system();
    let dest: u16 = 0x0200;
    // MOV $F4, #$F1 (the exact clobber Tommy Moe's driver performs at
    // its entry point), then NOPs so a generous budget has somewhere
    // harmless to spend the rest of its cycles.
    let program: [u8; 4] = [0x8F, 0xF1, 0xF4, 0x00];

    // The HLE only sees a write when the listed instruction that reads
    // that port completes (W14-37, W14-48). A setup helper that polled
    // once per write would overwrite port 0 before the ROM read it,
    // mangling the transfer exactly as it would on hardware. Match the
    // crate's own `write_port` helper: drain `SETTLE_CYCLES` per write.
    let write = |s: &mut SnesSystem, index: usize, value: u8| {
        s.bus.apu.cpu_write_port(index, value);
        s.bus.apu.poll_boot();
        for _ in 0..SETTLE_CYCLES {
            s.bus.apu.poll_boot();
        }
    };
    write(&mut s, 1, 0x01);
    write(&mut s, 2, dest as u8);
    write(&mut s, 3, (dest >> 8) as u8);
    write(&mut s, 0, 0xCC);
    for (i, b) in program.iter().enumerate() {
        write(&mut s, 1, *b);
        write(&mut s, 0, i as u8);
    }
    write(&mut s, 1, 0x00);
    write(&mut s, 2, dest as u8);
    write(&mut s, 3, (dest >> 8) as u8);
    // The counter-skip write that fires `Run` — stored but NOT polled,
    // exactly as `SnesBus::write_register`'s `$2140` arm does: a real
    // write's own `catch_up_apu()` call runs on the STALE port state,
    // and the fresh value is only seen the NEXT time something catches
    // the APU up — here, the single `catch_up_apu()` call below, mirroring
    // `SnesSystem::step`'s unconditional post-instruction catch-up.
    let run_echo = program.len() as u8 + 1;
    s.bus.apu.cpu_write_port(0, run_echo);

    // A large debt: what W14-39's corrected per-instruction charge hands
    // `catch_up_apu` after a heavier CPU instruction, not the handful of
    // access-only cycles a plain `STA` used to leave it. Since W14-37,
    // `Run` is not delivered the instant the ROM sees the counter skip:
    // the boot ROM's own listed instruction tail
    // (`RUN_HANDOFF_AFTER_TRANSFER_CYCLES` = 45, `boot.rs`) is paid out of
    // this exact call's SPC-cycle budget one poll per cycle — so the
    // budget must cover the full 45 cycles before the hand-over can
    // complete in a single call at all, with plenty left over to prove
    // the freshly-woken SPC700 still does not get to spend it.
    s.bus.apu_debt = 21 * 80;
    s.bus.catch_up_apu();

    assert!(
        s.bus.apu.boot.is_running(),
        "the hand-over itself must still happen this call"
    );
    assert_eq!(
        s.bus.apu.cpu_read_port(0),
        run_echo,
        "the 65816's next read must see the Run echo ({run_echo:#04X}), not \
         the uploaded program's own first port write ({:#04X})",
        s.bus.apu.cpu_read_port(0)
    );
}

/// **A CPU port READ that immediately follows the hand-over must not see
/// the freshly-run program's own first instruction either** (ticket
/// W14-41, Tommy Moe's Winter Extreme's residual — `docs/TESTING.md`'s
/// "second, deeper reason" section).
///
/// W14-39's follow-up fixed the hand-over CALL itself (the test above):
/// `catch_up_apu` now stops the instant `poll_boot` fires `BootAction::
/// Run`, carrying its leftover budget to the next call instead of
/// spending it on the freshly-woken SPC700 in the same call. But the
/// 65816's own very next instruction, per Tommy Moe's exact driver shape
/// (`$80:B8C5 CMP $2140`), is a bus **read** of that same port, and
/// `SnesBus`'s `$2140` read arm calls `catch_up_apu` again before
/// returning a value — with only that tiny carried remainder as debt.
/// Before this ticket, any nonzero owed budget once `boot.is_running()`
/// unconditionally committed the loop to running one whole SPC700
/// instruction (instructions cannot run partially) — here, the uploaded
/// driver's own entry point, `MOV $F4,#$F1` (Tommy Moe's exact opcode),
/// clobbering the `Run` echo the CPU's read was there to observe.
///
/// This test reproduces the shape from `docs/TESTING.md` exactly: the
/// uploaded SPC700 program is
/// ```text
/// $0200: MOV $F4,#$F1   ; 8F F1 F4
/// $0203: MOV $F5,#$F1   ; 8F F1 F5
/// $0206: MOV A,$F4      ; E4 F4
/// $0208: CMP A,#$FF     ; 68 FF
/// $020A: BNE $0200      ; D0 F4
/// ```
/// and, after the hand-over call leaves a small carried remainder (as
/// W14-39's fix does), a SEPARATE `catch_up_apu()` call — standing in for
/// the CPU's `CMP $2140` read, which on real hardware happens before any
/// further real time has elapsed for the SPC700 — must still observe the
/// `Run` echo, not `$F1`. Only once further real CPU time (more
/// `apu_debt`) actually elapses does the SPC's first instruction get to
/// run and the port then reads back the driver's own value.
#[test]
fn a_port_read_immediately_after_hand_over_does_not_see_the_next_instruction_early() {
    let build = || {
        let mut s = system();
        let dest: u16 = 0x0200;
        // The exact Tommy Moe's driver shape from docs/TESTING.md.
        let program: [u8; 10] = [
            0x8F, 0xF1, 0xF4, // MOV $F4, #$F1
            0x8F, 0xF1, 0xF5, // MOV $F5, #$F1
            0xE4, 0xF4, // MOV A, $F4
            0x68,
            0xFF, // CMP A, #$FF
                  // BNE $0200 would follow on real hardware; omitted here since
                  // this test only needs the FIRST instruction to stay unexecuted.
        ];

        // See the drain rationale on the test above.
        let write = |s: &mut SnesSystem, index: usize, value: u8| {
            s.bus.apu.cpu_write_port(index, value);
            s.bus.apu.poll_boot();
            for _ in 0..SETTLE_CYCLES {
                s.bus.apu.poll_boot();
            }
        };
        write(&mut s, 1, 0x01);
        write(&mut s, 2, dest as u8);
        write(&mut s, 3, (dest >> 8) as u8);
        write(&mut s, 0, 0xCC);
        for (i, b) in program.iter().enumerate() {
            write(&mut s, 1, *b);
            write(&mut s, 0, i as u8);
        }
        write(&mut s, 1, 0x00);
        write(&mut s, 2, dest as u8);
        write(&mut s, 3, (dest >> 8) as u8);
        let run_echo = program.len() as u8 + 1;
        s.bus.apu.cpu_write_port(0, run_echo);
        (s, run_echo)
    };
    let (mut s, run_echo) = build();
    let dest: u16 = 0x0200;

    // Since W14-48 the HLE walks the listing, so how many cycles the
    // hand-over takes depends on where in its `$FFDA` polling loop the
    // ROM was when the CPU wrote. Measure it on a copy rather than
    // hard-coding a count, then fund exactly that plus ONE cycle — the
    // 1-cycle remainder this test's whole point depends on: too small to
    // fund the driver's own first instruction (`MOV $F4,#$F1`, base cost 5
    // per `timing::CYCLES[0x8F]`), the shape W14-41's fix defers.
    let (mut probe, _) = build();
    let mut handover = 0u64;
    while !probe.bus.apu.boot.is_running() {
        probe.bus.apu.tick_clock(1);
        probe.bus.apu.poll_boot();
        handover += 1;
        assert!(handover < 200, "the hand-over never happened");
    }
    s.bus.apu_debt = 21 * (handover + 1);
    s.bus.catch_up_apu();
    assert!(s.bus.apu.boot.is_running(), "the hand-over must happen");
    assert_eq!(
        s.bus.apu.cpu_read_port(0),
        run_echo,
        "the hand-over call itself must not clobber its own echo"
    );
    assert_eq!(
        s.bus.apu.cpu.pc, dest,
        "the SPC700 must not have executed anything yet"
    );

    // The CPU's OWN NEXT INSTRUCTION is a READ of the same port — modelled
    // as a second, separate `catch_up_apu()` call with NO additional debt
    // added beyond whatever was carried over, exactly like `SnesBus`'s
    // `$2140` read arm calling this before `SnesSystem::step` has added
    // the current instruction's own cost to `apu_debt`.
    s.bus.catch_up_apu();
    assert_eq!(
        s.bus.apu.cpu_read_port(0),
        run_echo,
        "a port READ one instruction after the hand-over must still see \
         the Run echo ({run_echo:#04X}), not the driver's own first port \
         write ({:#04X}) run early on a debt too small to have earned it",
        s.bus.apu.cpu_read_port(0)
    );
    assert_eq!(
        s.bus.apu.cpu.pc, dest,
        "the driver's first instruction must not have run early either"
    );

    // Real time keeps passing on the CPU side even while it spins on
    // `CMP $2140` (each failed poll is itself an instruction with its own
    // real cost) — once enough of it has genuinely elapsed, the deferred
    // instruction is honestly earned and runs.
    s.bus.apu_debt += 21 * 10;
    s.bus.catch_up_apu();
    assert_ne!(
        s.bus.apu.cpu.pc, dest,
        "given enough real elapsed time the deferred instruction must \
         eventually run — this is a reordering fix, not a permanent stall"
    );
}

// ---------------------------------------------------------------------
// S-DSP skeleton
// ---------------------------------------------------------------------

/// A filter-0 block is plain 4-bit ADPCM: no prediction, just the range
/// shift. That makes it the one block whose output can be computed by
/// hand, so it pins the nibble order and the shift.
#[test]
fn brr_filter_zero_decodes_by_the_shift_alone() {
    // range 4, filter 0, no loop/end.
    let mut block = [0u8; 9];
    block[0] = 4 << 4; // range 4, filter 0
    block[1] = 0x12; // nibbles 1 then 2
    let out = decode_brr(&block, [0, 0]);
    assert_eq!(out.samples[0], (1 << 4) >> 1, "high nibble first");
    assert_eq!(out.samples[1], (2 << 4) >> 1);
    assert!(!out.loops && !out.end);
}

/// Nibbles are SIGNED: `$F` is -1, not 15.
#[test]
fn brr_nibbles_are_sign_extended() {
    let mut block = [0u8; 9];
    block[0] = 4 << 4;
    block[1] = 0xF0;
    let out = decode_brr(&block, [0, 0]);
    assert_eq!(out.samples[0], ((-1i32 << 4) >> 1) as i16, "$F is -1");
    assert_eq!(out.samples[1], 0);
}

#[test]
fn brr_header_flags_are_decoded() {
    let mut block = [0u8; 9];
    block[0] = 0x03; // loop + end
    let out = decode_brr(&block, [0, 0]);
    assert!(out.loops && out.end);
}

/// Silence in, silence out — and a DSP with nothing keyed on must be
/// silent, not idling at some DC offset.
#[test]
fn a_dsp_with_no_voices_keyed_on_is_silent() {
    let mut dsp = Dsp::new();
    let mut aram = vec![0u8; 1024];
    for _ in 0..64 {
        assert_eq!(dsp.mix(&mut aram), (0, 0));
    }
}

/// A keyed-on voice produces sound, and volume actually attenuates.
#[test]
fn a_keyed_voice_produces_output_scaled_by_its_volume() {
    let mut aram = vec![0u8; 1024];
    // A loud, non-silent block at $0000.
    aram[0] = (10 << 4) | 0x02; // range 10, filter 0, loop
    for b in aram.iter_mut().take(9).skip(1) {
        *b = 0x77;
    }
    let mut dsp = Dsp::new();
    // `Dsp::new()` now defaults FLG to hardware's `E0h` reset value (soft
    // reset + mute + echo-write-disable) — clear it, or soft reset would
    // key this voice back off (and zero its envelope) every sample.
    dsp.write_register(0x6C, 0x20, &aram);
    dsp.voices[0].start = 0;
    dsp.voices[0].loop_addr = 0;
    dsp.voices[0].vol_left = 0x7F;
    dsp.voices[0].vol_right = 0x7F;
    // W7-08 added envelopes: a voice with no envelope configured stays
    // silent, which is correct. GAIN mode at full is the simplest way to
    // hold it open for a mixing test.
    dsp.voices[0].envelope.gain = 0x7F;
    dsp.voices[0].pitch = 0x1000;
    dsp.key_on(0x01);

    let loud: i32 = (0..16).map(|_| i32::from(dsp.mix(&mut aram).0.abs())).sum();
    assert!(loud > 0, "a keyed voice must be audible");

    dsp.voices[0].vol_left = 0;
    dsp.key_on(0x01);
    // **Skip one sample.** A voice's VOLL is applied at its S4, which for
    // voice 0 is the last cycle of the loop — so the sample already in
    // flight when the volume changed still carries the old scaling. That
    // is a one-sample latency the hardware has and a sample-granular
    // mixer cannot express; asserting silence from the very next sample
    // would be asserting the absence of it.
    let _ = dsp.mix(&mut aram);
    let quiet: i32 = (0..16).map(|_| i32::from(dsp.mix(&mut aram).0.abs())).sum();
    assert_eq!(quiet, 0, "zero volume must be silent");
}

/// Key-on resets the filter history.
///
/// A voice that kept the previous sample's history would start with a
/// click — precisely the "not clean" this ticket's acceptance rules out.
#[test]
fn key_on_resets_the_filter_history() {
    let mut aram = vec![0u8; 1024];
    aram[0] = (8 << 4) | 0x02;
    for b in aram.iter_mut().take(9).skip(1) {
        *b = 0x44;
    }
    let mut dsp = Dsp::new();
    // `Dsp::new()` now defaults FLG to hardware's `E0h` reset value (soft
    // reset + mute + echo-write-disable) — clear it, or soft reset would
    // key this voice back off every sample, making the test pass
    // vacuously on all-silence rather than on a reproduced sample.
    dsp.write_register(0x6C, 0x20, &aram);
    dsp.voices[0].vol_left = 0x7F;
    dsp.voices[0].envelope.gain = 0x7F;
    dsp.voices[0].pitch = 0x1000;
    dsp.key_on(0x01);
    let first = dsp.mix(&mut aram).0;
    for _ in 0..40 {
        dsp.mix(&mut aram);
    }
    dsp.key_on(0x01);
    assert_eq!(
        dsp.mix(&mut aram).0,
        first,
        "re-keying must reproduce the sample's opening exactly"
    );
}

/// A non-looping sample keys itself off at its end rather than running
/// on into whatever follows it in ARAM.
#[test]
fn a_non_looping_sample_stops_at_its_end() {
    let mut aram = vec![0u8; 1024];
    aram[0] = (8 << 4) | 0x01; // end, no loop
    for b in aram.iter_mut().take(9).skip(1) {
        *b = 0x33;
    }
    let mut dsp = Dsp::new();
    // `Dsp::new()` now defaults FLG to hardware's `E0h` reset value (soft
    // reset + mute + echo-write-disable) — clear it so the trailing
    // `(0, 0)` below is because the sample ended, not because soft reset
    // silenced the voice from the very first sample.
    dsp.write_register(0x6C, 0x20, &aram);
    dsp.voices[0].vol_left = 0x7F;
    dsp.voices[0].envelope.gain = 0x7F;
    dsp.voices[0].pitch = 0x1000;
    dsp.key_on(0x01);
    for _ in 0..16 {
        dsp.mix(&mut aram);
    }
    // The 17th sample needs the next block, which is the end.
    dsp.mix(&mut aram);
    assert!(!dsp.voices[0].keyed_on, "the voice keyed itself off");
    assert_eq!(dsp.mix(&mut aram), (0, 0));
}

/// **The IPL hand-over must echo before it jumps** (ticket W7-08).
///
/// The CPU's upload loop is `STA $2140` then `CMP $2140 / BNE` — it spins
/// until the IPL sends the counter back, and the jump signal is no
/// exception. Handing control to the uploaded program without that echo
/// leaves the 65816 polling for a byte that will never come while the
/// SPC700 happily runs the code it was just given: two live processors,
/// each waiting on the other, from a handshake that otherwise completed.
///
#[test]
fn handing_control_to_the_spc700_still_echoes_the_final_counter() {
    let mut apu = Apu::new();
    upload(&mut apu, 0x0300, &[0x00, 0x00]);
    // The CPU signals "jump": port 1 = 0, entry in ports 2/3, counter+1.
    write_port(&mut apu, 1, 0x00);
    write_port(&mut apu, 2, 0x00);
    write_port(&mut apu, 3, 0x03);
    write_port(&mut apu, 0, 3);
    assert!(apu.boot.is_running());
    assert_eq!(apu.cpu.pc, 0x0300, "entry comes from ports 2/3");
    assert_eq!(
        apu.cpu_read_port(0),
        3,
        "the counter the CPU just wrote must come back, or its CMP $2140 \
         loop never exits"
    );
}

/// A 16-bit `STA $2140` must not be evaluated half-done.
///
/// **This is the bug that cost blargg's entire SPC test suite** (ticket
/// W7-08). Their uploader starts a transfer with one 16-bit store, which
/// lands as two byte writes with port 0 FIRST — traced, not assumed:
///
/// ```text
/// idx=0 val=CC  ports_in before [00, 00, 00, 04]
/// idx=1 val=01  ports_in before [CC, 00, 00, 04]
/// ```
///
/// An edge-triggered handshake reads the "kind" byte on the port-0 write,
/// one instruction too early, sees `0`, and takes the "transfer nothing,
/// just run" branch — jumping to an address nothing was uploaded to. The
/// SPC700 then NOP-slides through empty ARAM ($00 is a NOP) while the
/// 65816 spins forever waiting for a byte counter that is never echoed.
///
/// The failure is silent and looks like a DSP or CPU bug from every
/// angle, which is why it survived three passes of this ticket.
#[test]
fn a_sixteen_bit_port_write_is_seen_settled_not_half_done() {
    let mut apu = Apu::new();
    apu.cpu_write_port(2, 0x00);
    apu.cpu_write_port(3, 0x04);
    apu.poll_boot();

    // The two halves of one 16-bit store, port 0 first, with NO poll
    // between them — the APU cannot run inside a single CPU instruction.
    apu.cpu_write_port(0, 0xCC);
    apu.cpu_write_port(1, 0x01);
    for _ in 0..SETTLE_CYCLES {
        apu.poll_boot();
    }

    assert!(
        !apu.boot.is_running(),
        "kind=$01 means TRANSFER; handing over here is the bug — the SPC700 \
         would run from ${:04X} with nothing uploaded there",
        apu.cpu.pc
    );
    assert_eq!(
        apu.boot.state,
        BootState::AwaitingBlock(0xCC),
        "the handshake must be waiting for the first byte's counter (0)"
    );
    assert_eq!(apu.ports_out[0], 0xCC, "and must have echoed the $CC");
}

/// The zero-kind "just run" branch still works when it is genuinely what
/// the CPU asked for — the fix must not make it unreachable.
#[test]
fn a_settled_zero_kind_still_runs_immediately() {
    let mut apu = Apu::new();
    apu.cpu_write_port(2, 0x00);
    apu.cpu_write_port(3, 0x04);
    apu.cpu_write_port(1, 0x00);
    apu.cpu_write_port(0, 0xCC);
    apu.poll_boot();
    // W14-37: the immediate-run path is still not FREE — the boot ROM's
    // `jr main` fast path plus its shared jump tail costs
    // `RUN_HANDOFF_IMMEDIATE_CYCLES` (42) SPC cycles, walked one listed
    // step at a time (see `IplBoot::poll`) — drain it before asserting
    // the handoff completed.
    for _ in 0..crate::apu::boot::RUN_HANDOFF_IMMEDIATE_CYCLES {
        apu.poll_boot();
    }
    assert!(apu.boot.is_running());
    assert_eq!(apu.cpu.pc, 0x0400);
    assert_eq!(apu.boot.transferred, 0);
}

/// A user-supplied boot ROM replaces the stub in the `$FFC0` window.
///
/// The stub is ours by ruling (law 5, no ROM bytes in git, extended
/// 2026-08-20 to "not in a fetch list either"), and that ruling recorded
/// "requiring the user to supply it" as still open. This is that door,
/// and it exists because blargg's SPC test ROMs read `$FFC0` as DATA,
/// compare it against `$CD`, spin forever if it differs, and then jump to
/// it — so they cannot run on a stub at all.
#[test]
fn a_user_supplied_boot_rom_is_what_the_ipl_window_reads() {
    let mut apu = Apu::new();
    // Bank the IPL in: $F1 bit 7.
    apu.write_register(0x00F1, 0x80);
    // Byte 0 is $CD by design: programs read it as data and refuse to run
    // otherwise (see IPL_STUB). It is a compatibility constant, not
    // borrowed code -- everything after it is still SLEEP-filled and is
    // NOT Nintendo's ROM.
    assert_eq!(
        apu.read(0xFFC0),
        0xCD,
        "byte 0 is the compatibility constant"
    );
    assert_eq!(
        apu.read(0xFFC1),
        0xEF,
        "and the rest of the default stub is still ours: SLEEP-filled"
    );

    let mut rom = [0u8; crate::apu::IPL_LEN];
    rom[0] = 0xCD;
    rom[1] = 0xEF;
    rom[crate::apu::IPL_LEN - 1] = 0x42;
    apu.set_ipl_rom(rom);

    assert_eq!(
        apu.read(0xFFC0),
        0xCD,
        "byte 0, not byte 1 — the offset is \
        what blargg's CMP #$CD actually discriminates"
    );
    assert_eq!(apu.read(0xFFFF), 0x42, "and the window's last byte");
}

/// Writes still land in ARAM underneath a SUPPLIED rom, exactly as they
/// do under the stub — the banking rule is about reads only.
#[test]
fn writes_still_reach_aram_under_a_supplied_boot_rom() {
    let mut apu = Apu::new();
    apu.write_register(0x00F1, 0x80);
    let mut rom = [0xAAu8; crate::apu::IPL_LEN];
    rom[0] = 0xCD;
    apu.set_ipl_rom(rom);

    apu.write(0xFFC0, 0x99);
    assert_eq!(apu.read(0xFFC0), 0xCD, "the read still comes from the ROM");
    assert_eq!(apu.aram[0xFFC0], 0x99, "and the write still reached ARAM");
}

// ---------------------------------------------------------------------
// W14-37: the boot ROM's own instruction cost on the go->jump handoff
// and the per-byte handshake, from fullsnes' published disassembly.
// ---------------------------------------------------------------------

/// Pins the three cycle counts this ticket derived from fullsnes "SNES
/// APU Main CPU Communication Port" -> "Boot ROM Disassembly" (a
/// clean-room disassembly of the documented protocol — never Nintendo's
/// bytes, per law 5) against this crate's own vector-verified
/// `spc700::timing::CYCLES` table, so a change to either the constants
/// or the table's entries for these opcodes is caught here rather than
/// discovered as a fresh boot-race regression.
///
/// **After-transfer run** (`$FFDA`-`$FFFB`, the shape every title in this
/// ticket's census brief uses — the CPU's counter jumps by 2+ to end a
/// block, then hands over):
/// `cmp Y,$F4`(3) + `jnz taken`(2+2) + `jns nt`(2) + `cmp Y,$F4`(3) +
/// `jns nt`(2) + `movw YA,$F6`(5) + `movw $00,YA`(5) + `movw YA,$F4`(5) +
/// `mov $F4,A`(4) + `mov A,Y`(2) + `mov X,A`(2) + `jnz nt`(2) +
/// `jmp [$0000+X]`(6) = 45.
///
/// **Immediate run** (`$FFCF`-`$FFFB`, first `$CC` already carries kind
/// 0): `cmp $F4,#$CC`(5) + `jnz nt`(2) + `jr main`(4), then the same
/// `$FFEF`-`$FFFB` tail (31) = 42.
///
/// **Per-byte handshake** (`$FFDA`-`$FFE5`, one accepted byte): `cmp
/// Y,$F4`(3) + `jnz nt`(2) + `mov A,$F5`(3) + `mov $F4,Y`(4) + `mov
/// [$00]+Y,A`(7) + `inc Y`(2) + `jnz taken`(2+2) = 25.
#[test]
fn the_listings_cycle_counts_match_this_crates_own_timing_table() {
    use crate::apu::spc700::timing::cycles;

    // Opcode bytes are quoted here ONLY to index this crate's own
    // vector-derived table — not transcribed as executable ROM content,
    // and never stored, loaded or executed as SPC700 code anywhere in
    // this crate (law 5). Each is the exact byte fullsnes' disassembly
    // names at that address.
    let after_transfer = cycles(0x7E, false) // $FFDA cmp Y,dp
        + cycles(0xD0, true)   // $FFDC jnz, taken
        + cycles(0x10, false)  // $FFE9 jns, not taken
        + cycles(0x7E, false)  // $FFEB cmp Y,dp
        + cycles(0x10, false)  // $FFED jns, not taken
        + cycles(0xBA, false)  // $FFEF movw YA,dp
        + cycles(0xDA, false)  // $FFF1 movw dp,YA
        + cycles(0xBA, false)  // $FFF3 movw YA,dp
        + cycles(0xC4, false)  // $FFF5 mov dp,A (the echo)
        + cycles(0xDD, false)  // $FFF7 mov A,Y
        + cycles(0x5D, false)  // $FFF8 mov X,A
        + cycles(0xD0, false)  // $FFF9 jnz, not taken (cmd==0)
        + cycles(0x1F, false); // $FFFB jmp [!abs+X]
    assert_eq!(u16::from(after_transfer), RUN_HANDOFF_AFTER_TRANSFER_CYCLES);

    let immediate = cycles(0x78, false) // $FFCF cmp dp,#imm
        + cycles(0xD0, false)  // $FFD2 jnz, not taken
        + cycles(0x2F, false)  // $FFD4 jr (bra)
        + cycles(0xBA, false)  // $FFEF movw YA,dp
        + cycles(0xDA, false)  // $FFF1 movw dp,YA
        + cycles(0xBA, false)  // $FFF3 movw YA,dp
        + cycles(0xC4, false)  // $FFF5 mov dp,A
        + cycles(0xDD, false)  // $FFF7 mov A,Y
        + cycles(0x5D, false)  // $FFF8 mov X,A
        + cycles(0xD0, false)  // $FFF9 jnz, not taken
        + cycles(0x1F, false); // $FFFB jmp [!abs+X]
    assert_eq!(u16::from(immediate), RUN_HANDOFF_IMMEDIATE_CYCLES);

    let per_byte = cycles(0x7E, false) // $FFDA cmp Y,dp (the match)
        + cycles(0xD0, false)  // $FFDC jnz, not taken
        + cycles(0xE4, false)  // $FFDE mov A,dp (fetch the data byte)
        + cycles(0xCB, false)  // $FFE0 mov dp,Y (the echo)
        + cycles(0xD7, false)  // $FFE2 mov [dp]+Y,A (store)
        + cycles(0xFC, false)  // $FFE4 inc Y
        + cycles(0xD0, true); // $FFE5 jnz, taken (loop back)
    assert_eq!(u16::from(per_byte), BYTE_HANDSHAKE_CYCLES);
}

/// The handoff is not observable one cycle early: with
/// `RUN_HANDOFF_IMMEDIATE_CYCLES - 1` polls spent, the CPU's own spin on
/// `CMP $2140` must still see nothing new, and `pc`/`stopped` must be
/// untouched — the whole point of charging the listing's cycles rather
/// than handing over "for free" (the Super Turrican race this ticket was
/// filed from, docs/TESTING.md's W14-33 note).
#[test]
fn the_immediate_run_handoff_is_not_observable_before_its_listed_cycles_elapse() {
    let mut apu = Apu::new();
    apu.cpu_write_port(2, 0x00);
    apu.cpu_write_port(3, 0x04);
    apu.cpu_write_port(1, 0x00);
    apu.cpu_write_port(0, 0xCC);
    // From power-on the ROM is at the start of its `$FFCF` compare, so
    // the listed 42 cycles run from the very first poll.
    for _ in 0..u16::from(crate::apu::boot::RUN_HANDOFF_IMMEDIATE_CYCLES) - 1 {
        apu.poll_boot();
        assert!(
            !apu.boot.is_running(),
            "the handoff fired before its documented cycle count elapsed"
        );
    }
    apu.poll_boot();
    assert!(
        apu.boot.is_running(),
        "and exactly at the documented count, it must have"
    );
    assert_eq!(apu.cpu.pc, 0x0400);
}

/// The per-byte handshake at the listing's own intra-loop timing
/// (W14-48): the echo goes out 9 cycles after the matching compare
/// (`jnz` 2 + `mov A,$F5` 3 + `mov $F4,Y` 4), the store 7 cycles after
/// that, and a counter written the instant the echo appears is answered
/// exactly one loop — `BYTE_HANDSHAKE_CYCLES` — after the previous echo.
/// The snapshot model delivered echo and store together, 25 cycles after
/// the compare, which made every upload slower than hardware.
#[test]
fn the_byte_handshake_echoes_and_stores_at_the_listings_cycles() {
    let mut apu = Apu::new();
    write_port(&mut apu, 1, 0x01);
    write_port(&mut apu, 2, 0x00);
    write_port(&mut apu, 3, 0x02);
    write_port(&mut apu, 0, 0xCC);
    assert_eq!(apu.ports_out[0], 0xCC, "the $CC echo settled first");

    write_port(&mut apu, 1, 0xAB);
    write_port(&mut apu, 0, 0x00);
    assert_eq!(apu.ports_out[0], 0x00);
    assert_eq!(apu.aram[0x0200], 0xAB);

    // Counter 1, written the cycle its predecessor's echo is visible.
    let mut apu2 = Apu::new();
    write_port(&mut apu2, 1, 0x01);
    write_port(&mut apu2, 2, 0x00);
    write_port(&mut apu2, 3, 0x02);
    write_port(&mut apu2, 0, 0xCC);
    apu2.cpu_write_port(1, 0xAB);
    apu2.cpu_write_port(0, 0x00);
    let mut n = 0u16;
    while apu2.ports_out[0] != 0x00 {
        apu2.poll_boot();
        n += 1;
        assert!(n < 100, "byte 0 was never echoed");
    }
    apu2.cpu_write_port(1, 0xCD);
    apu2.cpu_write_port(0, 0x01);
    let (mut echo_at, mut store_at) = (None, None);
    for t in 1..=u16::from(BYTE_HANDSHAKE_CYCLES) + 10 {
        apu2.poll_boot();
        if echo_at.is_none() && apu2.ports_out[0] == 0x01 {
            echo_at = Some(t);
        }
        if store_at.is_none() && apu2.aram[0x0201] == 0xCD {
            store_at = Some(t);
        }
    }
    assert_eq!(
        echo_at,
        Some(BYTE_HANDSHAKE_CYCLES),
        "one loop from echo to echo"
    );
    assert_eq!(
        store_at.zip(echo_at).map(|(s, e)| s - e),
        Some(7),
        "the store lands 7 cycles (mov [$00]+Y,A) after the echo"
    );
}

/// A program that re-enters the boot ROM at `$FFC9` — past `MOV SP,X` and
/// the zero-page clear — keeps its zero page and its stack (W14-48
/// follow-up, Champions - World Class Soccer's shape: a stub jumps to
/// `$FFC9`, then the uploaded "driver" is a lone `RET` that returns
/// through the stub's own stack). Only an entry at `$FFC0` clears.
#[test]
fn re_entering_past_the_init_keeps_zero_page_and_the_stack() {
    for (target, clears) in [(0xFFC9u16, false), (0xFFC0, true)] {
        let mut apu = Apu::new();
        write_port(&mut apu, 1, 0x00);
        write_port(&mut apu, 2, 0x00);
        write_port(&mut apu, 3, 0x04);
        write_port(&mut apu, 0, 0xCC);
        assert!(apu.boot.is_running());
        apu.aram[0x0050] = 0x6F;
        apu.cpu.sp = 0x80;
        // The CPU has moved on from its `$CC` (else the ROM would see it
        // again and run straight back out).
        apu.cpu_write_port(0, 0x00);
        // `JMP !target` at the entry point.
        apu.aram[0x0400..0x0403].copy_from_slice(&[0x5F, target as u8, (target >> 8) as u8]);
        let _ = apu.step_counted();
        assert!(
            !apu.boot.is_running(),
            "{target:04X} re-enters the handshake"
        );
        for _ in 0..IPL_INIT_CYCLES + 10 {
            apu.poll_boot();
        }
        assert_eq!(apu.boot.state, BootState::Ready, "{target:04X}");
        assert_eq!(apu.cpu_read_port(0), 0xAA);
        assert_eq!(apu.aram[0x0050] == 0, clears, "{target:04X}: zero page");
        assert_eq!(apu.cpu.sp == 0xEF, clears, "{target:04X}: SP");
    }
}
