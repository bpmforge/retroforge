//! Ticket W5-02b: does the **offline** `metatile_screens` decoder agree
//! with what the game actually builds?
//!
//! ## Why this compares VRAM and not screenshots
//!
//! The ticket originally said "golden screenshots"; it was amended on
//! Brad's ruling because a screenshot comparison is weaker than it looks.
//! Rendering the decoded level and rendering the emulator's output and
//! comparing the two images can pass while **both** are wrong — a decoder
//! that misread the ROM and a renderer fed from that same decoder agree
//! perfectly. The nametable bytes the ROM's own code streams into VRAM
//! are the ground truth for "in-game appearance", and comparing against
//! them is exact: no tolerance, no driver, no anti-aliasing.
//!
//! ## The two snapshots, and why one is not enough
//!
//! RF-Scroller's level is 48 metatile columns = **96 raw tile columns**,
//! but the PPU holds only **64** at a time (two nametables under vertical
//! mirroring, `physical_col = raw_col & 0x3F`). So no single moment shows
//! the whole level, and a test that snapshotted once would silently check
//! two thirds of it:
//!
//! | snapshot | when | raw columns present | metatile columns |
//! |---|---|---|---|
//! | preload | after `init_video()` | 0-63 | 0-31 |
//! | streamed | after `columns_streamed` reaches 95 | 32-95 | 16-47 |
//!
//! Together they cover all 48, with columns 16-31 checked twice — which
//! is a feature, not waste: those are the columns present in both, so
//! they also prove the streamer rewrote them consistently.
//!
//! ## The expansion, taken from the ROM's source rather than inferred
//!
//! `main.c`'s `prepare_raw_column()` and FORMAT.md's "Raw tile columns vs.
//! metatile columns":
//!
//! * `raw_col = mc * 2 + side`, `side` 0 = left, 1 = right.
//! * A metatile's table row is `{TL, TR, BL, BR}`; the raw column emits
//!   `row[side]` (top) then `row[2 + side]` (bottom) for each of the 14
//!   metatile rows, giving 28 tiles.
//! * `blit_pending_column()` writes them at
//!   `$2000 + 64 + (physical_col & 0x1F)` — the `+ 64` skips the two HUD
//!   tile rows — into nametable 1 when `physical_col >= 32`, walking down
//!   with PPUCTRL's +32 auto-increment.
//!
//! Every one of those was read out of the fixture's own source before
//! this test was written. Inferring the 2x2 layout instead would have
//! been the classic symmetric bug: get `side` and the table order wrong
//! in the same way on both halves and everything still matches.

use std::path::Path;

use rf_core_api::{CoreEvent, CoreSink, PpuPixel};
use rf_enhance::decode::metatile_screens::{self, DecodedLevel, Spec};
use rf_nes::{Cpu, NesBus};

/// The last raw tile column `init_video()`'s preload fills. Everything
/// up to and including this is written before the camera can move, which
/// is what makes it verifiable from a quiescent machine.
pub const PRELOAD_LAST_RAW_COLUMN: usize = 63;

/// Tile rows of HUD above the playfield (`blit_pending_column`'s `+ 64`,
/// i.e. two rows of 32).
const HUD_TILE_ROWS: usize = 2;
/// Tiles per nametable row.
const NT_WIDTH: usize = 32;
/// Raw tile columns the PPU can hold at once, under vertical mirroring.
const VRAM_RAW_COLUMNS: usize = 64;

struct NullSink;
impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

/// One raw tile column expanded from the decoded level: 28 tile ids, top
/// to bottom.
///
/// # Panics
/// Panics if `raw_col` is outside the level or the metatile table is
/// short — both are test-time programming errors, not data conditions.
#[must_use]
pub fn expand_raw_column(level: &DecodedLevel, raw_col: usize) -> Vec<u8> {
    let mc = (raw_col / 2) as u32;
    let side = raw_col % 2;
    let column = level
        .column(mc)
        .unwrap_or_else(|| panic!("metatile column {mc} is outside the decoded level"));
    let mut out = Vec::with_capacity(column.len() * 2);
    for id in column {
        let tiles = level
            .tiles_for(*id)
            .unwrap_or_else(|| panic!("metatile {id} is not in the table"));
        out.push(tiles[side]); // TL or TR
        out.push(tiles[2 + side]); // BL or BR
    }
    out
}

/// Read one raw tile column straight out of the PPU's nametable RAM.
///
/// `vram` is [`rf_nes::NesBus::vram`]'s 4 KiB: under vertical mirroring
/// `$2000` is index 0 and `$2400` is index `0x400`.
#[must_use]
pub fn vram_raw_column(vram: &[u8; 0x1000], raw_col: usize, rows: usize) -> Vec<u8> {
    let physical = raw_col % VRAM_RAW_COLUMNS;
    let nametable = usize::from(physical >= NT_WIDTH);
    let tile_x = physical % NT_WIDTH;
    let base = nametable * 0x400 + HUD_TILE_ROWS * NT_WIDTH + tile_x;
    (0..rows).map(|r| vram[base + r * NT_WIDTH]).collect()
}

/// Compare one raw tile column against VRAM and record the outcome.
fn check_column(
    report: &mut VerifyReport,
    level: &DecodedLevel,
    spec: &Spec,
    vram: &[u8; 0x1000],
    col: usize,
) {
    if col >= spec.width as usize * 2 {
        return;
    }
    let expected = expand_raw_column(level, col);
    let actual = vram_raw_column(vram, col, spec.height as usize * 2);
    if expected == actual {
        report.verified.insert(col);
        return;
    }
    let first = expected
        .iter()
        .zip(&actual)
        .position(|(a, b)| a != b)
        .unwrap_or(0);
    report.problems.push(format!(
        "raw column {col} (metatile column {}, {} half) first differs at tile row {first}: \
         decoder says {:#04X}, VRAM holds {:#04X}",
        col / 2,
        if col.is_multiple_of(2) {
            "left"
        } else {
            "right"
        },
        expected[first],
        actual[first],
    ));
}

/// ## The streamed tail: all 96 as of ticket W5-02c
///
/// This module was written for W5-02b, which could verify only 81 of the
/// level's 96 raw tile columns. The other 15 were not a harness bug:
/// `stream_chunk()` in the fixture ran after `read_buttons()` and the
/// camera arithmetic, so it started near the end of vblank and its last
/// `$2007` writes landed on scanlines 0-1 — during rendering, where the
/// PPU owns `v` and a write goes wherever rendering left it. W5-02c
/// found that by stepping the machine instruction by instruction and
/// recording the scanline of every playfield VRAM write (they clustered
/// at {0, 1, 256, 257, 258}), and fixed it by streaming first.
///
/// The per-column structure below stays, and is still the right shape:
/// VRAM holds 64 raw columns and the level has 96, so slots are recycled
/// while the player walks and a single "final" snapshot photographs a
/// moving target. W5-02b measured that directly — a final snapshot
/// mismatched 47 of 96 where per-column checking mismatched 15.
///
/// Which raw tile columns were verified, and what went wrong.
pub struct VerifyReport {
    /// Raw columns compared against VRAM and found equal.
    pub verified: std::collections::BTreeSet<usize>,
    /// One message per mismatch.
    pub problems: Vec<String>,
    pub frames: u64,
}

/// Run the fixture and check every raw tile column against VRAM **at the
/// moment the ROM finishes writing it**.
///
/// ## Why per-column and not one final snapshot
///
/// The first version of this took two snapshots — after the preload and
/// after `columns_streamed` reached 95 — and compared 64 columns each.
/// The preload snapshot was clean; the final one had **32 of its 64 slots
/// matching no decoded column at all**, i.e. torn rather than wrong.
///
/// The reason is structural, not a settling bug, and it is worth stating
/// because the obvious fix (wait longer) is not one: VRAM holds 64 raw
/// columns and the level has 96, so slots are **recycled** as the camera
/// advances. A "final" snapshot is a photograph of a moving target — a
/// slot may hold the tail of its old occupant and the head of its new
/// one, and no amount of settling helps because the streamer is still
/// working the whole time the player is walking. Raising the settle from
/// 4 frames to 60 took the mismatch count from 64 to 32 and stalled
/// there, which is exactly the signature of a structural problem wearing
/// a timing disguise.
///
/// Checking each column when the ROM says it just finished one is both
/// robust to recycling and a **stronger** claim: every one of the 96 raw
/// columns is verified against the bytes the game wrote for it, rather
/// than against whatever survived to the end.
///
/// # Errors
/// Returns a message if the ROM will not load or the level never streams
/// out within `max_frames`.
pub fn verify_every_column(
    rom_bytes: &[u8],
    level: &DecodedLevel,
    spec: &Spec,
    max_frames: u64,
) -> Result<VerifyReport, String> {
    /// `columns_streamed`, from the shipped profile's memory map (and
    /// FORMAT.md's "Documented RAM addresses").
    const COLUMNS_STREAMED: u16 = 0x602D;
    /// TAIL_GATE_COL, `main.c`.
    const LAST_RAW_COLUMN: u8 = 95;
    /// Bit 7 is Right in `Controller`'s documented order (A, B, Select,
    /// Start, Up, Down, Left, Right). **The level will not stream itself**
    /// — the streamer only advances when the camera does, so left alone
    /// the fixture parks at `columns_streamed == 63` forever, which is
    /// what the first run of this reported.
    const RIGHT: u8 = 0x80;
    /// Frames to let a just-finished column's blit reach VRAM.
    /// `columns_streamed` moves when a column finishes DECODING; the
    /// write happens in a later vblank (`pending_valid`).
    const BLIT_SETTLE: u64 = 20;

    let mut bus = NesBus::from_ines_bytes(rom_bytes).map_err(|e| format!("rom: {e}"))?;
    let mut cpu = Cpu::power_on(&mut bus);

    let mut report = VerifyReport {
        verified: std::collections::BTreeSet::new(),
        problems: Vec::new(),
        frames: 0,
    };
    let raw_columns = || spec.width as usize * 2;
    let advance = |cpu: &mut Cpu, bus: &mut NesBus| {
        let start = bus.frame_count();
        while bus.frame_count() == start {
            cpu.step(bus);
            bus.drain_video(&mut NullSink);
            bus.drain_audio(&mut NullSink);
        }
    };

    // ---- phase 1: the preload, verified from a QUIESCENT machine ----
    //
    // `init_video()` fills raw columns 0-63 and then nothing streams
    // until the camera moves — the streamer is driven by camera position,
    // which is why the fixture parks at `columns_streamed == 63` when no
    // button is held. That makes this the one moment VRAM is genuinely
    // still, so all 64 columns can be read from a single snapshot with no
    // tearing and no per-column race.
    //
    // Doing phase 2's per-column dance here instead does NOT work, and
    // the failure is worth recording: the preload completes its 64
    // columns in a burst, so they all fall due at once while their blits
    // land over the following frames, and columns near the start read as
    // half-written.
    const PRELOAD_LAST_COLUMN: u8 = PRELOAD_LAST_RAW_COLUMN as u8;
    while bus.peek(COLUMNS_STREAMED) < PRELOAD_LAST_COLUMN {
        advance(&mut cpu, &mut bus);
        report.frames += 1;
        if report.frames > max_frames {
            return Err(format!(
                "init_video's preload never reached raw column {PRELOAD_LAST_COLUMN} (stalled at \
                 {})",
                bus.peek(COLUMNS_STREAMED)
            ));
        }
    }
    for _ in 0..30 {
        advance(&mut cpu, &mut bus);
        report.frames += 1;
    }
    {
        let vram = *bus.vram();
        for col in 0..=usize::from(PRELOAD_LAST_COLUMN) {
            check_column(&mut report, level, spec, &vram, col);
        }
    }

    // ---- phase 2: the streamed tail, verified column by column ----
    //
    // From here VRAM is a moving target: it holds 64 raw columns and the
    // level has 96, so slots are recycled as the camera advances. A
    // single "final" snapshot photographs that motion — the first version
    // of this test did exactly that and found 32 of 64 slots matching NO
    // decoded column at all, torn between their old and new occupants.
    // Raising the settle from 4 frames to 60 moved the mismatch count
    // from 64 to 32 and stalled, which is the signature of a structural
    // problem wearing a timing disguise. So each column is checked once
    // its own blit has landed and before the streamer can reach 64
    // columns further on.
    let mut pending: Vec<(usize, u64)> = Vec::new();
    let mut highest_seen = i64::from(PRELOAD_LAST_COLUMN);

    while report.frames < max_frames {
        advance(&mut cpu, &mut bus);
        report.frames += 1;

        // Queue every column that became complete this frame.
        let streamed = i64::from(bus.peek(COLUMNS_STREAMED));
        while highest_seen < streamed {
            highest_seen += 1;
            #[allow(clippy::cast_sign_loss)]
            pending.push((highest_seen as usize, report.frames + BLIT_SETTLE));
        }

        // Check the ones whose blit has had time to land. Checked here,
        // before the streamer can recycle the slot: with 64 slots and 96
        // columns, a column checked late may already have been
        // overwritten by the one 64 ahead of it.
        let now = report.frames;
        let vram = *bus.vram();
        let mut due_now = Vec::new();
        pending.retain(|(col, due)| {
            if *due > now {
                true
            } else {
                due_now.push(*col);
                false
            }
        });
        for col in due_now {
            check_column(&mut report, level, spec, &vram, col);
        }

        // Start walking once the preload is done; before that the pad is
        // not even polled.
        if streamed >= i64::from(LAST_RAW_COLUMN) && pending.is_empty() {
            // Second chance, at rest. A column checked the moment its own
            // blit was due can be caught mid-write, and six of RF-Scroller's
            // columns were: they are byte-identical to the occupant they
            // replaced, so nothing about the slot changes when they land
            // and the exact frame the write completes is unobservable.
            // Re-checking the stragglers once the machine has settled
            // resolves those without weakening anything — a column is
            // verified if it EVER matched, and a genuinely wrong column
            // matches at neither moment.
            for _ in 0..30 {
                advance(&mut cpu, &mut bus);
                report.frames += 1;
            }
            let vram = *bus.vram();
            let stragglers: Vec<usize> = (0..raw_columns())
                .filter(|c| !report.verified.contains(c))
                .collect();
            // Rebuilt from the retry rather than appended to: every
            // unverified column is retried, so the retry's failures ARE
            // the true failures, and keeping the first-pass messages would
            // report a column twice or report one that has since passed.
            report.problems.clear();
            for col in stragglers {
                check_column(&mut report, level, spec, &vram, col);
            }
            return Ok(report);
        }
        bus.set_controller_buttons(0, RIGHT);
    }
    Err(format!(
        "the level never finished streaming within {max_frames} frames (reached column {})",
        highest_seen
    ))
}

/// Load the shipped RF-Scroller profile and build a [`Spec`] from it.
///
/// # Errors
/// Returns the loader's or the decoder's own message.
pub fn spec_from_shipped_profile(repo_root: &Path) -> Result<Spec, String> {
    let path = repo_root.join("profiles/nes/rf-scroller/profile.toml");
    let profile = rf_profiles::load_file(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .profile;
    metatile_screens::spec_from_profile(&profile).map_err(|e| e.to_string())
}

/// Diagnostic: VRAM after the level has fully streamed.
///
/// # Errors
/// Same conditions as [`verify_every_column`].
pub fn final_vram(
    rom_bytes: &[u8],
    level: &DecodedLevel,
    spec: &Spec,
    max_frames: u64,
) -> Result<[u8; 0x1000], String> {
    let _ = (level, spec);
    let mut bus = NesBus::from_ines_bytes(rom_bytes).map_err(|e| format!("rom: {e}"))?;
    let mut cpu = Cpu::power_on(&mut bus);
    let mut frames = 0;
    while frames < max_frames {
        let start = bus.frame_count();
        while bus.frame_count() == start {
            cpu.step(&mut bus);
            bus.drain_video(&mut NullSink);
            bus.drain_audio(&mut NullSink);
        }
        frames += 1;
        if bus.peek(0x602D) >= 95 {
            break;
        }
        bus.set_controller_buttons(0, 0x80);
    }
    for _ in 0..30 {
        let start = bus.frame_count();
        while bus.frame_count() == start {
            cpu.step(&mut bus);
            bus.drain_video(&mut NullSink);
            bus.drain_audio(&mut NullSink);
        }
    }
    Ok(*bus.vram())
}
