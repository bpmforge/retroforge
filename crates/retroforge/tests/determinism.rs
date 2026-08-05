//! The determinism/replay suite (ticket W1-07, acceptance criterion 3:
//! "determinism CI: two runs same input log => identical state hashes",
//! FR-CORE-002/FR-CORE-003; plus criterion 2, FR-STATE-006's replay
//! record/playback).
//!
//! Driven entirely through [`EmuStepper`] — never through
//! `crate::core_thread`'s mpsc channel, which is timing-dependent and
//! would make this suite flaky.
//!
//! ## Why the fixture actually reads `$4016` (pre-flight finding)
//!
//! `crates/retroforge/src/stepper.rs`'s own tests use an all-zero
//! synthetic NROM: reset vector `$0000`, zeroed RAM, `BRK` forever. That
//! ROM never reads `$4016` at all, so "two runs, same input log =>
//! identical hashes" would be true even with the input latch entirely
//! disconnected, the keymap inverted, or `set_controller_buttons` never
//! called — the same shape as three vacuous tests already found in this
//! project (W1-04a, W1-05a x2). [`controller_reader_rom`] below is a
//! hand-assembled NROM program that actually strobes and reads `$4016`
//! every video frame and folds the result into a **sticky** running
//! checksum at `$0020` (`ADC`, never reset) — a divergence in one frame
//! permanently perturbs every later checksum byte, so it can never
//! silently heal, and [`different_logs_produce_different_and_sticky_hash_sequences`]
//! below is the anti-vacuity half this criterion needs.
//!
//! ## Measured pipeline delay
//!
//! Empirically measured (see `positive_known_button_byte_lands_at_expected_wram_address`'s
//! doc comment for the measurement and why the delay comes out to zero
//! extra frames): a button held for the `InputFrame` passed to
//! `EmuStepper::latch_and_advance_frame` is already visible in WRAM
//! `$0010-$0017` by the time that same call returns.
//!
//! ## Finding: the fixture's own poll loop periodically misses one frame's
//! vblank (not a bug — verify, don't assume)
//!
//! An early version of [`different_logs_produce_different_and_sticky_hash_sequences`]
//! substituted a different button at exactly one frame index and expected
//! the two runs to diverge from that exact frame. It failed — not because
//! input wiring was broken (`positive_known_button_byte_lands_at_expected_wram_address`
//! already proves that path works), but because the substituted frame
//! happened to land on one where the fixture's own `wait`/`read` loop
//! never captured *any* input that "video frame" at all. Measured directly
//! (holding a single-frame pulse at every index 0-39, one fresh run per
//! index, and checking whether the sticky checksum ever moved off zero):
//! frames 0, 8, 20, 32, ... — period 12, and frame 0 is the already-known
//! boot-artifact call — never got read. **What is measured, not
//! inferred:** this is a real, deterministic, input-independent pattern —
//! same misses every run, regardless of what buttons are held — not
//! fixture flakiness or a race. **What is plausible but NOT independently
//! verified from this ticket's scope:** the fixture's poll loop
//! (`wait`/`BPL`) has a fixed 7-CPU-cycle period that does not evenly
//! divide a video frame's cycle count, so the *dot* at which it happens to
//! poll `$2002` drifts every frame; this is *consistent with*
//! nesdev.org/wiki/PPU_frame_timing's documented quirk that a `$2002` read
//! landing on the exact dot *one before* the vblank flag would be set
//! suppresses that flag for the whole frame (the same quirk
//! `crates/rf-nes/src/ppu`'s `suppress_vblank_this_frame` already
//! implements, verified in ticket W1-05b) — but confirming that mechanism
//! specifically (rather than, say, the poll loop's phase simply not
//! reaching `$2002` before the frame boundary that particular frame) would
//! need instrumenting `rf-nes` internals, which is outside this ticket's
//! write scope (`rf-nes` is off limits — see the ticket's "DO NOT touch"
//! list). The measured pattern alone — deterministic, input-independent,
//! period 12 — is what [`build_script`]'s design below actually relies on;
//! the causal explanation is offered as the most likely reading, not a
//! verified claim. [`build_script`] holds a
//! divergent button for many consecutive frames rather than exactly one:
//! the very first frame of a hold might occasionally land on a miss, but a
//! multi-frame hold cannot miss *every* frame in its run, and the sticky
//! checksum then keeps the divergence forever once any one frame in the
//! hold is captured.
use retroforge::stepper::EmuStepper;
use rf_core_api::InputFrame;

const INES_MAGIC: [u8; 4] = [0x4E, 0x45, 0x53, 0x1A]; // "NES\x1A"

// ---------------------------------------------------------------------
// Fixture: a hand-assembled NROM program that strobes $4016, reads all 8
// button bits, and folds them into a sticky checksum every video frame.
// Verified instruction-by-instruction (addresses, opcode lengths, and
// both branch offsets) against the 6502 ISA before use — not trusted
// blindly; `positive_known_button_byte_lands_at_expected_wram_address`
// below is the actual proof the encoding is right.
//
// | Addr   | Bytes       | Instruction            |
// |--------|-------------|-------------------------|
// | $8000  | 78          | SEI                     |
// | $8001  | A2 FF       | LDX #$FF                |
// | $8003  | 9A          | TXS                     |
// | $8004  | 2C 02 20    | wait: BIT $2002         |
// | $8007  | 10 FB       | BPL wait                |
// | $8009  | A9 01       | LDA #$01                |
// | $800B  | 8D 16 40    | STA $4016 (strobe high) |
// | $800E  | A9 00       | LDA #$00                |
// | $8010  | 8D 16 40    | STA $4016 (strobe low, latches) |
// | $8013  | A2 00       | LDX #$00                |
// | $8015  | AD 16 40    | read: LDA $4016         |
// | $8018  | 29 01       | AND #$01                |
// | $801A  | 95 10       | STA $10,X               |
// | $801C  | E8          | INX                     |
// | $801D  | E0 08       | CPX #$08                |
// | $801F  | D0 F4       | BNE read                |
// | $8021  | 18          | CLC                     |
// | $8022  | A5 20       | LDA $20 (sticky checksum) |
// | $8024  | 65 10       | ADC $10                 |
// | $8026  | 65 11       | ADC $11                 |
// | $8028  | 65 12       | ADC $12                 |
// | $802A  | 65 13       | ADC $13                 |
// | $802C  | 65 14       | ADC $14                 |
// | $802E  | 65 15       | ADC $15                 |
// | $8030  | 65 16       | ADC $16                 |
// | $8032  | 65 17       | ADC $17                 |
// | $8034  | 85 20       | STA $20                 |
// | $8036  | 4C 04 80    | JMP wait                |
#[rustfmt::skip]
const PROGRAM: &[(u16, &[u8])] = &[
    (0x8000, &[0x78]),
    (0x8001, &[0xA2, 0xFF]),
    (0x8003, &[0x9A]),
    (0x8004, &[0x2C, 0x02, 0x20]),
    (0x8007, &[0x10, 0xFB]),
    (0x8009, &[0xA9, 0x01]),
    (0x800B, &[0x8D, 0x16, 0x40]),
    (0x800E, &[0xA9, 0x00]),
    (0x8010, &[0x8D, 0x16, 0x40]),
    (0x8013, &[0xA2, 0x00]),
    (0x8015, &[0xAD, 0x16, 0x40]),
    (0x8018, &[0x29, 0x01]),
    (0x801A, &[0x95, 0x10]),
    (0x801C, &[0xE8]),
    (0x801D, &[0xE0, 0x08]),
    (0x801F, &[0xD0, 0xF4]),
    (0x8021, &[0x18]),
    (0x8022, &[0xA5, 0x20]),
    (0x8024, &[0x65, 0x10]),
    (0x8026, &[0x65, 0x11]),
    (0x8028, &[0x65, 0x12]),
    (0x802A, &[0x65, 0x13]),
    (0x802C, &[0x65, 0x14]),
    (0x802E, &[0x65, 0x15]),
    (0x8030, &[0x65, 0x16]),
    (0x8032, &[0x65, 0x17]),
    (0x8034, &[0x85, 0x20]),
    (0x8036, &[0x4C, 0x04, 0x80]),
];

/// Build the iNES image: 16-byte header (same layout
/// `crates/retroforge/src/stepper.rs`'s own `synthetic_nrom` test fixture
/// uses — magic, 1x16KiB PRG, 1x8KiB CHR, mapper 0 / iNES 1.0), then the
/// 16 KiB PRG bank with [`PROGRAM`] at offset 0 and the reset vector at
/// PRG offset `$3FFC` pointing at `$8000`. Rendering is never enabled
/// (PPUMASK untouched) — the vblank flag in `$2002` bit 7 still sets at
/// scanline 241 regardless, which is all this fixture needs.
fn controller_reader_rom() -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&INES_MAGIC);
    data.push(1); // 1x16KiB PRG
    data.push(1); // 1x8KiB CHR
    data.extend_from_slice(&[0u8; 10]); // flags6/7 + 8 reserved => mapper 0

    let mut prg = vec![0u8; 16 * 1024];
    for (addr, bytes) in PROGRAM {
        let offset = (*addr - 0x8000) as usize;
        prg[offset..offset + bytes.len()].copy_from_slice(bytes);
    }
    prg[0x3FFC] = 0x00; // reset vector low  -> $8000
    prg[0x3FFD] = 0x80; // reset vector high
    data.extend(prg);
    data.extend(vec![0u8; 8 * 1024]); // CHR, unused (rendering never enabled)
    data
}

fn frame(bits: u16) -> InputFrame {
    let mut f = InputFrame::empty();
    f.ports[0] = bits;
    f
}

/// A fixed, deterministic input script: A held every 3rd frame, except
/// from `diverge_at` (if given) onward, where B is held **continuously**
/// instead of the periodic A pattern — a persistent divergence, not a
/// single substituted frame (module doc's "Finding" explains why a single
/// frame isn't reliable enough for this particular fixture).
fn build_script(len: usize, diverge_at: Option<usize>) -> Vec<InputFrame> {
    (0..len)
        .map(|i| {
            let diverged = diverge_at.is_some_and(|d| i >= d);
            let bits: u16 = if diverged {
                0b0000_0010 // B, held continuously from diverge_at onward
            } else if i % 3 == 0 {
                0b0000_0001 // A every 3rd frame
            } else {
                0
            };
            frame(bits)
        })
        .collect()
}

struct RunResult {
    /// One reachable-state hash per frame, in order (index 0 = after the
    /// first `latch_and_advance_frame` call).
    state_hashes: Vec<String>,
    /// The rendered framebuffer's digest after the last frame — a
    /// separately named component (module doc / `EmuStepper::state_hash`'s
    /// doc): output, not state.
    final_framebuffer_hash: String,
}

fn run_script(script: &[InputFrame]) -> RunResult {
    let mut stepper = EmuStepper::from_ines_bytes(&controller_reader_rom())
        .expect("fixture ROM must be a valid iNES image");
    let mut sink = rf_renderer::FrameBuffer::new();
    let mut state_hashes = Vec::with_capacity(script.len());
    for &input in script {
        stepper.latch_and_advance_frame(input, &mut sink);
        state_hashes.push(stepper.state_hash());
    }
    RunResult {
        state_hashes,
        final_framebuffer_hash: retroforge::hash::sha256_hex(&sink.to_vec()),
    }
}

/// Criterion 3, positive half: same input log, two independent runs (each
/// with its own fresh `EmuStepper`) must produce identical **per-frame**
/// hash sequences, not just a matching final hash — per-frame is what
/// makes a future failure locatable to one frame instead of "somewhere in
/// N frames".
#[test]
fn same_log_two_independent_runs_produce_identical_hash_sequences() {
    let script = build_script(24, None);
    let run1 = run_script(&script);
    let run2 = run_script(&script);
    assert_eq!(
        run1.state_hashes, run2.state_hashes,
        "identical input logs must produce identical per-frame state-hash sequences"
    );
    assert_eq!(
        run1.final_framebuffer_hash, run2.final_framebuffer_hash,
        "identical input logs must also reproduce the same rendered output"
    );
}

/// Criterion 3, anti-vacuity half (mandatory — without this, the test
/// above proves nothing, the same lesson as corrupting one line of
/// `nestest.log` and expecting exactly one mismatch). Two logs that agree
/// up to `diverge_at` and then hold different buttons must diverge by some
/// frame at or shortly after `diverge_at` (module doc's "Finding" — the
/// exact frame the divergence is first observed can slip by the fixture's
/// own periodic miss, so this locates it empirically rather than assuming
/// it is `diverge_at` itself) and — because the fixture's checksum is
/// sticky (`ADC`, never reset) — must **stay** diverged from there on,
/// never silently healing back to matching.
#[test]
fn different_logs_produce_different_and_sticky_hash_sequences() {
    let diverge_at = 8;
    let len = 32; // long enough to guarantee the persistent divergence is
                  // captured at least once even accounting for the
                  // fixture's ~12-frame miss period (module doc)
    let script_a = build_script(len, None);
    let script_b = build_script(len, Some(diverge_at));

    let run_a = run_script(&script_a);
    let run_b = run_script(&script_b);

    assert_ne!(
        run_a.state_hashes, run_b.state_hashes,
        "different input logs must produce different hash sequences"
    );

    for i in 0..diverge_at {
        assert_eq!(
            run_a.state_hashes[i], run_b.state_hashes[i],
            "frame {i} precedes the injected divergence and must still match"
        );
    }

    let first_divergent = (diverge_at..len)
        .find(|&i| run_a.state_hashes[i] != run_b.state_hashes[i])
        .expect("a button held continuously for 24 frames must be captured at least once");
    assert!(
        first_divergent < diverge_at + 12,
        "must be captured well within the fixture's measured ~12-frame miss \
         period (module doc), not merely 'somewhere before len' — a wider \
         gap here would mean the sticky-checksum check below covers too few \
         frames to be meaningful"
    );

    for i in first_divergent..len {
        assert_ne!(
            run_a.state_hashes[i], run_b.state_hashes[i],
            "frame {i} must remain diverged once captured — sticky checksum must not silently heal"
        );
    }
}

/// Criterion 3's third leg: a known button byte must land at the known
/// WRAM address the fixture writes it to, proving the actual `$4016`
/// read path works — not merely "some hash differs somewhere".
///
/// ## Measuring the pipeline delay
///
/// The host latch (`EmuStepper::latch_and_advance_frame`) happens at a
/// frame boundary; the ROM reads `$4016` during vblank, which is a
/// *different* point within that same frame boundary's PPU sweep — so the
/// two could in principle disagree about which frame a press "belongs
/// to". Measured directly (holding input empty for several calls, then
/// switching to a fixed nonzero pattern and printing `$0010..$0017` each
/// call): the pressed pattern is visible in WRAM by the end of the very
/// same `latch_and_advance_frame` call whose `InputFrame` carried it —
/// zero extra frame delay. This is not a coincidence: `NesBus::frame_count`
/// increments at the pre-render scanline's wrap to scanline 0 (261->0),
/// which happens strictly *after* vblank sets at scanline 241 — the point
/// where this fixture's `wait`/`read` loop fires when it isn't hitting the
/// periodic miss described in the module doc — within the same PPU sweep.
/// So the controller read for "this frame", when it happens at all,
/// always completes before the very call that closes the frame out
/// returns. The one exception is the first call ever made: `EmuStepper`
/// starts mid pre-render scanline (`crates/retroforge/src/stepper.rs`'s
/// own tests document this "boot frame" artifact), so its `frame_count`
/// tick fires almost immediately, before the CPU has executed far enough
/// to reach the read loop at all — this test's first call is therefore a
/// deliberate empty "boot artifact" call, exactly like that module's own
/// tests skip past it, and frame index 1 (this test's second call) is not
/// one of the periodic-miss frames (module doc: period 12 starting at
/// frame 8), so it reliably captures.
#[test]
fn positive_known_button_byte_lands_at_expected_wram_address() {
    let mut stepper = EmuStepper::from_ines_bytes(&controller_reader_rom())
        .expect("fixture ROM must be a valid iNES image");
    let mut sink = rf_renderer::FrameBuffer::new();

    // Boot-artifact call: completes almost immediately, before the CPU
    // reaches its first controller read (module doc).
    stepper.latch_and_advance_frame(InputFrame::empty(), &mut sink);

    // Go through the REAL host pipeline (rf_input::Key -> KeyMap ->
    // InputLatch -> InputFrame), not a hand-rolled bit literal — this is
    // what makes a `KeyMap::default_nes` mistake (e.g. two entries
    // swapped) show up here rather than only in `rf-input`'s own unit
    // tests. Default keymap: X -> A, Enter -> Start.
    let mut latch = rf_input::InputLatch::new();
    latch.key_down(rf_input::Key::X);
    latch.key_down(rf_input::Key::Enter);
    let pressed = latch.sample(&rf_input::KeyMap::default_nes());
    stepper.latch_and_advance_frame(pressed, &mut sink);

    let raw: Vec<u8> = (0x10u16..=0x17).map(|addr| stepper.peek(addr)).collect();
    assert_eq!(
        raw,
        vec![1, 0, 0, 1, 0, 0, 0, 0],
        "raw per-button bits at $0010-$0017 (A, B, Select, Start, Up, Down, \
         Left, Right order) must show A and Start held (X and Enter, per the \
         default keymap), everything else clear"
    );
}

/// Criterion 2 (FR-STATE-006): record a run through
/// `ReplayRecorder`/`EmuStepper::latch_and_advance_frame` — the one
/// shared latch-then-advance path (`crate::stepper` module doc) — then
/// serialize, parse, and replay through the exact same shared function.
/// Every periodic + the final hash must match, byte-exact, and the text
/// itself must round-trip byte-exact.
#[test]
fn replay_round_trip_matches_recorded_hashes_byte_exact() {
    let rom = controller_reader_rom();
    let rom_sha256 = rf_cart::hash::identity_nes(&rom).normalized.sha256;
    let script = build_script(17, Some(8)); // not a multiple of hash_interval
    let hash_interval = 5u64;

    let header = rf_input::ReplayHeader {
        console: "nes".to_string(),
        rom_sha256: rom_sha256.clone(),
        emu_version: "0.1.0".to_string(),
        core_config: "accuracy".to_string(),
        start_type: rf_input::StartType::PowerOn,
        hash_kind: "reachable-v1".to_string(),
        hash_interval,
    };

    // --- record ---
    let mut recorder = rf_input::ReplayRecorder::new(header);
    let mut rec_stepper = EmuStepper::from_ines_bytes(&rom).expect("valid fixture ROM");
    let mut rec_sink = rf_renderer::FrameBuffer::new();
    for (i, &input) in script.iter().enumerate() {
        rec_stepper.latch_and_advance_frame(input, &mut rec_sink);
        recorder.record_frame(input);
        let frame_no = i as u64;
        let is_final = i + 1 == script.len();
        if frame_no.is_multiple_of(hash_interval) || is_final {
            recorder.record_hash(frame_no, rec_stepper.state_hash());
        }
    }
    let log = recorder.finish();
    log.verify_rom_sha256(&rom_sha256)
        .expect("recorded log's own rom_sha256 must match the ROM it was recorded from");

    // --- serialize / parse round trip ---
    let text = log.to_string();
    let parsed = rf_input::ReplayLog::parse(&text).expect("recorded replay text must parse");
    assert_eq!(
        text,
        parsed.to_string(),
        "serialize -> parse -> serialize must be byte-exact"
    );
    assert_eq!(log, parsed);

    // --- replay ---
    let mut player = rf_input::ReplayPlayer::new(&parsed);
    let mut play_stepper = EmuStepper::from_ines_bytes(&rom).expect("valid fixture ROM");
    let mut play_sink = rf_renderer::FrameBuffer::new();
    let mut frame_no = 0u64;
    let mut hashes_checked = 0;
    while let Some(input) = player.next_frame() {
        play_stepper.latch_and_advance_frame(input, &mut play_sink);
        if let Some(expected) = player.expected_hash(frame_no) {
            assert_eq!(
                play_stepper.state_hash(),
                expected,
                "replayed state hash at frame {frame_no} must match the recorded one"
            );
            hashes_checked += 1;
        }
        frame_no += 1;
    }
    assert_eq!(
        hashes_checked,
        log.hashes.len(),
        "every recorded hash must have been checked during replay"
    );
    assert!(
        log.hashes
            .last()
            .is_some_and(|&(f, _)| f == script.len() as u64 - 1),
        "the final frame must always carry a hash, per hash_interval cadence + final-frame rule"
    );
}

// ---------------------------------------------------------------------
// Refusal cases, exercised end-to-end against a real ROM-derived replay
// (rf-input's own unit tests already cover the format-level parser in
// isolation; these prove the same refusals hold once a real recorded
// `.rfreplay` from this fixture is involved).
// ---------------------------------------------------------------------

fn recorded_replay_text() -> (String, String) {
    let rom = controller_reader_rom();
    let rom_sha256 = rf_cart::hash::identity_nes(&rom).normalized.sha256;
    let header = rf_input::ReplayHeader {
        console: "nes".to_string(),
        rom_sha256: rom_sha256.clone(),
        emu_version: "0.1.0".to_string(),
        core_config: "accuracy".to_string(),
        start_type: rf_input::StartType::PowerOn,
        hash_kind: "reachable-v1".to_string(),
        hash_interval: 60,
    };
    let mut recorder = rf_input::ReplayRecorder::new(header);
    let mut stepper = EmuStepper::from_ines_bytes(&rom).expect("valid fixture ROM");
    let mut sink = rf_renderer::FrameBuffer::new();
    for &input in &build_script(3, None) {
        stepper.latch_and_advance_frame(input, &mut sink);
        recorder.record_frame(input);
    }
    recorder.record_hash(2, stepper.state_hash());
    (recorder.finish().to_string(), rom_sha256)
}

#[test]
fn refusal_wrong_rom_sha256() {
    let (text, rom_sha256) = recorded_replay_text();
    let log = rf_input::ReplayLog::parse(&text).unwrap();
    assert!(log.verify_rom_sha256(&rom_sha256).is_ok());
    let wrong = "f".repeat(64);
    let err = log.verify_rom_sha256(&wrong).unwrap_err();
    assert!(matches!(err, rf_input::ReplayError::RomShaMismatch { .. }));
}

#[test]
fn refusal_bad_magic_and_version() {
    let (text, _) = recorded_replay_text();
    let bad_magic = text.replacen("RFREPLAY 1", "NOTAREPLAY 1", 1);
    assert!(matches!(
        rf_input::ReplayLog::parse(&bad_magic),
        Err(rf_input::ReplayError::BadMagic(_))
    ));

    let bad_version = text.replacen("RFREPLAY 1", "RFREPLAY 99", 1);
    assert!(matches!(
        rf_input::ReplayLog::parse(&bad_version),
        Err(rf_input::ReplayError::UnsupportedVersion(_))
    ));
}

#[test]
fn refusal_malformed_input_line() {
    let (text, _) = recorded_replay_text();
    // Corrupt frame 1's (all-unpressed) `[Input]` line by dropping one
    // char from its first port group (8 dots -> 7), which must fail the
    // per-port char-count check rather than being silently accepted.
    let corrupted = text.replacen("|........|........|\n", "|.......|........|\n", 1);
    assert_ne!(corrupted, text, "test setup must actually change something");
    assert!(matches!(
        rf_input::ReplayLog::parse(&corrupted),
        Err(rf_input::ReplayError::MalformedInputLine { .. })
    ));
}

#[test]
fn refusal_crlf() {
    let (text, _) = recorded_replay_text();
    let crlf = text.replace('\n', "\r\n");
    assert!(matches!(
        rf_input::ReplayLog::parse(&crlf),
        Err(rf_input::ReplayError::CrlfNotAllowed)
    ));
}
