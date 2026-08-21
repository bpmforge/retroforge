//! Corpus-driven mode-invariant harness (ticket W4-01, acceptance criterion
//! 3: "mode-invariant CI test: accuracy vs enhanced state hashes identical
//! over test corpus", FR-MODE-002; criterion 4: dump evidence on
//! divergence, R-F4).
//!
//! ## Honesty constraint: the corpus is thin
//!
//! FR-MODE-002's invariant needs an *enhanced* mode to compare against, and
//! exactly one enhancement exists in this workspace today: W3-05a's
//! sprite-limit-bypass overlay. So [`CORPUS`] has exactly one real entry.
//! What this module delivers is the **mechanism** — a runner that takes N
//! ROMs x M frames, each with its own "what enhanced mode does" hook
//! ([`CorpusEntry::enable_enhancement`]) — structured so W3-05/W4-03a add a
//! second/third [`CorpusEntry`] without touching [`check`] or
//! [`dump_divergence_evidence`] at all. Do not read a passing [`CORPUS`]
//! run as "mode invariance verified broadly" — it verifies exactly the one
//! enhancement wired in.
//!
//! ## The strong check, and why `state_hash` alone would be the weakest link
//!
//! `EmuStepper::state_hash`'s own doc is explicit about its coverage: WRAM,
//! OAM, PRG-RAM, CPU registers, `master_cycle`/`frame_count`. It does
//! **not** cover PPU-internal registers (mask/control/scroll/status) or the
//! rendered output. W3-05a proved this empirically (a status-flag
//! perturbation passed `state_hash` unchanged while a field-by-field PPU
//! comparison caught it); W4-00 followed the same lesson for its own
//! non-perturbation test, comparing PPU internals directly. This module
//! cannot do that field-by-field PPU comparison — `rf-nes` is explicitly
//! out of this ticket's write scope, and PPU-internal fields are not part
//! of any public `rf-nes` API. What it can do, entirely from already-public
//! surface: hash the full **indexed pixel stream**
//! (`rf_core_api::FrameBundleBuilder`'s accumulated `Vec<PpuPixel>` —
//! `palette_index`, `layer`, `sprite_id`, `priority` all preserved, unlike
//! a resolved-RGB digest which would collapse those distinctions into
//! color) alongside `state_hash`. This is strictly stronger than
//! `state_hash` alone: it catches any divergence that changes what
//! actually renders, which `state_hash` structurally cannot see at all.
//! [`check`]'s own harness self-test
//! (`mode_invariant_check_fails_when_enhanced_mode_reaches_into_ppu_registers`
//! in `crates/retroforge/tests/mode_invariant_corpus.rs`) proves this
//! addition has real teeth: it constructs a divergence that `state_hash`
//! misses entirely (identical WRAM/OAM/PRG-RAM/CPU-regs/cycles) and shows
//! only the video hash catches it.
//!
//! It is *not* as strong as W4-00's field-by-field PPU comparison — a PPU-
//! internal divergence that happens to render identically this frame would
//! still slip past both checks here. That residual gap is real and stays
//! documented rather than papered over; closing it needs `rf-nes` to grow
//! public PPU-internal read accessors, which is out of this ticket's scope
//! (see the ticket's own escalation clause about needing `rf-nes`).
//!
//! ## The "replay slice" artifact
//!
//! Reuses the existing `.rfreplay` format (`rf_input::ReplayRecorder`,
//! `docs/design/CONTRACTS.md` §3) rather than inventing a new one — a
//! divergence dump's "replay slice" is exactly what that format already
//! exists to carry. Today's [`CORPUS`] entries take no input, so the
//! dumped slice is legitimately all-empty frames; the mechanism is real and
//! wired for the day an input-driven entry lands.
use rf_core_api::PpuPixel;

use crate::stepper::EmuStepper;

/// One corpus entry: a ROM plus what "enhanced" mode does differently from
/// accuracy mode, called once per frame (matching ARCHITECTURE §8's main
/// loop, where `enhancement.on_frame` runs every frame, not once at
/// startup) so a persistent-flag enhancement (like the real overlay entry)
/// and an out-of-band perturbation (like the harness self-test) both
/// behave the way their real-world equivalents would.
pub struct CorpusEntry {
    pub name: &'static str,
    pub rom: fn() -> Vec<u8>,
    pub frames: usize,
    pub enable_enhancement: fn(&mut EmuStepper),
}

/// The real corpus: today, exactly the one enhancement that exists. See
/// module doc's honesty constraint before adding claims about what this
/// proves.
pub const CORPUS: &[CorpusEntry] = &[CorpusEntry {
    name: "w3-05a-sprite-limit-bypass-overlay",
    rom: many_sprites_rom,
    frames: 20,
    enable_enhancement: |s| s.set_sprite_overlay_enabled(true),
}];

const INES_MAGIC: [u8; 4] = [0x4E, 0x45, 0x53, 0x1A]; // "NES\x1A"

/// A hand-assembled NROM program that writes 10 sprites (Y=50, tile=1,
/// attr=0, non-overlapping X) into OAM via an indexed `$2004` write loop,
/// enables sprite rendering, then loops forever — same fixture (same
/// verified 6502 encoding) as
/// `crates/retroforge/tests/sprite_overlay_mode_invariant.rs`'s own
/// `many_sprites_rom`, duplicated here rather than shared across the
/// `src`/`tests` boundary — the same small-fixture-duplication convention
/// this crate already uses three times over (`stepper.rs`,
/// `core_thread.rs`, `determinism.rs` each define their own synthetic ROM
/// rather than widening a shared API for it). `pub` so
/// `tests/mode_invariant_corpus.rs`'s own harness self-test can build a
/// second [`CorpusEntry`] against the identical ROM.
///
/// | Addr   | Bytes       | Instruction              |
/// |--------|-------------|--------------------------|
/// | $8000  | 78          | SEI                      |
/// | $8001  | A2 FF       | LDX #$FF                 |
/// | $8003  | 9A          | TXS                      |
/// | $8004  | A9 00       | LDA #$00                 |
/// | $8006  | 8D 03 20    | STA $2003 (OAMADDR = 0)  |
/// | $8009  | A2 00       | LDX #$00                 |
/// | $800B  | BD 1E 80    | loop: LDA $801E,X        |
/// | $800E  | 8D 04 20    | STA $2004                |
/// | $8011  | E8          | INX                      |
/// | $8012  | E0 28       | CPX #$28 (40 = 10*4 bytes) |
/// | $8014  | D0 F5       | BNE loop                 |
/// | $8016  | A9 10       | LDA #$10                 |
/// | $8018  | 8D 01 20    | STA $2001 (show sprites) |
/// | $801B  | 4C 1B 80    | forever: JMP forever     |
/// | $801E  | ...         | sprite table, 40 bytes (below) |
#[rustfmt::skip]
const PROGRAM: &[(u16, &[u8])] = &[
    (0x8000, &[0x78]),
    (0x8001, &[0xA2, 0xFF]),
    (0x8003, &[0x9A]),
    (0x8004, &[0xA9, 0x00]),
    (0x8006, &[0x8D, 0x03, 0x20]),
    (0x8009, &[0xA2, 0x00]),
    (0x800B, &[0xBD, 0x1E, 0x80]),
    (0x800E, &[0x8D, 0x04, 0x20]),
    (0x8011, &[0xE8]),
    (0x8012, &[0xE0, 0x28]),
    (0x8014, &[0xD0, 0xF5]),
    (0x8016, &[0xA9, 0x10]),
    (0x8018, &[0x8D, 0x01, 0x20]),
    (0x801B, &[0x4C, 0x1B, 0x80]),
];

const SPRITE_TABLE_ADDR: u16 = 0x801E;

fn sprite_table() -> Vec<u8> {
    let mut table = Vec::with_capacity(40);
    for i in 0u8..10 {
        table.extend_from_slice(&[50, 1, 0, 8 + i * 9]);
    }
    table
}

/// Build the fixture ROM (see [`PROGRAM`]'s doc for the exact encoding).
#[must_use]
pub fn many_sprites_rom() -> Vec<u8> {
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
    let table = sprite_table();
    let table_offset = (SPRITE_TABLE_ADDR - 0x8000) as usize;
    prg[table_offset..table_offset + table.len()].copy_from_slice(&table);
    prg[0x3FFC] = 0x00; // reset vector low  -> $8000
    prg[0x3FFD] = 0x80; // reset vector high
    data.extend(prg);

    let mut chr = vec![0u8; 8 * 1024];
    for row in 0..8 {
        chr[16 + row] = 0xFF;
        chr[16 + 8 + row] = 0x00;
    }
    data.extend(chr);
    data
}

/// Both checks for one frame — kept as a pair, never collapsed into one
/// bool, so a divergence report can say which check(s) actually caught it
/// (module doc's honesty point about `state_hash`'s blind spot).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameHashes {
    pub state_hash: String,
    pub video_hash: String,
}

/// The first frame where either check diverged, with enough to file a
/// useful bug report and reproduce it (criterion 4, R-F4).
#[derive(Debug, Clone)]
pub struct Divergence {
    pub entry_name: &'static str,
    pub frame: usize,
    pub accuracy: FrameHashes,
    pub enhanced: FrameHashes,
    /// `.rfreplay`-formatted text covering frames `0..=frame` of the
    /// enhanced run (module doc's "replay slice" section).
    pub replay_slice_rfreplay: String,
}

impl Divergence {
    /// Whether `state_hash` alone would have caught this divergence — the
    /// precise, machine-checked answer to module doc's honesty point,
    /// rather than an assertion left to trust.
    #[must_use]
    pub fn state_hash_alone_would_have_caught_it(&self) -> bool {
        self.accuracy.state_hash != self.enhanced.state_hash
    }
}

/// Serialize one [`PpuPixel`] into a fixed 5-byte record for hashing.
/// Deliberately keeps `sprite_id`'s presence and value as two separate
/// bytes (rather than a sentinel value) so no real `sprite_id` (0-63 on
/// NES) can ever collide with "absent".
fn push_pixel_bytes(buf: &mut Vec<u8>, p: &PpuPixel) {
    buf.push(p.palette_index);
    let (layer_tag, layer_val) = match p.layer {
        rf_core_api::PixelLayer::Backdrop => (0u8, 0u8),
        rf_core_api::PixelLayer::Background(n) => (1u8, n),
        rf_core_api::PixelLayer::Sprite => (2u8, 0u8),
    };
    buf.push(layer_tag);
    buf.push(layer_val);
    buf.push(u8::from(p.sprite_id.is_some()));
    buf.push(p.sprite_id.unwrap_or(0));
    buf.push(p.priority);
}

/// The strong check's digest (module doc). `pub` so
/// `tests/mode_invariant_corpus.rs` can prove directly that it preserves
/// information a resolved-RGB digest would collapse (`sprite_id`/
/// `priority`/`layer`), rather than only inferring that from reading the
/// implementation.
#[must_use]
pub fn hash_indexed_video(pixels: &[PpuPixel]) -> String {
    let mut buf = Vec::with_capacity(pixels.len() * 7);
    for p in pixels {
        push_pixel_bytes(&mut buf, p);
    }
    crate::hash::sha256_hex(&buf)
}

/// Run one corpus entry through `frames` frames, in accuracy or enhanced
/// configuration, returning one [`FrameHashes`] pair per frame plus the
/// input log driven (today always empty — see module doc).
fn run(entry: &CorpusEntry, enhanced: bool) -> (Vec<FrameHashes>, Vec<rf_core_api::InputFrame>) {
    let mut stepper =
        EmuStepper::from_ines_bytes(&(entry.rom)()).expect("corpus ROM must be a valid iNES image");
    let mut builder = rf_core_api::FrameBundleBuilder::new(
        rf_renderer::frame::NES_WIDTH as u16,
        rf_renderer::frame::NES_HEIGHT as u16,
    );
    let mut hashes = Vec::with_capacity(entry.frames);
    let mut inputs = Vec::with_capacity(entry.frames);
    for _ in 0..entry.frames {
        if enhanced {
            (entry.enable_enhancement)(&mut stepper);
        }
        let input = rf_core_api::InputFrame::empty();
        stepper.latch_and_advance_frame(input, &mut builder);
        inputs.push(input);
        let bundle = builder.take(stepper.frame_count());
        hashes.push(FrameHashes {
            state_hash: stepper.state_hash(),
            video_hash: hash_indexed_video(&bundle.video),
        });
    }
    (hashes, inputs)
}

fn build_replay_slice(
    rom: &[u8],
    inputs: &[rf_core_api::InputFrame],
    through_frame: usize,
) -> String {
    let rom_sha256 = rf_cart::hash::identity_nes(rom).normalized.sha256;
    let header = rf_input::ReplayHeader {
        console: "nes".to_string(),
        rom_sha256,
        emu_version: env!("CARGO_PKG_VERSION").to_string(),
        core_config: "enhanced".to_string(),
        start_type: rf_input::StartType::PowerOn,
        hash_kind: crate::save_state::HASH_KIND.to_string(),
        hash_interval: 1,
    };
    let mut recorder = rf_input::ReplayRecorder::new(header);
    for &input in inputs.iter().take(through_frame + 1) {
        recorder.record_frame(input);
    }
    recorder.finish().to_string()
}

/// Run `entry` through both accuracy and enhanced modes and return the
/// first frame where either check diverges, or `None` if all
/// `entry.frames` frames matched on both.
#[must_use]
pub fn check(entry: &CorpusEntry) -> Option<Divergence> {
    let (accuracy, _accuracy_inputs) = run(entry, false);
    let (enhanced, enhanced_inputs) = run(entry, true);
    let n = entry.frames.min(accuracy.len()).min(enhanced.len());
    for i in 0..n {
        if accuracy[i] != enhanced[i] {
            let rom = (entry.rom)();
            return Some(Divergence {
                entry_name: entry.name,
                frame: i,
                accuracy: accuracy[i].clone(),
                enhanced: enhanced[i].clone(),
                replay_slice_rfreplay: build_replay_slice(&rom, &enhanced_inputs, i),
            });
        }
    }
    None
}

/// Write CI-artifact evidence for `div` under `out_dir` (criterion 4,
/// R-F4): a plain-text divergence summary (frame index + both hash pairs)
/// and the `.rfreplay` replay slice. `out_dir` is caller-chosen precisely
/// so this never writes into `docs/evidence/` (explicitly out of this
/// ticket's scope — `validate-evidence.mjs` must stay green with no
/// regeneration) — callers use `CARGO_TARGET_TMPDIR`-derived paths, which
/// are gitignored build output, not `docs/evidence/local-gate.json`.
///
/// # Errors
/// Returns [`std::io::Error`] if `out_dir` cannot be created or written to.
pub fn dump_divergence_evidence(
    div: &Divergence,
    out_dir: &std::path::Path,
) -> std::io::Result<()> {
    std::fs::create_dir_all(out_dir)?;
    let summary = format!(
        "mode-invariant divergence (ticket W4-01, FR-MODE-002, R-F4)\n\
         entry: {}\n\
         first divergent frame: {}\n\
         accuracy state_hash:  {}\n\
         enhanced state_hash:  {}\n\
         accuracy video_hash:  {}\n\
         enhanced video_hash:  {}\n\
         state_hash alone would have caught this: {}\n",
        div.entry_name,
        div.frame,
        div.accuracy.state_hash,
        div.enhanced.state_hash,
        div.accuracy.video_hash,
        div.enhanced.video_hash,
        div.state_hash_alone_would_have_caught_it(),
    );
    std::fs::write(out_dir.join("divergence.txt"), summary)?;
    std::fs::write(
        out_dir.join("replay-slice.rfreplay"),
        &div.replay_slice_rfreplay,
    )?;
    Ok(())
}
