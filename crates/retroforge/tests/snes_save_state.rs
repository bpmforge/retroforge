//! Ticket W7-09: SNES `.rfstate` round trips.
//!
//! The criterion that decides this ticket is the second one — a restored
//! machine must be **hash-identical to one that never stopped**. A missing
//! field almost always survives a one-frame check and diverges later, so
//! the test below saves, runs on, restores, and then runs the SAME number
//! of frames as an uninterrupted control before comparing.
//!
//! ## What these tests can and cannot see
//!
//! **A state-hash comparison cannot test the state format.** If a field is
//! missing from serialisation it is missing from the control's hash and
//! the subject's alike, so the two agree however wrong the restore was.
//! Demonstrated, not theorised: deleting the DSP echo ring from the
//! serializer left an earlier version of this file entirely green. The
//! divergence test therefore also compares [`observable_hash`] -- rendered
//! scanlines and mixed audio, produced by the emulator rather than by the
//! save format -- and the latch test at the bottom completes a pending
//! write after restoring, which is the only way to observe a field whose
//! effect lands on the NEXT write.
//!
//! **The residual gap, stated rather than left to be discovered**: a
//! roundtrip can only see fields the running ROM actually drives. The stub
//! cart here exercises the CPU, WRAM, the raster clock and the PPU
//! registers written through the bus; it does not drive the S-DSP.
//! Nothing could -- W7-09 found the DSP unreachable from SPC700 code
//! entirely (`$F3` has no write path, and `Dsp::mix` is called only from
//! tests), and that finding is recorded on W7-08, which owns the wiring.
//! Until it lands, the DSP half of `APU_` is covered by its payload size
//! and by construction, not by execution.

use retroforge::snes_save_state::{self, CONSOLE_SNES};
use rf_snes::cpu::CpuBus;
use rf_snes::system::SnesSystem;
use sha2::{Digest, Sha256};

/// A minimal but genuinely executing LoROM cart.
///
/// Real code, not zeros: the machine has to change state over the frames
/// this test runs, or "hash-identical" would be trivially true of a halted
/// CPU. The stub increments a WRAM location and loops, which touches the
/// CPU registers, WRAM and the raster clock.
fn test_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 64 * 1024];
    // Reset vector -> $8000.
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    // Header: title, LoROM, sizes.
    for (i, b) in b"RF W7-09 STATE TEST  ".iter().enumerate() {
        rom[0x7FC0 + i] = *b;
    }
    rom[0x7FD5] = 0x20; // LoROM, slow
    rom[0x7FD7] = 0x08; // 256 KiB-ish; only the shape matters here
                        // $8000: CLC / XCE (native), then INC $0000 / BRA back.
    let code: &[u8] = &[
        0x18, // CLC
        0xFB, // XCE
        0xE6, 0x00, // INC $00
        0xEE, 0x00, 0x02, // INC $0200
        0x80, 0xF8, // BRA -8
    ];
    rom[0..code.len()].copy_from_slice(code);
    rom
}

fn rom_hash(rom: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(rom);
    h.finalize().into()
}

/// Run until the frame counter has advanced by `frames`.
fn run_frames(s: &mut SnesSystem, frames: u64) {
    let target = s.bus.timing.frame + frames;
    let mut guard = 0u64;
    while s.bus.timing.frame < target && guard < 20_000_000 {
        let _ = s.step();
        guard += 1;
    }
    assert!(
        s.bus.timing.frame >= target,
        "ran {guard} instructions without reaching frame {target}"
    );
}

/// A hash over the machine's own state chunks.
///
/// Useful for "did the restore land", but **deliberately not what the
/// divergence test compares** — see [`observable_hash`].
fn machine_hash(s: &SnesSystem, rom: &[u8]) -> [u8; 32] {
    snes_save_state::save_state(s, rom_hash(rom), 0)
        .expect("a running machine always serialises")
        .state_hash()
}

/// A hash over what the machine **outputs**: rendered scanlines and mixed
/// audio samples.
///
/// **This exists because hashing the save payload cannot test the save
/// payload.** The obvious roundtrip check — save, restore, run on, compare
/// `Container::state_hash` — is circular: a field that is missing from
/// serialisation is missing from *both* the control's hash and the
/// subject's, so the two agree no matter how wrong the restore was. It was
/// demonstrated rather than reasoned about: deleting the DSP echo ring
/// from the serializer left all three tests in this file green.
///
/// Video and audio are produced by the emulator, not by the state format,
/// so they see the fields the format forgot. The echo ring in particular
/// is *only* observable as audio — it is a delay line whose contents are
/// still audible after a restore.
fn observable_hash(s: &mut SnesSystem) -> [u8; 32] {
    let mut h = Sha256::new();
    for y in 0..224u16 {
        for px in s.bus.ppu.render_scanline(y).pixels {
            h.update([px.palette_index]);
        }
    }
    // Mixing advances the DSP, so this is destructive — every caller runs
    // it at a point it is about to leave behind.
    for _ in 0..512 {
        let (l, r) = s.bus.apu.dsp.mix(&mut s.bus.apu.aram);
        h.update(l.to_le_bytes());
        h.update(r.to_le_bytes());
    }
    h.finalize().into()
}

/// Criterion 1 and 4: every region reaches the container, including the
/// APU and the PPU memories rather than only the CPU.
#[test]
fn a_snes_state_carries_every_core_chunk() {
    let rom = test_rom();
    let mut s = SnesSystem::load(&rom).expect("loads");
    run_frames(&mut s, 2);

    let c = snes_save_state::save_state(&s, rom_hash(&rom), 1_700_000_000).expect("saves");
    assert_eq!(c.header.console, CONSOLE_SNES);

    for tag in [
        b"CPU_", b"PPU_", b"APU_", b"WRAM", b"VRAM", b"OAM_", b"CGRM", b"MAPR", b"CART",
    ] {
        assert!(
            c.chunk(*tag).is_some(),
            "missing chunk {}",
            String::from_utf8_lossy(tag)
        );
    }

    // Criterion 4 is about SIZE as much as presence: an APU_ chunk that
    // forgot ARAM, or a VRAM chunk that forgot VRAM, would still be
    // "present" and would still be wrong.
    assert!(
        c.chunk(*b"APU_").unwrap().payload.len() > 64 * 1024,
        "APU_ must carry the SPC700's 64 KiB of ARAM, not just its registers"
    );
    assert_eq!(c.chunk(*b"VRAM").unwrap().payload.len(), 64 * 1024);
    assert_eq!(c.chunk(*b"WRAM").unwrap().payload.len(), 128 * 1024);
    assert_eq!(c.chunk(*b"CGRM").unwrap().payload.len(), 512);

    // And it must survive the real encoder, not just live in memory.
    let bytes = c.encode().expect("encodes");
    let (decoded, warnings) = rf_state::Container::decode_default(&bytes).expect("decodes");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    assert_eq!(decoded.state_hash(), c.state_hash());
}

/// **Criterion 2, the one that matters.** Restore must be
/// indistinguishable from never having stopped.
#[test]
fn a_restored_machine_runs_identically_to_one_that_never_stopped() {
    let rom = test_rom();
    const AFTER: u64 = 8;

    // Control: run straight through.
    let mut control = SnesSystem::load(&rom).expect("loads");
    run_frames(&mut control, 3);
    let at_save = machine_hash(&control, &rom);
    run_frames(&mut control, AFTER);
    let control_hash = machine_hash(&control, &rom);
    let control_observable = observable_hash(&mut control);

    // Subject: run to the same point, save, run on, then restore and
    // re-run the same number of frames.
    let mut subject = SnesSystem::load(&rom).expect("loads");
    run_frames(&mut subject, 3);
    let container =
        snes_save_state::save_state(&subject, rom_hash(&rom), 1_700_000_000).expect("saves");
    assert_eq!(at_save, container.state_hash(), "save is deterministic");

    // Run past the save point so the restore has something to undo.
    run_frames(&mut subject, 5);
    assert_ne!(
        machine_hash(&subject, &rom),
        at_save,
        "the machine must actually advance, or this test proves nothing"
    );

    snes_save_state::load_state(&mut subject, rom_hash(&rom), &container).expect("restores");
    assert_eq!(
        machine_hash(&subject, &rom),
        at_save,
        "the restored machine does not match the state that was saved"
    );

    run_frames(&mut subject, AFTER);
    assert_eq!(
        machine_hash(&subject, &rom),
        control_hash,
        "after {AFTER} more frames the restored machine diverged from the \
         uninterrupted one - some field is missing from a region"
    );
    assert_eq!(
        observable_hash(&mut subject),
        control_observable,
        "the restored machine's VIDEO/AUDIO output diverged. This is the \
         check that can see fields the save format omits entirely - a \
         state-hash comparison cannot, because it hashes the same payload \
         on both sides"
    );
}

/// Criterion 3: a state from a different ROM is refused, **and refused
/// before the machine is touched**.
#[test]
fn a_state_from_a_different_rom_is_refused_without_disturbing_the_machine() {
    let rom = test_rom();
    let mut other_rom = test_rom();
    other_rom[0x100] = 0x42; // a different cartridge

    let mut s = SnesSystem::load(&rom).expect("loads");
    run_frames(&mut s, 2);
    let container = snes_save_state::save_state(&s, rom_hash(&other_rom), 0).expect("saves");

    let before = machine_hash(&s, &rom);
    let err = snes_save_state::load_state(&mut s, rom_hash(&rom), &container)
        .expect_err("a state for another ROM must be refused");
    let msg = err.to_string();
    assert!(
        msg.to_lowercase().contains("rom"),
        "the diagnostic should say the ROM is wrong: {msg}"
    );
    assert_eq!(
        machine_hash(&s, &rom),
        before,
        "a refused load must leave the running machine untouched"
    );
}

/// **The write-twice latches survive a save**, tested by completing the
/// pending write after the restore (ticket W7-09).
///
/// This is the shape a roundtrip test has to take for a field that
/// influences the *next* write rather than the current picture: perturb,
/// save, restore into a **fresh** machine, then finish the write and check
/// the result. Comparing state hashes could not do it — hashing the save
/// payload cannot test the save payload — and comparing rendered output
/// could not either, because a half-written matrix register changes
/// nothing until its second byte arrives.
///
/// Mode 7's registers all share ONE latch, so a dropped `latch` byte
/// combines the new high byte with a zero low byte: a scale of $0100
/// instead of $01C0, which is a visibly wrong picture from an invisible
/// field.
#[test]
fn a_pending_write_twice_latch_survives_the_round_trip() {
    let rom = test_rom();
    let mut s = SnesSystem::load(&rom).expect("loads");
    run_frames(&mut s, 1);

    // Half of an M7A write: the low byte lands in the shared latch and
    // nothing else has happened yet.
    s.bus.write(0x00_211B, 0xC0);
    let container = snes_save_state::save_state(&s, rom_hash(&rom), 0).expect("saves");

    // Restore into a machine that never saw the first byte.
    let mut fresh = SnesSystem::load(&rom).expect("loads");
    run_frames(&mut fresh, 1);
    snes_save_state::load_state(&mut fresh, rom_hash(&rom), &container).expect("restores");

    // Complete the write on both. They must agree.
    s.bus.write(0x00_211B, 0x01);
    fresh.bus.write(0x00_211B, 0x01);
    assert_eq!(
        fresh.bus.ppu.mode7.a, s.bus.ppu.mode7.a,
        "the restored machine completed the M7A write differently - the \
         shared write-twice latch was not saved"
    );
    assert_eq!(
        s.bus.ppu.mode7.a, 0x01C0,
        "sanity: the two halves should combine to $01C0"
    );
}
