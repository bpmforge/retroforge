//! CPU<->APU ports, the boot handshake, and the S-DSP skeleton
//! (ticket W6-04b).

use crate::apu::boot::{BootAction, BootState, IplBoot};
use crate::apu::dsp::{decode_brr, Dsp};
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
}

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
    assert!(
        !apu.ipl_enabled,
        "and the IPL banks out so the uploaded program owns the space"
    );
}

/// The counter SKIP is what distinguishes "new block" from "next byte" —
/// the single easiest part of this protocol to get wrong.
#[test]
fn the_counter_distinguishes_the_next_byte_from_a_new_block() {
    let mut boot = IplBoot::new();
    // Start a transfer to $0300.
    let p = [0u8, 0x01, 0x00, 0x03];
    assert_eq!(boot.cpu_wrote(0, 0xCC, p), BootAction::Echo(0xCC));

    // Counter 0 = store the first byte.
    let p = [0u8, 0xAB, 0x00, 0x03];
    assert_eq!(
        boot.cpu_wrote(0, 0, p),
        BootAction::Store {
            address: 0x0300,
            value: 0xAB,
            echo: 0
        }
    );

    // Counter 1 = the NEXT byte, not a new block.
    assert_eq!(
        boot.cpu_wrote(0, 1, p),
        BootAction::Store {
            address: 0x0301,
            value: 0xAB,
            echo: 1
        }
    );

    // Counter skipping to 3 (expected is 2) = a new block at ports 2/3.
    let p = [0u8, 0x01, 0x00, 0x08];
    assert_eq!(boot.cpu_wrote(0, 3, p), BootAction::Echo(3));
    assert_eq!(boot.state, BootState::AwaitingBlock(3));
    assert_eq!(boot.address, 0x0800, "the new block's address was taken");
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
        0xA9, 0xCC, 0x8D, 0x40, 0x21, // LDA #$5A : STA $2141      (data)
        0xA9, 0x5A, 0x8D, 0x41, 0x21, // LDA #$00 : STA $2140      (counter 0 -> store)
        0xA9, 0x00, 0x8D, 0x40, 0x21, // LDA #$00 : STA $2141      (kind 0 = finish)
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
    assert!(
        s.bus.apu.boot.is_running(),
        "a game-shaped init sequence must complete the handshake"
    );
    assert_eq!(s.bus.apu.aram[0x0200], 0x5A, "the uploaded byte landed");
    assert_eq!(s.bus.apu.cpu.pc, 0x0200, "and the APU starts there");
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
/// Asserted on the ACTION rather than through a whole boot, so the reason
/// this byte exists is visible at the point that produces it.
#[test]
fn handing_control_to_the_spc700_still_echoes_the_final_counter() {
    use crate::apu::boot::{BootAction, BootState, IplBoot};

    // Mid-upload: one block transferred, counter at 2.
    let mut boot = IplBoot::new();
    boot.state = BootState::Transferring(2);
    boot.address = 0x0300;

    // The CPU signals "jump": port 1 = 0, entry in ports 2/3, counter+1.
    let ports = [0u8, 0x00, 0x00, 0x03];
    match boot.cpu_wrote(0, 3, ports) {
        BootAction::Run { entry, echo } => {
            assert_eq!(entry, 0x0300, "entry comes from ports 2/3");
            assert_eq!(
                echo, 3,
                "the counter the CPU just wrote must come back, or its \
                 CMP $2140 loop never exits"
            );
        }
        other => panic!("expected a Run action, got {other:?}"),
    }
    assert_eq!(boot.state, BootState::Running);
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
    apu.poll_boot();

    assert!(
        !apu.boot.is_running(),
        "kind=$01 means TRANSFER; handing over here is the bug — the SPC700 \
         would run from ${:04X} with nothing uploaded there",
        apu.cpu.pc
    );
    assert_eq!(
        apu.boot.state,
        BootState::Transferring(0),
        "the handshake must be waiting for the first byte's counter"
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
    assert_eq!(
        apu.read(0xFFC0),
        0xEF,
        "the default stub is SLEEP-filled and is NOT Nintendo's ROM"
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
