//! CI-facing half of ticket W4-01's mode-invariant harness
//! (`retroforge::mode_invariant`) — acceptance criteria 3 and 4. Read that
//! module's doc first: the corpus is deliberately thin (one enhancement
//! exists today), and the strong check is deliberately not as strong as
//! W4-00's field-by-field PPU comparison (out of this ticket's write
//! scope). This file exists to (a) run the real, thin corpus every commit
//! and (b) prove the checker actually discriminates, not just that it
//! never fires.
use retroforge::mode_invariant::{self, check, dump_divergence_evidence, CorpusEntry, CORPUS};
use retroforge::stepper::EmuStepper;

/// Criterion 3's actual CI gate: every entry in the real (thin) corpus
/// must show zero divergence over its full frame window, on both checks.
///
/// Criterion 4 (R-F4) is wired in at the one place it matters: if this
/// ever DOES diverge (a real regression, not a harness self-test), the
/// evidence is dumped to `$CARGO_TARGET_TMPDIR/mode-invariant-evidence/`
/// *before* the assertion panics, so `.github/workflows/ci.yml`'s
/// `if: failure()` artifact-upload step (which runs after this same
/// `cargo test --workspace` step) has something to pick up. The harness
/// self-tests below exercise `dump_divergence_evidence` directly on their
/// own deliberate divergences — this is the site that makes the dump
/// happen on a *real*, unplanned one.
#[test]
fn every_corpus_entry_is_mode_invariant_over_its_full_frame_window() {
    for entry in CORPUS {
        let divergence = check(entry);
        if let Some(div) = &divergence {
            let out_dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
                .join("mode-invariant-evidence")
                .join(entry.name.replace(' ', "_"));
            if let Err(e) = dump_divergence_evidence(div, &out_dir) {
                eprintln!("also failed to dump divergence evidence to {out_dir:?}: {e}");
            } else {
                eprintln!("divergence evidence dumped to {out_dir:?}");
            }
        }
        assert!(
            divergence.is_none(),
            "corpus entry {:?} diverged: {:?}",
            entry.name,
            divergence
        );
    }
}

/// Sanity check that the corpus isn't accidentally empty (which would make
/// the test above vacuously pass regardless of `check`'s correctness).
#[test]
fn the_corpus_is_not_empty() {
    assert!(
        !CORPUS.is_empty(),
        "an empty corpus would make every-entry-passes vacuous"
    );
}

/// Mutation-test #1 (ticket W4-01's own instructions): "make an
/// enhancement perturb core state => the mode-invariant test must FAIL".
///
/// Routed through `CorpusEntry::enable_enhancement` itself, not a
/// standalone call to `EmuStepper::poke_bus` off to the side — the whole
/// point is proving the *runner* is wired to the path a real enhancement
/// travels (`check`'s per-frame `enable_enhancement` call), not just that
/// `state_hash`/the video hash can detect *some* difference between two
/// byte strings. If `check` ever stopped calling `enable_enhancement` at
/// all, this test would stop catching anything and FAIL for the right
/// reason (`divergence.is_none()` would trip).
///
/// The perturbation writes `$0000` (zero-page WRAM) every frame — a byte
/// [`mode_invariant::many_sprites_rom`]'s own program never touches (its
/// only writes are to `$2001`/`$2003`/`$2004`, all PPU registers), so any
/// divergence there is unambiguously the poke's doing, not an artifact.
/// `$0000` is squarely inside `EmuStepper::state_hash`'s own hashed range
/// (`$0000-$07FF`), so this is a clean, legible "an enhancement perturbed
/// core state" case with no ambiguity about *why* it's caught.
#[test]
fn mode_invariant_check_fails_when_enhanced_mode_perturbs_wram() {
    let poking_entry = CorpusEntry {
        name: "harness-self-test: wram poke (not part of the real corpus)",
        rom: mode_invariant::many_sprites_rom,
        frames: 10,
        enable_enhancement: |s: &mut EmuStepper| {
            s.set_sprite_overlay_enabled(true); // the legitimate part
            s.poke_bus(0x0000, 0x99); // simulated bug: reaches into WRAM the ROM never touches
        },
    };

    let divergence =
        check(&poking_entry).expect("an enhancement that perturbs WRAM every frame must be caught");
    assert!(
        divergence.state_hash_alone_would_have_caught_it(),
        "a direct WRAM write is exactly what state_hash's own byte range ($0000-$07FF) covers; \
         if state_hash did NOT catch it, state_hash itself is broken, not a limitation to note"
    );
}

/// ## Why this file does NOT also try to demonstrate "video_hash catches a
/// divergence state_hash misses" against a *live* core run
///
/// That would need a perturbation that changes rendering without touching
/// anything `state_hash` hashes. The only externally-reachable
/// perturbation primitive available outside `rf-nes` is
/// `EmuStepper::poke_bus`, which routes through `rf_nes::CpuBus::write` —
/// and `NesBus::write`'s own source (read, not modified, to confirm this)
/// calls `self.tick_master(1)` for every write except the `$4014` OAM-DMA
/// case. `master_cycle` is one of `state_hash`'s hashed bytes, so **any**
/// externally-injected bus write trips `state_hash` too, regardless of
/// whether it has a rendering-visible effect — confirmed directly: an
/// earlier draft of this test poked `$2001` (PPUMASK) expecting exactly
/// the "video catches it, state_hash doesn't" shape and got the opposite
/// (`state_hash` diverged from the write's own cycle cost; `video_hash`
/// did not, because the divergence was first detected before the ROM's
/// own rendering-visible frames). That result is recorded, not discarded:
/// it means a genuinely video-only, state_hash-invisible divergence cannot
/// be constructed from outside `rf-nes` with today's public surface — only
/// W4-00's own test, *inside* `rf-nes` (a direct field write, zero
/// cycles), demonstrates that sharper property, and it is out of this
/// ticket's write scope to duplicate. [`hash_indexed_video_is_more_precise_than_a_resolved_rgb_hash_would_be`]
/// below proves the weaker, still-real, and fully in-scope claim: the
/// *hash itself* preserves information a resolved-color hash would
/// collapse, which is why it was chosen over hashing `FrameBuffer::rgba()`.
#[test]
fn hash_indexed_video_is_more_precise_than_a_resolved_rgb_hash_would_be() {
    use rf_core_api::{PixelLayer, PpuPixel};

    // Two single-pixel "frames" that resolve to the exact same palette
    // index (and therefore the exact same RGB color) but differ in
    // sprite_id/priority/layer -- the fields a resolved-RGB digest would
    // lose. `mode_invariant`'s own hash must still tell them apart.
    let as_background = PpuPixel {
        palette_index: 0x16,
        layer: PixelLayer::Background(0),
        sprite_id: None,
        priority: 0,
        dropped_by_limit: false,
    };
    let as_sprite = PpuPixel {
        palette_index: 0x16,
        layer: PixelLayer::Sprite,
        sprite_id: Some(3),
        priority: 1,
        dropped_by_limit: false,
    };

    assert_eq!(
        as_background.palette_index, as_sprite.palette_index,
        "sanity: this test is only meaningful if a resolved-RGB digest would have seen \
         these two pixels as identical"
    );

    let hash_a = mode_invariant::hash_indexed_video(&[as_background]);
    let hash_b = mode_invariant::hash_indexed_video(&[as_sprite]);
    assert_ne!(
        hash_a, hash_b,
        "hash_indexed_video must distinguish two pixels that share a palette_index but differ \
         in layer/sprite_id/priority -- a resolved-RGB hash could not"
    );
}

/// The control half of the WRAM mutation test above: the SAME entry with
/// the poke removed (i.e. the real corpus entry, sprite-overlay-only) must
/// pass. Without this, the test above would only prove the comparator
/// always fires, not that it discriminates. This is exactly
/// `every_corpus_entry_is_mode_invariant_over_its_full_frame_window`'s
/// first (only) entry, spelled out here explicitly so the two tests read
/// as an obvious positive/negative pair.
#[test]
fn same_entry_without_the_poke_passes_the_control() {
    let control_entry = CorpusEntry {
        name: "harness-self-test: control (sprite overlay only, no poke)",
        rom: mode_invariant::many_sprites_rom,
        frames: 10,
        enable_enhancement: |s: &mut EmuStepper| s.set_sprite_overlay_enabled(true),
    };
    assert!(
        check(&control_entry).is_none(),
        "the legitimate enhancement alone (no poke) must not diverge"
    );
}

/// Criterion 4 (R-F4): the dump mechanism itself, exercised against a real
/// divergence (the WRAM mutation-test entry) rather than trusted to work
/// because the `if let Some(div)` branch merely compiles.
#[test]
fn dump_divergence_evidence_writes_a_summary_and_a_replay_slice() {
    let poking_entry = CorpusEntry {
        name: "harness-self-test: dump mechanism",
        rom: mode_invariant::many_sprites_rom,
        frames: 5,
        enable_enhancement: |s: &mut EmuStepper| {
            s.set_sprite_overlay_enabled(true);
            s.poke_bus(0x0000, 0x99);
        },
    };
    let divergence = check(&poking_entry)
        .expect("this entry must diverge (same shape as the mutation test above)");

    let out_dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("mode-invariant-evidence-test")
        .join(poking_entry.name.replace(' ', "_"));
    let _ = std::fs::remove_dir_all(&out_dir); // start clean if a previous run left files
    dump_divergence_evidence(&divergence, &out_dir).expect("dump must succeed");

    let summary =
        std::fs::read_to_string(out_dir.join("divergence.txt")).expect("divergence.txt must exist");
    assert!(summary.contains(&format!("frame: {}", divergence.frame)));
    assert!(summary.contains(&divergence.accuracy.state_hash));
    assert!(summary.contains(&divergence.enhanced.video_hash));

    let replay = std::fs::read_to_string(out_dir.join("replay-slice.rfreplay"))
        .expect("replay-slice.rfreplay must exist");
    assert!(
        replay.starts_with("RFREPLAY"),
        "the replay slice must be real .rfreplay-formatted text, not an ad hoc dump: {replay}"
    );
    // Round-trip: the dumped slice must actually parse back as a valid log
    // (proving it's real reusable evidence, not just text that happens to
    // start with the right magic).
    rf_input::ReplayLog::parse(&replay)
        .expect("dumped replay slice must parse as a valid .rfreplay log");
}
