//! RF-Scroller 5-minute replay: the Tier-A real-game regression for the
//! project's own in-repo NES fixture (FR-CORE-026, ticket W2-10,
//! `docs/TESTING.md` §4's "RF-Scroller 5-min replay" row). RF-Scroller
//! (`fixtures/nes/rf-scroller`, D-001) is built from source, not fetched
//! -- its ROM binary never enters git (gitignored `build/`); only
//! `fixtures/nes/rf-scroller/rom.sha256` is checked in, and
//! `fixtures/nes/rf-scroller/build.sh` is the single command (used
//! identically here, in CI, and by a developer rebuilding locally) that
//! turns source into that exact hash.
//!
//! ## Shape, deliberately mirrored from `alter_ego_replay.rs`
//!
//! Same `NesBus::from_ines_bytes` + `Cpu::power_on` setup, the same
//! `run_frame` helper, the same `reachable_state_hash` (duplicated, not
//! shared -- see that file's module doc for why), the same
//! record-then-independently-replay `.rfreplay` round trip via
//! `rf_input`'s `ReplayRecorder`/`ReplayLog`/`ReplayPlayer`, the same
//! fast always-run anti-vacuity test plus a slow `#[ignore]`'d full
//! 5-minute replay. The one structural difference: RF-Scroller has no
//! `tests/rom-manifest.toml` entry by design (that file's own comment:
//! in-repo fixtures "have no entry here by design; see FR-CORE-026/
//! 035/037") -- [`resolve_rom`] below looks for the *built* ROM directly
//! rather than a fetched/manifest-verified archive.
//!
//! ## Anti-vacuity (the brief's central trap, and this fixture's own
//! history with it)
//!
//! A prior version of this fixture's runtime streamed content into VRAM
//! late enough that the HUD/playfield split visibly glitched on the
//! frames it happened -- see `fixtures/nes/rf-scroller/FORMAT.md`'s
//! "Known defects" section for the full, honest account (root cause,
//! measurements, and the residual that shipped anyway). That defect was
//! found by *driving the scripted input far enough to exercise real
//! wraparound*, not by a hash matching itself -- the exact anti-vacuity
//! discipline this test's own fast check exists to enforce going
//! forward: [`rf_scroller_scripted_input_actually_drives_the_game`]
//! asserts [`PLAYER_X_ADDR`], [`CAMERA_X_ADDR`], and
//! [`COLUMNS_STREAMED_ADDR`] all reach specific, non-boot values --
//! specifically `columns_streamed == 95`, which (per FORMAT.md's
//! "Streaming writes" section) is only reachable once a physical
//! nametable has been genuinely reused for new content. A do-nothing
//! input log leaves `columns_streamed == 63` (the initial two-nametable
//! preload only) forever -- verified by hand (conductor note): replacing
//! [`scripted_buttons`] with a constant `0` leaves `player_x == 16`
//! (spawn), `camera_x == 0`, and `columns_streamed == 63` at frame 900,
//! failing all three assertions below.
use rf_core_api::{CoreEvent, CoreSink, InputFrame, PpuPixel};
use rf_harness::{hash_frame_palette_indices, hex_sha256};
use rf_input::{NesButton, ReplayHeader, ReplayLog, ReplayPlayer, ReplayRecorder, StartType};
use rf_nes::{Cpu, NesBus};
use std::path::PathBuf;

/// One NTSC frame at 60 fps, times 300 seconds = 5 minutes exactly (same
/// convention as `alter_ego_replay.rs::TOTAL_FRAMES`).
const TOTAL_FRAMES: u64 = 18_000;

/// `.rfreplay`'s periodic-hash cadence: 600 frames = 10 seconds, 30
/// checkpoints plus the final frame (same as `alter_ego_replay.rs`).
const HASH_INTERVAL: u64 = 600;

/// Golden-frame indices, chosen from the CONFIRMED CLEAN region of the
/// run (module doc / FORMAT.md "Known defects"): the residual
/// HUD/playfield-split-lands-late defect is correlated with
/// `stream_chunk()` actually doing chunk work, which only happens while
/// `columns_streamed < 95`. Every index below was individually verified
/// (via a throwaway instrumented run of the same ROM, comparing each
/// candidate frame's `ScrollWrite` placement against the vblank/
/// scanline-14-18 window) to land AFTER `columns_streamed` reaches 95
/// (streaming fully drained) and to be free of the defect at that exact
/// frame -- not merely "probably fine because streaming looks done".
/// `655` and `1200` are close to the streaming/idle boundary
/// specifically to prove the split is clean even shortly after
/// wraparound completes, not just deep in a settled idle tail.
const GOLDEN_FRAMES: [u64; 6] = [655, 1200, 2400, 5000, 10000, 15000];

/// Documented RAM addresses (`FORMAT.md` "Documented RAM addresses").
/// `FRAME_COUNTER_ADDR` moved from `0x602E` (its W2-10 address) to
/// `0x6030` at ticket W2-10a -- see that section's "Address re-pin" note:
/// inserting `camera_y`/`blink_visible` (both non-`static` globals, like
/// `player_x`/`camera_x`/`columns_streamed`) ahead of `frame_counter` in
/// source order pushed `frame_counter` (a `static`) two bytes later,
/// because cc65 buckets non-`static` globals together ahead of file-scope
/// statics regardless of source interleaving -- confirmed by rebuilding
/// with `-g -Wl --dbgfile,...` and reading the resulting symbol table
/// (the checked-in `build/rf-scroller.dbg` is stale and must not be
/// trusted -- `build.sh` never regenerates it).
const PLAYER_X_ADDR: u16 = 0x6029;
const CAMERA_X_ADDR: u16 = 0x602B;
const COLUMNS_STREAMED_ADDR: u16 = 0x602D;
const FRAME_COUNTER_ADDR: u16 = 0x6030;

/// Level-end / wraparound-complete values (`FORMAT.md`): `player_x`
/// saturates at `MAX_PLAYER_X` (752), `camera_x` at `MAX_CAMERA_X` (512),
/// and `columns_streamed` reaches 95 (the level's last raw tile column)
/// only once every raw column -- including the ones that required
/// physical-nametable reuse -- has been streamed.
const PLAYER_X_AT_END: u16 = 752;
const CAMERA_X_AT_END: u16 = 512;
const COLUMNS_STREAMED_AT_END: u8 = 95;

struct NullSink;
impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _ev: CoreEvent) {}
}

/// Golden-capture robustness fix (ticket W2-10a, discovered while
/// regenerating `GOLDEN_HASHES` for this ticket's ROM): `run_frame`'s stop
/// condition ("`bus.frame_count()` changed") does not guarantee a fresh
/// sink receives exactly scanlines 0..239 of one frame. `OAM_DMA` is a
/// ~513-CPU-cycle (~1539-dot, ~5.9-scanline) halting write; the single
/// `cpu.step()` call that executes it can span a `Scanline` dot-256 event
/// for several scanlines at once, INCLUDING, if timing lines up, a few of
/// the NEXT frame's early scanlines -- which land in `drain_video`'s
/// caller-supplied sink for whichever `run_frame` call happens to be
/// active at that instant. A prior version of this test used
/// `rf_harness::FrameCapture` per golden index (fresh `FrameCapture::new()`
/// per capture) and panicked ("expected 0, got 3") the first time this
/// ticket's cycle-count changes (the `read_buttons()`/call-site-gating fix
/// -- see FORMAT.md) shifted which frame straddles a boundary -- this ROM
/// always had this latent fragility; the OLD ROM's goldens (655, 1200,
/// ...) simply never happened to land on a straddling frame.
///
/// Earlier fix attempts here (documented for the next person who hits
/// this, not left in the diff): (1) a `TolerantFrameCapture` shared across
/// the golden call and the PRECEDING call, resetting on every `y==0` --
/// recovered 655/1200 but discarded golden 2400's own complete data when
/// THAT call itself straddled forward into 2401. (2) a single-call
/// version banking "the most recent complete 240-row frame" and returning
/// the instant `frame_count` advances -- panicked instead of silently
/// mislabeling (good), but revealed that golden 655's own LEADING rows
/// were already being lost to the call BEFORE it (the preceding, ordinary
/// `run_frame`+`NullSink` call): a fresh capture starting mid-frame can
/// see fewer than 240 rows before the next `y==0`, so it never banks
/// anything for the target at all.
///
/// The fix that actually holds: track "most recently completed 240-row
/// frame" the same way as attempt (2), but thread ONE persistent
/// [`GoldenCaptureState`] across BOTH the frame immediately before a
/// golden AND the golden itself (`GOLDEN_FRAMES` entries are >600 frames
/// apart, so this never needs to look back more than one frame). Any
/// straggler rows from the target that leak into the PRECEDING call are
/// no longer lost to a `NullSink` -- they accumulate into the SAME
/// `current` buffer that will go on to complete the target frame during
/// the following call. `last_complete` gets overwritten every time a
/// frame genuinely completes (first the priming frame's own, then the
/// target's), so by the time the target's own call ticks `frame_count`
/// past its start, whatever's banked is guaranteed to be the target's own
/// data -- and if it isn't (some larger drift ate the target's OWN
/// trailing rows too), this panics loudly rather than mislabeling.
struct GoldenCaptureState {
    current: Vec<Vec<u8>>,
    last_complete: Option<Vec<Vec<u8>>>,
}
impl GoldenCaptureState {
    fn new() -> Self {
        GoldenCaptureState {
            current: Vec::new(),
            last_complete: None,
        }
    }
}
struct GoldenCaptureSink<'a> {
    state: &'a mut GoldenCaptureState,
}
impl CoreSink for GoldenCaptureSink<'_> {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        if y == 0 {
            if self.state.current.len() == 240 {
                self.state.last_complete = Some(std::mem::take(&mut self.state.current));
            } else {
                self.state.current.clear();
            }
        }
        self.state
            .current
            .push(pixels.iter().map(|p| p.palette_index).collect());
    }
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _ev: CoreEvent) {}
}

/// Same contract as `run_frame` (advances `bus.frame_count()` by exactly
/// one), but feeds scanlines into a persistent [`GoldenCaptureState`]
/// instead of an arbitrary sink -- see that struct's doc for why.
fn run_frame_tracking_goldens(
    bus: &mut NesBus,
    cpu: &mut Cpu,
    buttons: u8,
    state: &mut GoldenCaptureState,
) {
    bus.set_controller_buttons(0, buttons);
    let start = bus.frame_count();
    let mut sink = GoldenCaptureSink { state };
    let mut guard = 0u64;
    while bus.frame_count() == start {
        cpu.step(bus);
        bus.drain_video(&mut sink);
        guard += 1;
        assert!(
            guard <= 400_000,
            "frame did not complete within guard cycles at frame {start}"
        );
    }
}

/// Called only when `frame` is a `GOLDEN_FRAMES` entry, immediately after
/// `run_frame_tracking_goldens` was used for both `frame` and `frame - 1`
/// (`GoldenCaptureState`'s own doc). Takes and validates the banked
/// capture.
fn take_golden_capture(state: &mut GoldenCaptureState, frame: u64) -> Vec<Vec<u8>> {
    let rows = state.last_complete.take().unwrap_or_else(|| {
        panic!(
            "golden frame {frame}: no complete 240-row frame was ever banked -- \
             straggler-scanline recovery failed entirely (see GoldenCaptureState's doc)"
        )
    });
    assert_eq!(
        rows.len(),
        240,
        "golden frame {frame}: banked capture had {} rows, not 240 -- GoldenCaptureSink's own \
         invariant (only banks at exactly 240) should make this impossible",
        rows.len()
    );
    rows
}

/// Resolves the built (never committed, gitignored `build/`) ROM:
/// `RF_SCROLLER_ROM` env var if set and it names a real file, else the
/// default relative-to-this-crate path if that is a real file, else
/// `None` (the absence-skip path, same convention
/// `alter_ego_replay.rs::resolve_zip` and
/// `crates/rf-nes/src/system/tests/nestest.rs::resolve` use).
fn resolve_rom() -> Option<PathBuf> {
    if let Ok(configured) = std::env::var("RF_SCROLLER_ROM") {
        let path = PathBuf::from(configured);
        return if path.is_file() { Some(path) } else { None };
    }
    let default = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/nes/rf-scroller/build/rf-scroller.nes");
    if default.is_file() {
        Some(default)
    } else {
        None
    }
}

/// Reads `fixtures/nes/rf-scroller/rom.sha256`'s pinned hash (the
/// checked-in half of D-001's "hash in git, binary never in git" split).
/// This is a RAW-FILE-BYTES sha256 (`build.sh`'s own convention: `shasum
/// -a 256` over the built `.nes`), deliberately checked with
/// `hex_sha256(&rom_bytes)` below rather than `rf_cart::hash::
/// identity_nes` -- `identity_nes` computes a normalized hash over the
/// parsed cartridge (used for the `.rfreplay` header's `rom_sha256`
/// field below, where header-byte-insensitivity is the point) and does
/// NOT agree with a raw-file shasum; comparing the two would be
/// comparing different things by name coincidence alone (confirmed
/// empirically: they produced different hashes for the same file when
/// this test was first written).
fn expected_rom_sha256() -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/nes/rf-scroller/rom.sha256");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    text.split_whitespace()
        .next()
        .unwrap_or_else(|| panic!("{} is empty", path.display()))
        .to_string()
}

/// Holds Right for every frame -- this fixture has exactly one input
/// (`FORMAT.md`'s "What this ticket's runtime does NOT do": no Left, no
/// jump). `main.c`'s own movement gate (`frame_counter & 0x07u`, 8px
/// every 8 frames) is what turns a constant "Right held" input into
/// discrete, budget-paced movement -- this function does not need to
/// replicate that pacing itself.
fn scripted_buttons(_frame: u64) -> u8 {
    1u8 << NesButton::Right.bit()
}

/// Latch `buttons` into port 0 and run exactly one frame, streaming video
/// to `sink` (same shape as `alter_ego_replay.rs::run_frame`).
fn run_frame(bus: &mut NesBus, cpu: &mut Cpu, buttons: u8, sink: &mut dyn CoreSink) {
    bus.set_controller_buttons(0, buttons);
    let start = bus.frame_count();
    let mut guard = 0u64;
    while bus.frame_count() == start {
        cpu.step(bus);
        bus.drain_video(sink);
        guard += 1;
        assert!(
            guard <= 400_000,
            "frame did not complete within guard cycles at frame {start}"
        );
    }
}

/// `camera_x`/`player_x` are little-endian `unsigned int` in `main.c`
/// (`FORMAT.md`'s RAM address table) -- read both bytes.
fn peek_u16(bus: &NesBus, addr: u16) -> u16 {
    u16::from(bus.peek(addr)) | (u16::from(bus.peek(addr + 1)) << 8)
}

/// Root-caused at ticket W2-10a (recorded in FORMAT.md's "Address re-pin"
/// section): `run_frame` returns the instant `bus.frame_count()` (a PPU-
/// clock-driven counter, independent of `main.c`'s own control flow)
/// ticks -- that tick can land at ANY point inside one `main_loop()`
/// iteration's C-level body, including between `camera_x`'s two writes in
/// the same iteration (the ternary's provisional value, then the clamp's
/// corrected one a few instructions later). A `bus.peek(CAMERA_X_ADDR)`
/// taken at an arbitrary `run_frame` boundary can therefore observe the
/// PRE-clamp value even though the clamp always fires correctly by the
/// time the iteration actually finishes -- confirmed by single-stepping
/// `Cpu::pc` through the clamp's machine code at several frame indices
/// and finding it fires every time, while the SAME peek taken immediately
/// after alternated between the clamped and unclamped value frame to
/// frame. `camera_x` is the only one of the three RAM witnesses written
/// TWICE per iteration (`player_x` and `columns_streamed` are each
/// written at most once), which is exactly why only `camera_x` exhibited
/// this.
///
/// This is a genuine, pre-existing latent fragility in sampling `camera_x`
/// at an arbitrary `run_frame` boundary -- not something this ticket's
/// added per-frame cost created, though added cost does change which
/// frames land on which side of the ternary/clamp gap, which is how this
/// was found (see the ticket report's mutation/verification section).
/// The fix is a real synchronization point in the PROGRAM's own terms
/// rather than a PPU-side coincidence: `frame_counter` (module doc /
/// FORMAT.md) increments exactly once, near the very top of each
/// `main_loop()` iteration, right after that iteration's own
/// `read_buttons()` call. Once `frame_counter` is OBSERVED to have
/// changed value, the PREVIOUS iteration -- including its own
/// `camera_x` clamp, OAM DMA, and split write -- is GUARANTEED to have
/// run to completion (main_loop is a single sequential loop; iteration
/// N+1 cannot begin incrementing frame_counter until iteration N's full
/// body, all the way through its split-write, has finished). Advancing
/// until `frame_counter` changes therefore samples a point that is
/// structurally after the clamp, not merely usually after it.
fn settle_to_iteration_boundary(
    bus: &mut NesBus,
    cpu: &mut Cpu,
    scripted_from_frame: u64,
    sink: &mut dyn CoreSink,
) {
    let before = bus.peek(FRAME_COUNTER_ADDR);
    let mut extra = 0u64;
    while bus.peek(FRAME_COUNTER_ADDR) == before {
        run_frame(
            bus,
            cpu,
            scripted_buttons(scripted_from_frame + extra),
            sink,
        );
        extra += 1;
        assert!(
            extra <= 20,
            "frame_counter did not advance within 20 extra frames -- main_loop() appears stuck"
        );
    }
}

/// The "reachable-v1" state hash -- byte-for-byte the same formula as
/// `alter_ego_replay.rs::reachable_state_hash` and
/// `crates/retroforge/src/stepper.rs::EmuStepper::state_hash`, duplicated
/// here for the same reason that file's module doc gives (rf-harness
/// cannot depend on the `retroforge` app-shell crate).
fn reachable_state_hash(bus: &NesBus, cpu: &Cpu) -> String {
    let mut buf = Vec::with_capacity(0x0800 + 256 + 0x2000 + 32);
    for addr in 0x0000u16..=0x07FF {
        buf.push(bus.peek(addr));
    }
    buf.extend_from_slice(bus.oam());
    buf.extend_from_slice(bus.prg_ram());
    buf.push(cpu.a);
    buf.push(cpu.x);
    buf.push(cpu.y);
    buf.push(cpu.s);
    buf.push(cpu.p);
    buf.extend_from_slice(&cpu.pc.to_le_bytes());
    buf.push(u8::from(cpu.jammed));
    buf.extend_from_slice(&bus.master_cycle().to_le_bytes());
    buf.extend_from_slice(&bus.frame_count().to_le_bytes());
    hex_sha256(&buf)
}

/// Re-frozen a SECOND time for ticket W2-10a, after a conductor-side `git
/// checkout` destroyed the in-flight `main.c` mid-ticket (recorded in
/// `plan.json`'s W2-10a notes) and the reconstruction fixed a real bug the
/// first pass had shipped without catching (gems corrupted in transit to
/// `Ppu::oam()` via an `OAMADDR`-reset-during-DMA interaction -- see
/// FORMAT.md's "Sprite-overflow scene" and "The `git checkout` incident"
/// sections). Golden-frame hashes for [`GOLDEN_FRAMES`], SHA-256 over
/// `palette_index` only (`hash_frame_palette_indices`), in frame order,
/// against ROM
/// `sha256=c79f6f61ae82a87beaf57edb6757d75c1cfb2a1316196b1c8295f22a1699e087`
/// (`rom.sha256`). The reorder that fixed the gem transfer (tail-scene
/// updates now run AFTER the split write, one-frame latency) legitimately
/// changes every golden frame's rendered content again -- the prior
/// hashes (first W2-10a freeze) no longer apply and are not a regression.
///
/// **Verified before freezing** (same discipline as both prior freezes,
/// re-run against this ROM): (1) this test's own recording run and
/// independent replay run agree on all six byte-exact (the assertion
/// using this constant, which fails loudly on any divergence -- see the
/// golden-frame comparison below) -- this is the actual determinism
/// proof; the frozen-constant comparison below it is a regression trip
/// wire, not the proof itself. (2) `columns_streamed` is `95` and
/// `player_x`/`camera_x` are at their level-end values at every one of
/// these frame indices (all six are well after wraparound completion, in
/// the same idle tail `FORMAT.md`'s split-timing section characterizes).
///
/// Unlike the first W2-10a freeze, golden frames 2400 and 10000 do NOT
/// hash identically this time (that was a coincidence of the prior
/// `gem_order[]` rotation scheme's own phase alignment, not a fixture
/// property -- the redesigned `update_gems()` that fixed the OAM transfer
/// also changed that incidental alignment).
/// RE-FROZEN 2026-08-17 by ticket **W2-21**, and the re-verification the
/// doc above demands was done by the test itself rather than by eye.
///
/// Cause: W2-21 flipped the OAM-DMA get/put phase (resolved against
/// blargg's `cpu_interrupts_v2` `4-irq_and_dma`, which pins it) and
/// corrected the CPU's view of the APU IRQ line and the taken-branch
/// interrupt poll. This fixture runs an OAM DMA every frame, so a
/// one-cycle change in DMA length shifts rendering by a cycle and every
/// golden hash necessarily moves.
///
/// Why this is a re-baseline and not a masked regression — all three
/// checks that make these goldens *mean* something still passed, and they
/// are asserted BEFORE the frozen comparison, so the run reached this
/// point having already proved them:
///   * the recording run and the INDEPENDENT replay produced byte-identical
///     goldens (determinism intact);
///   * every recorded periodic state hash matched on replay;
///   * the anti-vacuity witnesses are unchanged — `player_x` still reaches
///     `PLAYER_X_AT_END` and `columns_streamed` still reaches
///     `COLUMNS_STREAMED_AT_END`, reproduced from the log alone.
///
/// The game therefore plays out identically; only the pixels moved, which
/// is exactly what a one-cycle DMA shift does. (W2-11's lesson — a replay
/// can look healthy by hashes while the player has actually died and
/// returned to the title screen — is what those witnesses exist for.)
/// # Regenerated at W7-14 (2026-08-20), after diagnosis rather than by
/// re-pinning
///
/// These had been stale since **W5-02c**, which changed the fixture ROM
/// (`rom.sha256` moved in 226b137) without regenerating the goldens that
/// gate it (last touched in 3d9607e, W2-21). The suite was red locally
/// and green in CI the whole time, because it is `#[ignore]`d and CI runs
/// plain `cargo test --workspace`. That gap is now closed by
/// `scripts/local-gate.sh`, which runs this suite.
///
/// **The last five entries are identical, and that is correct.** The
/// level is 96 raw tile columns (FORMAT.md), so a scripted log of
/// constant Right walks the player to the end at around frame 900 and
/// parks him there: `player_x` reaches 240, `columns_streamed` reaches
/// 95, and the screen stops changing. Frames 1200, 2400, 5000, 10000 and
/// 15000 are all the same static end-of-level view.
///
/// Far from being wasted coverage, that makes them a **drift detector**:
/// five identical hashes spread over four minutes prove the emulator
/// produces a bit-identical frame from identical state indefinitely.
/// A drift of any kind — an uninitialised byte, an accumulating counter
/// leaking into video, a mis-scheduled DMA — breaks them.
///
/// Each frame was **rendered to a PNG and looked at** before its hash was
/// pinned (655 shows mid-level scrolling; 1200 and 15000 show the same
/// coherent end-of-level screen with HUD, platforms and collectibles).
/// A self-generated golden proves only that behaviour stopped changing,
/// so re-pinning without looking would have enshrined whatever was there
/// — which is precisely the trap W6-03b's golden runner was built to
/// avoid on the SNES side.
const GOLDEN_HASHES: [&str; 6] = [
    "ff81bcbcb485fd2f56f506e04dc3ac821469855d6b1f05c8416bab3cf86d9214",
    "c890423f5e8efed5f95daeaf4253242d0d80d8fb2ae88f4893503517bb4aa6c9",
    "c890423f5e8efed5f95daeaf4253242d0d80d8fb2ae88f4893503517bb4aa6c9",
    "c890423f5e8efed5f95daeaf4253242d0d80d8fb2ae88f4893503517bb4aa6c9",
    "c890423f5e8efed5f95daeaf4253242d0d80d8fb2ae88f4893503517bb4aa6c9",
    "c890423f5e8efed5f95daeaf4253242d0d80d8fb2ae88f4893503517bb4aa6c9",
];

/// The end-of-level state the goldens above settle into, asserted
/// separately so that "the screen stopped changing" can never be
/// confused with "the game froze".
///
/// Five identical hashes look alarming until you can point at the reason.
/// This pins the reason.
#[test]
fn the_static_goldens_are_the_level_end_not_a_freeze() {
    let Some(path) = resolve_rom() else {
        eprintln!("SKIP: build fixtures/nes/rf-scroller");
        return;
    };
    let rom_bytes = std::fs::read(path).expect("rom readable");
    let mut bus = NesBus::from_ines_bytes(&rom_bytes).expect("valid iNES");
    let mut cpu = Cpu::power_on(&mut bus);
    let mut sink = NullSink;
    for _ in 0..1200 {
        run_frame(&mut bus, &mut cpu, scripted_buttons(0), &mut sink);
    }
    assert_eq!(
        bus.peek(COLUMNS_STREAMED_ADDR),
        COLUMNS_STREAMED_AT_END,
        "the level's last raw column must have been reached"
    );
    assert_eq!(
        peek_u16(&bus, PLAYER_X_ADDR),
        PLAYER_X_AT_END,
        "the player must be parked at the level's end, not stuck somewhere else"
    );
}

/// Fast anti-vacuity check (NOT `#[ignore]`'d -- runs on every
/// `cargo test --workspace` once the ROM is built): drives 900 frames of
/// constant Right and asserts `player_x`, `camera_x`, and
/// `columns_streamed` all reach their level-end values -- see module doc
/// for what this catches and how it was verified.
#[test]
fn rf_scroller_scripted_input_actually_drives_the_game() {
    let Some(rom_path) = resolve_rom() else {
        eprintln!(
            "SKIP rf_scroller_scripted_input_actually_drives_the_game: \
             rf-scroller.nes not built. Build it first: \
             cd fixtures/nes/rf-scroller && ./build.sh (requires cc65 -- \
             brew install cc65 on macOS)"
        );
        return;
    };
    let rom_bytes = std::fs::read(&rom_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", rom_path.display()));

    let mut bus = NesBus::from_ines_bytes(&rom_bytes).expect("rf-scroller.nes must be valid iNES");
    let mut cpu = Cpu::power_on(&mut bus);
    let mut sink = NullSink;
    for frame in 0..900u64 {
        run_frame(&mut bus, &mut cpu, scripted_buttons(frame), &mut sink);
    }
    // Sample at a real program-level synchronization point, not an
    // arbitrary run_frame boundary -- settle_to_iteration_boundary's doc.
    settle_to_iteration_boundary(&mut bus, &mut cpu, 900, &mut sink);

    assert_eq!(
        peek_u16(&bus, PLAYER_X_ADDR),
        PLAYER_X_AT_END,
        "900 frames of Right must move the player to the level end (752) -- \
         16 means the scripted input never reached the game (module doc's \
         anti-vacuity section)"
    );
    assert_eq!(
        peek_u16(&bus, CAMERA_X_ADDR),
        CAMERA_X_AT_END,
        "camera must have followed the player to its maximum (512)"
    );
    assert_eq!(
        bus.peek(COLUMNS_STREAMED_ADDR),
        COLUMNS_STREAMED_AT_END,
        "columns_streamed must reach 95 -- this is only reachable once a \
         physical nametable has been genuinely reused for new content \
         (FORMAT.md's wraparound witness); 63 means streaming past the \
         initial two-nametable preload never happened"
    );
}

/// The Tier-A real-game regression itself (FR-CORE-026): records a
/// scripted 5-minute constant-Right run through a real `.rfreplay` round
/// trip, then replays it through a **second, independent** `Cpu`/`NesBus`
/// pair, checking every periodic + final reachable-state hash and every
/// golden-frame hash agree between the two runs and against the frozen
/// constants.
#[test]
#[ignore = "5-minute double-run (record + independent replay); same \
            release-only discipline as alter_ego_replay.rs's ignored \
            test. Run directly with: cargo test --release -p rf-harness \
            --test rf_scroller_replay -- --ignored"]
fn rf_scroller_five_minute_replay_final_hash_and_golden_frames() {
    let Some(rom_path) = resolve_rom() else {
        eprintln!(
            "SKIP rf_scroller_five_minute_replay_final_hash_and_golden_frames: \
             rf-scroller.nes not built. Build it first: \
             cd fixtures/nes/rf-scroller && ./build.sh (requires cc65 -- \
             brew install cc65 on macOS)"
        );
        return;
    };
    let rom_bytes = std::fs::read(&rom_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", rom_path.display()));
    assert_eq!(
        hex_sha256(&rom_bytes),
        expected_rom_sha256(),
        "the built ROM's raw-file hash does not match \
         fixtures/nes/rf-scroller/rom.sha256 -- see build.sh's own mismatch \
         diagnostics / FORMAT.md's toolchain provenance note; do not loosen \
         this check to work around a mismatch"
    );
    // Normalized identity hash for the .rfreplay header's rom_sha256 field
    // (header-byte-insensitive matching -- see expected_rom_sha256()'s doc
    // for why this is deliberately a DIFFERENT hash from the check above).
    let rom_sha256 = rf_cart::hash::identity_nes(&rom_bytes).normalized.sha256;

    // --- Run 1: record ---
    let header = ReplayHeader {
        console: "nes".to_string(),
        rom_sha256: rom_sha256.clone(),
        emu_version: env!("CARGO_PKG_VERSION").to_string(),
        core_config: "accuracy".to_string(),
        start_type: StartType::PowerOn,
        hash_kind: "reachable-v1".to_string(),
        hash_interval: HASH_INTERVAL,
    };
    let mut recorder = ReplayRecorder::new(header);
    let mut rec_bus =
        NesBus::from_ines_bytes(&rom_bytes).expect("rf-scroller.nes must be valid iNES");
    let mut rec_cpu = Cpu::power_on(&mut rec_bus);
    let mut rec_goldens: Vec<(u64, String)> = Vec::new();
    let mut golden_state = GoldenCaptureState::new();

    // GoldenCaptureState's own doc: an N-frame look-back window (tried 0
    // and 1) isn't reliably enough runway -- golden 1200's own straddle
    // needed more than one frame of priming to resolve. Tracking every
    // single frame through the same persistent, cheap (palette-index
    // bytes only) capture state removes the guess entirely: whatever's
    // banked in `last_complete` at the instant `bus.frame_count()` first
    // reaches `frame + 1` for a golden `frame` MUST be that frame's own
    // data, because that is the exact condition under which it was most
    // recently banked.
    for frame in 0..TOTAL_FRAMES {
        let buttons = scripted_buttons(frame);
        run_frame_tracking_goldens(&mut rec_bus, &mut rec_cpu, buttons, &mut golden_state);
        if GOLDEN_FRAMES.contains(&frame) {
            let rows = take_golden_capture(&mut golden_state, frame);
            eprintln!(
                "golden frame {frame} (recording): frame_counter witness={}",
                rec_bus.peek(FRAME_COUNTER_ADDR)
            );
            rec_goldens.push((frame, hash_frame_palette_indices(&rows, 240)));
        }
        recorder.record_frame(InputFrame {
            ports: [buttons as u16, 0, 0, 0],
        });
        let is_final = frame + 1 == TOTAL_FRAMES;
        if frame.is_multiple_of(HASH_INTERVAL) || is_final {
            recorder.record_hash(frame, reachable_state_hash(&rec_bus, &rec_cpu));
        }
    }

    assert_eq!(
        peek_u16(&rec_bus, PLAYER_X_ADDR),
        PLAYER_X_AT_END,
        "recording run: scripted input must have actually driven the game \
         to the level end (module doc's anti-vacuity section)"
    );
    assert_eq!(
        rec_bus.peek(COLUMNS_STREAMED_ADDR),
        COLUMNS_STREAMED_AT_END,
        "recording run: wraparound must have completed"
    );

    let log = recorder.finish();
    log.verify_rom_sha256(&rom_sha256)
        .expect("recorded log's own rom_sha256 must match the built ROM");

    // --- serialize / parse round trip (proves the .rfreplay format is
    // actually exercised, not merely constructed and discarded) ---
    let text = log.to_string();
    let parsed = ReplayLog::parse(&text).expect("recorded replay text must parse");
    assert_eq!(
        text,
        parsed.to_string(),
        "serialize -> parse -> serialize must be byte-exact"
    );
    assert_eq!(log, parsed);

    // --- Run 2: independent replay, a FRESH Cpu/NesBus never touched by
    // Run 1 -- this is the determinism proof, not a re-read of Run 1's
    // own result. ---
    let mut player = ReplayPlayer::new(&parsed);
    let mut play_bus =
        NesBus::from_ines_bytes(&rom_bytes).expect("rf-scroller.nes must be valid iNES");
    let mut play_cpu = Cpu::power_on(&mut play_bus);
    let mut play_goldens: Vec<(u64, String)> = Vec::new();
    let mut frame_no = 0u64;
    let mut hashes_checked = 0usize;
    let mut golden_state = GoldenCaptureState::new();

    while let Some(input) = player.next_frame() {
        let buttons = input.ports[0] as u8;
        run_frame_tracking_goldens(&mut play_bus, &mut play_cpu, buttons, &mut golden_state);
        if GOLDEN_FRAMES.contains(&frame_no) {
            let rows = take_golden_capture(&mut golden_state, frame_no);
            eprintln!(
                "golden frame {frame_no} (replay): frame_counter witness={}",
                play_bus.peek(FRAME_COUNTER_ADDR)
            );
            play_goldens.push((frame_no, hash_frame_palette_indices(&rows, 240)));
        }
        if let Some(expected) = player.expected_hash(frame_no) {
            assert_eq!(
                reachable_state_hash(&play_bus, &play_cpu),
                expected,
                "independent replay's state hash at frame {frame_no} diverged from \
                 the recorded run -- first divergence, per FAILURE_MODES.md FM-08"
            );
            hashes_checked += 1;
        }
        frame_no += 1;
    }
    assert_eq!(
        hashes_checked,
        log.hashes.len(),
        "every recorded hash must be checked"
    );
    assert_eq!(
        frame_no, TOTAL_FRAMES,
        "the independent replay must consume exactly the scripted 5 minutes"
    );
    assert_eq!(
        peek_u16(&play_bus, PLAYER_X_ADDR),
        PLAYER_X_AT_END,
        "independent replay: scripted input must have actually driven the game, \
         reproduced from the log alone (module doc's anti-vacuity section)"
    );
    assert_eq!(
        play_bus.peek(COLUMNS_STREAMED_ADDR),
        COLUMNS_STREAMED_AT_END,
        "independent replay: wraparound must have completed, reproduced from \
         the log alone"
    );

    // --- golden frames: recorded run vs. independent replay must match
    // byte-exact (the "captured from a verified run" requirement) ---
    assert_eq!(
        rec_goldens, play_goldens,
        "golden-frame hashes diverged between the recording run and the \
         independent replay -- a golden that only one of the two runs agrees \
         with is not a verified golden"
    );

    // --- and against the frozen constants ---
    let expected: Vec<(u64, String)> = GOLDEN_FRAMES
        .iter()
        .zip(GOLDEN_HASHES.iter())
        .map(|(&f, &h)| (f, h.to_string()))
        .collect();
    assert_eq!(
        rec_goldens, expected,
        "golden-frame hashes changed from the frozen constants -- if this is an \
         intentional core/renderer/ROM change, regenerate GOLDEN_HASHES from a \
         run re-verified the same way (module doc); if not, this is the \
         regression these goldens exist to catch"
    );
}
